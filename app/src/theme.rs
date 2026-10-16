//! Colours, sizes and the egui style shared by the whole UI. The colours come from the
//! current `Palette`, which can be one of the built-in ones or made from a VS Code theme.

#![allow(non_snake_case)]

use eframe::egui::{
    self, vec2, Color32, Context, CornerRadius, FontFamily, FontId, Frame, Margin, Stroke,
    TextStyle,
};
use serde_json::Value;
use std::cell::Cell;
use std::sync::Arc;

/// Claude's orange, for the sparkle and other marks of the assistant. Not themed.
const CLAUDE_ORANGE: Color32 = Color32::from_rgb(0xd9, 0x77, 0x57);

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// Behind the title bar and the status bar.
    pub window_bg: Color32,
    pub editor_bg: Color32,
    /// Side panels: files, chat, agents.
    pub panel_bg: Color32,
    /// The hairlines between panes.
    pub line: Color32,
    pub current_line: Color32,
    pub selection: Color32,
    pub cursor: Color32,
    pub line_number: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub keyword: Color32,
    pub type_: Color32,
    pub func: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
    pub punct: Color32,
    /// Colours only a language server can tell apart (semantic highlighting).
    pub variable: Color32,
    pub param: Color32,
    pub property: Color32,
    pub constant: Color32,
    pub term_fg: Color32,
    /// The 16 ANSI colours: normal, then bright.
    pub ansi: [Color32; 16],
    /// The whole interface looks like a terminal: one monospaced font everywhere, square
    /// corners, hard shadows, and a shell prompt in the title bar.
    pub tty: bool,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const DARK_ANSI: [Color32; 16] = [
    rgb(0x3b4048),
    rgb(0xe06c75),
    rgb(0x98c379),
    rgb(0xe5c07b),
    rgb(0x61afef),
    rgb(0xc678dd),
    rgb(0x56b6c2),
    rgb(0xc8ccd4),
    rgb(0x6b717d),
    rgb(0xf0808a),
    rgb(0xa9d68a),
    rgb(0xf2d08c),
    rgb(0x78bfff),
    rgb(0xd68cec),
    rgb(0x66c9d6),
    rgb(0xffffff),
];

const LIGHT_ANSI: [Color32; 16] = [
    rgb(0x1f1f1f),
    rgb(0xc4382c),
    rgb(0x1f8a3b),
    rgb(0x9a6a00),
    rgb(0x2f6fde),
    rgb(0x8a3fc7),
    rgb(0x12808f),
    rgb(0x8a8a8a),
    rgb(0x5a5a5a),
    rgb(0xde4f43),
    rgb(0x2ca04d),
    rgb(0xb88200),
    rgb(0x4a86ee),
    rgb(0xa35ada),
    rgb(0x1c9aab),
    rgb(0x1f1f1f),
];

/// Neutral greys, in the manner of Cursor's dark theme.
pub const GRAPHITE: Palette = Palette {
    dark: true,
    window_bg: rgb(0x181818),
    editor_bg: rgb(0x1c1c1c),
    panel_bg: rgb(0x181818),
    line: rgb(0x2a2a2a),
    current_line: rgb(0x222222),
    selection: rgb(0x33404f),
    cursor: rgb(0xd6d6d6),
    line_number: rgb(0x5a5a5a),
    accent: rgb(0x87b4ff),
    text: rgb(0xd6d6d6),
    keyword: rgb(0x83a8d9),
    type_: rgb(0x8fc7b9),
    func: rgb(0xd9c98a),
    string: rgb(0xc9a26b),
    number: rgb(0xc98f8f),
    comment: rgb(0x6a6f76),
    punct: rgb(0xa9a9a9),
    variable: rgb(0xc4ccd6),
    param: rgb(0xc9b3dd),
    property: rgb(0xa7c3c8),
    constant: rgb(0xd0a4a4),
    term_fg: rgb(0xd6d6d6),
    ansi: DARK_ANSI,
    tty: false,
};

/// Graphite with a cool, blue-grey cast.
pub const SLATE: Palette = Palette {
    window_bg: rgb(0x14161b),
    editor_bg: rgb(0x181b21),
    panel_bg: rgb(0x14161b),
    line: rgb(0x262a33),
    current_line: rgb(0x1d2028),
    selection: rgb(0x2b3650),
    cursor: rgb(0xd3d8e2),
    line_number: rgb(0x555c6b),
    accent: rgb(0x7aa2f7),
    text: rgb(0xd3d8e2),
    keyword: rgb(0x9aa9ff),
    type_: rgb(0x7fd0c0),
    func: rgb(0x7fc1e8),
    string: rgb(0xa8cf96),
    number: rgb(0xe0a57c),
    comment: rgb(0x626a7a),
    punct: rgb(0xa3abba),
    variable: rgb(0xc3cde0),
    param: rgb(0xcfb6f2),
    property: rgb(0x99c6dc),
    constant: rgb(0xe8b48f),
    term_fg: rgb(0xd3d8e2),
    ..GRAPHITE
};

