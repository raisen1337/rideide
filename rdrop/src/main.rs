#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! rdrop: files between this computer and your servers, over SFTP. As many panes as you
//! like, each on this computer or on any server, laid out by dragging; rows dragged from one
//! pane to another are copied, server to server included; and the transfers run in the
//! background, shown in the bar at the bottom.

// The look is ride's; the window parts, shell profiles (for the SSH hosts) and the split
// layout are rterm's, shared as files.
#[allow(dead_code)]
#[path = "../../app/src/highlight.rs"]
mod highlight;
#[allow(dead_code)]
#[path = "../../app/src/icons.rs"]
mod icons;
#[allow(dead_code)]
#[path = "../../rterm/src/os.rs"]
mod os;
#[allow(dead_code)]
#[path = "../../rterm/src/profiles.rs"]
mod profiles;
#[allow(dead_code)]
#[path = "../../rterm/src/split.rs"]
mod split;
#[allow(dead_code)]
#[path = "../../app/src/theme.rs"]
mod theme;
#[allow(dead_code)]
#[path = "../../rterm/src/window.rs"]
mod window;

mod bulk;
mod cli;
mod compare;
mod engine;
mod explorer;
mod palette;
mod pane;
mod preview;
mod remote;
mod search;
mod sftp;
mod sites;
mod transfer;
mod ui;
mod watch;
mod winshell;

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Context, CornerRadius, Event, FontId, Frame, Key,
    Layout, Margin, Modifiers, Panel, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
    Vec2, ViewportBuilder,
};
use pane::{KnownFolder, Thumbs, View};
use remote::{Answer, Ask, Remote};
use sites::Site;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use transfer::{Place, Queue, State};
use window::{resize_edges, title_drag, window_buttons};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const ROW_H: f32 = 26.0;
const HEADER_H: f32 = 34.0;
const GOOD: Color32 = Color32::from_rgb(0x7f, 0xd0, 0x9a);
const BAD: Color32 = Color32::from_rgb(0xf0, 0x80, 0x8a);

fn main() -> eframe::Result {
    // ssh starting rdrop to ask for a password: answer it and go.
    if sftp::answer_askpass() {
        return Ok(());
    }
    // `rdrop ls|cp|get|put|sync|rm|mkdir ...` works from a terminal, without the window.
    if let Some(code) = cli::run() {
        std::process::exit(code);
    }
    if std::env::args().any(|arg| arg == "--version") {
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "{VERSION}");
        return Ok(());
    }
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        setup.device_descriptor = Arc::new(|_adapter| eframe::wgpu::DeviceDescriptor {
            label: Some("rdrop"),
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
            .with_inner_size([1240.0, 760.0])
            .with_min_inner_size([760.0, 440.0])
            .with_title("rdrop")
            .with_icon(egui::IconData {
                rgba: include_bytes!(concat!(env!("OUT_DIR"), "/icon-128.rgba")).to_vec(),
                width: 128,
                height: 128,
            })
            .with_decorations(true)
            .with_drag_and_drop(true),
        wgpu_options,
        persistence_path: std::env::var_os("APPDATA")
            .map(|dir| PathBuf::from(dir).join("rdrop").join("window.ron")),
        ..Default::default()
    };
    eframe::run_native("rdrop", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

// ---- what a pane lists ----

#[derive(Clone)]
struct Item {
    name: String,
    is_dir: bool,
    size: u64,
    /// Seconds since 1970.
    mtime: Option<u64>,
    /// Hidden as Explorer hides it, or a dot file on a server.
    hidden: bool,
    /// Its permissions, on a server.
    perms: Option<u32>,
    /// A symbolic link.
    link: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Col {
    Name,
    Size,
    Modified,
}

struct Pane {
    id: u64,
    /// Where it looks; `None` until a server or this computer is picked for it.
    place: Option<Place>,
    /// A folder here (empty for the list of drives) or on the server.
    path: String,
    path_edit: String,
    items: Vec<Item>,
    selected: HashSet<String>,
    anchor: Option<usize>,
    sort: (Col, bool),
    loading: bool,
    error: Option<String>,
    /// What was typed to jump to a name, and when.
    jump: (String, f64),
    /// A row to bring into view.
    scroll_to: Option<String>,
    view: View,
    conn: Option<Remote>,
    connected: bool,
    /// Folders to go back to.
    back: Vec<String>,
    /// What is typed in the search box, and whether it looks inside files.
    search_edit: String,
    search_contents: bool,
    /// A search under the folder: while there is one, its results are the items.
    search: Option<search::Search>,
    focus_search: bool,
    /// When the folder here last changed, to list it again when something else changes it.
    dir_mtime: Option<std::time::SystemTime>,
}

impl Pane {
    fn new(id: u64) -> Self {
        Pane {
            id,
            place: None,
            path: String::new(),
            path_edit: String::new(),
            items: Vec::new(),
            selected: HashSet::new(),
            anchor: None,
            sort: (Col::Name, true),
            loading: false,
            error: None,
            jump: (String::new(), 0.0),
            scroll_to: None,
            view: View::Details,
            conn: None,
            connected: false,
            back: Vec::new(),
            search_edit: String::new(),
            search_contents: false,
            search: None,
            focus_search: false,
            dir_mtime: None,
        }
    }

    fn is_local(&self) -> bool {
        self.place == Some(Place::Local)
    }

    /// Whether files can be listed and copied here now.
    fn ready(&self) -> bool {
        match &self.place {
            Some(Place::Local) => true,
            Some(Place::Server(_)) => self.connected,
            None => false,
        }
    }

    fn sort_items(&mut self) {
        let (col, up) = self.sort;
        self.items.sort_by(|a, b| {
            let order = match col {
                Col::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Col::Size => a.size.cmp(&b.size),
                Col::Modified => a.mtime.cmp(&b.mtime),
            };
            // Folders first, whichever way the rest goes.
            b.is_dir
                .cmp(&a.is_dir)
                .then(if up { order } else { order.reverse() })
        });
    }

    fn set_items(&mut self, path: String, items: Vec<Item>) {
        if self.search.is_some() && path == self.path {
            return;
        }
        if path != self.path {
            self.jump.0.clear();
            self.selected.clear();
            self.anchor = None;
        }
        self.path = path.clone();
        self.path_edit = path;
        self.items = items;
        self.sort_items();
        let names: HashSet<&str> = self.items.iter().map(|i| i.name.as_str()).collect();
        self.selected.retain(|n| names.contains(n.as_str()));
        self.loading = false;
        self.error = None;
    }

    fn visible(&self, hidden: bool) -> Vec<Item> {
        self.items
            .iter()
            .filter(|i| hidden || !i.hidden)
            .cloned()
            .collect()
    }

    /// The full path of an item in this pane.
    fn child(&self, name: &str) -> String {
        if self.is_local() {
            if self.path.is_empty() {
                name.to_string()
            } else {
                PathBuf::from(&self.path).join(name).display().to_string()
            }
        } else {
            sftp::join(&self.path, name)
        }
    }

    fn up(&self) -> Option<String> {
        match &self.place {
            Some(Place::Server(_)) => {
                (self.path != "/" && !self.path.is_empty()).then(|| sftp::parent(&self.path))
            }
            Some(Place::Local) if !self.path.is_empty() => Some(
                Path::new(&self.path)
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            _ => None,
        }
    }

    fn selected_names(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|i| self.selected.contains(&i.name))
            .map(|i| i.name.clone())
            .collect()
    }
}

fn list_local(path: &str) -> Result<Vec<Item>, String> {
    // "This PC", with the drives, is drawn on its own.
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let link = entry.file_type().is_ok_and(|t| t.is_symlink());
        items.push(Item {
            hidden: winshell::is_hidden(&meta) || name.starts_with('.'),
            perms: None,
            link,
            name,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            mtime: meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
        });
    }
    Ok(items)
}

fn from_remote(entries: Vec<sftp::Entry>) -> Vec<Item> {
    entries
        .into_iter()
        .map(|e| Item {
            is_dir: e.attrs.is_dir(),
            size: if e.attrs.is_dir() {
                0
            } else {
                e.attrs.size.unwrap_or(0)
            },
            mtime: e.attrs.mtime.map(u64::from),
            hidden: e.name.starts_with('.'),
            perms: e.attrs.permissions,
            link: e.link,
            name: e.name,
        })
        .collect()
}

// ---- formatting ----

fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// This computer's offset from UTC, in seconds, for showing times as the clock does.
fn local_offset() -> i64 {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Zone {
            bias: i32,
            standard_name: [u16; 32],
            standard_date: [u16; 8],
            standard_bias: i32,
            daylight_name: [u16; 32],
            daylight_date: [u16; 8],
            daylight_bias: i32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetTimeZoneInformation(zone: *mut Zone) -> u32;
        }
        // SAFETY: the struct is the size and layout the call writes, and outlives it.
        unsafe {
            let mut zone: Zone = std::mem::zeroed();
            let kind = GetTimeZoneInformation(&mut zone);
            let extra = if kind == 2 { zone.daylight_bias } else { 0 };
            -((zone.bias + extra) as i64) * 60
        }
    }
    #[cfg(not(windows))]
    0
}

fn time_text(secs: Option<u64>, offset: i64) -> String {
    let Some(secs) = secs else {
        return String::new();
    };
    let t = secs as i64 + offset;
    let days = t.div_euclid(86_400);
    let rest = t.rem_euclid(86_400);
    // Days since 1970 to a calendar date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y}-{m:02}-{d:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}

// ---- settings ----

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
#[serde(default)]
struct SavedPane {
    /// "" for this computer, a host, or nothing picked yet.
    place: Option<String>,
    path: String,
    view: View,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct Settings {
    panes: Vec<SavedPane>,
    /// Hidden items and dot files are shown.
    #[serde(default = "yes")]
    show_hidden: bool,
    /// The last folder on each server, by host.
    remote: HashMap<String, String>,
    /// What transfers do with files already there, and whether they are checked.
    transfer: engine::Options,
    /// Folders here whose changes go up to a server.
    watches: Vec<watch::WatchSpec>,
}

fn yes() -> bool {
    true
}

fn settings_path() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("APPDATA")?)
            .join("rdrop")
            .join("settings.json"),
    )
}

