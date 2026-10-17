//! The terminal panel. `alacritty_terminal` interprets the escape sequences and keeps
//! the grid; this module draws that grid and encodes keyboard and mouse input.
//!
//! It draws as Windows Terminal does: real bold from the font's weight axis, italics,
//! every kind of underline in its own colour, strikethrough, lines and blocks drawn as
//! shapes so they join from cell to cell and row to row, wide and fallback glyphs fitted
//! into their cells, links opened with Ctrl+click, and mouse reports for programs that ask.

use crate::theme;
use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb};
use eframe::egui::{
    self, pos2, vec2, Color32, Event, EventFilter, FontId, Key, Modifiers, Painter, PointerButton,
    Rect, Sense, Stroke, Ui,
};
use std::sync::mpsc::{channel, Receiver, Sender};

struct Listener(Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.send(event);
    }
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct Terminal {
    pub id: u64,
    pub title: String,
    /// A name given by hand; it stays instead of the title programs set.
    pub name: Option<String>,
    /// A colour picked for it, as an index into the theme's terminal colours.
    pub color: Option<usize>,
    /// The pane of the terminal grid it is in.
    pub pane: u32,
    pub request_focus: bool,
    term: Term<Listener>,
    parser: Processor,
    events: Receiver<TermEvent>,
    cols: u16,
    rows: u16,
    cell: (f32, f32),
    /// Wheel movement not yet turned into whole lines.
    scroll_acc: f32,
    /// Whether it had the keyboard last frame, to tell programs that asked when it changes.
    had_focus: bool,
    /// The mouse button held down while a program gets the mouse, and the cell it was last
    /// reported in.
    mouse_held: Option<(u8, (usize, i32))>,
    /// The link under the pointer, underlined while it is there.
    hover_link: Option<String>,
}

/// What the widget wants sent to the remote pty after a frame.
#[derive(Default)]
pub struct Output {
    pub input: Vec<u8>,
    pub resize: Option<(u16, u16)>,
    pub focused: bool,
}

fn rgb(c: Color32) -> Rgb {
    Rgb {
        r: c.r(),
        g: c.g(),
        b: c.b(),
    }
}