/// The light counterpart.
pub const DAY: Palette = Palette {
    dark: false,
    window_bg: rgb(0xf7f7f7),
    editor_bg: rgb(0xffffff),
    panel_bg: rgb(0xf7f7f7),
    line: rgb(0xe3e3e3),
    current_line: rgb(0xf5f5f5),
    selection: rgb(0xcfe0ff),
    cursor: rgb(0x1f1f1f),
    line_number: rgb(0xa8a8a8),
    accent: rgb(0x2f6fde),
    text: rgb(0x1f1f1f),
    keyword: rgb(0x2f5fbf),
    type_: rgb(0x12808f),
    func: rgb(0x6b4fbb),
    string: rgb(0x9a5b13),
    number: rgb(0xb3462e),
    comment: rgb(0x9a9a9a),
    punct: rgb(0x555555),
    variable: rgb(0x1f3b5c),
    param: rgb(0x7a3e9d),
    property: rgb(0x2c5d75),
    constant: rgb(0xa8432d),
    term_fg: rgb(0x1f1f1f),
    ansi: LIGHT_ANSI,
    tty: false,
};

/// A Linux console: near-black with a green cast, the xterm 256-colour greens, cyans and
/// blues a shell prompt is drawn in, and the terminal look for the whole interface.
pub const TERMINAL: Palette = Palette {
    dark: true,
    window_bg: rgb(0x090b09),
    editor_bg: rgb(0x0c0f0c),
    panel_bg: rgb(0x090b09),
    line: rgb(0x1e281e),
    current_line: rgb(0x121812),
    selection: rgb(0x1f3d26),
    cursor: rgb(0x5fd75f),
    line_number: rgb(0x3d553d),
    accent: rgb(0x5fd75f),
    text: rgb(0xc5d1c2),
    keyword: rgb(0x87d75f),
    type_: rgb(0x5fd7d7),
    func: rgb(0x87afff),
    string: rgb(0xd7d787),
    number: rgb(0xd787d7),
    comment: rgb(0x5c745c),
    punct: rgb(0x97a597),
    variable: rgb(0xc5d1c2),
    param: rgb(0xd7af87),
    property: rgb(0x87d7af),
    constant: rgb(0xffaf5f),
    term_fg: rgb(0xc5d1c2),
    ansi: [
        rgb(0x1c201c),
        rgb(0xd75f5f),
        rgb(0x5fd75f),
        rgb(0xd7d75f),
        rgb(0x5f87d7),
        rgb(0xaf5fd7),
        rgb(0x5fafaf),
        rgb(0xc5d1c2),
        rgb(0x4e5a4e),
        rgb(0xff7b7b),
        rgb(0x87ff87),
        rgb(0xffff87),
        rgb(0x87afff),
        rgb(0xd787ff),
        rgb(0x87d7d7),
        rgb(0xffffff),
    ],
    tty: true,
};

thread_local! {
    // The UI runs on one thread, so the palette needs no locking.
    static CURRENT: Cell<Palette> = const { Cell::new(GRAPHITE) };
}

fn current() -> Palette {
    CURRENT.with(Cell::get)
}

/// Switches the palette and restyles egui to match.
pub fn set(ctx: &Context, palette: Palette) {
    CURRENT.with(|c| c.set(palette));
    apply(ctx);
}