/// Transfers not finished when rdrop closed, to carry on with.
fn queue_path() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("APPDATA")?)
            .join("rdrop")
            .join("queue.json"),
    )
}

fn load_settings() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn known_folders() -> Vec<KnownFolder> {
    winshell::known_folders()
        .into_iter()
        .map(|k| KnownFolder {
            icon: match k.name {
                "Desktop" => ui::DESKTOP,
                "Downloads" => icons::ARROW_DOWN,
                "Documents" => ui::BOOK,
                "Pictures" => icons::IMAGE,
                "Music" => icons::MUSIC,
                _ => ui::FOLDER,
            },
            name: k.name.to_string(),
            path: k.path.display().to_string(),
        })
        .collect()
}

fn home() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into())
}

// ---- the app ----

enum Dialog {
    NewFolder(u64, String),
    Delete(u64, Vec<String>),
}

/// Signing in: a saved site being made or changed, or a password asked for.
enum Auth {
    Site {
        /// The name it had, when an existing site is changed.
        old: Option<String>,
        site: Site,
        port: String,
        password: String,
        focus: bool,
    },
    Password {
        pane: u64,
        key: String,
        /// user@host, for the question.
        label: String,
        password: String,
        remember: bool,
        /// The last password tried was refused.
        wrong: bool,
        focus: bool,
    },
}

enum Drag {
    /// Rows dragged out of a pane, to copy into another.
    Rows { from: u64, names: Vec<String> },
    /// A pane dragged by its header, to put somewhere else.
    Pane(u64),
    /// A server (or this computer) dragged from the list on the left.
    Place(Place),
}

#[derive(Clone)]
enum PaneAct {
    Open(String),
    Back,
    Pick(Place),
    Split(bool),
    Close,
    NewFolder,
    Paste,
    Refresh,
    View(View),
    ToggleHidden,
    /// Enter or a double click on an item: a folder opens, a file shows.
    Activate(String),
    OpenWith(String),
    /// Copy (false) or cut (true) to the clipboard.
    Clip(Vec<String>, bool),
    StartRename(String),
    FinishRename(bool),
    Delete(Vec<String>),
    DeleteForGood(Vec<String>),
    Properties(String),
    CopyTo(Vec<String>),
    CopyPaths(Vec<String>),
    Reveal(String),
    /// Look under the folder for what was typed, by name or (true) inside files.
    Search(String, bool),
    EndSearch,
    /// Compare this pane's folder with the other pane's.
    Compare,
    /// Open a file (to edit) or a folder in ride, where it is.
    Ride(String),
    /// Send what changes in this folder to the other pane's server folder.
    Watch,
    StartRows(Vec<String>),
    StartPane,
}

