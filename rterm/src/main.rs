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

mod os;
mod profiles;
mod pty;
mod settings;
mod split;

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

const TITLE_H: f32 = 38.0;

fn main() -> eframe::Result {
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
struct Pane {
    id: u64,
    term: Terminal,
    pty: Option<Pty>,
    profile: Profile,
    /// The program has ended in a way worth reading, and the pane waits for Enter or Esc.
    ended: bool,
}

impl Pane {
    fn title(&self) -> String {
        profiles::tidy_title(&self.term.title)
            .map(str::to_string)
            .unwrap_or_else(|| self.profile.name.clone())
    }
}

struct Tab {
    panes: Vec<Pane>,
    layout: split::Node,
    /// The pane that has the keyboard.
    focus: u64,
    /// Where each pane was drawn last, for splitting along the longer side and moving focus.
    rects: Vec<(u64, Rect)>,
}

impl Tab {
    fn focused(&self) -> &Pane {
        self.panes
            .iter()
            .find(|p| p.id == self.focus)
            .unwrap_or(&self.panes[0])
    }

    fn focused_mut(&mut self) -> &mut Pane {
        let index = self
            .panes
            .iter()
            .position(|p| p.id == self.focus)
            .unwrap_or(0);
        &mut self.panes[index]
    }

    fn title(&self) -> String {
        let title = self.focused().title();
        match self.panes.len() {
            1 => title,
            n => format!("{title}  ({n})"),
        }
    }

    fn focus_on(&mut self, id: u64) {
        self.focus = id;
        if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
            pane.term.request_focus = true;
        }
    }
}

/// What to do with a pane once this frame's input is handled.
enum PaneAct {
    Restart(u64),
    Close(u64),
}

struct App {
    tabs: Vec<Tab>,
    active: usize,
    profiles: Vec<Profile>,
    /// WSL distributions and SSH hosts, still being looked for.
    more_profiles: Option<Receiver<Vec<Profile>>>,
    dir: PathBuf,
    next_id: u64,
    adopted: bool,
    /// The backdrop last given to the window: its tint and whether it is dark.
    applied: Option<(Option<[u8; 4]>, bool)>,
    theme_shown: String,
}

