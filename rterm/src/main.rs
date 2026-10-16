#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! rterm: a terminal for Windows, made from ride's. Tabs run PowerShell, Command Prompt,
//! Git Bash, WSL or SSH in a pseudo console, under the same theme and glass as ride.

// The terminal widget, the colours and the icons are ride's own files, shared as they are.
#[allow(dead_code)]
#[path = "../../app/src/icons.rs"]
mod icons;
#[allow(dead_code)]
#[path = "../../app/src/term.rs"]
mod term;
#[allow(dead_code)]
#[path = "../../app/src/theme.rs"]
mod theme;

mod handoff;
mod os;
mod profiles;
mod pty;
mod settings;
mod split;
#[path = "../../app/src/userenv.rs"]
mod userenv;
mod window;

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Context, CornerRadius, Event, FontId, Frame, Key,
    Layout, Margin, Modifiers, Panel, Rect, RichText, Sense, Ui, ViewportBuilder, ViewportCommand,
};
use profiles::Profile;
use pty::Pty;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use term::Terminal;
use window::{bar_button, resize_edges, title_drag, window_buttons};

const TITLE_H: f32 = 38.0;

fn main() -> eframe::Result {
    // `--version` answers rkit, which shows what is installed.
    if std::env::args().any(|arg| arg == "--version") {
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // `--default-terminal=on|off` makes rterm Windows' default terminal or gives it back,
    // for the setup program and scripts.
    if let Some(value) =
        std::env::args().find_map(|a| a.strip_prefix("--default-terminal=").map(str::to_string))
    {
        let result = handoff::set_default(value == "on");
        if let Err(e) = &result {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "{e}");
        }
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    let launch = Launch::from_args();
    // The same graphics setup as ride: a DirectComposition swapchain, which the glass
    // backdrop needs, and the GPU allocator told to keep memory low.
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .presentation_system = eframe::wgpu::Dx12SwapchainKind::DxgiFromVisual;
        setup.device_descriptor = Arc::new(|_adapter| eframe::wgpu::DeviceDescriptor {
            label: Some("rterm"),
            required_limits: eframe::wgpu::Limits {
                max_texture_dimension_2d: 8192,
                ..Default::default()
            },
            memory_hints: eframe::wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        });
    }
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([360.0, 200.0])
            .with_title("rterm")
            .with_icon(egui::IconData {
                rgba: include_bytes!(concat!(env!("OUT_DIR"), "/icon-128.rgba")).to_vec(),
                width: 128,
                height: 128,
            })
            // The system frame stays in the window's style and is hidden by `os::adopt_window`.
            .with_decorations(true)
            .with_transparent(true),
        wgpu_options,
        persistence_path: std::env::var_os("APPDATA")
            .map(|dir| PathBuf::from(dir).join("rterm").join("window.ron")),
        ..Default::default()
    };
    eframe::run_native(
        "rterm",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc, launch)))),
    )
}

/// What the command line asks for: `rterm [-d DIR] [-p PROFILE] [COMMAND ARGS…]`.
struct Launch {
    dir: PathBuf,
    profile: Option<String>,
    command: Vec<String>,
}

impl Launch {
    fn from_args() -> Self {
        let mut args = std::env::args().skip(1);
        let (mut dir, mut profile, mut command) = (None, None, Vec::new());
        while let Some(arg) = args.next() {
            match arg.as_str() {
                // COM starting rterm to take a console handed over by Windows.
                "-Embedding" | "/Embedding" => {}
                "-d" | "--dir" | "--startingDirectory" => dir = args.next().map(clean_dir),
                "-p" | "--profile" => profile = args.next(),
                "--" => command.extend(args.by_ref()),
                _ => {
                    command.push(arg);
                    command.extend(args.by_ref());
                }
            }
        }
        Self {
            dir: dir.unwrap_or_else(default_dir),
            profile,
            command,
        }
    }
}

/// A folder as Explorer passes it. For a drive it ends in a backslash, which escapes the
/// closing quote, so `"C:\"` arrives as `C:"`.
fn clean_dir(arg: String) -> PathBuf {
    let trimmed = arg.trim_end_matches('"');
    if trimmed.ends_with(':') {
        PathBuf::from(format!("{trimmed}\\"))
    } else {
        PathBuf::from(trimmed)
    }
}

/// The folder a plain start opens in: the one it was started from, as typing `cmd` in
/// Explorer's address bar does, unless that is only where Windows or rterm itself lives.
fn default_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let here = std::env::current_dir().ok();
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from));
    match here {
        Some(here)
            if !windows.as_ref().is_some_and(|w| here.starts_with(w))
                && exe_dir.as_ref() != Some(&here) =>
        {
            here
        }
        _ => home.unwrap_or_else(|| PathBuf::from("C:\\")),
    }
}

/// One terminal and the program running in it.
struct Term {
    id: u64,
    term: Terminal,
    pty: Option<Pty>,
    profile: Profile,
    /// The program has ended in a way worth reading, and the terminal waits for Enter or Esc.
    ended: bool,
}

impl Term {
    fn title(&self) -> String {
        profiles::tidy_title(&self.term.title)
            .map(str::to_string)
            .unwrap_or_else(|| self.profile.name.clone())
    }
}

/// A pane of the window's grid: its own row of tabs, one terminal shown at a time.
struct Pane {
    id: u64,
    terms: Vec<Term>,
    active: usize,
}