struct App {
    panes: Vec<Pane>,
    layout: split::Node,
    /// The pane the keys go to, and the one before it, which F5 copies to.
    focus: u64,
    previous: u64,
    rects: Vec<(u64, Rect)>,
    queue: Queue,
    hosts: Vec<String>,
    hosts_rx: Option<Receiver<Vec<profiles::Profile>>>,
    host_input: String,
    dialog: Option<Dialog>,
    focus_dialog: bool,
    drag: Option<Drag>,
    show_jobs: bool,
    status: String,
    settings: Settings,
    sites: Vec<Site>,
    /// Passwords given this session, by connection, remembered or not.
    passwords: HashMap<String, String>,
    auth: Option<Auth>,
    preview: Option<preview::Preview>,
    clip: Option<explorer::Clip>,
    renaming: Option<explorer::Renaming>,
    /// A click on an item already picked, which edits its name if left alone a moment.
    slow_click: Option<(u64, String, f64)>,
    /// A folder just made, whose name is edited once it shows.
    pending_rename: Option<(u64, String)>,
    opened: Vec<explorer::Opened>,
    props: Option<explorer::Props>,
    /// What happened last, when it went well.
    note: String,
    /// Ctrl+V is held, so one press pastes once.
    paste_down: bool,
    drives: Vec<winshell::Drive>,
    drives_at: f64,
    /// When the folders here were last checked for changes.
    dirs_at: f64,
    known: Vec<KnownFolder>,
    thumbs: Thumbs,
    now: f64,
    ctx: Context,
    offset: i64,
    adopted: bool,
    next_id: u64,
    /// How many jobs there were, and how many had stopped, when the queue was last saved.
    queue_mark: (usize, usize),
    /// RDROP_SEARCH: a search for the first pane once it can run.
    start_search: Option<(u64, String, bool)>,
    /// Two folders being compared, and the sync that may follow.
    compare: Option<compare::Compare>,
    /// RDROP_COMPARE: compare the first two panes once both can be.
    start_compare: bool,
    watches: Vec<watch::Watch>,
    palette: Option<palette::Palette>,
    /// Commands run in a server's shell for a pane (chown, chmod -R), and how they ended.
    shell_jobs: Vec<(u64, Receiver<Result<String, String>>)>,
    /// RDROP_PROPS=NAME: that item's properties, in the first pane, once it is listed.
    start_props: Option<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install_fonts(ctx);
        theme::set(ctx, theme::GRAPHITE);
        let settings = load_settings();
        let (tx, hosts_rx) = std::sync::mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(profiles::slow());
            repaint.request_repaint();
        });
        let mut app = App {
            panes: Vec::new(),
            layout: split::Node::Pane(0),
            focus: 0,
            previous: 0,
            rects: Vec::new(),
            queue: Queue::new(ctx),
            hosts: Vec::new(),
            hosts_rx: Some(hosts_rx),
            host_input: String::new(),
            dialog: None,
            focus_dialog: false,
            drag: None,
            show_jobs: false,
            sites: sites::load(),
            passwords: HashMap::new(),
            auth: None,
            preview: None,
            clip: None,
            renaming: None,
            slow_click: None,
            pending_rename: None,
            opened: Vec::new(),
            props: None,
            note: String::new(),
            paste_down: false,
            drives: winshell::drives(),
            drives_at: 0.0,
            dirs_at: 0.0,
            known: known_folders(),
            thumbs: Thumbs::default(),
            now: 0.0,
            ctx: ctx.clone(),
            status: String::new(),
            settings,
            offset: local_offset(),
            adopted: false,
            next_id: 0,
            queue_mark: (0, 0),
            start_search: None,
            compare: None,
            start_compare: std::env::var("RDROP_COMPARE").is_ok(),
            watches: Vec::new(),
            palette: std::env::var("RDROP_PALETTE")
                .ok()
                .map(palette::Palette::with),
            shell_jobs: Vec::new(),
            start_props: std::env::var("RDROP_PROPS").ok(),
        };
        // `rdrop FOLDER` starts with that folder here; `--host=NAME` with that server beside it.
        let args: Vec<String> = std::env::args().skip(1).collect();
        let folder = args
            .iter()
            .find(|a| !a.starts_with("--"))
            .map(|a| a.trim_end_matches('"').to_string())
            .filter(|a| Path::new(a).is_dir());
        let host = args
            .iter()
            .find_map(|a| a.strip_prefix("--host=").map(str::to_string));
        let mut saved = app.settings.panes.clone();
        if saved.is_empty() || folder.is_some() || host.is_some() {
            saved = vec![
                SavedPane {
                    place: Some(String::new()),
                    path: folder.unwrap_or_else(home),
                    view: View::Details,
                },
                SavedPane {
                    place: host,
                    path: String::new(),
                    view: View::Details,
                },
            ];
        }
        *app.queue.options.lock().unwrap() = app.settings.transfer;
        app.watches = app
            .settings
            .watches
            .iter()
            .cloned()
            .map(watch::Watch::new)
            .collect();
        if let Some(path) = queue_path() {
            app.queue.load(&path);
            let hosts: Vec<String> = app
                .queue
                .jobs
                .lock()
                .unwrap()
                .iter()
                .flat_map(|j| [j.from.clone(), j.to.clone()])
                .filter_map(|p| match p {
                    Place::Server(h) => Some(h),
                    Place::Local => None,
                })
                .collect();
            for host in hosts {
                app.register(&host);
            }
        }
        let mut ids = Vec::new();
        for s in &saved {
            let place = s.place.as_ref().map(|p| {
                if p.is_empty() {
                    Place::Local
                } else {
                    Place::Server(p.clone())
                }
            });
            let id = app.add_pane(ctx, place, s.path.clone());
            if let Some(p) = app.pane_mut(id) {
                p.view = s.view;
            }
            ids.push(id);
        }
        app.layout = row(&ids);
        app.focus = ids.first().copied().unwrap_or(0);
        app.previous = app.panes.get(1).map_or(app.focus, |p| p.id);
        // RDROP_PREVIEW=FILE shows that file of the first pane's folder at the start.
        if let Ok(name) = std::env::var("RDROP_PREVIEW") {
            let first = app.focus;
            if let Some(item) = app
                .pane(first)
                .and_then(|p| p.items.iter().find(|i| i.name == name).cloned())
            {
                app.open_preview(first, &item);
            }
        }
        // RDROP_SEARCH=TEXT (or in:TEXT, inside files) searches the first pane at the start;
        // RDROP_JOBS=1 opens the transfers.
        if let Ok(q) = std::env::var("RDROP_SEARCH") {
            let (contents, q) = match q.strip_prefix("in:") {
                Some(rest) => (true, rest.to_string()),
                None => (false, q),
            };
            app.start_search = Some((app.focus, q, contents));
        }
        app.show_jobs = std::env::var("RDROP_JOBS").is_ok();
        // RDROP_DIALOG opens a dialog at the start, to look at it without clicking.
        let focus = app.focus;
        match std::env::var("RDROP_DIALOG").as_deref() {
            Ok("site") => {
                app.auth = Some(Auth::Site {
                    old: None,
                    site: Site {
                        name: "Production".into(),
                        host: "example.com".into(),
                        port: 22,
                        user: "deploy".into(),
                        remember: true,
                        ..Site::default()
                    },
                    port: "22".into(),
                    password: "secret".into(),
                    focus: false,
                })
            }
            Ok("password") => {
                app.auth = Some(Auth::Password {
                    pane: focus,
                    key: "site:Production".into(),
                    label: "deploy@example.com".into(),
                    password: String::new(),
                    remember: false,
                    wrong: false,
                    focus: true,
                })
            }
            Ok("delete") => {
                app.dialog = Some(Dialog::Delete(
                    focus,
                    vec![
                        "node_modules".into(),
                        "build.log".into(),
                        "README.md".into(),
                    ],
                ))
            }
            Ok("folder") => {
                app.dialog = Some(Dialog::NewFolder(focus, String::new()));
                app.focus_dialog = true;
            }
            _ => {}
        }
        app
    }

    fn add_pane(&mut self, ctx: &Context, place: Option<Place>, path: String) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.panes.push(Pane::new(id));
        if let Some(place) = place {
            self.set_place(ctx, id, place, Some(path));
        }
        id
    }

    fn pane(&self, id: u64) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    fn pane_mut(&mut self, id: u64) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    fn set_focus(&mut self, id: u64) {
        if id != self.focus {
            self.previous = self.focus;
            self.focus = id;
        }
    }

    /// Points a pane at this computer or at a server, in `path` or where it was last.
    fn site(&self, key: &str) -> Option<&Site> {
        let name = key.strip_prefix("site:")?;
        self.sites.iter().find(|s| s.name == name)
    }

    /// Tells the SFTP side how to reach `key`: a saved site's address, or a host from the
    /// SSH config by its name; with the password given this session or remembered.
    fn register(&self, key: &str) {
        let args = match self.site(key) {
            Some(site) => site.ssh_args(),
            None => vec![key.strip_prefix("site:").unwrap_or(key).to_string()],
        };
        let password = self
            .passwords
            .get(key)
            .cloned()
            .or_else(|| sites::password(key));
        sftp::register(key, sftp::Target { args, password });
    }

    fn set_place(&mut self, ctx: &Context, id: u64, place: Place, path: Option<String>) {
        let path = match &place {
            Place::Server(key) => {
                self.register(key);
                path.or_else(|| {
                    self.site(key)
                        .map(|s| s.folder.clone())
                        .filter(|f| !f.is_empty())
                })
            }
            Place::Local => path,
        };
        let remembered = match &place {
            Place::Server(host) => self.settings.remote.get(host).cloned(),
            Place::Local => None,
        };
        let Some(pane) = self.pane_mut(id) else {
            return;
        };
        pane.conn = None;
        pane.connected = false;
        pane.items.clear();
        pane.selected.clear();
        pane.back.clear();
        pane.error = None;
        pane.place = Some(place.clone());
        match &place {
            Place::Local => {
                let path = path
                    .filter(|p| p.is_empty() || Path::new(p).is_dir())
                    .unwrap_or_else(home);
                self.open(id, path);
            }
            Place::Server(host) => {
                pane.conn = Some(Remote::connect(host, ctx));
                pane.loading = true;
                pane.path = path
                    .filter(|p| !p.is_empty())
                    .or(remembered)
                    .unwrap_or_default();
            }
        }
    }

    fn open(&mut self, id: u64, path: String) {
        let Some(pane) = self.pane_mut(id) else {
            return;
        };
        pane.search = None;
        if pane.is_local() {
            pane.dir_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            match list_local(&path) {
                Ok(items) => pane.set_items(path, items),
                Err(e) => pane.error = Some(e),
            }
        } else if let Some(conn) = &pane.conn {
            pane.loading = true;
            pane.path_edit = path.clone();
            conn.ask(Ask::List(path));
        }
    }

    /// Starts looking under the pane's folder; the results take the place of its items.
    fn search(&mut self, id: u64, query: String, contents: bool) {
        let ctx = self.ctx.clone();
        let Some(pane) = self.pane_mut(id) else {
            return;
        };
        let Some(place) = pane.place.clone() else {
            return;
        };
        if query.trim().is_empty() || !pane.ready() || (pane.is_local() && pane.path.is_empty()) {
            return;
        }
        pane.search = Some(search::Search::start(
            &ctx,
            &place,
            &pane.path,
            query.trim(),
            contents,
        ));
        pane.items.clear();
        pane.selected.clear();
        pane.anchor = None;
        pane.error = None;
    }

    fn end_search(&mut self, id: u64) {
        let Some(pane) = self.pane_mut(id) else {
            return;
        };
        if pane.search.take().is_some() {
            pane.search_edit.clear();
            let path = pane.path.clone();
            self.open(id, path);
        }
    }

    /// Goes into a folder, remembering where it came from.
    fn go(&mut self, id: u64, path: String) {
        if let Some(pane) = self.pane_mut(id) {
            if pane.path != path {
                let from = pane.path.clone();
                pane.back.push(from);
            }
        }
        self.open(id, path);
    }

    fn refresh(&mut self, id: u64) {
        let again = self
            .pane(id)
            .and_then(|p| p.search.as_ref())
            .map(|s| (s.query.clone(), s.contents));
        if let Some((query, contents)) = again {
            self.search(id, query, contents);
            return;
        }
        if let Some(path) = self.pane(id).map(|p| p.path.clone()) {
            self.open(id, path);
        }
    }

    fn close_pane(&mut self, id: u64) {
        if self.panes.len() < 2 || !self.layout.remove(id) {
            return;
        }
        self.panes.retain(|p| p.id != id);
        if self.focus == id {
            self.focus = self.layout.panes()[0];
        }
        if self.previous == id || self.previous == self.focus {
            self.previous = self
                .layout
                .panes()
                .into_iter()
                .find(|p| *p != self.focus)
                .unwrap_or(self.focus);
        }
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.hosts_rx {
            if let Ok(found) = rx.try_recv() {
                self.hosts = found
                    .iter()
                    .filter_map(|p| p.name.strip_prefix("SSH: ").map(str::to_string))
                    .collect();
                self.hosts_rx = None;
            }
        }
        let mut lists = Vec::new();
        let mut refused = Vec::new();
        let mut reads = Vec::new();
        for pane in &mut self.panes {
            let Some(conn) = &pane.conn else { continue };
            let mut lost = None;
            let answers: Vec<Answer> = conn.rx.try_iter().collect();
            for answer in answers {
                match answer {
                    Answer::Connected(home) => {
                        pane.connected = true;
                        let start = if pane.path.is_empty() {
                            home
                        } else {
                            pane.path.clone()
                        };
                        lists.push((pane.id, start));
                    }
                    Answer::Listed(path, entries) => {
                        pane.set_items(path.clone(), from_remote(entries));
                        if let Some(Place::Server(host)) = &pane.place {
                            self.settings.remote.insert(host.clone(), path);
                        }
                    }
                    Answer::Changed(folder) => {
                        if folder == pane.path {
                            lists.push((pane.id, folder));
                        }
                    }
                    Answer::Failed(e) => {
                        pane.loading = false;
                        // A folder that can't be opened leaves the pane where it was.
                        pane.path_edit = pane.path.clone();
                        self.status = e;
                    }
                    Answer::Read { path, data, size } => {
                        reads.push((pane.id, path, data, size));
                    }
                    Answer::Lost(e) => lost = Some(e),
                }
            }
            if let Some(e) = lost {
                if sftp::is_refused(&e) {
                    if let Some(Place::Server(key)) = &pane.place {
                        refused.push((pane.id, key.clone()));
                    }
                }
                pane.connected = false;
                pane.loading = false;
                pane.error = Some(e);
                pane.conn = None;
            }
        }
        for (id, path) in lists {
            self.open(id, path);
        }
        for (id, path, data, size) in reads {
            let ctx = self.ctx.clone();
            if let Some(p) = self
                .preview
                .as_mut()
                .filter(|p| p.pane == id && p.path == path)
            {
                p.set_data(&ctx, data, size);
            }
        }
        // A server that wouldn't take the keys asks for a password, here.
        for (pane, key) in refused {
            if self.auth.is_some() {
                break;
            }
            let wrong = self.passwords.remove(&key).is_some() || sites::password(&key).is_some();
            let label = self.site(&key).map_or(key.clone(), Site::address);
            let remember = self.site(&key).is_some_and(|s| s.remember);
            self.auth = Some(Auth::Password {
                pane,
                key,
                label,
                password: String::new(),
                remember,
                wrong,
                focus: true,
            });
        }
        for pane in &mut self.panes {
            if let Some(found) = pane.search.as_mut().and_then(|s| s.poll()) {
                pane.items.extend(found);
                pane.sort_items();
            }
        }
        let changed: Vec<(Place, String)> = self.queue.changed.try_iter().collect();
        for (place, folder) in changed {
            let ids: Vec<u64> = self
                .panes
                .iter()
                .filter(|p| p.place.as_ref() == Some(&place))
                .filter(|p| match place {
                    Place::Local => Path::new(&p.path) == Path::new(&folder),
                    Place::Server(_) => p.path == folder,
                })
                .map(|p| p.id)
                .collect();
            for id in ids {
                self.refresh(id);
            }
        }
        if self.start_compare
            && self.panes.len() > 1
            && self.panes.iter().all(|p| p.ready() && !p.loading)
        {
            self.start_compare = false;
            let first = self.panes[0].id;
            self.previous = self.panes[1].id;
            self.compare_panes(first);
        }
        if let Some((id, q, contents)) = self.start_search.clone() {
            if self
                .pane(id)
                .is_some_and(|p| p.ready() && !p.loading && !p.path.is_empty())
            {
                self.start_search = None;
                if let Some(p) = self.pane_mut(id) {
                    p.search_edit = q.clone();
                    p.search_contents = contents;
                }
                self.search(id, q, contents);
            }
        }
        if let Some(name) = self.start_props.clone() {
            let first = self.panes.first().map(|p| p.id).unwrap_or(0);
            if self
                .pane(first)
                .is_some_and(|p| p.items.iter().any(|i| i.name == name))
            {
                self.start_props = None;
                self.open_props(first, &name);
            }
        }
        let mut ended = Vec::new();
        self.shell_jobs.retain(|(pane, rx)| match rx.try_recv() {
            Ok(result) => {
                ended.push((*pane, result));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => false,
        });
        for (pane, result) in ended {
            match result {
                Ok(note) => self.note = note,
                Err(e) => self.status = e,
            }
            self.refresh(pane);
        }
        // A folder here that something else changed is listed again, as Explorer does.
        if self.now - self.dirs_at > 1.0 {
            self.dirs_at = self.now;
            let stale: Vec<u64> = self
                .panes
                .iter()
                .filter(|p| p.is_local() && !p.path.is_empty() && p.search.is_none())
                .filter(|p| {
                    std::fs::metadata(&p.path).and_then(|m| m.modified()).ok() != p.dir_mtime
                })
                .map(|p| p.id)
                .collect();
            for id in stale {
                self.refresh(id);
            }
        }
        if self.panes.iter().any(|p| p.is_local()) {
            self.ctx
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
        // The queue is kept on disk whenever a job comes or goes, so a crash loses nothing.
        let mark = {
            let jobs = self.queue.jobs.lock().unwrap();
            (jobs.len(), jobs.iter().filter(|j| !j.active()).count())
        };
        if mark != self.queue_mark {
            self.queue_mark = mark;
            if let Some(path) = queue_path() {
                self.queue.save(&path);
            }
        }
    }

    /// Copies the named items of pane `from` into pane `to`'s folder, or `into` a folder of it.
    fn copy(&mut self, from: u64, names: &[String], to: u64, into: Option<String>) {
        let (Some(src), Some(dst)) = (self.pane(from), self.pane(to)) else {
            return;
        };
        if from == to && into.is_none() {
            return;
        }
        if !src.ready() || !dst.ready() {
            self.status = "Both panes need to be connected to copy between them".into();
            return;
        }
        if dst.is_local() && dst.path.is_empty() && into.is_none() {
            self.status = "Open a folder to copy into".into();
            return;
        }
        let (src_place, dst_place) = (src.place.clone().unwrap(), dst.place.clone().unwrap());
        let dir = into.unwrap_or_else(|| dst.path.clone());
        let jobs: Vec<String> = names.iter().map(|n| src.child(n)).collect();
        for path in jobs {
            self.queue
                .add(src_place.clone(), path, dst_place.clone(), dir.clone());
        }
    }

    /// The pane F5 copies to: the one in focus before this one.
    fn other_pane(&self, of: u64) -> Option<u64> {
        if self.previous != of && self.pane(self.previous).is_some() {
            return Some(self.previous);
        }
        self.panes.iter().map(|p| p.id).find(|p| *p != of)
    }

    fn upload_paths(&mut self, paths: Vec<PathBuf>, to: u64) {
        let Some(dst) = self.pane(to) else { return };
        if !dst.ready() || (dst.is_local() && dst.path.is_empty()) {
            self.status = "Drop files on a pane that shows a folder".into();
            return;
        }
        let (place, dir) = (dst.place.clone().unwrap(), dst.path.clone());
        for path in paths {
            self.queue.add(
                Place::Local,
                path.display().to_string(),
                place.clone(),
                dir.clone(),
            );
        }
    }

    fn delete(&mut self, id: u64, names: Vec<String>, _confirmed: bool) {
        let Some(pane) = self.pane(id) else { return };
        if pane.is_local() {
            let paths: Vec<String> = names.iter().map(|n| pane.child(n)).collect();
            for path in paths {
                let p = PathBuf::from(&path);
                let result = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
                if let Err(e) = result {
                    self.status = format!("{path}: {e}");
                }
            }
            self.refresh(id);
        } else if let Some(conn) = &pane.conn {
            conn.ask(Ask::Delete(names.iter().map(|n| pane.child(n)).collect()));
        }
    }

    // ---- drawing ----

    fn title_bar(&mut self, ui: &mut Ui) {
        Panel::top("title")
            .exact_size(40.0)
            .frame(Frame::NONE.fill(theme::WINDOW_BG()).inner_margin(Margin {
                left: 14,
                right: 4,
                top: 6,
                bottom: 0,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                title_drag(ui);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("rdrop")
                            .size(15.0)
                            .strong()
                            .color(theme::TEXT()),
                    );
                    ui.label(
                        RichText::new("files between this computer and your servers")
                            .size(12.5)
                            .color(theme::COMMENT()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Min), window_buttons);
                });
            });
    }

    /// One line of the list on the left: a place to open, drag or click.
    fn place_row(&mut self, ui: &mut Ui, ctx: &Context, place: Place) -> egui::Response {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click_and_drag());
        let open_somewhere = self
            .panes
            .iter()
            .any(|p| p.place.as_ref() == Some(&place) && (p.connected || p.is_local()));
        let fill = if resp.hovered() {
            theme::tint(9)
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 7.0, fill);
        let (icon, name) = match &place {
            Place::Local => (icons::FOLDER_ROOT, "This computer"),
            Place::Server(key) if key.starts_with("site:") => (icons::SAVE, place.label()),
            Place::Server(_) => (icons::REMOTE, place.label()),
        };
        let color = if open_somewhere {
            GOOD
        } else {
            theme::COMMENT()
        };
        ui.painter().text(
            rect.left_center() + vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            icon,
            FontId::proportional(14.0),
            color,
        );
        ui.painter().text(
            rect.left_center() + vec2(32.0, 0.0),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(13.5),
            theme::TEXT(),
        );
        let resp = resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Click: open in the pane in focus · Ctrl+click: in a new pane · Drag onto a pane: open beside it");
        if resp.drag_started() {
            self.drag = Some(Drag::Place(place.clone()));
        }
        if resp.clicked() {
            if ui.input(|i| i.modifiers.command) {
                let id = self.add_pane(ctx, Some(place), String::new());
                self.layout.split(self.focus, id, true, false);
                self.set_focus(id);
            } else {
                self.set_place(ctx, self.focus, place, None);
            }
        }
        resp
    }

    /// A folder here, in the list on the left: opens in the pane in focus.
    fn local_row(&mut self, ui: &mut Ui, icon: char, name: &str, path: &str) {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
        let here = self
            .pane(self.focus)
            .is_some_and(|p| p.is_local() && Path::new(&p.path) == Path::new(path));
        let fill = if here {
            theme::tint(14)
        } else if resp.hovered() {
            theme::tint(9)
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 7.0, fill);
        ui.painter().text(
            rect.left_center() + vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            icon,
            FontId::proportional(13.0),
            theme::ACCENT(),
        );
        ui.painter().with_clip_rect(rect).text(
            rect.left_center() + vec2(32.0, 0.0),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(13.0),
            theme::TEXT(),
        );
        if resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            let focus = self.focus;
            let ctx = self.ctx.clone();
            if self.pane(focus).is_some_and(|p| p.is_local()) {
                self.go(focus, path.to_string());
            } else {
                self.set_place(&ctx, focus, Place::Local, Some(path.to_string()));
            }
        }
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        Panel::left("places")
            .exact_size(220.0)
            .resizable(false)
            .frame(Frame::NONE.fill(theme::WINDOW_BG()).inner_margin(Margin {
                left: 12,
                right: 10,
                top: 6,
                bottom: 10,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("PLACES")
                        .size(11.5)
                        .strong()
                        .color(theme::COMMENT()),
                );
                ui.add_space(4.0);
                self.local_row(ui, ui::DESKTOP, "This PC", "");
                for k in self.known.clone() {
                    self.local_row(ui, k.icon, &k.name, &k.path);
                }
                for d in self
                    .drives
                    .iter()
                    .map(|d| (pane::drive_icon(d), d.title(), d.root.clone()))
                    .collect::<Vec<_>>()
                {
                    self.local_row(ui, d.0, &d.1, &d.2);
                }
                self.watches_ui(ui);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("SITES")
                            .size(11.5)
                            .strong()
                            .color(theme::COMMENT()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("+ New")
                            .on_hover_text("Save a connection: host, user, password")
                            .clicked()
                        {
                            self.auth = Some(Auth::Site {
                                old: None,
                                site: Site {
                                    port: 22,
                                    ..Site::default()
                                },
                                port: "22".into(),
                                password: String::new(),
                                focus: true,
                            });
                        }
                    });
                });
                ui.add_space(2.0);
                if self.sites.is_empty() {
                    ui.label(
                        RichText::new("Servers you save, with their user and password.")
                            .size(12.0)
                            .color(theme::COMMENT()),
                    );
                }
                for site in self.sites.clone() {
                    let resp = self.place_row(ui, &ctx, Place::Server(site.key()));
                    resp.context_menu(|ui| {
                        if ui.button("Edit…").clicked() {
                            self.auth = Some(Auth::Site {
                                old: Some(site.name.clone()),
                                port: site.port.to_string(),
                                site: site.clone(),
                                password: String::new(),
                                focus: true,
                            });
                        }
                        if ui.button("Forget the password").clicked() {
                            sites::forget(&site.key());
                            self.passwords.remove(&site.key());
                            if let Some(s) = self.sites.iter_mut().find(|s| s.name == site.name) {
                                s.remember = false;
                            }
                            sites::save(&self.sites);
                        }
                        if ui.button("Delete").clicked() {
                            sites::forget(&site.key());
                            self.sites.retain(|s| s.name != site.name);
                            sites::save(&self.sites);
                        }
                    });
                }
                ui.add_space(10.0);
                ui.label(
                    RichText::new("SSH CONFIG")
                        .size(11.5)
                        .strong()
                        .color(theme::COMMENT()),
                );
                ui.add_space(4.0);
                if self.hosts.is_empty() {
                    let text = if self.hosts_rx.is_some() {
                        "Reading ~/.ssh/config…"
                    } else {
                        "No hosts in ~/.ssh/config"
                    };
                    ui.label(RichText::new(text).size(12.0).color(theme::COMMENT()));
                }
                for host in self.hosts.clone() {
                    self.place_row(ui, &ctx, Place::Server(host));
                }
                ui.add_space(10.0);
                let edit = ui.add(
                    TextEdit::singleline(&mut self.host_input)
                        .hint_text("Other: user@host")
                        .desired_width(f32::INFINITY),
                );
                if edit.lost_focus()
                    && ui.input(|i| i.key_pressed(Key::Enter))
                    && !self.host_input.trim().is_empty()
                {
                    let host = self.host_input.trim().to_string();
                    self.set_place(&ctx, self.focus, Place::Server(host), None);
                }
            });
    }

    /// The bar at the bottom: what happened last, and the transfers, which open above it.
    fn bottom_bar(&mut self, ui: &mut Ui) {
        let (running, waiting, failed, done, total, sum_speed) = {
            let jobs = self.queue.jobs.lock().unwrap();
            let running: Vec<&transfer::Job> =
                jobs.iter().filter(|j| j.state == State::Running).collect();
            let progress: (u64, u64) = jobs
                .iter()
                .filter(|j| j.active())
                .fold((0, 0), |a, j| (a.0 + j.done, a.1 + j.total));
            (
                running.len(),
                jobs.iter().filter(|j| j.state == State::Waiting).count(),
                jobs.iter()
                    .filter(|j| matches!(j.state, State::Failed(_)))
                    .count(),
                jobs.iter().filter(|j| j.state == State::Done).count(),
                progress,
                running.iter().map(|j| j.speed()).sum::<f64>(),
            )
        };
        let mut toggle = false;
        Panel::bottom("bar")
            .exact_size(30.0)
            .frame(
                Frame::NONE
                    .fill(theme::WINDOW_BG())
                    .inner_margin(Margin::symmetric(14, 4)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if self.status.is_empty() {
                        let text = if self.note.is_empty() {
                            "Ready"
                        } else {
                            self.note.as_str()
                        };
                        ui.label(RichText::new(text).size(12.0).color(theme::COMMENT()));
                    } else {
                        ui.label(RichText::new(&self.status).size(12.0).color(BAD));
                        if ui.small_button(icons::CLOSE.to_string()).clicked() {
                            self.status.clear();
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let text = if running + waiting > 0 {
                            let pct = if total.1 > 0 {
                                total.0 as f64 * 100.0 / total.1 as f64
                            } else {
                                0.0
                            };
                            format!(
                                "↑↓  {running} running{}  ·  {pct:.0}%  ·  {}/s",
                                if waiting > 0 {
                                    format!(", {waiting} waiting")
                                } else {
                                    String::new()
                                },
                                size_text(sum_speed as u64)
                            )
                        } else if done + failed > 0 {
                            format!(
                                "↑↓  {done} done{}",
                                if failed > 0 {
                                    format!(", {failed} failed")
                                } else {
                                    String::new()
                                }
                            )
                        } else {
                            "↑↓  No transfers".to_string()
                        };
                        let color = if failed > 0 {
                            BAD
                        } else if running > 0 {
                            theme::ACCENT()
                        } else {
                            theme::COMMENT()
                        };
                        let button = egui::Button::new(RichText::new(text).size(12.0).color(color))
                            .frame(self.show_jobs);
                        if ui.add(button).on_hover_text("Transfers").clicked() {
                            toggle = true;
                        }
                        if running + waiting > 0 && total.1 > 0 {
                            let fraction = total.0 as f32 / total.1 as f32;
                            ui.add(
                                egui::ProgressBar::new(fraction)
                                    .desired_width(120.0)
                                    .desired_height(5.0),
                            );
                        }
                    });
                });
            });
        if toggle {
            self.show_jobs = !self.show_jobs;
        }
    }

    /// The list of transfers, over the window above the bar, out of the way of everything else.
    fn jobs_popup(&mut self, ctx: &Context) {
        if !self.show_jobs {
            return;
        }
        let screen = ctx.content_rect();
        let width = 560.0;
        let mut close = false;
        let (mut resume, mut resume_all) = (None, false);
        egui::Area::new(egui::Id::new("jobs"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(screen.right() - width - 14.0, screen.bottom() - 34.0))
            .pivot(Align2::LEFT_BOTTOM)
            .show(ctx, |ui| {
                Frame::popup(ui.style()).inner_margin(Margin::same(12)).show(ui, |ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Transfers").size(14.0).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button(icons::CLOSE.to_string()).clicked() {
                                close = true;
                            }
                                                        if ui.small_button("Clear finished").clicked() {
                                self.queue.clear_finished();
                            }
                            if ui.small_button("Stop all").clicked() {
                                self.queue.stop_all();
                            }
                            let stopped = self.queue.jobs.lock().unwrap().iter().any(|j| matches!(j.state, State::Stopped | State::Failed(_)));
                            if stopped && ui.small_button("Resume all").clicked() {
                                resume_all = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    let mut opts = *self.queue.options.lock().unwrap();
                    let before = opts;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Already there").size(12.0).color(theme::COMMENT()));
                        ui.add_space(4.0);
                        ui::segmented(ui, &mut opts.conflict, &engine::Conflict::ALL);
                    });
                    ui.add_space(2.0);
                    ui::switch(ui, &mut opts.verify, "Check copies", "SHA-256 at both ends, where the server has sha256sum");
                    if opts != before {
                        *self.queue.options.lock().unwrap() = opts;
                        self.settings.transfer = opts;
                    }
                    ui.add_space(6.0);
                    ui.separator();
                    let jobs = self.queue.jobs.lock().unwrap();
                    if jobs.is_empty() {
                        ui.label(RichText::new("Drag files from one pane to another, or from Explorer, to copy them.").size(12.5).color(theme::COMMENT()));
                    }
                    ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for job in jobs.iter().rev() {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(job.name.as_str()).size(13.0).strong().color(theme::TEXT()));
                                ui.label(RichText::new(format!("{}  →  {}", job.from.label(), job.to.label())).size(11.5).color(theme::COMMENT()));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                                                        if job.active() && ui.small_button(icons::CLOSE.to_string()).on_hover_text("Stop").clicked() {
                                        job.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    if matches!(job.state, State::Stopped | State::Failed(_)) && ui.small_button("Resume").on_hover_text("Carry on from where it got to").clicked() {
                                        resume = Some(job.id);
                                    }
                                });
                            });
                            let detail = match &job.state {
                                State::Waiting => "Waiting".to_string(),
                                State::Running => format!(
                                    "{} of {}  ·  {}/s{}",
                                    size_text(job.done),
                                    size_text(job.total),
                                    size_text(job.speed() as u64),
                                    if job.current.is_empty() || job.current == job.name { String::new() } else { format!("  ·  {}", job.current) }
                                ),
                                                                State::Done => format!("Done  ·  {}  ·  {}/s{}  ·  into {}", size_text(job.total), size_text(job.speed() as u64), if job.detail.is_empty() { String::new() } else { format!("  ·  {}", job.detail) }, job.to_dir),
                                State::Failed(e) => format!("Failed: {e}"),
                                State::Stopped if !job.detail.is_empty() => format!("Stopped  ·  {}", job.detail),
                                State::Stopped => "Stopped".to_string(),
                            };
                            let fraction = if job.total > 0 { job.done as f32 / job.total as f32 } else if job.state == State::Done { 1.0 } else { 0.0 };
                            ui.add(egui::ProgressBar::new(fraction).desired_height(4.0));
                            let color = if matches!(job.state, State::Failed(_)) { BAD } else { theme::COMMENT() };
                            ui.add(egui::Label::new(RichText::new(detail).size(11.5).color(color)).truncate());
                        }
                    });
                });
            });
        if let Some(id) = resume {
            self.queue.resume(id);
        }
        if resume_all {
            self.queue.resume_all();
        }
        if close || ctx.input(|i| i.key_pressed(Key::Escape)) && self.dialog.is_none() {
            self.show_jobs = false;
        }
    }

    /// The panes, laid out by the split tree, with the lines between them to drag.
    fn panes_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let area = ui.available_rect_before_wrap();
        let (mut rects, mut dividers) = (Vec::new(), Vec::new());
        self.layout.layout(area, &mut rects, &mut dividers);
        for divider in &dividers {
            let id = ui.id().with(("divider", &divider.path));
            let resp = ui.interact(divider.line, id, Sense::drag());
            let (delta, size, icon) = if divider.across {
                (
                    resp.drag_delta().x,
                    divider.area.width(),
                    egui::CursorIcon::ResizeHorizontal,
                )
            } else {
                (
                    resp.drag_delta().y,
                    divider.area.height(),
                    egui::CursorIcon::ResizeVertical,
                )
            };
            if resp.hovered() || resp.dragged() {
                ctx.set_cursor_icon(icon);
            }
            if delta != 0.0 {
                self.layout.drag(&divider.path, delta, size);
            }
        }
        let mut acts = Vec::new();
        for (id, r) in &rects {
            let inner = r.shrink(5.0);
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("pane", *id))
                    .max_rect(inner),
            );
            self.pane_ui(&mut child, *id, inner, &mut acts);
        }
        self.rects = rects;
        for (id, act) in acts {
            self.apply(&ctx, id, act);
        }
        self.drops(&ctx);
    }

    fn apply(&mut self, ctx: &Context, id: u64, act: PaneAct) {
        match act {
            PaneAct::Open(path) => self.go(id, path),
            PaneAct::Back => {
                if let Some(back) = self.pane_mut(id).and_then(|p| p.back.pop()) {
                    self.open(id, back);
                }
            }
            PaneAct::Pick(place) => self.set_place(ctx, id, place, None),
            PaneAct::Split(across) => {
                // The new pane looks where this one does.
                let (place, path) = self
                    .pane(id)
                    .map_or((None, String::new()), |p| (p.place.clone(), p.path.clone()));
                let new = self.add_pane(ctx, place, path);
                self.layout.split(id, new, across, false);
                self.set_focus(new);
            }
            PaneAct::Close => self.close_pane(id),
            PaneAct::NewFolder => self.new_folder(id),
            PaneAct::Paste => self.paste(id),
            PaneAct::Refresh => self.refresh(id),
            PaneAct::Search(query, contents) => self.search(id, query, contents),
            PaneAct::EndSearch => self.end_search(id),
            PaneAct::Compare => self.compare_panes(id),
            PaneAct::Watch => self.start_watch(id),
            PaneAct::View(view) => {
                if let Some(p) = self.pane_mut(id) {
                    p.view = view;
                }
            }
            PaneAct::ToggleHidden => self.settings.show_hidden = !self.settings.show_hidden,
            PaneAct::Activate(name) => {
                // In This PC, a drive or folder's path is its name.
                let local_root = self
                    .pane(id)
                    .is_some_and(|p| p.is_local() && p.path.is_empty());
                if local_root {
                    self.go(id, name);
                } else {
                    self.open_item(id, &name);
                }
            }
            PaneAct::OpenWith(name) => self.open_with_app(id, &name),
            PaneAct::Ride(name) => self.open_in_ride(id, &name),
            PaneAct::Clip(names, cut) => self.set_clip(id, &names, cut),
            PaneAct::StartRename(name) => self.start_rename(id, &name),
            PaneAct::FinishRename(keep) => self.finish_rename(keep),
            PaneAct::Delete(names) => self.delete_soft(id, names),
            PaneAct::DeleteForGood(names) => self.dialog = Some(Dialog::Delete(id, names)),
            PaneAct::Properties(name) => self.open_props(id, &name),
            PaneAct::CopyTo(names) => {
                if let Some(to) = self.other_pane(id) {
                    self.copy(id, &names, to, None);
                }
            }
            PaneAct::CopyPaths(names) => {
                if let Some(p) = self.pane(id) {
                    ctx.copy_text(
                        names
                            .iter()
                            .map(|n| p.child(n))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
            }
            PaneAct::Reveal(path) => {
                let _ = std::process::Command::new("explorer")
                    .arg(format!("/select,{path}"))
                    .spawn();
            }
            PaneAct::StartRows(names) => self.drag = Some(Drag::Rows { from: id, names }),
            PaneAct::StartPane => self.drag = Some(Drag::Pane(id)),
        }
    }

    /// What a drag shows while it is held, and what happens where it is let go.
    fn drops(&mut self, ctx: &Context) {
        let Some(drag) = &self.drag else { return };
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let released = ctx.input(|i| i.pointer.any_released() || !i.pointer.any_down());
        let target = pointer.and_then(|p| {
            self.rects
                .iter()
                .find(|(_, r)| r.contains(p))
                .map(|(id, r)| (*id, *r, p))
        });
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop"));
        let painter = ctx.layer_painter(layer);
        let label;
        let mut action: Option<Box<dyn FnOnce(&mut App)>> = None;
        match drag {
            Drag::Rows { from, names } => {
                label = match names.as_slice() {
                    [one] => one.clone(),
                    many => format!("{} items", many.len()),
                };
                if let Some((id, r, p)) = target {
                    if id != *from {
                        painter.rect(
                            r.shrink(2.0),
                            10,
                            theme::ACCENT().gamma_multiply(0.08),
                            (2.0, theme::ACCENT()),
                            egui::StrokeKind::Inside,
                        );
                        let into = self.pane(id).map_or("", |p| p.path.as_str()).to_string();
                        let hint = format!(
                            "Copy into {}",
                            if into.is_empty() { "this pane" } else { &into }
                        );
                        painter.text(
                            pos2(r.center().x, r.bottom() - 24.0),
                            Align2::CENTER_CENTER,
                            hint,
                            FontId::proportional(13.0),
                            theme::ACCENT(),
                        );
                        let _ = p;
                        let (from, names) = (*from, names.clone());
                        action = Some(Box::new(move |app: &mut App| {
                            app.copy(from, &names, id, None)
                        }));
                    }
                }
            }
            Drag::Pane(moving) => {
                label = self
                    .pane(*moving)
                    .and_then(|p| p.place.as_ref())
                    .map_or("Pane".into(), |p| p.label().to_string());
                if let Some((id, r, p)) = target {
                    if id != *moving {
                        let (zone, shape) = split::zone_at(r, p);
                        painter.rect(
                            shape.shrink(4.0),
                            10,
                            theme::ACCENT().gamma_multiply(0.14),
                            (1.5, theme::ACCENT()),
                            egui::StrokeKind::Inside,
                        );
                        let moving = *moving;
                        action = Some(Box::new(move |app: &mut App| {
                            if zone == split::Zone::Into {
                                app.layout.swap(moving, id);
                            } else if app.layout.remove(moving) {
                                let across = matches!(zone, split::Zone::Left | split::Zone::Right);
                                let before = matches!(zone, split::Zone::Left | split::Zone::Above);
                                app.layout.split(id, moving, across, before);
                            }
                        }));
                    }
                }
            }
            Drag::Place(place) => {
                label = place.label().to_string();
                if let Some((id, r, p)) = target {
                    let (zone, shape) = split::zone_at(r, p);
                    painter.rect(
                        shape.shrink(4.0),
                        10,
                        theme::ACCENT().gamma_multiply(0.14),
                        (1.5, theme::ACCENT()),
                        egui::StrokeKind::Inside,
                    );
                    let place = place.clone();
                    let ctx = ctx.clone();
                    action = Some(Box::new(move |app: &mut App| {
                        if zone == split::Zone::Into {
                            app.set_place(&ctx, id, place, None);
                            app.set_focus(id);
                        } else {
                            let new = app.add_pane(&ctx, Some(place), String::new());
                            let across = matches!(zone, split::Zone::Left | split::Zone::Right);
                            let before = matches!(zone, split::Zone::Left | split::Zone::Above);
                            app.layout.split(id, new, across, before);
                            app.set_focus(new);
                        }
                    }));
                }
            }
        }
        if let Some(p) = pointer {
            let tip = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new("drag_label"),
            ));
            let galley = tip.layout_no_wrap(label, FontId::proportional(12.5), theme::TEXT());
            let r = Rect::from_min_size(p + vec2(14.0, 10.0), galley.size() + vec2(16.0, 10.0));
            tip.rect(
                r,
                CornerRadius::same(7),
                theme::PANEL_BG(),
                (1.0, theme::ACCENT()),
                egui::StrokeKind::Inside,
            );
            tip.galley(r.min + vec2(8.0, 5.0), galley, theme::TEXT());
        }
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        if released {
            self.drag = None;
            if let Some(action) = action {
                action(self);
            }
        }
    }

    fn dialogs(&mut self, ctx: &Context) {
        let delete_here = match &self.dialog {
            Some(Dialog::Delete(id, _)) => self.pane(*id).is_some_and(|p| p.is_local()),
            _ => false,
        };
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let focus = std::mem::take(&mut self.focus_dialog);
        let place_of = |panes: &[Pane], id: u64| {
            panes
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.place.as_ref())
                .map_or(String::new(), |p| p.label().to_string())
        };
        let answer = match dialog {
            Dialog::NewFolder(id, name) => {
                let place = place_of(&self.panes, *id);
                let can = valid_name(name);
                ui::Dialog {
                    id: "new-folder",
                    icon: icons::NEW_FOLDER,
                    title: "New folder",
                    subtitle: &format!("In {place}"),
                    confirm: "Create",
                    other: None,
                    tone: ui::Tone::Normal,
                    can,
                    width: 420.0,
                }
                .show(ctx, |ui| {
                    let edit = ui::field(ui, "folder", "Name", name, "New folder", focus);
                    edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                })
            }
            Dialog::Delete(id, names) => {
                let place = place_of(&self.panes, *id);
                let title = match names.as_slice() {
                    [one] => format!("Delete {one}?"),
                    many => format!("Delete {} items?", many.len()),
                };
                let subtitle = if delete_here {
                    format!("From {place}, for good, not to the Recycle Bin.")
                } else {
                    format!("From {place}, for good: there is no Recycle Bin on a server.")
                };
                ui::Dialog {
                    id: "delete",
                    icon: icons::TRASH,
                    title: &title,
                    subtitle: &subtitle,
                    confirm: "Delete",
                    other: None,
                    tone: ui::Tone::Danger,
                    can: true,
                    width: 440.0,
                }
                .show(ctx, |ui| {
                    ui::callout(ui, ui::RED, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for name in names.iter().take(8) {
                            ui.label(RichText::new(name).size(12.5).color(theme::TEXT()));
                        }
                        if names.len() > 8 {
                            ui.label(
                                RichText::new(format!("and {} more", names.len() - 8))
                                    .size(12.0)
                                    .color(theme::COMMENT()),
                            );
                        }
                    });
                    ui.label(
                        RichText::new("Folders go with everything in them.")
                            .size(12.0)
                            .color(theme::COMMENT()),
                    );
                    ui.input(|i| i.key_pressed(Key::Enter))
                })
            }
        };
        match answer {
            ui::Answer::Waiting => {}
            ui::Answer::Cancel => self.dialog = None,
            ui::Answer::Confirm | ui::Answer::Other => match self.dialog.take() {
                Some(Dialog::NewFolder(id, name)) => {
                    let Some(pane) = self.pane(id) else { return };
                    let path = pane.child(name.trim());
                    if pane.is_local() {
                        if let Err(e) = std::fs::create_dir(&path) {
                            self.status = e.to_string();
                        }
                        self.refresh(id);
                    } else if let Some(c) = &pane.conn {
                        c.ask(Ask::Mkdir(path));
                    }
                }
                Some(Dialog::Delete(id, names)) => self.delete(id, names, true),
                None => {}
            },
        }
    }

    /// Renames an item of a pane, here or on its server.
    fn rename(&mut self, id: u64, old: &str, new: &str) {
        if new.is_empty() || new == old {
            return;
        }
        let Some(pane) = self.pane(id) else { return };
        let new = match old.rfind(['/', '\\']) {
            Some(at) if !new.contains(['/', '\\']) => format!("{}{new}", &old[..=at]),
            _ => new.to_string(),
        };
        let (from, to) = (pane.child(old), pane.child(&new));
        if pane.is_local() {
            if let Err(e) = std::fs::rename(&from, &to) {
                self.status = e.to_string();
            }
            self.refresh(id);
        } else if let Some(c) = &pane.conn {
            c.ask(Ask::Rename(from, to));
        }
    }

    /// Saving a site, and the password questions.
    fn auth_dialog(&mut self, ctx: &Context) {
        let Some(auth) = &mut self.auth else { return };
        let answer = match auth {
            Auth::Site {
                old,
                site,
                port,
                password,
                focus,
            } => {
                let focus = std::mem::take(focus);
                let remembered = old
                    .as_ref()
                    .is_some_and(|name| sites::password(&format!("site:{name}")).is_some());
                let taken = self
                    .sites
                    .iter()
                    .any(|s| s.name == site.name.trim() && Some(&s.name) != old.as_ref());
                let can = !site.name.trim().is_empty()
                    && !site.host.trim().is_empty()
                    && !taken
                    && port.trim().parse::<u16>().is_ok();
                let title = if old.is_some() {
                    "Edit site"
                } else {
                    "New site"
                };
                ui::Dialog {
                    id: "site",
                    icon: icons::REMOTE,
                    title,
                    subtitle: "A server to connect to with one click, with its user and password.",
                    confirm: "Save and connect",
                    other: Some("Save"),
                    tone: ui::Tone::Normal,
                    can,
                    width: 480.0,
                }
                .show(ctx, |ui| {
                    let enter = std::cell::Cell::new(false);
                    let pressed = |ui: &Ui, r: &egui::Response| {
                        r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                    };
                    let r = ui::field(
                        ui,
                        "site-name",
                        "Name",
                        &mut site.name,
                        "Production, shop…",
                        focus,
                    );
                    enter.set(enter.get() | pressed(ui, &r));
                    if taken {
                        ui.label(
                            RichText::new("A site with that name is already saved.")
                                .size(12.0)
                                .color(ui::RED),
                        );
                    }
                    ui::pair(
                        ui,
                        |ui| {
                            let r = ui::field(
                                ui,
                                "site-host",
                                "Host",
                                &mut site.host,
                                "example.com or 10.0.0.5",
                                false,
                            );
                            enter.set(enter.get() | pressed(ui, &r));
                        },
                        |ui| {
                            let r = ui::field(ui, "site-port", "Port", port, "22", false);
                            enter.set(enter.get() | pressed(ui, &r));
                        },
                        86.0,
                    );
                    let r = ui::field(
                        ui,
                        "site-user",
                        "User",
                        &mut site.user,
                        "root, deploy…",
                        false,
                    );
                    enter.set(enter.get() | pressed(ui, &r));
                    let hint = if remembered {
                        "Remembered; type to change it"
                    } else {
                        "Empty: sign in with your SSH keys"
                    };
                    let r = ui::password(ui, "site-password", "Password", password, hint, false);
                    enter.set(enter.get() | pressed(ui, &r));
                    ui::switch(
                        ui,
                        &mut site.remember,
                        "Remember password",
                        "encrypted by Windows for your account",
                    );
                    let r = ui::field(
                        ui,
                        "site-folder",
                        "Start in",
                        &mut site.folder,
                        "The home folder",
                        false,
                    );
                    enter.set(enter.get() | pressed(ui, &r));
                    enter.get()
                })
            }
            Auth::Password {
                label,
                password,
                remember,
                wrong,
                focus,
                ..
            } => {
                let focus = std::mem::take(focus);
                let subtitle = if *wrong {
                    "That password didn't work. Try again.".to_string()
                } else {
                    "The server didn't take your SSH keys; it wants a password.".to_string()
                };
                let title = format!("Sign in to {label}");
                let can = !password.is_empty();
                ui::Dialog {
                    id: "password",
                    icon: if *wrong { ui::WARNING } else { ui::LOCK },
                    title: &title,
                    subtitle: &subtitle,
                    confirm: "Connect",
                    other: None,
                    tone: if *wrong {
                        ui::Tone::Danger
                    } else {
                        ui::Tone::Normal
                    },
                    can,
                    width: 440.0,
                }
                .show(ctx, |ui| {
                    let r = ui::password(ui, "password", "Password", password, "", focus);
                    ui::switch(
                        ui,
                        remember,
                        "Remember password",
                        "encrypted by Windows for your account",
                    );
                    r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                })
            }
        };
        let connect = match answer {
            ui::Answer::Waiting => return,
            ui::Answer::Cancel => {
                self.auth = None;
                return;
            }
            ui::Answer::Confirm => true,
            ui::Answer::Other => false,
        };
        match self.auth.take() {
            Some(Auth::Site {
                old,
                mut site,
                port,
                password,
                ..
            }) => {
                site.name = site.name.trim().to_string();
                site.host = site.host.trim().to_string();
                site.port = port.trim().parse().unwrap_or(22);
                let key = site.key();
                // A renamed site takes its password along.
                if let Some(old_name) = &old {
                    let old_key = format!("site:{old_name}");
                    if old_key != key {
                        if let Some(pw) = sites::password(&old_key) {
                            if password.is_empty() && site.remember {
                                sites::remember(&key, &pw);
                            }
                        }
                        sites::forget(&old_key);
                        self.passwords.remove(&old_key);
                    }
                    self.sites.retain(|s| &s.name != old_name);
                }
                if !password.is_empty() {
                    self.passwords.insert(key.clone(), password.clone());
                    if site.remember {
                        sites::remember(&key, &password);
                    } else {
                        sites::forget(&key);
                    }
                } else if !site.remember {
                    sites::forget(&key);
                }
                site.remember = site.remember && sites::password(&key).is_some();
                self.sites.push(site);
                self.sites.sort_by_key(|s| s.name.to_lowercase());
                sites::save(&self.sites);
                if connect {
                    self.set_place(ctx, self.focus, Place::Server(key), None);
                }
            }
            Some(Auth::Password {
                pane,
                key,
                password,
                remember,
                ..
            }) => {
                self.passwords.insert(key.clone(), password.clone());
                if remember {
                    sites::remember(&key, &password);
                } else {
                    sites::forget(&key);
                }
                if let Some(site) = self.sites.iter_mut().find(|s| s.key() == key) {
                    site.remember = remember;
                }
                sites::save(&self.sites);
                self.set_place(ctx, pane, Place::Server(key), None);
            }
            None => {}
        }
    }

    fn save(&mut self) {
        let order = self.layout.panes();
        self.settings.panes = order
            .iter()
            .filter_map(|id| self.pane(*id))
            .map(|p| SavedPane {
                place: p.place.as_ref().map(|pl| match pl {
                    Place::Local => String::new(),
                    Place::Server(h) => h.clone(),
                }),
                path: p.path.clone(),
                view: p.view,
            })
            .collect();
        self.settings.transfer = *self.queue.options.lock().unwrap();
        self.settings.watches = self.watches.iter().map(|w| w.spec.clone()).collect();
        if let Some(path) = queue_path() {
            self.queue.save(&path);
        }
        if let (Some(path), Ok(json)) = (settings_path(), serde_json::to_vec_pretty(&self.settings))
        {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let _ = std::fs::write(path, json);
        }
    }
}