fn indexed(i: u8) -> Color32 {
    match i {
        0..=15 => theme::ANSI()[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            Color32::from_rgb(level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

fn named(n: NamedColor) -> Color32 {
    let dim = |c: Color32| c.gamma_multiply(0.66);
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground => theme::TERM_FG(),
        NamedColor::Background => theme::TERM_BG(),
        NamedColor::Cursor => theme::CURSOR(),
        NamedColor::DimForeground => dim(theme::TERM_FG()),
        NamedColor::DimBlack => dim(theme::ANSI()[0]),
        NamedColor::DimRed => dim(theme::ANSI()[1]),
        NamedColor::DimGreen => dim(theme::ANSI()[2]),
        NamedColor::DimYellow => dim(theme::ANSI()[3]),
        NamedColor::DimBlue => dim(theme::ANSI()[4]),
        NamedColor::DimMagenta => dim(theme::ANSI()[5]),
        NamedColor::DimCyan => dim(theme::ANSI()[6]),
        NamedColor::DimWhite => dim(theme::ANSI()[7]),
        other => theme::ANSI()[other as usize],
    }
}

/// Colours set by the program at runtime (OSC 4/10/11) win over the theme.
fn resolve(color: Color, overrides: &Colors) -> Color32 {
    let (index, fallback) = match color {
        Color::Spec(c) => return Color32::from_rgb(c.r, c.g, c.b),
        Color::Named(n) => (n as usize, named(n)),
        Color::Indexed(i) => (i as usize, indexed(i)),
    };
    overrides[index].map_or(fallback, |c| Color32::from_rgb(c.r, c.g, c.b))
}

/// Bold text in one of the eight basic colours takes its bright twin, as Windows Terminal
/// and most others draw it.
fn brighten(color: Color) -> Color {
    match color {
        Color::Named(n) if (n as usize) < 8 => Color::Indexed(n as u8 + 8),
        Color::Indexed(i) if i < 8 => Color::Indexed(i + 8),
        other => other,
    }
}

/// `a` faded toward `b`, for dim text.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// xterm's modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_code(m: Modifiers) -> u8 {
    1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8
}

fn key_bytes(key: Key, m: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let code = modifier_code(m);
    // Cursor-style keys: `ESC [ <letter>`, `ESC O <letter>` in application mode,
    // and `ESC [ 1 ; <mod> <letter>` when modifiers are held.
    let cursor = |letter: char| {
        if code > 1 {
            format!("\x1b[1;{code}{letter}")
        } else if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{letter}")
        } else {
            format!("\x1b[{letter}")
        }
    };
    let tilde = |n: u8| {
        if code > 1 {
            format!("\x1b[{n};{code}~")
        } else {
            format!("\x1b[{n}~")
        }
    };

    let text = match key {
        Key::Enter if m.shift || m.alt => "\x1b\r".to_string(),
        Key::Enter => "\r".to_string(),
        Key::Backspace if m.ctrl => "\x17".to_string(),
        Key::Backspace if m.alt => "\x1b\x7f".to_string(),
        Key::Backspace => "\x7f".to_string(),
        Key::Tab if m.shift => "\x1b[Z".to_string(),
        Key::Tab => "\t".to_string(),
        Key::Escape => "\x1b".to_string(),
        Key::ArrowUp => cursor('A'),
        Key::ArrowDown => cursor('B'),
        Key::ArrowRight => cursor('C'),
        Key::ArrowLeft => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => cursor('P').replace("\x1b[P", "\x1bOP"),
        Key::F2 => cursor('Q').replace("\x1b[Q", "\x1bOQ"),
        Key::F3 => cursor('R').replace("\x1b[R", "\x1bOR"),
        Key::F4 => cursor('S').replace("\x1b[S", "\x1bOS"),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ if m.ctrl => {
            let byte = match key {
                Key::Space => 0,
                Key::OpenBracket => 0x1b,
                Key::Backslash => 0x1c,
                Key::CloseBracket => 0x1d,
                Key::Slash => 0x1f,
                _ => match key.name().as_bytes() {
                    [c] if c.is_ascii_alphabetic() => c.to_ascii_uppercase() & 0x1f,
                    _ => return None,
                },
            };
            let mut out = if m.alt { vec![0x1b] } else { Vec::new() };
            out.push(byte);
            return Some(out);
        }
        _ => return None,
    };
    Some(text.into_bytes())
}

/// A mouse event as the program asked for it: SGR (`ESC [ < b ; x ; y M/m`) or the
/// original X10 bytes. `button` already has the motion bit; `col` and `row` count from 0.
fn mouse_report(
    button: u8,
    m: Modifiers,
    col: usize,
    row: i32,
    pressed: bool,
    sgr: bool,
) -> Vec<u8> {
    let code = button + 4 * m.shift as u8 + 8 * m.alt as u8 + 16 * m.ctrl as u8;
    let (x, y) = (col + 1, row.max(0) as usize + 1);
    if sgr {
        format!("\x1b[<{code};{x};{y}{}", if pressed { 'M' } else { 'm' }).into_bytes()
    } else {
        // A release has no button in the old encoding.
        let code = if pressed { code } else { 3 + (code & !3) };
        let byte = |v: usize| (32 + v.min(223)) as u8;
        vec![0x1b, b'[', b'M', 32 + code, byte(x), byte(y)]
    }
}

fn button_code(button: PointerButton) -> Option<u8> {
    match button {
        PointerButton::Primary => Some(0),
        PointerButton::Middle => Some(1),
        PointerButton::Secondary => Some(2),
        _ => None,
    }
}

// ---- lines, blocks and other shapes drawn instead of taken from the font ----

/// The arms of U+2500–U+257F: up, right, down, left; 0 none, 1 light, 2 heavy, 3 double.
/// "...." marks the rounded corners and diagonals, drawn on their own.
const BOX_ARMS: &str = "\
0101020210102020010102021010202001010202101020200110021001200220\
0011001200210022110012002100220010011002200120021110121021101120\
2120221012202220101110122011102120212012102220220111011202110212\
0121012202210222110111021201120221012102220122021111111212111212\
2111112121212112221111221221221212222122222122220101020210102020\
0303303003100130033000130031003313003100330010033001300313103130\
3330101330313033031301310333130331013303131331313333000000000000\
0000000000000000000110000100001000022000020000200201102001022010";

/// How many dashes a dashed line has, or 0 for a whole one.
fn box_dashes(code: u32) -> u32 {
    match code {
        0x2504..=0x2507 => 3,
        0x2508..=0x250B => 4,
        0x254C..=0x254F => 2,
        _ => 0,
    }
}

struct Shapes<'a> {
    painter: &'a Painter,
    ppp: f32,
    /// A light line's thickness; heavy lines are twice as thick.
    light: f32,
}