impl Pane {
    fn shown(&self) -> &Term {
        &self.terms[self.active.min(self.terms.len() - 1)]
    }

    fn shown_mut(&mut self) -> &mut Term {
        let index = self.active.min(self.terms.len() - 1);
        &mut self.terms[index]
    }

    fn select(&mut self, index: usize) {
        if index < self.terms.len() {
            self.active = index;
            self.terms[index].term.request_focus = true;
        }
    }
}

/// A tab being dragged by its title: which terminal, and where the drag started.
#[derive(Clone, Copy)]
struct Drag {
    term: u64,
    from: u64,
}

/// What the tab rows asked for this frame.
enum TabAct {
    Select(u64, usize),
    Close(u64),
    New(u64, Profile),
    Split(u64, split::Zone, Profile),
    CloseOthers(u64),
}

struct App {
    panes: Vec<Pane>,
    layout: split::Node,
    /// The pane that has the keyboard.
    focus: u64,
    /// Where each pane was drawn last, for splitting along the longer side, moving focus
    /// and dropping tabs.
    rects: Vec<(u64, Rect)>,
    drag: Option<Drag>,
    profiles: Vec<Profile>,
    /// WSL distributions and SSH hosts, still being looked for.
    more_profiles: Option<Receiver<Vec<Profile>>>,
    dir: PathBuf,
    next_id: u64,
    adopted: bool,
    /// The backdrop last given to the window: its tint and whether it is dark.
    applied: Option<(Option<[u8; 4]>, bool)>,
    theme_shown: String,
    /// Console programs Windows hands to rterm as the default terminal.
    handoffs: Receiver<handoff::Handoff>,
    /// Why rterm couldn't be made the default terminal, shown in Settings.
    default_error: Option<String>,
}

fn palette(name: &str) -> theme::Palette {
    match name {
        "slate" => theme::SLATE,
        "day" => theme::DAY,
        _ => theme::GRAPHITE,
    }
}

/// The height of a pane's own row of tabs, when there are several panes.
const STRIP_H: f32 = 34.0;

impl App {
    fn new(cc: &eframe::CreationContext, launch: Launch) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install_fonts(ctx);
        let name = settings::with(|s| s.theme.clone());
        theme::set(ctx, palette(&name));
        // Ctrl with + and - sizes the terminal's text, not the whole window.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);