/// Whether a text box has the keyboard (a row being picked doesn't count).
fn typing(ctx: &Context) -> bool {
    ctx.memory(|m| m.focused())
        .is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
}

/// A name a file or folder can have, here and on a server.
fn valid_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
}

/// Panes side by side, each as wide as the others.
fn row(ids: &[u64]) -> split::Node {
    match ids {
        [] => split::Node::Pane(0),
        [one] => split::Node::Pane(*one),
        [first, rest @ ..] => split::Node::Split {
            across: true,
            ratio: 1.0 / ids.len() as f32,
            first: Box::new(split::Node::Pane(*first)),
            second: Box::new(row(rest)),
        },
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !std::mem::replace(&mut self.adopted, true) {
            os::adopt_window(frame);
            os::set_backdrop(frame, None, true);
        }
        self.now = ctx.input(|i| i.time);
        if self.now - self.drives_at > 5.0 {
            self.drives = winshell::drives();
            self.drives_at = self.now;
        }
        self.poll();
        self.watch_tick();
        self.pending_renames();
        self.opened_tick();
        let open_palette = ctx.input(|i| {
            i.modifiers.command
                && (i.key_pressed(Key::K) || i.modifiers.shift && i.key_pressed(Key::P))
        });
        if open_palette && self.dialog.is_none() && self.auth.is_none() && self.compare.is_none() {
            self.palette = Some(palette::Palette::with(String::new()));
        }
        self.keys(&ctx);
        // Files dropped from Explorer go into the pane they land on.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            let at = ctx.input(|i| i.pointer.hover_pos());
            let target = at
                .and_then(|p| {
                    self.rects
                        .iter()
                        .find(|(_, r)| r.contains(p))
                        .map(|(id, _)| *id)
                })
                .unwrap_or(self.focus);
            self.upload_paths(dropped, target);
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            if let Some(p) = ctx.input(|i| i.pointer.hover_pos()) {
                if let Some((_, r)) = self.rects.iter().find(|(_, r)| r.contains(p)) {
                    ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("files"),
                    ))
                    .rect_stroke(
                        r.shrink(2.0),
                        10,
                        (2.0, theme::ACCENT()),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        if self
            .queue
            .jobs
            .lock()
            .unwrap()
            .iter()
            .any(|j| j.state == State::Running)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.title_bar(ui);
        self.bottom_bar(ui);
        self.sidebar(ui);
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(theme::WINDOW_BG()).inner_margin(Margin {
                left: 0,
                right: 8,
                top: 0,
                bottom: 4,
            }))
            .show(ui, |ui| self.panes_ui(ui));
        self.jobs_popup(&ctx);
        self.preview_tick(&ctx);
        self.props_dialog(&ctx);
        self.dialogs(&ctx);
        self.auth_dialog(&ctx);
        self.compare_ui(&ctx);
        self.palette_ui(&ctx);
        if self.drag.is_none() {
            resize_edges(&ctx);
        }
    }

    fn on_exit(&mut self) {
        self.save();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::WINDOW_BG().to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_times_read_as_people_write_them() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(5 * 1024 * 1024 * 1024), "5.0 GB");
        assert_eq!(time_text(Some(0), 0), "1970-01-01 00:00");
        assert_eq!(time_text(Some(1_700_000_000), 0), "2023-11-14 22:13");
        assert_eq!(time_text(Some(1_700_000_000), 7200), "2023-11-15 00:13");
    }
}