impl Shapes<'_> {
    fn snap(&self, v: f32) -> f32 {
        (v * self.ppp).round() / self.ppp
    }

    fn fill(&self, x0: f32, y0: f32, x1: f32, y1: f32, color: Color32) {
        let r = Rect::from_min_max(
            pos2(self.snap(x0), self.snap(y0)),
            pos2(self.snap(x1), self.snap(y1)),
        );
        if r.width() > 0.0 && r.height() > 0.0 {
            self.painter.rect_filled(r, 0.0, color);
        }
    }

    /// A horizontal line of thickness `w` centred on `y`, from `x0` to `x1`, in dashes.
    fn hline(&self, x0: f32, x1: f32, y: f32, w: f32, dashes: u32, color: Color32) {
        let top = self.snap(y - w / 2.0);
        let w = self.snap(w).max(1.0 / self.ppp);
        if dashes == 0 {
            self.fill(x0, top, x1, top + w, color);
            return;
        }
        let step = (x1 - x0) / dashes as f32;
        for i in 0..dashes {
            let a = x0 + step * i as f32 + step * 0.15;
            self.fill(a, top, a + step * 0.7, top + w, color);
        }
    }

    fn vline(&self, y0: f32, y1: f32, x: f32, w: f32, dashes: u32, color: Color32) {
        let left = self.snap(x - w / 2.0);
        let w = self.snap(w).max(1.0 / self.ppp);
        if dashes == 0 {
            self.fill(left, y0, left + w, y1, color);
            return;
        }
        let step = (y1 - y0) / dashes as f32;
        for i in 0..dashes {
            let a = y0 + step * i as f32 + step * 0.15;
            self.fill(left, a, left + w, a + step * 0.7, color);
        }
    }

    /// Draws `c` in `r` when it is a shape this knows; false leaves it to the font.
    fn draw(&self, r: Rect, c: char, color: Color32) -> bool {
        let code = c as u32;
        match code {
            0x2500..=0x257F => self.box_char(r, code, color),
            0x2580..=0x259F => self.block(r, code, color),
            0xE0B0..=0xE0B3 => self.powerline(r, code, color),
            _ => return false,
        }
        true
    }

    fn box_char(&self, r: Rect, code: u32, color: Color32) {
        let i = (code - 0x2500) as usize * 4;
        let (cx, cy) = (
            self.snap(r.center().x - self.light / 2.0) + self.light / 2.0,
            self.snap(r.center().y - self.light / 2.0) + self.light / 2.0,
        );
        match code {
            0x256D..=0x2570 => return self.arc(r, code, cx, cy, color),
            0x2571..=0x2573 => {
                let stroke = Stroke::new(self.light, color);
                if code != 0x2572 {
                    self.painter
                        .line_segment([r.left_bottom(), r.right_top()], stroke);
                }
                if code != 0x2571 {
                    self.painter
                        .line_segment([r.left_top(), r.right_bottom()], stroke);
                }
                return;
            }
            _ => {}
        }
        let arms: Vec<u8> = BOX_ARMS.as_bytes()[i..i + 4]
            .iter()
            .map(|b| b - b'0')
            .collect();
        let (up, right, down, left) = (arms[0], arms[1], arms[2], arms[3]);
        let dashes = box_dashes(code);
        let lt = self.light;
        let width = |a: u8| match a {
            1 => lt,
            2 => lt * 2.0,
            _ => 0.0,
        };
        // Double lines sit this far either side of the centre.
        let g = lt;
        // How far past the centre a single or heavy arm reaches toward its opposite side,
        // given the arms across it: into their thickness, or, across a double line, to
        // the nearer of its two lines when nothing carries on beyond.
        let reach = |a: u8, b: u8, opposite: u8| -> f32 {
            if a == 3 || b == 3 {
                if opposite == 0 {
                    -g + lt / 2.0
                } else {
                    0.0
                }
            } else {
                width(a).max(width(b)) / 2.0
            }
        };
        // How far a line of a double arm reaches past the centre, by the arm across it on
        // that line's side: to its inner line, to a single line, or round the corner.
        let double_end = |side: u8| -> f32 {
            match side {
                3 => -g + lt / 2.0,
                1 | 2 => width(side) / 2.0,
                _ => g + lt / 2.0,
            }
        };
        // Horizontal arms.
        for (arm, toward_left) in [(left, true), (right, false)] {
            if arm == 0 {
                continue;
            }
            let edge = if toward_left { r.left() } else { r.right() };
            let sign = if toward_left { 1.0 } else { -1.0 };
            if arm == 3 {
                for (y, side) in [(cy - g, up), (cy + g, down)] {
                    let end = cx + sign * double_end(side);
                    let (a, b) = if toward_left {
                        (edge, end)
                    } else {
                        (end, edge)
                    };
                    self.hline(a, b, y, lt, dashes, color);
                }
            } else {
                let other = if toward_left { right } else { left };
                let end = cx + sign * reach(up, down, other);
                let (a, b) = if toward_left {
                    (edge, end)
                } else {
                    (end, edge)
                };
                self.hline(a, b, cy, width(arm), dashes, color);
            }
        }
        // Vertical arms.
        for (arm, toward_top) in [(up, true), (down, false)] {
            if arm == 0 {
                continue;
            }
            let edge = if toward_top { r.top() } else { r.bottom() };
            let sign = if toward_top { 1.0 } else { -1.0 };
            if arm == 3 {
                for (x, side) in [(cx - g, left), (cx + g, right)] {
                    let end = cy + sign * double_end(side);
                    let (a, b) = if toward_top { (edge, end) } else { (end, edge) };
                    self.vline(a, b, x, lt, dashes, color);
                }
            } else {
                let other = if toward_top { down } else { up };
                let end = cy + sign * reach(left, right, other);
                let (a, b) = if toward_top { (edge, end) } else { (end, edge) };
                self.vline(a, b, cx, width(arm), dashes, color);
            }
        }
    }

    /// ╭ ╮ ╯ ╰: a quarter circle joining the two arms.
    fn arc(&self, r: Rect, code: u32, cx: f32, cy: f32, color: Color32) {
        let (dx, dy): (f32, f32) = match code {
            0x256D => (1.0, 1.0),
            0x256E => (-1.0, 1.0),
            0x256F => (-1.0, -1.0),
            _ => (1.0, -1.0),
        };
        let radius = (r.width() / 2.0).min(r.height() / 2.0);
        let centre = pos2(cx + dx * radius, cy + dy * radius);
        let mut points = vec![pos2(cx, if dy > 0.0 { r.bottom() } else { r.top() })];
        let steps = 12;
        for s in 0..=steps {
            let t = s as f32 / steps as f32 * std::f32::consts::FRAC_PI_2;
            points.push(pos2(
                centre.x - dx * radius * t.cos(),
                centre.y - dy * radius * t.sin(),
            ));
        }
        points.push(pos2(if dx > 0.0 { r.right() } else { r.left() }, cy));
        self.painter
            .add(egui::Shape::line(points, Stroke::new(self.light, color)));
    }

    /// U+2580–U+259F: halves, eighths, quadrants and shades, edge to edge.
    fn block(&self, r: Rect, code: u32, color: Color32) {
        let (l, t, rr, b) = (r.left(), r.top(), r.right(), r.bottom());
        let (w, h) = (r.width(), r.height());
        let (mx, my) = (self.snap(r.center().x), self.snap(r.center().y));
        match code {
            0x2580 => self.fill(l, t, rr, my, color),
            0x2581..=0x2587 => {
                let k = (code - 0x2580) as f32;
                self.fill(l, b - h * k / 8.0, rr, b, color)
            }
            0x2588 => self.fill(l, t, rr, b, color),
            0x2589..=0x258F => {
                let k = (0x2590 - code) as f32;
                self.fill(l, t, l + w * k / 8.0, b, color)
            }
            0x2590 => self.fill(mx, t, rr, b, color),
            0x2591..=0x2593 => {
                let alpha = (code - 0x2590) as f32 * 0.25;
                self.fill(l, t, rr, b, color.gamma_multiply(alpha))
            }
            0x2594 => self.fill(l, t, rr, t + h / 8.0, color),
            0x2595 => self.fill(rr - w / 8.0, t, rr, b, color),
            _ => {
                // Quadrants: upper left 1, upper right 2, lower left 4, lower right 8.
                let bits = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14][(code - 0x2596) as usize];
                if bits & 1 != 0 {
                    self.fill(l, t, mx, my, color);
                }
                if bits & 2 != 0 {
                    self.fill(mx, t, rr, my, color);
                }
                if bits & 4 != 0 {
                    self.fill(l, my, mx, b, color);
                }
                if bits & 8 != 0 {
                    self.fill(mx, my, rr, b, color);
                }
            }
        }
    }

    /// Powerline's arrows, the full height of the row.
    fn powerline(&self, r: Rect, code: u32, color: Color32) {
        let (l, t, rr, b, cy) = (r.left(), r.top(), r.right(), r.bottom(), r.center().y);
        match code {
            0xE0B0 => self.painter.add(egui::Shape::convex_polygon(
                vec![pos2(l, t), pos2(rr, cy), pos2(l, b)],
                color,
                Stroke::NONE,
            )),
            0xE0B2 => self.painter.add(egui::Shape::convex_polygon(
                vec![pos2(rr, t), pos2(rr, b), pos2(l, cy)],
                color,
                Stroke::NONE,
            )),
            0xE0B1 => self.painter.add(egui::Shape::line(
                vec![pos2(l, t), pos2(rr, cy), pos2(l, b)],
                Stroke::new(self.light, color),
            )),
            _ => self.painter.add(egui::Shape::line(
                vec![pos2(rr, t), pos2(l, cy), pos2(rr, b)],
                Stroke::new(self.light, color),
            )),
        };
    }

    /// The underline styles, across `r`'s bottom.
    fn underline(&self, r: Rect, flags: Flags, color: Color32) {
        let lt = self.light;
        let y = r.bottom() - lt * 1.5;
        if flags.contains(Flags::DOUBLE_UNDERLINE) {
            self.hline(r.left(), r.right(), y - lt * 2.0, lt, 0, color);
            self.hline(r.left(), r.right(), y, lt, 0, color);
        } else if flags.contains(Flags::UNDERCURL) {
            let amp = (lt * 1.5).max(1.5);
            let steps = (r.width() / 2.0).ceil().max(4.0) as usize;
            let points = (0..=steps)
                .map(|s| {
                    let x = r.left() + r.width() * s as f32 / steps as f32;
                    let phase = (x - r.left()) / r.width() * std::f32::consts::TAU;
                    pos2(x, y - amp + amp * phase.sin())
                })
                .collect();
            self.painter
                .add(egui::Shape::line(points, Stroke::new(lt, color)));
        } else if flags.contains(Flags::DOTTED_UNDERLINE) {
            let dot = lt.max(1.0);
            let mut x = r.left();
            while x < r.right() {
                self.fill(
                    x,
                    y - dot / 2.0,
                    (x + dot).min(r.right()),
                    y + dot / 2.0,
                    color,
                );
                x += dot * 2.0;
            }
        } else if flags.contains(Flags::DASHED_UNDERLINE) {
            self.hline(r.left(), r.right(), y, lt, 2, color);
        } else {
            self.hline(r.left(), r.right(), y, lt, 0, color);
        }
    }
}

