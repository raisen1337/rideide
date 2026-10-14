//! The terminal panel. `alacritty_terminal` interprets the escape sequences and keeps
//! the grid; this module draws that grid and encodes keyboard and mouse input.

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
    self, vec2, Align2, Color32, Event, EventFilter, FontId, Key, Modifiers, Rect, Sense, Ui,
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
    pub request_focus: bool,
    term: Term<Listener>,
    parser: Processor,
    events: Receiver<TermEvent>,
    cols: u16,
    rows: u16,
    cell: (f32, f32),
    /// Wheel movement not yet turned into whole lines.
    scroll_acc: f32,
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
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Feeds pty output to the emulator. Replies the terminal owes the program
    /// (cursor position reports, colour queries…) are appended to `reply`.
    pub fn feed(&mut self, data: &[u8], reply: &mut Vec<u8>, ctx: &egui::Context) {
        self.parser.advance(&mut self.term, data);
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

    fn selected_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    pub fn show(&mut self, ui: &mut Ui) -> Output {
        let mut out = Output::default();
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

        // ---- mouse: selection and wheel ----
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
        if let Some(p) = resp.interact_pointer_pos() {
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
                    if mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE) {
                        let button = if up { 64 } else { 65 };
                        out.input
                            .extend(format!("\x1b[<{button};{};{}M", col + 1, row + 1).bytes());
                    } else if mode.contains(TermMode::ALT_SCREEN) {
                        out.input.extend(if up { b"\x1bOA" } else { b"\x1bOB" });
                    }
                }
                if !mode.intersects(TermMode::MOUSE_MODE | TermMode::ALT_SCREEN) {
                    self.term.scroll_display(Scroll::Delta(lines));
                }
            }
        }

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
            for event in ui.input(|i| i.events.clone()) {
                match event {
                    Event::Text(text) => {
                        if alt {
                            out.input.push(0x1b);
                        }
                        out.input.extend(text.bytes());
                    }
                    Event::Paste(text) => {
                        if mode.contains(TermMode::BRACKETED_PASTE) {
                            out.input.extend(b"\x1b[200~");
                            out.input.extend(text.replace('\x1b', "").bytes());
                            out.input.extend(b"\x1b[201~");
                        } else {
                            out.input.extend(text.replace('\n', "\r").bytes());
                        }
                    }
                    // Ctrl+C copies when there is a selection and interrupts otherwise.
                    Event::Copy => match self.selected_text() {
                        Some(text) => {
                            ui.ctx().copy_text(text);
                            self.term.selection = None;
                        }
                        None => out.input.push(0x03),
                    },
                    Event::Cut => out.input.push(0x18),
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
                        }
                    }
                    _ => {}
                }
            }
            if !out.input.is_empty() {
                self.term.selection = None;
                self.term.scroll_display(Scroll::Bottom);
            }
        }

        // ---- paint ----
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let cursor = content.cursor;
        let selection = content.selection;
        let cursor_visible = cursor.shape != CursorShape::Hidden && offset == 0;
        let block_cursor = cursor_visible && out.focused && cursor.shape == CursorShape::Block;

        // Consecutive ASCII cells of one colour are drawn as a single piece of text.
        let mut run = String::new();
        let mut run_at = (0i32, 0usize, theme::TERM_FG());
        let flush = |run: &mut String, at: (i32, usize, Color32)| {
            if !run.is_empty() {
                let pos = origin + vec2(at.1 as f32 * cw, at.0 as f32 * row_h + 1.0);
                painter.text(pos, Align2::LEFT_TOP, run.as_str(), font.clone(), at.2);
                run.clear();
            }
        };

        for cell in content.display_iter {
            let (row, col) = (cell.point.line.0 + offset, cell.point.column.0);
            let flags = cell.flags;
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let mut fg = resolve(cell.fg, colors);
            let mut bg = resolve(cell.bg, colors);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::DIM) {
                fg = fg.gamma_multiply(0.66);
            }
            if selection.is_some_and(|s| s.contains(cell.point)) {
                bg = theme::SELECTION();
            }
            if block_cursor && cell.point == cursor.point {
                (fg, bg) = (theme::TERM_BG(), theme::CURSOR());
            }

            let width = if flags.contains(Flags::WIDE_CHAR) {
                2.0
            } else {
                1.0
            };
            let min = origin + vec2(col as f32 * cw, row as f32 * row_h);
            let cell_rect = Rect::from_min_size(min, vec2(cw * width, row_h));
            if bg != theme::TERM_BG() {
                painter.rect_filled(cell_rect, 0.0, bg);
            }
            if flags.contains(Flags::UNDERLINE) {
                painter.hline(cell_rect.x_range(), cell_rect.max.y - 1.0, (1.0, fg));
            }

            let c = cell.c;
            let blank = c == ' ' || c == '\0' || flags.contains(Flags::HIDDEN);
            let continues = run_at.0 == row && run_at.1 + run.len() == col && run_at.2 == fg;
            if blank || !c.is_ascii() || !continues {
                flush(&mut run, run_at);
            }
            if blank {
                continue;
            }
            if c.is_ascii() {
                if run.is_empty() {
                    run_at = (row, col, fg);
                }
                run.push(c);
            } else {
                // Drawn alone, so a glyph from a fallback font can't shift its neighbours.
                painter.text(min + vec2(0.0, 1.0), Align2::LEFT_TOP, c, font.clone(), fg);
            }
        }
        flush(&mut run, run_at);

        if cursor_visible && !block_cursor {
            let min = origin
                + vec2(
                    cursor.point.column.0 as f32 * cw,
                    cursor.point.line.0 as f32 * row_h,
                );
            let cell_rect = Rect::from_min_size(min, vec2(cw, row_h));
            match cursor.shape {
                CursorShape::Beam => {
                    painter.rect_filled(
                        Rect::from_min_size(min, vec2(2.0, row_h)),
                        0.0,
                        theme::CURSOR(),
                    );
                }
                CursorShape::Underline => {
                    painter.hline(
                        cell_rect.x_range(),
                        cell_rect.max.y - 1.0,
                        (2.0, theme::CURSOR()),
                    );
                }
                // A block in an unfocused terminal is drawn hollow.
                _ => {
                    painter.rect_stroke(
                        cell_rect.shrink(0.5),
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