// The colours are read through these, so call sites follow the current theme.
pub fn WINDOW_BG() -> Color32 {
    current().window_bg
}
pub fn EDITOR_BG() -> Color32 {
    current().editor_bg
}
pub fn PANEL_BG() -> Color32 {
    current().panel_bg
}
pub fn LINE() -> Color32 {
    current().line
}
pub fn CURRENT_LINE() -> Color32 {
    current().current_line
}
pub fn SELECTION() -> Color32 {
    current().selection
}
pub fn CURSOR() -> Color32 {
    current().cursor
}
pub fn LINE_NUMBER() -> Color32 {
    current().line_number
}
pub fn ACCENT() -> Color32 {
    current().accent
}
pub fn CLAUDE() -> Color32 {
    CLAUDE_ORANGE
}
pub fn TEXT() -> Color32 {
    current().text
}
pub fn KEYWORD() -> Color32 {
    current().keyword
}
pub fn TYPE() -> Color32 {
    current().type_
}
pub fn FUNC() -> Color32 {
    current().func
}
pub fn STRING() -> Color32 {
    current().string
}
pub fn NUMBER() -> Color32 {
    current().number
}
pub fn COMMENT() -> Color32 {
    current().comment
}
pub fn PUNCT() -> Color32 {
    current().punct
}
pub fn VARIABLE() -> Color32 {
    current().variable
}
pub fn PARAM() -> Color32 {
    current().param
}
pub fn PROPERTY() -> Color32 {
    current().property
}
pub fn CONSTANT() -> Color32 {
    current().constant
}
/// The terminal's default background as programs see it; the pane behind supplies the fill.
pub fn TERM_BG() -> Color32 {
    current().editor_bg
}
pub fn TERM_FG() -> Color32 {
    current().term_fg
}
pub fn ANSI() -> [Color32; 16] {
    current().ansi
}
/// The terminal look is on (see `Palette::tty`).
pub fn tty() -> bool {
    current().tty
}
/// Secondary interface text: the text colour, stepped back toward the background.
/// Unlike a syntax colour, it stays neutral whatever the theme does with its tokens.
pub fn MUTED() -> Color32 {
    let p = current();
    mix(p.panel_bg, p.text, 0.72)
}
/// For text that should stand out from `TEXT`: headings, names.
pub fn STRONG() -> Color32 {
    if current().dark {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

thread_local! {
    /// How opaque the main window's panes are while it has a blurred backdrop; `None` when it has none.
    static GLASS: Cell<Option<f32>> = const { Cell::new(None) };
}

pub fn set_glass(opacity: Option<f32>) {
    GLASS.with(|g| g.set(opacity));
}

pub fn glass_opacity() -> Option<f32> {
    GLASS.with(Cell::get)
}

/// A pane fill that lets the backdrop through when glass is on. `extra` makes a pane
/// more opaque than the rest, for the ones that hold code.
fn see_through(color: Color32, extra: f32) -> Color32 {
    match glass_opacity() {
        Some(opacity) => {
            let alpha = ((opacity + extra).clamp(0.0, 1.0) * 255.0) as u8;
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
        }
        None => color,
    }
}

/// The colour laid over the blurred backdrop under every pane, as red, green, blue and
/// alpha: the theme's own background, so what shows through is darkened (or lightened)
/// toward the theme instead of washing it out.
pub fn glass_tint() -> [u8; 4] {
    let bg = current().window_bg;
    let alpha = 0.5;
    [bg.r(), bg.g(), bg.b(), (alpha * 255.0) as u8]
}

/// Fill for the side panels.
pub fn glass() -> Color32 {
    see_through(current().panel_bg, 0.0)
}

/// Fill for the editor and terminal.
pub fn surface() -> Color32 {
    see_through(current().editor_bg, 0.12)
}

/// Fill for the title bar and the status bar.
pub fn chrome() -> Color32 {
    see_through(current().window_bg, 0.0)
}

/// A faint overlay for hover states and chips: white on dark themes, black on light ones.
pub fn tint(alpha: u8) -> Color32 {
    if current().dark {
        Color32::from_white_alpha(alpha)
    } else {
        Color32::from_black_alpha(alpha)
    }
}

/// A pane of the window: a plain fill with some padding. Panes meet edge to edge.
pub fn card(fill: Color32) -> Frame {
    Frame::NONE.fill(fill).inner_margin(Margin::same(8))
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

fn luminance(c: Color32) -> f32 {
    (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) / 255.0
}

#[allow(dead_code)] // rterm, rdrop and rkit use it; ride picks its code font.
pub fn install_fonts(ctx: &Context) {
    install_fonts_with(ctx, None, None, Vec::new());
}

/// Whether the code font came with a bold face of its own (for fonts that aren't
/// variable); the terminal draws bold text in the "code-bold" family then.
static CODE_BOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn code_bold() -> bool {
    CODE_BOLD.load(std::sync::atomic::Ordering::Relaxed)
}

/// The fonts, with `code` (or Cascadia Mono) for code and terminals, its `bold` face if it
/// has one, and `named` fonts in families of their own, for previews.
pub fn install_fonts_with(
    ctx: &Context,
    code: Option<egui::FontData>,
    bold: Option<egui::FontData>,
    named: Vec<(String, egui::FontData)>,
) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or("C:\\Windows".into()))
        .join("Fonts");
    let icons = include_bytes!("../assets/SymbolsNerdFontMono-Regular.ttf");
    fonts
        .font_data
        .insert("icons".into(), Arc::new(egui::FontData::from_static(icons)));

    // System fonts first, then the icon font, then egui's bundled fonts as a fallback.
    let system = [
        ("ui", "segoeui.ttf", FontFamily::Proportional),
        ("code", "CascadiaMono.ttf", FontFamily::Monospace),
    ];
    let mut code = code;
    for (name, file, family) in system {
        let list = fonts.families.entry(family).or_default();
        list.insert(0, "icons".into());
        let data = match (name, code.take()) {
            ("code", Some(data)) => Some(data),
            _ => std::fs::read(dir.join(file))
                .ok()
                .map(egui::FontData::from_owned),
        };
        if let Some(data) = data {
            fonts.font_data.insert(name.into(), Arc::new(data));
            list.insert(0, name.into());
        }
    }
    CODE_BOLD.store(bold.is_some(), std::sync::atomic::Ordering::Relaxed);
    let mut extra: Vec<(String, egui::FontData)> = named;
    if let Some(bold) = bold {
        extra.push(("code-bold".into(), bold));
    }
    let mut extra_names = Vec::new();
    for (name, data) in extra {
        fonts.font_data.insert(name.clone(), Arc::new(data));
        extra_names.push(name);
    }
    if let Ok(bytes) = std::fs::read(dir.join("seguisym.ttf")) {
        fonts.font_data.insert(
            "symbols".into(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("symbols".into());
        }
    }
    // Chinese, Japanese and Korean, last: large files, so they are mapped from the disk
    // rather than read in, and cost memory only for the characters that show.
    let cjk = [
        ("cjk-sc", "msyh.ttc"),
        ("cjk-jp", "YuGothM.ttc"),
        ("cjk-kr", "malgun.ttf"),
    ];
    for (name, file) in cjk {
        if let Some(bytes) = map_file(&dir.join(file)) {
            fonts
                .font_data
                .insert(name.into(), Arc::new(egui::FontData::from_static(bytes)));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push(name.into());
            }
        }
    }
    // Each named font in a family of its own, falling back as code does.
    let rest: Vec<String> = fonts
        .families
        .get(&FontFamily::Monospace)
        .map(|l| l.iter().skip(1).cloned().collect())
        .unwrap_or_default();
    for name in extra_names {
        let mut list = vec![name.clone()];
        list.extend(rest.iter().cloned());
        fonts
            .families
            .insert(FontFamily::Name(name.as_str().into()), list);
    }
    FONTS.with(|f| *f.borrow_mut() = Some(fonts.clone()));
    FONTS_TTY.with(|f| f.set(Some(tty())));
    ctx.set_fonts(tty_fonts(fonts));
}

thread_local! {
    /// The fonts last installed, and whether they were given out with the terminal look.
    static FONTS: std::cell::RefCell<Option<egui::FontDefinitions>> =
        const { std::cell::RefCell::new(None) };
    static FONTS_TTY: Cell<Option<bool>> = const { Cell::new(None) };
}

/// With the terminal look, interface text is drawn in the code font too.
fn tty_fonts(mut fonts: egui::FontDefinitions) -> egui::FontDefinitions {
    if tty() {
        if let Some(mono) = fonts.families.get(&FontFamily::Monospace).cloned() {
            fonts.families.insert(FontFamily::Proportional, mono);
        }
    }
    fonts
}

/// Installs the fonts again when the terminal look has been switched on or off.
fn sync_fonts(ctx: &Context) {
    let tty = tty();
    if FONTS_TTY.with(|f| f.replace(Some(tty))) == Some(tty) {
        return;
    }
    if let Some(fonts) = FONTS.with(|f| f.borrow().clone()) {
        ctx.set_fonts(tty_fonts(fonts));
    }
}

/// With the terminal look, takes the rounding off every rectangle painted this frame,
/// including the ones drawn with a radius of their own.
pub fn square_corners(ctx: &Context) {
    if !tty() {
        return;
    }
    fn square(shape: &mut egui::Shape) {
        match shape {
            egui::Shape::Rect(rect) => rect.corner_radius = CornerRadius::ZERO,
            egui::Shape::Vec(shapes) => shapes.iter_mut().for_each(square),
            _ => {}
        }
    }
    let layers: Vec<egui::LayerId> = ctx.memory(|m| m.layer_ids().collect());
    ctx.graphics_mut(|graphics| {
        for layer in layers.into_iter().chain([egui::LayerId::background()]) {
            if let Some(list) = graphics.get_mut(layer) {
                for i in 0..list.next_idx().0 {
                    list.mutate_shape(egui::layers::ShapeIdx(i), |s| square(&mut s.shape));
                }
            }
        }
    });
}

/// A file mapped into memory for as long as the program runs: its pages are read from the
/// disk when touched and shared with other programs mapping it.
#[cfg(windows)]
fn map_file(path: &std::path::Path) -> Option<&'static [u8]> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *const std::ffi::c_void,
            disposition: u32,
            flags: u32,
            template: isize,
        ) -> isize;
        fn GetFileSizeEx(file: isize, size: *mut i64) -> i32;
        fn CreateFileMappingW(
            file: isize,
            security: *const std::ffi::c_void,
            protect: u32,
            high: u32,
            low: u32,
            name: *const u16,
        ) -> isize;
        fn MapViewOfFile(map: isize, access: u32, high: u32, low: u32, bytes: usize) -> *const u8;
        fn CloseHandle(handle: isize) -> i32;
    }
    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 1;
    const OPEN_EXISTING: u32 = 3;
    const PAGE_READONLY: u32 = 2;
    const FILE_MAP_READ: u32 = 4;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        let file = CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        );
        if file == -1 {
            return None;
        }
        let mut size = 0i64;
        let ok = GetFileSizeEx(file, &mut size) != 0 && size > 0;
        let map = if ok {
            CreateFileMappingW(
                file,
                std::ptr::null(),
                PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            )
        } else {
            0
        };
        CloseHandle(file);
        if map == 0 {
            return None;
        }
        let view = MapViewOfFile(map, FILE_MAP_READ, 0, 0, 0);
        // The view keeps the mapping alive; the handle isn't needed any more.
        CloseHandle(map);
        if view.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(view, size as usize))
    }
}