/// The URL around column `col` of a line of text, as Windows Terminal finds them.
fn url_at(line: &[(usize, char)], col: usize) -> Option<String> {
    let text: String = line.iter().map(|(_, c)| *c).collect();
    let at = line.iter().position(|(c, _)| *c >= col)?;
    let chars: Vec<char> = text.chars().collect();
    let ends = |c: char| c.is_whitespace() || "\"'<>`|()[]{}".contains(c);
    let mut start = at;
    while start > 0 && !ends(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < chars.len() && !ends(chars[end]) {
        end += 1;
    }
    let word: String = chars[start..end].iter().collect();
    let word = word.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    if let Some(found) = ["https://", "http://", "file://"]
        .iter()
        .filter_map(|scheme| word.find(scheme))
        .min()
    {
        return Some(word[found..].to_string()).filter(|u| u.len() > 8);
    }
    // A bare absolute path printed by a program (e.g. `C:\Users\me\pic.png`); the caller
    // only treats it as a link when it points at a file that exists.
    is_win_abs(word).then(|| word.to_string())
}

/// Is `s` an absolute Windows path (`C:\…`, `C:/…`) or a UNC path (`\\server\…`)?
fn is_win_abs(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
        || s.starts_with("\\\\")
}

/// Percent-decode `%XX` escapes in a `file://` URL's path.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Drop a trailing `:line[:col]` that editors append to a file reference, keeping the
/// drive-letter colon.
fn strip_line_suffix(p: &str) -> String {
    let mut s = p;
    for _ in 0..2 {
        match s.rfind(':') {
            Some(i)
                if i > 2 && !s[i + 1..].is_empty() && s[i + 1..].bytes().all(|c| c.is_ascii_digit()) =>
            {
                s = &s[..i]
            }
            _ => break,
        }
    }
    s.to_string()
}

/// A local file target — a `file://` URL or a bare absolute path — as a filesystem path,
/// or `None` for anything else (a web URL).
fn local_path(target: &str) -> Option<String> {
    let raw = if let Some(rest) = target.strip_prefix("file://") {
        // file:///C:/x -> C:/x, but keep a UNC path's leading slashes
        let rest = rest.strip_prefix('/').filter(|r| is_win_abs(r)).unwrap_or(rest);
        percent_decode(rest)
    } else if is_win_abs(target) {
        target.to_string()
    } else {
        return None;
    };
    Some(strip_line_suffix(&raw))
}

/// Open a local file or folder with the OS default handler (images open in Photos, etc.).
fn open_local(path: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

impl Terminal {
    pub fn new(id: u64, cols: u16, rows: u16) -> Self {
        let (tx, events) = channel();
        let size = Size {
            cols: cols as usize,
            rows: rows as usize,
        };
        Self {
            id,
            title: String::new(),
            name: None,
            color: None,
            pane: 0,
            request_focus: true,
            term: Term::new(
                Config {
                    scrolling_history: crate::settings::with(|s| s.scrollback),
                    ..Config::default()
                },
                &size,
                Listener(tx),
            ),
            parser: Processor::new(),
            events,
            cols,
            rows,
            cell: (8.0, 16.0),
            scroll_acc: 0.0,
            had_focus: false,
            mouse_held: None,
            hover_link: None,
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Feeds pty output to the emulator. Replies the terminal owes the program
    /// (cursor position reports, colour queries…) are appended to `reply`.
    pub fn feed(&mut self, data: &[u8], reply: &mut Vec<u8>, ctx: &egui::Context) {
        self.parser.advance(&mut self.term, data);
        self.drain_events(reply, ctx);
        // A synchronized update shows when it ends, or at the latest when it times out.
        if let Some(deadline) = self.parser.sync_timeout().sync_timeout() {
            ctx.request_repaint_after(
                deadline.saturating_duration_since(std::time::Instant::now()),
            );
        }
    }

    fn drain_events(&mut self, reply: &mut Vec<u8>, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                TermEvent::PtyWrite(text) => reply.extend_from_slice(text.as_bytes()),
                TermEvent::Title(title) => self.title = title,
                TermEvent::ResetTitle => self.title.clear(),
                TermEvent::ClipboardStore(_, text) => ctx.copy_text(text),
                TermEvent::ColorRequest(index, format) => {
                    let color = self.term.colors()[index].unwrap_or_else(|| match index {
                        0..=255 => rgb(indexed(index as u8)),
                        257 => rgb(theme::TERM_BG()),
                        258 => rgb(theme::CURSOR()),
                        _ => rgb(theme::TERM_FG()),
                    });
                    reply.extend_from_slice(format(color).as_bytes());
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    let size = WindowSize {
                        num_lines: self.rows,
                        num_cols: self.cols,
                        cell_width: self.cell.0 as u16,
                        cell_height: self.cell.1 as u16,
                    };
                    reply.extend_from_slice(format(size).as_bytes());
                }
                _ => {}
            }
        }
    }

    /// The text on screen, for handing the terminal's contents to Claude.
    pub fn visible_text(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        let mut current = None;
        for cell in self.term.renderable_content().display_iter {
            if current != Some(cell.point.line) {
                current = Some(cell.point.line);
                lines.push(String::new());
            }
            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                lines.last_mut().unwrap().push(cell.c);
            }
        }
        let text: Vec<&str> = lines.iter().map(|l| l.trim_end()).collect();
        text.join("\n").trim_end().to_string()
    }

    pub fn selected_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    #[allow(dead_code)] // rterm's right click uses it.
    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// Whether a program has the mouse, so a right click is its, not a paste.
    #[allow(dead_code)] // rterm's right click uses it.
    pub fn mouse_reporting(&self) -> bool {
        self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    pub fn show(&mut self, ui: &mut Ui) -> Output {
        let mut out = Output::default();
        // A synchronized update that never got its end is shown once it times out.
        if self
            .parser
            .sync_timeout()
            .sync_timeout()
            .is_some_and(|t| t <= std::time::Instant::now())
        {
            self.parser.stop_sync(&mut self.term);
            self.drain_events(&mut out.input, ui.ctx());
        }
        let font = FontId::monospace(crate::settings::with(|s| s.term_size));
        let (cw, glyph_h) = ui
            .ctx()
            .fonts_mut(|f| (f.glyph_width(&font, 'm'), f.row_height(&font)));
        let row_h = (glyph_h + 2.0).round();
        self.cell = (cw, row_h);

        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let origin = rect.min + vec2(4.0, 2.0);

        let cols = (((rect.width() - 8.0) / cw).floor() as u16).max(2);
        let rows = (((rect.height() - 4.0) / row_h).floor() as u16).max(1);
        if (cols, rows) != (self.cols, self.rows) {
            (self.cols, self.rows) = (cols, rows);
            self.term.resize(Size {
                cols: cols as usize,
                rows: rows as usize,
            });
            out.resize = Some((cols, rows));
        }

        if self.request_focus {
            self.request_focus = false;
            resp.request_focus();
        }
        let mode = *self.term.mode();
        let display_offset = self.term.grid().display_offset() as i32;

        // ---- mouse: selection, reports to programs, links and the wheel ----
        let cell_at = |p: egui::Pos2| {
            let x = ((p.x - origin.x) / cw).max(0.0);
            let col = (x.floor() as usize).min(cols as usize - 1);
            let row = (((p.y - origin.y) / row_h).floor().max(0.0) as i32).min(rows as i32 - 1);
            let side = if x.fract() < 0.5 {
                Side::Left
            } else {
                Side::Right
            };
            (
                Point::new(Line(row - display_offset), Column(col)),
                row,
                col,
                side,
            )
        };
        let modifiers = ui.input(|i| i.modifiers);
        // A program that asked for the mouse gets it; Shift keeps selecting text, as in
        // Windows Terminal.
        let reporting = mode.intersects(TermMode::MOUSE_MODE) && !modifiers.shift;
        let sgr = mode.contains(TermMode::SGR_MOUSE);
        if reporting {
            for event in ui.input(|i| i.events.clone()) {
                match event {
                    Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers,
                    } => {
                        let Some(code) = button_code(button) else {
                            continue;
                        };
                        if pressed && !rect.contains(pos) {
                            continue;
                        }
                        if !pressed && self.mouse_held.is_none() {
                            continue;
                        }
                        let (_, row, col, _) = cell_at(pos);
                        if pressed {
                            resp.request_focus();
                            self.mouse_held = Some((code, (col, row)));
                        } else {
                            self.mouse_held = None;
                        }
                        out.input
                            .extend(mouse_report(code, modifiers, col, row, pressed, sgr));
                    }
                    Event::PointerMoved(pos) => {
                        let (_, row, col, _) = cell_at(pos);
                        match self.mouse_held {
                            Some((code, last))
                                if last != (col, row)
                                    && mode.intersects(
                                        TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
                                    ) =>
                            {
                                self.mouse_held = Some((code, (col, row)));
                                out.input.extend(mouse_report(
                                    code + 32,
                                    modifiers,
                                    col,
                                    row,
                                    true,
                                    sgr,
                                ));
                            }
                            None if mode.contains(TermMode::MOUSE_MOTION) && rect.contains(pos) => {
                                out.input
                                    .extend(mouse_report(35, modifiers, col, row, true, sgr));
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        } else if let Some(p) = resp.interact_pointer_pos() {
            self.mouse_held = None;
            let (point, _, _, side) = cell_at(p);
            let (pressed, down) =
                ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down()));
            if pressed {
                resp.request_focus();
                self.term.selection = Some(Selection::new(SelectionType::Simple, point, side));
            } else if down && resp.dragged() {
                if let Some(selection) = &mut self.term.selection {
                    selection.update(point, side);
                }
            }
            if resp.double_clicked() {
                self.term.selection = Some(Selection::new(SelectionType::Semantic, point, side));
            } else if resp.triple_clicked() {
                self.term.selection = Some(Selection::new(SelectionType::Lines, point, side));
            }
        }
        if resp.hovered() {
            self.scroll_acc += ui.input(|i| i.smooth_scroll_delta().y) / row_h;
            let lines = self.scroll_acc.trunc() as i32;
            self.scroll_acc -= lines as f32;
            if lines != 0 {
                let hover = ui.input(|i| i.pointer.hover_pos()).unwrap_or(origin);
                let (_, row, col, _) = cell_at(hover);
                let up = lines > 0;
                for _ in 0..lines.abs() {
                    if mode.intersects(TermMode::MOUSE_MODE) {
                        let button = if up { 64 } else { 65 };
                        out.input
                            .extend(mouse_report(button, modifiers, col, row, true, sgr));
                    } else if mode.contains(TermMode::ALT_SCREEN) {
                        out.input.extend(if up { b"\x1bOA" } else { b"\x1bOB" });
                    }
                }
                if !mode.intersects(TermMode::MOUSE_MODE | TermMode::ALT_SCREEN) {
                    self.term.scroll_display(Scroll::Delta(lines));
                }
            }
        }
        let hover_cell = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| rect.contains(*p))
            .map(|p| cell_at(p).0);

        // ---- keyboard ----
        if resp.has_focus() {
            out.focused = true;
            let filter = EventFilter {
                tab: true,
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: true,
            };
            ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, filter));
            let alt = ui.input(|i| i.modifiers.alt && !i.modifiers.ctrl);
            let mut typed = false;
            for event in ui.input(|i| i.events.clone()) {
                match event {
                    Event::Text(text) => {
                        if alt {
                            out.input.push(0x1b);
                        }
                        out.input.extend(text.bytes());
                        typed = true;
                    }
                    Event::Paste(text) => {
                        if mode.contains(TermMode::BRACKETED_PASTE) {
                            out.input.extend(b"\x1b[200~");
                            out.input.extend(text.replace('\x1b', "").bytes());
                            out.input.extend(b"\x1b[201~");
                        } else {
                            out.input.extend(text.replace('\n', "\r").bytes());
                        }
                        typed = true;
                    }
                    // Ctrl+C copies when there is a selection and interrupts otherwise.
                    Event::Copy => match self.selected_text() {
                        Some(text) => {
                            ui.ctx().copy_text(text);
                            self.term.selection = None;
                        }
                        None => {
                            out.input.push(0x03);
                            typed = true;
                        }
                    },
                    Event::Cut => {
                        out.input.push(0x18);
                        typed = true;
                    }
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if modifiers.shift && matches!(key, Key::PageUp | Key::PageDown) {
                            let page = if key == Key::PageUp {
                                Scroll::PageUp
                            } else {
                                Scroll::PageDown
                            };
                            self.term.scroll_display(page);
                        } else if let Some(bytes) = key_bytes(key, modifiers, mode) {
                            out.input.extend(bytes);
                            typed = true;
                        }
                    }
                    _ => {}
                }
            }
            if typed {
                self.term.selection = None;
                self.term.scroll_display(Scroll::Bottom);
            }
        }
        // Programs that asked hear when the terminal gains or loses the keyboard.
        if out.focused != self.had_focus {
            self.had_focus = out.focused;
            if mode.contains(TermMode::FOCUS_IN_OUT) {
                out.input
                    .extend(if out.focused { b"\x1b[I" } else { b"\x1b[O" });
            }
        }

        // ---- paint ----
        let ppp = ui.ctx().pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let cell_rect = |row: i32, col: usize, width: usize| {
            Rect::from_min_max(
                pos2(
                    snap(origin.x + col as f32 * cw),
                    snap(origin.y + row as f32 * row_h),
                ),
                pos2(
                    snap(origin.x + (col + width) as f32 * cw),
                    snap(origin.y + (row + 1) as f32 * row_h),
                ),
            )
        };
        let shapes = Shapes {
            painter: &painter,
            ppp,
            light: ((row_h * ppp) / 18.0).round().max(1.0) / ppp,
        };
        let bold_coords = egui::epaint::text::VariationCoords::new([(b"wght", 700.0)]);
        // Bold from the code font's own bold face when it has one, else from its weight axis.
        let bold_face = theme::code_bold();
        let text_job = |text: &str, size: f32, color: Color32, bold: bool, italic: bool| {
            let mut job = egui::text::LayoutJob::default();
            let font_id = if bold && bold_face {
                FontId::new(size, egui::FontFamily::Name("code-bold".into()))
            } else {
                FontId::monospace(size)
            };
            job.append(
                text,
                0.0,
                egui::TextFormat {
                    font_id,
                    color,
                    italics: italic,
                    coords: if bold && !bold_face {
                        bold_coords.clone()
                    } else {
                        Default::default()
                    },
                    ..Default::default()
                },
            );
            job
        };

        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let cursor = content.cursor;
        let selection = content.selection;
        let cursor_visible = cursor.shape != CursorShape::Hidden && offset == 0;
        let block_cursor = cursor_visible && out.focused && cursor.shape == CursorShape::Block;
        let term_bg = theme::TERM_BG();

        // Consecutive ASCII cells of one look are drawn as a single piece of text.
        struct Run {
            text: String,
            row: i32,
            col: usize,
            fg: Color32,
            bold: bool,
            italic: bool,
        }
        let mut run: Option<Run> = None;
        let size = font.size;
        let flush = |run: &mut Option<Run>| {
            if let Some(r) = run.take() {
                let pos = cell_rect(r.row, r.col, 1).min + vec2(0.0, 1.0);
                let galley = painter.layout_job(text_job(&r.text, size, r.fg, r.bold, r.italic));
                painter.galley(pos, galley, r.fg);
            }
        };
        let mut hover_link = None;
        let mut hover_line: Vec<(usize, char)> = Vec::new();
        let ctrl = modifiers.command || modifiers.ctrl;

        for cell in content.display_iter {
            let (row, col) = (cell.point.line.0 + offset, cell.point.column.0);
            let flags = cell.flags;
            if hover_cell.is_some_and(|h| h.line == cell.point.line) {
                hover_line.push((col, cell.c));
            }
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let link = cell.hyperlink();
            if hover_cell == Some(cell.point) {
                hover_link = link.as_ref().map(|l| l.uri().to_string());
            }
            let bold = flags.contains(Flags::BOLD);
            let italic = flags.contains(Flags::ITALIC);
            let fg_color = if bold { brighten(cell.fg) } else { cell.fg };
            let mut fg = resolve(fg_color, colors);
            let mut bg = resolve(cell.bg, colors);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::DIM) {
                fg = mix(fg, bg, 0.4);
            }
            if selection.is_some_and(|s| s.contains(cell.point)) {
                bg = theme::SELECTION();
            }
            if block_cursor && cell.point == cursor.point {
                (fg, bg) = (term_bg, theme::CURSOR());
            }

            let width = if flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let r = cell_rect(row, col, width);
            if bg != term_bg {
                painter.rect_filled(r, 0.0, bg);
            }

            let c = cell.c;
            let blank = c == ' ' || c == '\0' || flags.contains(Flags::HIDDEN);
            let ascii = c.is_ascii() && !blank;
            let continues = run.as_ref().is_some_and(|run| {
                run.row == row
                    && run.col + run.text.len() == col
                    && run.fg == fg
                    && run.bold == bold
                    && run.italic == italic
            });
            if !ascii || !continues {
                flush(&mut run);
            }
            if ascii {
                match &mut run {
                    Some(run) => run.text.push(c),
                    None => {
                        run = Some(Run {
                            text: c.to_string(),
                            row,
                            col,
                            fg,
                            bold,
                            italic,
                        })
                    }
                }
            } else if !blank && !shapes.draw(r, c, fg) {
                // Drawn alone, fitted into its cells and centred in them, so a glyph from a
                // fallback font can't spill over its neighbours.
                let text = c.to_string();
                let mut galley = painter.layout_job(text_job(&text, size, fg, bold, italic));
                let room = r.width();
                if galley.size().x > room * 1.04 {
                    let scaled = size * room / galley.size().x;
                    galley = painter.layout_job(text_job(&text, scaled, fg, bold, italic));
                }
                let gs = galley.size();
                let pos = pos2(
                    r.left() + ((room - gs.x) / 2.0).max(0.0),
                    if gs.y < row_h - 2.5 {
                        r.top() + (row_h - gs.y) / 2.0
                    } else {
                        r.top() + 1.0
                    },
                );
                painter.galley(pos, galley, fg);
            }

            let mut underline = flags & Flags::ALL_UNDERLINES;
            let hovered_link = link.is_some()
                && hover_link.is_some()
                && link.as_ref().map(|l| l.uri()) == self.hover_link.as_deref();
            if hovered_link && underline.is_empty() {
                underline = Flags::UNDERLINE;
            }
            if !underline.is_empty() && !blank || !underline.is_empty() && c == ' ' {
                let color = cell.underline_color().map_or(fg, |u| resolve(u, colors));
                shapes.underline(r, underline, color);
            }
            if flags.contains(Flags::STRIKEOUT) {
                shapes.hline(r.left(), r.right(), r.center().y + 1.0, shapes.light, 0, fg);
            }
        }
        flush(&mut run);

        // Links: the one under the pointer is underlined, and Ctrl+click opens it, as do
        // web addresses written out in the text.
        let plain_url = hover_cell
            .filter(|_| hover_link.is_none())
            .and_then(|h| url_at(&hover_line, h.column.0))
            // a bare path is a link only when it is a file that exists on disk
            .filter(|u| {
                !is_win_abs(u) || local_path(u).is_some_and(|p| std::path::Path::new(&p).exists())
            });
        let target = hover_link.clone().or(plain_url);
        self.hover_link = hover_link;
        if let Some(url) = &target {
            if ctrl {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                if resp.clicked() {
                    match local_path(url) {
                        Some(path) => open_local(&path),
                        None => ui.ctx().open_url(egui::OpenUrl::new_tab(url)),
                    }
                }
            }
            if resp.hovered() {
                resp.clone()
                    .on_hover_text_at_pointer(format!("{url}\nCtrl+click to open"));
            }
        }

        if cursor_visible && !block_cursor {
            let r = cell_rect(cursor.point.line.0, cursor.point.column.0, 1);
            match cursor.shape {
                CursorShape::Beam => {
                    painter.rect_filled(
                        Rect::from_min_size(r.min, vec2(2.0, r.height())),
                        0.0,
                        theme::CURSOR(),
                    );
                }
                CursorShape::Underline => {
                    shapes.hline(
                        r.left(),
                        r.right(),
                        r.bottom() - 1.0,
                        2.0,
                        0,
                        theme::CURSOR(),
                    );
                }
                // A block in an unfocused terminal is drawn hollow.
                _ => {
                    painter.rect_stroke(
                        r.shrink(0.5),
                        0.0,
                        (1.0, theme::CURSOR()),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_arms_cover_the_block() {
        assert_eq!(BOX_ARMS.len(), 128 * 4);
        let arms = |c: char| {
            let i = (c as u32 - 0x2500) as usize * 4;
            &BOX_ARMS[i..i + 4]
        };
        assert_eq!(arms('─'), "0101");
        assert_eq!(arms('┼'), "1111");
        assert_eq!(arms('╋'), "2222");
        assert_eq!(arms('╔'), "0330");
        assert_eq!(arms('╬'), "3333");
        assert_eq!(arms('╡'), "1013");
        assert_eq!(arms('╿'), "2010");
        assert_eq!(arms('┌'), "0110");
        assert_eq!(arms('┘'), "1001");
    }

    #[test]
    fn mouse_reports_as_programs_expect() {
        let none = Modifiers::NONE;
        assert_eq!(mouse_report(0, none, 4, 2, true, true), b"\x1b[<0;5;3M");
        assert_eq!(mouse_report(0, none, 4, 2, false, true), b"\x1b[<0;5;3m");
        assert_eq!(
            mouse_report(0, none, 0, 0, true, false),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
        assert_eq!(
            mouse_report(0, none, 0, 0, false, false),
            vec![0x1b, b'[', b'M', 35, 33, 33]
        );
    }

    #[test]
    fn urls_are_found_in_text() {
        let line: Vec<(usize, char)> = "see https://example.com/a?b=1. ok"
            .chars()
            .enumerate()
            .collect();
        assert_eq!(
            url_at(&line, 8).as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(url_at(&line, 1), None);
    }

    #[test]
    fn local_paths_are_links_and_resolve() {
        assert!(is_win_abs("C:\\Users\\me\\a.png"));
        assert!(is_win_abs("C:/Users/me/a.png"));
        assert!(is_win_abs("\\\\srv\\share\\a"));
        assert!(!is_win_abs("hello"));
        assert_eq!(local_path("C:/x/a.rs:12:3").as_deref(), Some("C:/x/a.rs"));
        assert_eq!(
            local_path("file:///C:/Users/me/a%20b.png").as_deref(),
            Some("C:/Users/me/a b.png")
        );
        assert_eq!(local_path("https://x.com/a").as_deref(), None);
    }
}