        let (tx, more_profiles) = std::sync::mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(profiles::slow());
            repaint.request_repaint();
        });
        let mut app = Self {
            panes: Vec::new(),
            layout: split::Node::Pane(0),
            focus: 0,
            rects: Vec::new(),
            drag: None,
            profiles: profiles::local(),
            more_profiles: Some(more_profiles),
            dir: launch.dir,
            next_id: 1,
            adopted: false,
            applied: None,
            theme_shown: name,
            handoffs: handoff::listen(ctx.clone()),
            default_error: None,
        };
        // As the default terminal, the registration follows rterm and Windows Terminal's
        // proxy wherever updates move them.
        std::thread::spawn(handoff::keep_registration);
        // Started by Windows to show a console program: its tab is the first one.
        if handoff::started_by_com() {
            match app
                .handoffs
                .recv_timeout(std::time::Duration::from_secs(15))
            {
                Ok(h) => {
                    let term = app.handed_term(ctx, h);
                    let pane = app.new_pane(term);
                    app.layout = split::Node::Pane(pane);
                    app.focus = pane;
                    return app;
                }
                Err(_) => std::process::exit(0),
            }
        }
        let first = if let Some((program, args)) = launch.command.split_first() {
            let mut profile = app.default_profile();
            profile.name = PathBuf::from(program)
                .file_stem()
                .map_or(program.clone(), |s| s.to_string_lossy().into_owned());
            profile.program = program.into();
            profile.args = args.to_vec();
            profile.env.clear();
            profile
        } else {
            launch
                .profile
                .and_then(|name| app.find_profile(&name))
                .unwrap_or_else(|| app.default_profile())
        };
        let term = app.new_term(ctx, first);
        let pane = app.new_pane(term);
        app.layout = split::Node::Pane(pane);
        app.focus = pane;
        // RTERM_SPLIT=right,down splits the window that way at the start, to look at panes
        // without the keyboard.
        for way in std::env::var("RTERM_SPLIT").unwrap_or_default().split(',') {
            match way {
                "right" => app.split_focused(ctx, Some(true)),
                "down" => app.split_focused(ctx, Some(false)),
                _ => {}
            }
        }
        app
    }

    fn find_profile(&self, name: &str) -> Option<Profile> {
        self.profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .cloned()
    }

    fn default_profile(&self) -> Profile {
        let wanted = settings::with(|s| s.default_profile.clone());
        self.find_profile(&wanted)
            .unwrap_or_else(|| self.profiles[0].clone())
    }

    fn next(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// A terminal running `profile`; if it can't start, the terminal says why.
    fn new_term(&mut self, ctx: &Context, profile: Profile) -> Term {
        let id = self.next();
        let mut term = Terminal::new(id, 120, 30);
        let (pty, ended) = match Pty::spawn(&profile, &self.dir, term.size(), ctx) {
            Ok(pty) => (Some(pty), false),
            Err(error) => {
                let note = format!(
                    "\x1b[31mCouldn't start {}\x1b[0m\r\n{error}\r\n",
                    profile.name
                );
                term.feed(note.as_bytes(), &mut Vec::new(), ctx);
                (None, true)
            }
        };
        Term {
            id,
            term,
            pty,
            profile,
            ended,
        }
    }

    /// A tab for a console program Windows handed over.
    fn handed_term(&mut self, ctx: &Context, h: handoff::Handoff) -> Term {
        let id = self.next();
        let mut profile = self.default_profile();
        let program = PathBuf::from(&h.program);
        profile.name = program
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Console".into());
        if !h.program.is_empty() {
            profile.program = program;
            profile.args.clear();
            profile.env.clear();
        }
        let mut term = Terminal::new(id, 120, 30);
        if !h.title.is_empty() {
            term.title = h.title.clone();
        }
        let pty = Pty::from_handoff(h, ctx);
        Term {
            id,
            term,
            pty: Some(pty),
            profile,
            ended: false,
        }
    }

    /// A new pane holding `term`, not yet placed in the grid. Returns its id.
    fn new_pane(&mut self, term: Term) -> u64 {
        let id = self.next();
        self.panes.push(Pane {
            id,
            terms: vec![term],
            active: 0,
        });
        id
    }

    fn pane(&self, id: u64) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    fn pane_mut(&mut self, id: u64) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    fn focused(&self) -> &Pane {
        self.pane(self.focus).unwrap_or(&self.panes[0])
    }

    fn focus_on(&mut self, id: u64) {
        self.focus = id;
        if let Some(pane) = self.pane_mut(id) {
            pane.shown_mut().term.request_focus = true;
        }
    }

    /// Opens `profile` as a new tab of pane `pane`.
    fn open_in(&mut self, ctx: &Context, pane: u64, profile: Profile) {
        let term = self.new_term(ctx, profile);
        if let Some(p) = self.pane_mut(pane) {
            p.terms.push(term);
            p.active = p.terms.len() - 1;
        }
        self.focus_on(pane);
    }

    /// Puts `term` in a new pane on the `zone` side of pane `target`.
    fn place_beside(&mut self, target: u64, zone: split::Zone, term: Term) {
        let id = self.new_pane(term);
        let across = matches!(zone, split::Zone::Left | split::Zone::Right);
        let before = matches!(zone, split::Zone::Left | split::Zone::Above);
        self.layout.split(target, id, across, before);
        self.focus_on(id);
    }

    /// Splits the focused pane with a new terminal of the same profile: beside it, under
    /// it, or (with `None`) along its longer side.
    fn split_focused(&mut self, ctx: &Context, across: Option<bool>) {
        let focus = self.focus;
        let across = across.unwrap_or_else(|| {
            let rect = self
                .rects
                .iter()
                .find(|(id, _)| *id == focus)
                .map(|(_, r)| *r);
            // Cells are about twice as tall as wide, so a pane goes side by side when it
            // is wider than it is tall, give or take.
            rect.is_none_or(|r| r.width() >= r.height())
        });
        let profile = self.focused().shown().profile.clone();
        let term = self.new_term(ctx, profile);
        let zone = if across {
            split::Zone::Right
        } else {
            split::Zone::Below
        };
        self.place_beside(focus, zone, term);
    }

    /// Takes a terminal out of its pane, closing the pane when it was its last.
    fn take_term(&mut self, id: u64) -> Option<Term> {
        let index = self
            .panes
            .iter()
            .position(|p| p.terms.iter().any(|t| t.id == id))?;
        let pane = &mut self.panes[index];
        let at = pane.terms.iter().position(|t| t.id == id)?;
        let term = pane.terms.remove(at);
        if pane.active > at || pane.active >= pane.terms.len() {
            pane.active = pane.active.saturating_sub(1);
        }
        if pane.terms.is_empty() {
            let gone = pane.id;
            self.panes.remove(index);
            self.layout.remove(gone);
            if self.focus == gone {
                if let Some(&next) = self.layout.panes().first() {
                    self.focus_on(next);
                }
            }
        } else {
            let pane_id = pane.id;
            if self.focus == pane_id {
                self.focus_on(pane_id);
            }
        }
        Some(term)
    }

    /// Closes a terminal; the window goes with the last one.
    fn close_term(&mut self, ctx: &Context, id: u64) {
        self.take_term(id);
        if self.panes.is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    /// Where a dragged tab was let go: in among another pane's tabs, or beside a pane.
    fn drop_tab(&mut self, drag: Drag, target: u64, zone: split::Zone) {
        let alone = self.pane(drag.from).is_some_and(|p| p.terms.len() == 1);
        // A pane's only tab, dropped on itself, stays where it is.
        if target == drag.from && (zone == split::Zone::Into || alone) {
            return;
        }
        let Some(term) = self.take_term(drag.term) else {
            return;
        };
        // Taking the tab may have closed its pane; the target is still there, as it isn't
        // that pane (or the pane had other tabs).
        if zone == split::Zone::Into {
            if let Some(p) = self.pane_mut(target) {
                p.terms.push(term);
                p.active = p.terms.len() - 1;
            }
            self.focus_on(target);
        } else {
            self.place_beside(target, zone, term);
        }
    }

    fn restart(&mut self, ctx: &Context, id: u64) {
        let dir = self.dir.clone();
        let Some(t) = self
            .panes
            .iter_mut()
            .flat_map(|p| p.terms.iter_mut())
            .find(|t| t.id == id)
        else {
            return;
        };
        t.pty = None;
        match Pty::spawn(&t.profile, &dir, t.term.size(), ctx) {
            Ok(pty) => {
                t.term.feed(b"\x1b[2J\x1b[3J\x1b[H", &mut Vec::new(), ctx);
                t.pty = Some(pty);
                t.ended = false;
            }
            Err(error) => {
                let note = format!("\r\n\x1b[31m{error}\x1b[0m\r\n");
                t.term.feed(note.as_bytes(), &mut Vec::new(), ctx);
            }
        }
    }

    /// Feeds every terminal's output to it, shown or not, and notices programs ending.
    fn poll(&mut self, ctx: &Context) {
        // Console programs started elsewhere, handed to rterm by Windows: a tab each, and
        // the window comes forward.
        while let Ok(h) = self.handoffs.try_recv() {
            let term = self.handed_term(ctx, h);
            let focus = self.focus;
            if let Some(pane) = self.pane_mut(focus) {
                pane.terms.push(term);
                let last = pane.terms.len() - 1;
                pane.select(last);
            }
            // RTERM_NO_FOCUS keeps a test window from taking the keyboard.
            if std::env::var_os("RTERM_NO_FOCUS").is_none() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        if let Some(more) = &self.more_profiles {
            if let Ok(found) = more.try_recv() {
                self.profiles.extend(found);
                self.more_profiles = None;
            }
        }
        let mut close = Vec::new();
        for t in self.panes.iter_mut().flat_map(|p| p.terms.iter_mut()) {
            let Some(pty) = &mut t.pty else { continue };
            let mut reply = Vec::new();
            while let Ok(data) = pty.output.try_recv() {
                t.term.feed(&data, &mut reply, ctx);
            }
            if !reply.is_empty() {
                pty.write(&reply);
            }
            match pty.exit_code() {
                // A program that ends cleanly takes its tab with it.
                Some(0) => close.push(t.id),
                Some(code) => {
                    let note = format!(
                        "\r\n\x1b[90m[process exited with code {code}]\r\nPress Enter to start it again, or Esc to close.\x1b[0m"
                    );
                    t.term.feed(note.as_bytes(), &mut Vec::new(), ctx);
                    t.pty = None;
                    t.ended = true;
                }
                None => {}
            }
        }
        for id in close {
            self.close_term(ctx, id);
        }
    }

    fn sync_look(&mut self, ctx: &Context, frame: &eframe::Frame) {
        let name = settings::with(|s| s.theme.clone());
        if name != self.theme_shown {
            theme::set(ctx, palette(&name));
            self.theme_shown = name;
        }
        if !std::mem::replace(&mut self.adopted, true) {
            os::adopt_window(frame);
        }
        let (glass, opacity) = settings::with(|s| (s.glass, s.glass_opacity));
        theme::set_glass(glass.then_some(opacity));
        let dark = theme::STRONG() == Color32::WHITE;
        let tint = glass.then(theme::glass_tint);
        if self.applied == Some((tint, dark)) {
            return;
        }
        self.applied = Some((tint, dark));
        if !os::set_backdrop(frame, tint, dark) && glass {
            settings::update(|s| s.glass = false);
            theme::set_glass(None);
        }
    }

    fn shortcuts(&mut self, ctx: &Context) {
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
        let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
        let focus = self.focus;
        if take_key(ctx, ctrl_shift, Key::T) {
            let profile = self.default_profile();
            self.open_in(ctx, focus, profile);
        }
        if take_key(ctx, ctrl_shift, Key::W) {
            let id = self.focused().shown().id;
            self.close_term(ctx, id);
            if self.panes.is_empty() {
                return;
            }
        }
        if take_key(ctx, ctrl_shift, Key::D) {
            let profile = self.focused().shown().profile.clone();
            self.open_in(ctx, self.focus, profile);
        }
        if take_key(ctx, ctrl_shift, Key::N) {
            new_window();
        }
        // Splits, as in Windows Terminal.
        if take_key(ctx, alt_shift, Key::D) {
            self.split_focused(ctx, None);
        }
        if take_key(ctx, alt_shift, Key::Equals) || take_key(ctx, alt_shift, Key::Plus) {
            self.split_focused(ctx, Some(true));
        }
        if take_key(ctx, alt_shift, Key::Minus) {
            self.split_focused(ctx, Some(false));
        }
        for (key, dir) in [
            (Key::ArrowLeft, split::Dir::Left),
            (Key::ArrowRight, split::Dir::Right),
            (Key::ArrowUp, split::Dir::Up),
            (Key::ArrowDown, split::Dir::Down),
        ] {
            if take_key(ctx, Modifiers::ALT, key) {
                if let Some(next) = split::neighbour(&self.rects, self.focus, dir) {
                    self.focus_on(next);
                }
            }
        }
        // Moving between the tabs of the pane in focus.
        let focus = self.focus;
        let count = self.focused().terms.len();
        let active = self.focused().active;
        if take_key(ctx, Modifiers::CTRL, Key::Tab) {
            self.pane_mut(focus).map(|p| p.select((active + 1) % count));
        }
        if take_key(ctx, ctrl_shift, Key::Tab) {
            self.pane_mut(focus)
                .map(|p| p.select((active + count - 1) % count));
        }
        const DIGITS: [Key; 9] = [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
        ];
        for (n, key) in DIGITS.into_iter().enumerate() {
            // Ctrl+Alt+number goes to that tab, Ctrl+Shift+number opens that profile.
            if take_key(ctx, Modifiers::CTRL | Modifiers::ALT, key) {
                let index = if n == 8 { count - 1 } else { n };
                self.pane_mut(focus).map(|p| p.select(index));
            }
            if take_key(ctx, ctrl_shift, key) {
                if let Some(profile) = self.profiles.get(n).cloned() {
                    self.open_in(ctx, focus, profile);
                }
            }
        }
        let mut grow = 0.0;
        for key in [Key::Equals, Key::Plus] {
            if take_key(ctx, Modifiers::CTRL, key) {
                grow += 1.0;
            }
        }
        if take_key(ctx, Modifiers::CTRL, Key::Minus) {
            grow -= 1.0;
        }
        let zoom = ctx.input(|i| i.zoom_delta());
        if zoom != 1.0 {
            grow += if zoom > 1.0 { 1.0 } else { -1.0 };
        }
        if grow != 0.0 {
            settings::update(|s| s.term_size += grow);
        }
        if take_key(ctx, Modifiers::CTRL, Key::Num0) {
            settings::update(|s| s.term_size = settings::Settings::default().term_size);
        }
        let fullscreen =
            take_key(ctx, Modifiers::NONE, Key::F11) || take_key(ctx, Modifiers::ALT, Key::Enter);
        if fullscreen {
            let now = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!now));
        }
    }

    /// A pane's row of tabs, laid out left to right in `ui`, with a "+" after them.
    fn tab_row(
        &self,
        ui: &mut Ui,
        pane: &Pane,
        height: f32,
        room: f32,
        acts: &mut Vec<TabAct>,
        drag: &mut Option<Drag>,
    ) {
        let focused = pane.id == self.focus || self.panes.len() == 1;
        let width = (room / pane.terms.len() as f32).clamp(60.0, 220.0);
        for (index, t) in pane.terms.iter().enumerate() {
            let title = t.title();
            let active = index == pane.active;
            let dragging = self.drag.is_some_and(|d| d.term == t.id);
            let (clicked, closed, resp) = tab_button(
                ui,
                &title,
                t.profile.icon,
                active,
                focused,
                dragging,
                width,
                height,
            );
            if clicked {
                acts.push(TabAct::Select(pane.id, index));
            }
            if closed || resp.middle_clicked() {
                acts.push(TabAct::Close(t.id));
            }
            if resp.drag_started() {
                *drag = Some(Drag {
                    term: t.id,
                    from: pane.id,
                });
            }
            resp.on_hover_text(&title).context_menu(|ui| {
                let item = |ui: &mut Ui, text: &str, keys: &str| {
                    ui.add(egui::Button::new(text).shortcut_text(keys))
                        .clicked()
                };
                let profile = t.profile.clone();
                if item(ui, "Split right", "Alt+Shift+=") {
                    acts.push(TabAct::Split(pane.id, split::Zone::Right, profile.clone()));
                }
                if item(ui, "Split down", "Alt+Shift+-") {
                    acts.push(TabAct::Split(pane.id, split::Zone::Below, profile.clone()));
                }
                ui.weak("Or drag a tab to the edge of a pane");
                ui.separator();
                if item(ui, "Duplicate tab", "Ctrl+Shift+D") {
                    acts.push(TabAct::New(pane.id, profile));
                }
                if ui.button("Close other tabs").clicked() {
                    acts.push(TabAct::CloseOthers(t.id));
                }
                if ui.button("Close tab").clicked() {
                    acts.push(TabAct::Close(t.id));
                }
            });
        }
        ui.add_space(4.0);
        if bar_button(ui, icons::ADD, "New tab (Ctrl+Shift+T)") {
            acts.push(TabAct::New(pane.id, self.default_profile()));
        }
    }

    fn apply_tab_acts(&mut self, ctx: &Context, acts: Vec<TabAct>) {
        for act in acts {
            match act {
                TabAct::Select(pane, index) => {
                    self.pane_mut(pane).map(|p| p.select(index));
                    self.focus_on(pane);
                }
                TabAct::Close(id) => self.close_term(ctx, id),
                TabAct::New(pane, profile) => self.open_in(ctx, pane, profile),
                TabAct::Split(pane, zone, profile) => {
                    let term = self.new_term(ctx, profile);
                    self.place_beside(pane, zone, term);
                }
                TabAct::CloseOthers(id) => {
                    let others: Vec<u64> = self
                        .panes
                        .iter()
                        .find(|p| p.terms.iter().any(|t| t.id == id))
                        .map(|p| p.terms.iter().map(|t| t.id).filter(|t| *t != id).collect())
                        .unwrap_or_default();
                    for other in others {
                        self.close_term(ctx, other);
                    }
                }
            }
        }
    }

    fn title_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        let mut drag = None;
        let mut open = None;
        let single = self.panes.len() == 1;
        Panel::top("title")
            .exact_size(TITLE_H)
            .frame(Frame::NONE.fill(theme::chrome()).inner_margin(Margin {
                left: 8,
                right: 4,
                top: 6,
                bottom: 0,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                title_drag(ui);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if single {
                        // One pane: its tabs are the window's, up here as in Windows Terminal.
                        let room = ui.available_width() - 220.0;
                        self.tab_row(
                            ui,
                            &self.panes[0],
                            TITLE_H - 6.0,
                            room,
                            &mut acts,
                            &mut drag,
                        );
                    } else {
                        // Several: each pane has its own row, and this bar holds the rest.
                        ui.add_space(6.0);
                        ui.label(RichText::new(icons::TERMINAL).color(theme::COMMENT()));
                        ui.label(RichText::new("rterm").color(theme::MUTED()).size(13.0));
                        ui.add_space(8.0);
                    }
                    let menu = egui::Button::new(RichText::new(icons::CHEVRON_DOWN).size(13.0))
                        .frame_when_inactive(false)
                        .min_size(vec2(26.0, 26.0));
                    egui::containers::menu::MenuButton::from_button(menu).ui(ui, |ui| {
                        ui.set_min_width(240.0);
                        self.profile_menu(ui, &mut open);
                    });
                    ui.with_layout(Layout::right_to_left(Align::Min), window_buttons);
                });
            });
        if drag.is_some() {
            self.drag = drag;
        }
        if let Some(profile) = open {
            self.open_in(&ctx, self.focus, profile);
        }
        self.apply_tab_acts(&ctx, acts);
    }

    /// The menu beside the tabs: a new tab of any profile, then the settings.
    fn profile_menu(&mut self, ui: &mut Ui, open: &mut Option<Profile>) {
        for (n, profile) in self.profiles.iter().enumerate() {
            let text = format!("{}   {}", profile.icon, profile.name);
            let mut button = egui::Button::new(text);
            if n < 9 {
                button = button.shortcut_text(format!("Ctrl+Shift+{}", n + 1));
            }
            if ui.add(button).clicked() {
                *open = Some(profile.clone());
            }
        }
        if self.more_profiles.is_some() {
            ui.weak("Looking for WSL and SSH…");
        }
        ui.separator();
        ui.menu_button(format!("{}   Settings", icons::GEAR), |ui| {
            ui.set_min_width(220.0);
            let size = settings::with(|s| s.term_size);
            ui.horizontal(|ui| {
                ui.label(format!("Font size  {size:.0}"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("+").clicked() {
                        settings::update(|s| s.term_size += 1.0);
                    }
                    if ui.small_button("−").clicked() {
                        settings::update(|s| s.term_size -= 1.0);
                    }
                });
            });
            ui.separator();
            let mut glass = settings::with(|s| s.glass);
            if ui.checkbox(&mut glass, "Glass").changed() {
                settings::update(|s| s.glass = glass);
            }
            if glass {
                let mut opacity = settings::with(|s| s.glass_opacity);
                let slider = egui::Slider::new(&mut opacity, 0.0..=1.0).text("Opacity");
                if ui.add(slider).changed() {
                    settings::update(|s| s.glass_opacity = opacity);
                }
            }
            ui.separator();
            ui.label(RichText::new("Theme").small().color(theme::COMMENT()));
            let current = settings::with(|s| s.theme.clone());
            for (key, label) in [("graphite", "Graphite"), ("slate", "Slate"), ("day", "Day")] {
                if ui.radio(current == key, label).clicked() {
                    settings::update(|s| s.theme = key.into());
                }
            }
            ui.separator();
            let mut default = handoff::is_default();
            if ui
                .checkbox(&mut default, "Default terminal for Windows")
                .on_hover_text(
                    "Console programs started anywhere (cmd, PowerShell, scripts from Explorer) open in rterm instead of a console window or Windows Terminal",
                )
                .changed()
            {
                self.default_error = handoff::set_default(default).err();
            }
            if let Some(error) = &self.default_error {
                ui.add(
                    egui::Label::new(RichText::new(error).small().color(theme::COMMENT()))
                        .wrap(),
                );
            }
            ui.separator();
            ui.label(
                RichText::new("New tabs open")
                    .small()
                    .color(theme::COMMENT()),
            );
            let default = self.default_profile().name;
            for profile in &self.profiles {
                if ui.radio(profile.name == default, &profile.name).clicked() {
                    let name = profile.name.clone();
                    settings::update(|s| s.default_profile = name);
                }
            }
        });
        if ui.button(format!("{}   New window", icons::ADD)).clicked() {
            new_window();
        }
    }

    fn body(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme::surface());
        let area = Rect::from_min_max(rect.min + vec2(2.0, 0.0), rect.max - vec2(2.0, 2.0));
        let (mut rects, mut dividers) = (Vec::new(), Vec::new());
        self.layout.layout(area, &mut rects, &mut dividers);
        let several = rects.len() > 1;

        // The lines between panes, which can be dragged to resize them.
        for divider in &dividers {
            let id = ui.id().with(("divider", &divider.path));
            let resp = ui.interact(divider.line, id, Sense::drag());
            let (delta, size, icon) = if divider.across {
                let d = resp.drag_delta().x;
                (d, divider.area.width(), egui::CursorIcon::ResizeHorizontal)
            } else {
                let d = resp.drag_delta().y;
                (d, divider.area.height(), egui::CursorIcon::ResizeVertical)
            };
            if resp.hovered() || resp.dragged() {
                ctx.set_cursor_icon(icon);
            }
            if delta != 0.0 {
                self.layout.drag(&divider.path, delta, size);
            }
            let line = if divider.across {
                [divider.line.center_top(), divider.line.center_bottom()]
            } else {
                [divider.line.left_center(), divider.line.right_center()]
            };
            let color = if resp.hovered() || resp.dragged() {
                theme::ACCENT()
            } else {
                theme::LINE()
            };
            ui.painter().line_segment(line, (1.0, color));
        }

        // Each pane's own row of tabs, when there are several.
        let mut acts = Vec::new();
        let mut drag = None;
        if several {
            for (id, r) in &rects {
                let Some(pane) = self.pane(*id) else { continue };
                let strip = Rect::from_min_size(
                    r.min + vec2(4.0, 4.0),
                    vec2(r.width() - 8.0, STRIP_H - 4.0),
                );
                let builder = egui::UiBuilder::new()
                    .id_salt(("strip", *id))
                    .max_rect(strip)
                    .layout(Layout::left_to_right(Align::Center));
                ui.scope_builder(builder, |ui| {
                    ui.set_clip_rect(strip.intersect(ui.clip_rect()));
                    ui.spacing_mut().item_spacing.x = 2.0;
                    self.tab_row(
                        ui,
                        pane,
                        STRIP_H - 4.0,
                        strip.width() - 40.0,
                        &mut acts,
                        &mut drag,
                    );
                });
                ui.painter()
                    .hline(r.x_range(), r.top() + STRIP_H, (1.0, theme::LINE()));
            }
        }
        if drag.is_some() {
            self.drag = drag;
        }
        let content = |r: &Rect| {
            if several {
                Rect::from_min_max(r.min + vec2(6.0, STRIP_H + 4.0), r.max - vec2(4.0, 4.0))
            } else {
                Rect::from_min_max(r.min + vec2(8.0, 6.0), r.max - vec2(6.0, 6.0))
            }
        };

        let pointer = ui.input(|i| i.pointer.interact_pos());
        let under = |p: egui::Pos2| rects.iter().find(|(_, r)| r.contains(p)).map(|(id, _)| *id);

        // A tab being dragged: where it would land, and letting go there.
        if let Some(d) = self.drag {
            let released = ui.input(|i| i.pointer.any_released() || !i.pointer.any_down());
            let hover = ui.input(|i| i.pointer.hover_pos());
            let target = hover.and_then(|p| {
                let (id, r) = rects.iter().find(|(_, r)| r.contains(p))?;
                // Over a row of tabs means "among these tabs".
                let in_strip = several && p.y < r.top() + STRIP_H;
                let (zone, shape) = if in_strip {
                    (split::Zone::Into, *r)
                } else {
                    split::zone_at(content(r), p)
                };
                Some((*id, zone, shape))
            });
            if let Some((_, _, shape)) = target {
                let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop"));
                ctx.layer_painter(layer).rect(
                    shape.shrink(4.0),
                    8,
                    theme::ACCENT().gamma_multiply(0.16),
                    (1.5, theme::ACCENT()),
                    egui::StrokeKind::Inside,
                );
            }
            // The tab's title follows the pointer.
            if let (Some(p), Some(t)) = (
                hover,
                self.panes
                    .iter()
                    .flat_map(|p| &p.terms)
                    .find(|t| t.id == d.term),
            ) {
                let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("drag_tab"));
                let painter = ctx.layer_painter(layer);
                let label = format!("{}  {}", t.profile.icon, t.title());
                let galley =
                    painter.layout_no_wrap(label, FontId::proportional(13.0), theme::TEXT());
                let r = Rect::from_min_size(p + vec2(12.0, 10.0), galley.size() + vec2(20.0, 12.0));
                painter.rect(
                    r,
                    8,
                    theme::surface(),
                    (1.0, theme::ACCENT()),
                    egui::StrokeKind::Inside,
                );
                painter.galley(r.min + vec2(10.0, 6.0), galley, theme::TEXT());
            }
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            if released {
                self.drag = None;
                if let Some((target, zone, _)) = target {
                    self.drop_tab(d, target, zone);
                }
                ctx.request_repaint();
                return;
            }
        }

        // A right click copies the selection, or pastes when there is none, as in the
        // Windows console. It goes to the pane under the pointer.
        let right = ui
            .input(|i| i.pointer.secondary_clicked())
            .then_some(pointer)
            .flatten()
            .filter(|p| rects.iter().any(|(_, r)| content(r).contains(*p)))
            .and_then(under);
        if let Some(id) = right {
            self.focus_on(id);
            if let Some(pane) = self.pane_mut(id) {
                let t = pane.shown_mut();
                match t.term.selected_text() {
                    Some(text) => {
                        ctx.copy_text(text);
                        t.term.clear_selection();
                    }
                    None => {
                        if let Some(text) = clipboard_text() {
                            ui.input_mut(|i| i.events.push(Event::Paste(text)));
                        }
                    }
                }
            }
        }
        // Files dropped on the window are typed in as their paths, in the pane they land on.
        let dropped: Vec<String> = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| {
                    let p = f.path().display().to_string();
                    if p.contains(' ') {
                        format!("\"{p}\"")
                    } else {
                        p
                    }
                })
                .collect()
        });
        if !dropped.is_empty() {
            if let Some(id) = ui.input(|i| i.pointer.hover_pos()).and_then(under) {
                self.focus_on(id);
            }
        }
        if ui.memory(|m| m.focused().is_none()) && self.drag.is_none() {
            let focus = self.focus;
            if let Some(p) = self.pane_mut(focus) {
                p.shown_mut().term.request_focus = true;
            }
        }

        let focus = self.focus;
        let mut restart = None;
        let mut close = None;
        let mut new_focus = None;
        for (id, pane_rect) in &rects {
            let Some(pane) = self.panes.iter_mut().find(|p| p.id == *id) else {
                continue;
            };
            let inner = content(pane_rect);
            let t = pane.shown_mut();
            // Only the pane in focus gets the keys; the others still take clicks.
            if *id != focus {
                t.term.request_focus = false;
            }
            let builder = egui::UiBuilder::new()
                .id_salt(("term", t.id))
                .max_rect(inner);
            let out = ui
                .scope_builder(builder, |ui| {
                    ui.set_clip_rect(inner.intersect(ui.clip_rect()));
                    t.term.show(ui)
                })
                .inner;
            if out.focused && *id != focus {
                new_focus = Some(*id);
            }
            let mut input = out.input;
            if *id == focus && !dropped.is_empty() {
                input.extend(dropped.join(" ").bytes());
            }
            if t.ended {
                if input.contains(&b'\r') {
                    restart = Some(t.id);
                } else if input.contains(&0x1b) || input.contains(&0x04) {
                    close = Some(t.id);
                }
                continue;
            }
            if let Some(pty) = &mut t.pty {
                if let Some((cols, rows)) = out.resize {
                    pty.resize(cols, rows);
                }
                if !input.is_empty() {
                    pty.write(&input);
                }
            }
        }
        // With several panes, the one in focus has a quiet accent along its top.
        if several {
            if let Some((_, r)) = rects.iter().find(|(id, _)| *id == focus) {
                let top = Rect::from_min_size(r.min + vec2(1.0, 0.0), vec2(r.width() - 2.0, 2.0));
                ui.painter()
                    .rect_filled(top, 0.0, theme::ACCENT().gamma_multiply(0.8));
            }
        }
        if let Some(id) = new_focus {
            self.focus = id;
        }
        self.rects = rects;
        self.apply_tab_acts(&ctx, acts);
        if let Some(id) = restart {
            self.restart(&ctx, id);
        }
        if let Some(id) = close {
            self.close_term(&ctx, id);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.sync_look(&ctx, frame);
        self.poll(&ctx);
        if self.panes.is_empty() {
            return;
        }
        self.shortcuts(&ctx);
        if self.panes.is_empty() {
            return;
        }
        self.title_bar(ui);
        if self.panes.is_empty() {
            return;
        }
        egui::CentralPanel::default()
            .frame(Frame::NONE)
            .show(ui, |ui| self.body(ui));
        if self.drag.is_none() {
            resize_edges(&ctx);
        }
        let title = match self.pane(self.focus) {
            Some(p) => p.shown().title(),
            None => "rterm".into(),
        };
        ctx.send_viewport_cmd(ViewportCommand::Title(title));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if theme::glass_opacity().is_some() {
            [0.0; 4]
        } else {
            theme::WINDOW_BG().to_normalized_gamma_f32()
        }
    }
}