#[cfg(not(windows))]
fn map_file(path: &std::path::Path) -> Option<&'static [u8]> {
    std::fs::read(path)
        .ok()
        .map(|b| &*Box::leak(b.into_boxed_slice()))
}

/// Styles egui from the current palette.
pub fn apply(ctx: &Context) {
    let p = current();
    sync_fonts(ctx);
    ctx.set_theme(if p.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(19.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = vec2(8.0, 6.0);
        style.spacing.button_padding = vec2(8.0, 4.0);
        style.spacing.menu_margin = Margin::same(5);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = true;

        let v = &mut style.visuals;
        v.dark_mode = p.dark;
        v.override_text_color = Some(p.text);
        // Panes paint their own fills.
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = mix(p.panel_bg, p.text, 0.05);
        v.window_stroke = Stroke::new(1.0, p.line);
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(10);
        // Menus and popups float clearly above what is under them.
        v.popup_shadow = egui::epaint::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(if p.dark { 120 } else { 50 }),
        };
        v.window_shadow = egui::epaint::Shadow {
            offset: [0, 12],
            blur: 36,
            spread: 0,
            color: Color32::from_black_alpha(if p.dark { 140 } else { 60 }),
        };
        v.extreme_bg_color = mix(
            p.panel_bg,
            if p.dark {
                Color32::BLACK
            } else {
                Color32::WHITE
            },
            0.35,
        );
        v.faint_bg_color = tint(6);
        // Selected rows, tabs and toggles: a quiet step up from the background, not a colour.
        v.selection.bg_fill = mix(p.panel_bg, p.text, 0.14);
        v.selection.stroke = Stroke::new(1.0, p.text);
        v.hyperlink_color = p.accent;

        // Only what you click or type in is rounded, and only slightly.
        let radius = CornerRadius::same(6);
        v.widgets.noninteractive.corner_radius = radius;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        for (widget, fill) in [
            (&mut v.widgets.inactive, tint(10)),
            (&mut v.widgets.hovered, tint(22)),
            (&mut v.widgets.active, tint(34)),
            (&mut v.widgets.open, tint(22)),
        ] {
            widget.corner_radius = radius;
            widget.weak_bg_fill = fill;
            widget.bg_fill = fill;
            widget.bg_stroke = Stroke::NONE;
            widget.fg_stroke = Stroke::new(1.0, p.text);
            widget.expansion = 0.0;
        }

        if p.tty {
            // One monospaced font, a little smaller, since its letters are wider.
            style.text_styles = [
                (TextStyle::Heading, FontId::monospace(16.0)),
                (TextStyle::Body, FontId::monospace(13.0)),
                (TextStyle::Button, FontId::monospace(13.0)),
                (TextStyle::Small, FontId::monospace(11.5)),
                (TextStyle::Monospace, FontId::monospace(13.0)),
            ]
            .into();
            // Boxes as a text interface draws them: edges in the prompt's green, and hard
            // shadows like a dialog in a console.
            let edge = mix(p.panel_bg, p.accent, 0.45);
            v.window_stroke = Stroke::new(1.0, edge);
            v.window_fill = p.panel_bg;
            let hard = |offset: i8| egui::epaint::Shadow {
                offset: [offset, offset],
                blur: 0,
                spread: 0,
                color: Color32::from_black_alpha(200),
            };
            v.popup_shadow = hard(4);
            v.window_shadow = hard(6);
            v.selection.bg_fill = mix(p.panel_bg, p.accent, 0.22);
            v.selection.stroke = Stroke::new(1.0, p.accent);
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.line);
            v.widgets.hovered.bg_stroke = Stroke::new(1.0, edge);
            v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
            v.widgets.open.bg_stroke = Stroke::new(1.0, edge);
            v.text_cursor.stroke = Stroke::new(2.0, p.cursor);
        }
    });
}