fn palette(name: &str) -> theme::Palette {
    match name {
        "slate" => theme::SLATE,
        "day" => theme::DAY,
        _ => theme::GRAPHITE,
    }
}

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
            tabs: Vec::new(),
            active: 0,
            profiles: profiles::local(),
            more_profiles: Some(more_profiles),
            dir: launch.dir,
            next_id: 1,
            adopted: false,
            applied: None,
            theme_shown: name,
        };
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
        app.open_tab(ctx, first);
        // RTERM_SPLIT=right,down splits the first tab that way at the start, to look at panes
        // without the keyboard.
        for way in std::env::var("RTERM_SPLIT").unwrap_or_default().split(',') {
            match way {
                "right" => app.split(ctx, Some(true)),
                "down" => app.split(ctx, Some(false)),
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

    /// A pane running `profile`; if it can't start, the pane says why.
    fn new_pane(&mut self, ctx: &Context, profile: Profile) -> Pane {
        let id = self.next_id;
        self.next_id += 1;
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
        Pane {
            id,
            term,
            pty,
            profile,
            ended,
        }
    }

    fn open_tab(&mut self, ctx: &Context, profile: Profile) {
        let pane = self.new_pane(ctx, profile);
        self.push_tab(pane);
    }

    fn push_tab(&mut self, pane: Pane) {
        let id = pane.id;
        self.tabs.push(Tab {
            panes: vec![pane],
            layout: split::Node::Pane(id),
            focus: id,
            rects: Vec::new(),
        });
        self.active = self.tabs.len() - 1;
    }

    /// Splits the focused pane of the current tab: side by side, one under the other, or
    /// (with `None`) along its longer side. The new pane runs the same profile.
    fn split(&mut self, ctx: &Context, across: Option<bool>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let (focus, profile) = (tab.focus, tab.focused().profile.clone());
        let across = across.unwrap_or_else(|| {
            let rect = tab
                .rects
                .iter()
                .find(|(id, _)| *id == focus)
                .map(|(_, r)| *r);
            // Cells are about twice as tall as wide, so a pane is split across when it is
            // wider than it is tall, give or take.
            rect.is_none_or(|r| r.width() >= r.height())
        });
        let pane = self.new_pane(ctx, profile);
        let id = pane.id;
        let tab = &mut self.tabs[self.active];
        tab.layout.split(focus, id, across);
        tab.panes.push(pane);
        tab.focus_on(id);
    }

    /// Closes a pane, and its tab with it when it was the last one.
    fn close_pane(&mut self, ctx: &Context, index: usize, id: u64) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        if !tab.layout.remove(id) {
            self.close_tab(ctx, index);
            return;
        }
        tab.panes.retain(|p| p.id != id);
        if tab.focus == id {
            let next = tab.layout.panes()[0];
            tab.focus_on(next);
        }
    }

    fn close_tab(&mut self, ctx: &Context, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.focus_active();
    }

    fn select(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
            self.focus_active();
        }
    }

    fn focus_active(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focused_mut().term.request_focus = true;
        }
    }

    fn restart(&mut self, ctx: &Context, id: u64) {
        let dir = self.dir.clone();
        let Some(pane) = self.tabs[self.active].panes.iter_mut().find(|p| p.id == id) else {
            return;
        };
        pane.pty = None;
        match Pty::spawn(&pane.profile, &dir, pane.term.size(), ctx) {
            Ok(pty) => {
                pane.term
                    .feed(b"\x1b[2J\x1b[3J\x1b[H", &mut Vec::new(), ctx);
                pane.pty = Some(pty);
                pane.ended = false;
            }
            Err(error) => {
                let note = format!("\r\n\x1b[31m{error}\x1b[0m\r\n");
                pane.term.feed(note.as_bytes(), &mut Vec::new(), ctx);
            }
        }
    }

    /// Feeds every pane's output to its terminal, shown or not, and notices programs ending.
    fn poll(&mut self, ctx: &Context) {
        if let Some(more) = &self.more_profiles {
            if let Ok(found) = more.try_recv() {
                self.profiles.extend(found);
                self.more_profiles = None;
            }
        }
        let mut close = Vec::new();
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            for pane in &mut tab.panes {
                let Some(pty) = &mut pane.pty else { continue };
                let mut reply = Vec::new();
                while let Ok(data) = pty.output.try_recv() {
                    pane.term.feed(&data, &mut reply, ctx);
                }
                if !reply.is_empty() {
                    pty.write(&reply);
                }
                match pty.exit_code() {
                    // A program that ends cleanly takes its pane with it.
                    Some(0) => close.push((index, pane.id)),
                    Some(code) => {
                        let note = format!(
                            "\r\n\x1b[90m[process exited with code {code}]\r\nPress Enter to start it again, or Esc to close.\x1b[0m"
                        );
                        pane.term.feed(note.as_bytes(), &mut Vec::new(), ctx);
                        pane.pty = None;
                        pane.ended = true;
                    }
                    None => {}
                }
            }
        }
        // From the last tab back, so closing one doesn't move the others' places.
        for (index, id) in close.into_iter().rev() {
            self.close_pane(ctx, index, id);
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
        if take_key(ctx, ctrl_shift, Key::T) {
            self.open_tab(ctx, self.default_profile());
        }
        if take_key(ctx, ctrl_shift, Key::W) {
            let focus = self.tabs[self.active].focus;
            self.close_pane(ctx, self.active, focus);
            if self.tabs.is_empty() {
                return;
            }
        }
        if take_key(ctx, ctrl_shift, Key::D) {
            let profile = self.tabs[self.active].focused().profile.clone();
            self.open_tab(ctx, profile);
        }
        if take_key(ctx, ctrl_shift, Key::N) {
            new_window();
        }
        // Splits, as in Windows Terminal.
        if take_key(ctx, alt_shift, Key::D) {
            self.split(ctx, None);
        }
        if take_key(ctx, alt_shift, Key::Equals) || take_key(ctx, alt_shift, Key::Plus) {
            self.split(ctx, Some(true));
        }
        if take_key(ctx, alt_shift, Key::Minus) {
            self.split(ctx, Some(false));
        }
        for (key, dir) in [
            (Key::ArrowLeft, split::Dir::Left),
            (Key::ArrowRight, split::Dir::Right),
            (Key::ArrowUp, split::Dir::Up),
            (Key::ArrowDown, split::Dir::Down),
        ] {
            if take_key(ctx, Modifiers::ALT, key) {
                let tab = &mut self.tabs[self.active];
                if let Some(next) = split::neighbour(&tab.rects, tab.focus, dir) {
                    tab.focus_on(next);
                }
            }
        }
        let count = self.tabs.len();
        if take_key(ctx, Modifiers::CTRL, Key::Tab) {
            self.select((self.active + 1) % count);
        }
        if take_key(ctx, ctrl_shift, Key::Tab) {
            self.select((self.active + count - 1) % count);
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
                self.select(if n == 8 { count - 1 } else { n });
            }
            if take_key(ctx, ctrl_shift, key) {
                if let Some(profile) = self.profiles.get(n).cloned() {
                    self.open_tab(ctx, profile);
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

    fn title_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut select = None;
        let mut close = None;
        let mut open = None;
        let mut close_others = None;
        let mut split = None;
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
                    let room = ui.available_width() - 190.0;
                    let width = (room / self.tabs.len() as f32).clamp(60.0, 220.0);
                    for (index, tab) in self.tabs.iter().enumerate() {
                        let title = tab.title();
                        let active = index == self.active;
                        let icon = tab.focused().profile.icon;
                        let (clicked, closed, resp) = tab_button(ui, &title, icon, active, width);
                        if clicked {
                            select = Some(index);
                        }
                        if closed || resp.middle_clicked() {
                            close = Some(index);
                        }
                        resp.on_hover_text(&title).context_menu(|ui| {
                            let split_item = |ui: &mut Ui, text: &str, keys: &str| {
                                ui.add(egui::Button::new(text).shortcut_text(keys))
                                    .clicked()
                            };
                            if split_item(ui, "Split right", "Alt+Shift+=") {
                                split = Some((index, true));
                            }
                            if split_item(ui, "Split down", "Alt+Shift+-") {
                                split = Some((index, false));
                            }
                            ui.separator();
                            if split_item(ui, "Duplicate tab", "Ctrl+Shift+D") {
                                open = Some(tab.focused().profile.clone());
                            }
                            if ui.button("Close other tabs").clicked() {
                                close_others = Some(index);
                            }
                            if ui.button("Close tab").clicked() {
                                close = Some(index);
                            }
                        });
                    }
                    ui.add_space(4.0);
                    if bar_button(ui, icons::ADD, "New tab (Ctrl+Shift+T)") {
                        open = Some(self.default_profile());
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
        if let Some(index) = select {
            self.select(index);
        }
        if let Some((index, across)) = split {
            self.select(index);
            self.split(&ctx, Some(across));
        }
        if let Some(profile) = open {
            self.open_tab(&ctx, profile);
        }
        if let Some(keep) = close_others {
            let tab = self.tabs.remove(keep);
            self.tabs = vec![tab];
            self.active = 0;
            self.focus_active();
        } else if let Some(index) = close {
            self.close_tab(&ctx, index);
        }
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
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, theme::surface());
        let area = Rect::from_min_max(rect.min + vec2(2.0, 0.0), rect.max - vec2(2.0, 2.0));
        let (mut rects, mut dividers) = (Vec::new(), Vec::new());
        tab.layout.layout(area, &mut rects, &mut dividers);
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
                tab.layout.drag(&divider.path, delta, size);
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

        let pointer = ui.input(|i| i.pointer.interact_pos());
        let under = |p: egui::Pos2| rects.iter().find(|(_, r)| r.contains(p)).map(|(id, _)| *id);
        // A right click copies the selection, or pastes when there is none, as in the
        // Windows console. It goes to the pane under the pointer.
        let right = ui
            .input(|i| i.pointer.secondary_clicked())
            .then_some(pointer)
            .flatten()
            .and_then(under);
        if let Some(id) = right {
            tab.focus_on(id);
            let pane = tab.focused_mut();
            match pane.term.selected_text() {
                Some(text) => {
                    ctx.copy_text(text);
                    pane.term.clear_selection();
                }
                None => {
                    if let Some(text) = clipboard_text() {
                        ui.input_mut(|i| i.events.push(Event::Paste(text)));
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
                tab.focus_on(id);
            }
        }
        if ui.memory(|m| m.focused().is_none()) {
            tab.focused_mut().term.request_focus = true;
        }

        let mut act = None;
        let focus = tab.focus;
        let mut new_focus = None;
        for (id, pane_rect) in &rects {
            let Some(pane) = tab.panes.iter_mut().find(|p| p.id == *id) else {
                continue;
            };
            let inner = Rect::from_min_max(
                pane_rect.min + vec2(6.0, if several { 4.0 } else { 6.0 }),
                pane_rect.max - vec2(4.0, 4.0),
            );
            // Only the pane with the keyboard gets the keys; the others still take clicks.
            if *id != focus {
                pane.term.request_focus = false;
            }
            let builder = egui::UiBuilder::new()
                .id_salt(("term", pane.id))
                .max_rect(inner);
            let out = ui
                .scope_builder(builder, |ui| {
                    ui.set_clip_rect(inner.intersect(ui.clip_rect()));
                    pane.term.show(ui)
                })
                .inner;
            if out.focused && *id != focus {
                new_focus = Some(*id);
            }
            let mut input = out.input;
            if *id == focus && !dropped.is_empty() {
                input.extend(dropped.join(" ").bytes());
            }
            if pane.ended {
                if input.contains(&b'\r') {
                    act = Some(PaneAct::Restart(pane.id));
                } else if input.contains(&0x1b) || input.contains(&0x04) {
                    act = Some(PaneAct::Close(pane.id));
                }
                continue;
            }
            if let Some(pty) = &mut pane.pty {
                if let Some((cols, rows)) = out.resize {
                    pty.resize(cols, rows);
                }
                if !input.is_empty() {
                    pty.write(&input);
                }
            }
        }
        // With several panes, the one with the keyboard has a quiet accent along its top.
        if several {
            if let Some((_, r)) = rects.iter().find(|(id, _)| *id == focus) {
                let top = Rect::from_min_size(r.min + vec2(1.0, 0.0), vec2(r.width() - 2.0, 2.0));
                ui.painter()
                    .rect_filled(top, 0.0, theme::ACCENT().gamma_multiply(0.7));
            }
        }
        if let Some(id) = new_focus {
            tab.focus = id;
        }
        tab.rects = rects;
        match act {
            Some(PaneAct::Restart(id)) => self.restart(&ctx, id),
            Some(PaneAct::Close(id)) => self.close_pane(&ctx, self.active, id),
            None => {}
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.sync_look(&ctx, frame);
        self.poll(&ctx);
        if self.tabs.is_empty() {
            return;
        }
        self.shortcuts(&ctx);
        if self.tabs.is_empty() {
            return;
        }
        self.title_bar(ui);
        egui::CentralPanel::default()
            .frame(Frame::NONE)
            .show(ui, |ui| self.body(ui));
        resize_edges(&ctx);
        let title = self
            .tabs
            .get(self.active)
            .map_or("rterm".into(), Tab::title);
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
fn tab_button(
    ui: &mut Ui,
    title: &str,
    icon: char,
    active: bool,
    width: f32,
) -> (bool, bool, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, TITLE_H - 6.0), Sense::click());
    let close_rect =
        Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), vec2(20.0, 20.0));
    let over_close = resp.hovered() && ui.rect_contains_pointer(close_rect);
    let painter = ui.painter();
    let fill = if active {
        theme::surface()
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

fn bar_button(ui: &mut Ui, icon: char, hint: &str) -> bool {
    let button = egui::Button::new(RichText::new(icon).size(14.0))
        .frame_when_inactive(false)
        .min_size(vec2(28.0, 26.0));
    ui.add(button).on_hover_text(hint).clicked()
}

/// Lets the window be resized by its edges, as it has no system frame showing.
fn resize_edges(ctx: &Context) {
    use egui::{CursorIcon as Icon, ResizeDirection as Dir};
    let state = ctx.input(|i| {
        let v = i.viewport();
        v.maximized.unwrap_or(false) || v.fullscreen.unwrap_or(false)
    });
    if state {
        return;
    }
    let rect = ctx.content_rect();
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    const EDGE: f32 = 5.0;
    let (left, right) = (p.x - rect.left() < EDGE, rect.right() - p.x < EDGE);
    let (top, bottom) = (p.y - rect.top() < EDGE, rect.bottom() - p.y < EDGE);
    let (direction, icon) = match (left, right, top, bottom) {
        (true, _, true, _) => (Dir::NorthWest, Icon::ResizeNwSe),
        (_, true, true, _) => (Dir::NorthEast, Icon::ResizeNeSw),
        (true, _, _, true) => (Dir::SouthWest, Icon::ResizeNeSw),
        (_, true, _, true) => (Dir::SouthEast, Icon::ResizeNwSe),
        (true, ..) => (Dir::West, Icon::ResizeHorizontal),
        (_, true, ..) => (Dir::East, Icon::ResizeHorizontal),
        (_, _, true, _) => (Dir::North, Icon::ResizeVertical),
        (_, _, _, true) => (Dir::South, Icon::ResizeVertical),
        _ => return,
    };
    ctx.set_cursor_icon(icon);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}

/// Makes the title bar move the window when dragged and maximize it on a double click.
/// Called before the bar's widgets, so they stay clickable on top.
fn title_drag(ui: &Ui) {
    let rect = ui.max_rect().expand2(vec2(12.0, 8.0));
    let resp = ui.interact(rect, ui.id().with("title_drag"), Sense::click_and_drag());
    if resp.double_clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if resp.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

fn caption_button(ui: &mut Ui, icon: char, hint: &str, danger: bool) -> bool {
    ui.scope(|ui| {
        if danger {
            let red = Color32::from_rgb(0xc4, 0x2b, 0x1c);
            ui.visuals_mut().widgets.hovered.weak_bg_fill = red;
            ui.visuals_mut().widgets.active.weak_bg_fill = red;
        }
        let button = egui::Button::new(RichText::new(icon).size(13.0))
            .frame_when_inactive(false)
            .min_size(vec2(40.0, 28.0));
        ui.add(button).on_hover_text(hint).clicked()
    })
    .inner
}

fn window_buttons(ui: &mut Ui) {
    let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
    ui.spacing_mut().item_spacing.x = 0.0;
    if caption_button(ui, icons::WIN_CLOSE, "Close", true) {
        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
    }
    let (icon, hint) = if maximized {
        (icons::WIN_RESTORE, "Restore")
    } else {
        (icons::WIN_MAXIMIZE, "Maximize")
    };
    if caption_button(ui, icon, hint, false) {
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    if caption_button(ui, icons::WIN_MINIMIZE, "Minimize", false) {
        ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
    }
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