/// Takes a key press out of this frame's input, so the terminal doesn't also get it.
/// Digits are matched by the key's place on the keyboard, since Shift changes what they type.
fn take_key(ctx: &Context, mods: Modifiers, wanted: Key) -> bool {
    ctx.input_mut(|i| {
        let mut hit = false;
        i.events.retain(|event| match event {
            Event::Key {
                key,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } if !hit
                && modifiers.matches_exact(mods)
                && (*key == wanted || *physical_key == Some(wanted)) =>
            {
                hit = true;
                false
            }
            _ => true,
        });
        hit
    })
}

fn clipboard_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

fn new_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// One tab in the title bar. Returns whether it was clicked, whether its close button
/// was, and its response for menus and hover text.
#[allow(clippy::too_many_arguments)]
fn tab_button(
    ui: &mut Ui,
    title: &str,
    icon: char,
    active: bool,
    focused: bool,
    dragging: bool,
    width: f32,
    height: f32,
) -> (bool, bool, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
    let close_rect =
        Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), vec2(20.0, 20.0));
    let over_close = resp.hovered() && ui.rect_contains_pointer(close_rect);
    let painter = ui.painter();
    let fill = if dragging {
        theme::tint(6)
    } else if active && focused {
        theme::surface()
    } else if active {
        theme::tint(12)
    } else if resp.hovered() {
        theme::tint(10)
    } else {
        Color32::TRANSPARENT
    };
    let corners = CornerRadius {
        nw: 8,
        ne: 8,
        sw: 0,
        se: 0,
    };
    painter.rect_filled(rect, corners, fill);
    if active && focused {
        let line = Rect::from_min_size(rect.min + vec2(8.0, 0.0), vec2(rect.width() - 16.0, 2.0));
        painter.rect_filled(line, 1.0, theme::ACCENT());
    }
    let color = if active {
        theme::TEXT()
    } else {
        theme::MUTED()
    };
    painter.text(
        pos2(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(14.0),
        theme::COMMENT(),
    );
    let text_clip = Rect::from_min_max(rect.min, pos2(rect.right() - 30.0, rect.max.y));
    painter.with_clip_rect(text_clip).text(
        pos2(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(13.0),
        color,
    );
    if active || resp.hovered() {
        if over_close {
            painter.rect_filled(close_rect, 4.0, theme::tint(20));
        }
        painter.text(
            close_rect.center(),
            Align2::CENTER_CENTER,
            icons::CLOSE,
            FontId::proportional(13.0),
            if over_close {
                theme::TEXT()
            } else {
                theme::COMMENT()
            },
        );
    }
    let clicked = resp.clicked();
    (clicked && !over_close, clicked && over_close, resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_folders_from_explorer() {
        assert_eq!(clean_dir("C:\"".into()), PathBuf::from("C:\\"));
        assert_eq!(clean_dir(r"D:\work".into()), PathBuf::from(r"D:\work"));
    }
}