// ---- VS Code themes ----

/// Parses JSON as VS Code writes it: with comments and trailing commas.
pub fn parse_jsonc(text: &str) -> Result<Value, String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                out.extend(chars.next());
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = ' ';
                for skipped in chars.by_ref() {
                    if previous == '*' && skipped == '/' {
                        break;
                    }
                    previous = skipped;
                }
            }
            // A comma directly before a closing bracket is dropped.
            (',', _) => {
                let mut ahead = chars.clone();
                let next = loop {
                    match ahead.next() {
                        Some(n) if n.is_whitespace() => continue,
                        other => break other,
                    }
                };
                if !matches!(next, Some('}' | ']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    serde_json::from_str(out.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())
}

/// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`; a translucent colour is flattened onto `over`.
fn hex(text: &str, over: Color32) -> Option<Color32> {
    let digits = text.trim().strip_prefix('#')?;
    let digits: String = match digits.len() {
        3 | 4 => digits.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => digits.to_string(),
        _ => return None,
    };
    let byte = |i: usize| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok();
    let color = Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?);
    Some(match digits.len() {
        8 => mix(over, color, byte(6)? as f32 / 255.0),
        _ => color,
    })
}

/// Builds a palette from a VS Code colour theme. Whatever the theme leaves out comes
/// from Graphite (or Day, for a light theme).
pub fn from_vscode(theme: &Value) -> Palette {
    let colors = &theme["colors"];
    let get = |key: &str, over: Color32| colors[key].as_str().and_then(|s| hex(s, over));

    let editor_bg = get("editor.background", Color32::BLACK);
    // The editor's own lightness decides; a theme's declared type can disagree with its colours.
    let dark = match (editor_bg, theme["type"].as_str()) {
        (Some(bg), _) => luminance(bg) < 0.5,
        (None, Some("light" | "hcLight")) => false,
        (None, _) => true,
    };
    let base = if dark { GRAPHITE } else { DAY };
    let editor_bg = editor_bg.unwrap_or(base.editor_bg);
    let first = |keys: &[&str]| keys.iter().find_map(|k| get(k, editor_bg));
    let text = first(&["editor.foreground", "foreground"]).unwrap_or(base.text);
    let panel_bg = first(&["sideBar.background", "panel.background"]).unwrap_or(editor_bg);

    // Token colours: the rule whose scope is the longest prefix of what we look for wins.
    let mut rules: Vec<(String, Color32)> = Vec::new();
    for rule in theme["tokenColors"].as_array().into_iter().flatten() {
        let Some(color) = rule["settings"]["foreground"]
            .as_str()
            .and_then(|s| hex(s, editor_bg))
        else {
            continue;
        };
        let scopes: Vec<&str> = match &rule["scope"] {
            Value::String(s) => s.split(',').collect(),
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        for scope in scopes {
            // "source.js keyword" applies to `keyword` inside JavaScript; the last part is what counts here.
            if let Some(last) = scope.split_whitespace().last() {
                rules.push((last.to_string(), color));
            }
        }
    }
    let token = |wanted: &[&str], fallback: Color32| {
        wanted
            .iter()
            .find_map(|scope| {
                rules
                    .iter()
                    .filter(|(rule, _)| scope == rule || scope.starts_with(&format!("{rule}.")))
                    .max_by_key(|(rule, _)| rule.len())
                    .map(|(_, color)| *color)
            })
            .unwrap_or(fallback)
    };

    // A theme's own colours for what language servers tell apart, then the TextMate scopes
    // that mean the same.
    let semantic = |names: &[&str]| {
        names.iter().find_map(|name| {
            let rule = &theme["semanticTokenColors"][*name];
            let text = rule.as_str().or(rule["foreground"].as_str())?;
            hex(text, editor_bg)
        })
    };
    let variable = semantic(&["variable"])
        .unwrap_or_else(|| token(&["variable.other.readwrite", "variable"], text));
    let param =
        semantic(&["parameter"]).unwrap_or_else(|| token(&["variable.parameter"], variable));
    let property = semantic(&["property"]).unwrap_or_else(|| {
        token(
            &[
                "variable.other.property",
                "variable.other.object.property",
                "support.variable.property",
                "meta.object-literal.key",
            ],
            variable,
        )
    });
    let number = token(&["constant.numeric", "constant"], text);
    let constant = semantic(&["enumMember", "variable.readonly"]).unwrap_or_else(|| {
        token(
            &[
                "variable.other.constant",
                "variable.other.enummember",
                "constant.other",
            ],
            number,
        )
    });

    let ansi_names = [
        "Black",
        "Red",
        "Green",
        "Yellow",
        "Blue",
        "Magenta",
        "Cyan",
        "White",
        "BrightBlack",
        "BrightRed",
        "BrightGreen",
        "BrightYellow",
        "BrightBlue",
        "BrightMagenta",
        "BrightCyan",
        "BrightWhite",
    ];
    let mut ansi = base.ansi;
    for (slot, name) in ansi.iter_mut().zip(ansi_names) {
        if let Some(color) = get(&format!("terminal.ansi{name}"), editor_bg) {
            *slot = color;
        }
    }

    let palette = Palette {
        dark,
        window_bg: first(&[
            "titleBar.activeBackground",
            "activityBar.background",
            "editorGroupHeader.tabsBackground",
        ])
        .unwrap_or(panel_bg),
        editor_bg,
        panel_bg,
        line: first(&[
            "panel.border",
            "sideBar.border",
            "editorGroup.border",
            "tab.border",
            "contrastBorder",
        ])
        .unwrap_or(mix(editor_bg, text, 0.12)),
        current_line: first(&["editor.lineHighlightBackground"])
            .unwrap_or(mix(editor_bg, text, 0.05)),
        selection: first(&["editor.selectionBackground"]).unwrap_or(mix(editor_bg, text, 0.2)),
        cursor: first(&["editorCursor.foreground"]).unwrap_or(text),
        line_number: first(&["editorLineNumber.foreground"]).unwrap_or(mix(editor_bg, text, 0.35)),
        accent: first(&[
            "focusBorder",
            "button.background",
            "activityBarBadge.background",
            "textLink.foreground",
        ])
        .unwrap_or(base.accent),
        text,
        keyword: token(
            &["keyword.control", "keyword", "storage.type", "storage"],
            text,
        ),
        type_: token(
            &[
                "entity.name.type",
                "support.type",
                "entity.name.class",
                "support.class",
                "storage.type",
            ],
            text,
        ),
        func: token(
            &[
                "entity.name.function",
                "support.function",
                "meta.function-call",
            ],
            text,
        ),
        string: token(&["string.quoted", "string"], text),
        number,
        comment: token(&["comment.line", "comment"], mix(editor_bg, text, 0.45)),
        punct: token(
            &["punctuation", "keyword.operator"],
            mix(editor_bg, text, 0.75),
        ),
        variable,
        param,
        property,
        constant,
        term_fg: first(&["terminal.foreground"]).unwrap_or(text),
        ansi,
        tty: false,
    };
    legible(palette)
}

/// WCAG contrast ratio between two colours, from 1 (identical) to 21 (black on white).
fn contrast(a: Color32, b: Color32) -> f32 {
    let linear = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let level =
        |c: Color32| 0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b());
    let (la, lb) = (level(a), level(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Makes sure a palette can be read. ride draws every pane's text in one colour, while
/// a VS Code theme may pair a dark editor with a light side bar or title bar, each with
/// its own text colour; taken as is, that gives light text on a light pane. Any colour
/// that would be unreadable here is replaced by one derived from the editor's.
fn legible(mut p: Palette) -> Palette {
    // Text has to read on the editor itself, or nothing else can be fixed around it.
    if contrast(p.text, p.editor_bg) < 4.5 {
        p.text = if p.dark { GRAPHITE.text } else { DAY.text };
    }
    if contrast(p.text, p.editor_bg) < 4.5 {
        p.editor_bg = if p.dark {
            GRAPHITE.editor_bg
        } else {
            DAY.editor_bg
        };
    }
    let (bg, text) = (p.editor_bg, p.text);
    let shade = if p.dark {
        Color32::BLACK
    } else {
        rgb(0x808080)
    };

    // Panes carry the same text as the editor, so they must sit on the same side of it.
    for pane in [&mut p.panel_bg, &mut p.window_bg] {
        if contrast(text, *pane) < 4.5 {
            *pane = mix(bg, shade, 0.14);
        }
    }
    // Highlights lie under text.
    if contrast(text, p.selection) < 3.0 {
        p.selection = mix(bg, text, 0.22);
    }
    if contrast(text, p.current_line) < 4.5 {
        p.current_line = mix(bg, text, 0.05);
    }
    // A separator as bright as the text would shout; one that reads as a pane edge is enough.
    if contrast(p.line, bg) > 6.0 {
        p.line = mix(bg, text, 0.14);
    }
    // Text colours that would vanish into the background fall back to plain text.
    for color in [
        &mut p.keyword,
        &mut p.type_,
        &mut p.func,
        &mut p.string,
        &mut p.number,
        &mut p.punct,
        &mut p.variable,
        &mut p.param,
        &mut p.property,
        &mut p.constant,
        &mut p.cursor,
        &mut p.term_fg,
    ] {
        if contrast(*color, bg) < 2.5 {
            *color = text;
        }
    }
    for dim in [&mut p.comment, &mut p.line_number] {
        if contrast(*dim, bg) < 1.8 {
            *dim = mix(bg, text, 0.45);
        }
    }
    if contrast(p.accent, bg) < 1.8 {
        p.accent = if p.dark { GRAPHITE.accent } else { DAY.accent };
    }
    p
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_comments_and_trailing_commas_are_accepted() {
        let v = parse_jsonc(
            "{\n  // a comment\n  \"a\": \"x // not a comment\", /* block */\n  \"b\": [1, 2,],\n}",
        )
        .unwrap();
        assert_eq!(v["a"], "x // not a comment");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
    }

    #[test]
    fn hex_colours_in_every_length() {
        assert_eq!(hex("#fff", Color32::BLACK), Some(Color32::WHITE));
        assert_eq!(
            hex("#102030", Color32::BLACK),
            Some(Color32::from_rgb(0x10, 0x20, 0x30))
        );
        // Half-transparent white over black is mid grey.
        assert_eq!(
            hex("#ffffff80", Color32::BLACK),
            Some(Color32::from_rgb(128, 128, 128))
        );
        assert_eq!(hex("red", Color32::BLACK), None);
    }

    #[test]
    fn a_vscode_theme_becomes_a_palette() {
        let theme = parse_jsonc(
            r##"{
                "type": "dark",
                "colors": { "editor.background": "#282a36", "editor.foreground": "#f8f8f2", "terminal.ansiRed": "#ff5555" },
                "tokenColors": [
                    { "scope": ["comment", "punctuation.definition.comment"], "settings": { "foreground": "#6272a4" } },
                    { "scope": "keyword, storage", "settings": { "foreground": "#ff79c6" } },
                    { "scope": "source.js string", "settings": { "foreground": "#f1fa8c" } },
                    { "scope": "entity.name.function", "settings": { "foreground": "#50fa7b" } }
                ]
            }"##,
        )
        .unwrap();
        let p = from_vscode(&theme);
        assert!(p.dark);
        assert_eq!(p.editor_bg, rgb(0x282a36));
        assert_eq!(p.keyword, rgb(0xff79c6));
        assert_eq!(p.comment, rgb(0x6272a4));
        assert_eq!(p.string, rgb(0xf1fa8c));
        assert_eq!(p.func, rgb(0x50fa7b));
        assert_eq!(p.ansi[1], rgb(0xff5555));
        // Not in the theme, so it falls back to the text colour.
        assert_eq!(p.number, p.text);
    }
    /// The shape of "Sublime Monokai": a dark editor with a light side bar and title bar,
    /// each meant to carry dark text of its own.
    #[test]
    fn a_theme_with_light_panes_around_a_dark_editor_stays_readable() {
        let theme = parse_jsonc(
            r##"{
                "type": "dark",
                "colors": {
                    "editor.background": "#282923", "editor.foreground": "#f8f8f2",
                    "sideBar.background": "#EBEDEF", "sideBar.foreground": "#141411",
                    "titleBar.activeBackground": "#DBDDE0", "activityBar.background": "#EBEDEF",
                    "editor.selectionBackground": "#f8f8f2", "panel.border": "#ffffff"
                },
                "tokenColors": [{ "scope": "keyword", "settings": { "foreground": "#282924" } }]
            }"##,
        )
        .unwrap();
        let p = from_vscode(&theme);
        for (name, pane) in [
            ("panel", p.panel_bg),
            ("window", p.window_bg),
            ("editor", p.editor_bg),
        ] {
            assert!(
                contrast(p.text, pane) >= 4.5,
                "{name} background must carry the text colour"
            );
        }
        assert!(
            contrast(p.text, p.selection) >= 3.0,
            "selected text must stay readable"
        );
        assert!(
            contrast(p.keyword, p.editor_bg) >= 2.5,
            "a keyword the colour of the background is replaced"
        );
        assert!(
            contrast(p.line, p.editor_bg) <= 6.0,
            "separators stay quiet"
        );
    }

    #[test]
    fn every_built_in_palette_passes_its_own_legibility_check() {
        for palette in [GRAPHITE, SLATE, DAY, TERMINAL] {
            let checked = legible(palette);
            assert_eq!(checked.panel_bg, palette.panel_bg);
            assert_eq!(checked.text, palette.text);
            assert_eq!(checked.selection, palette.selection);
        }
    }
}
