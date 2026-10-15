#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! rdrop: files between this computer and your servers, over SFTP. As many panes as you
//! like, each on this computer or on any server, laid out by dragging; rows dragged from one
//! pane to another are copied, server to server included; and the transfers run in the
//! background, shown in the bar at the bottom.

// The look is ride's; the window parts, shell profiles (for the SSH hosts) and the split
// layout are rterm's, shared as files.
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

mod remote;
mod sftp;
mod transfer;

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Context, CornerRadius, Event, FontId, Frame, Key,
    Layout, Margin, Modifiers, Panel, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
    ViewportBuilder,
};
use remote::{Answer, Ask, Remote};
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
    /// Text typed to narrow the list.
    filter: String,
    conn: Option<Remote>,
    connected: bool,
    /// Folders to go back to.
    back: Vec<String>,
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
            filter: String::new(),
            conn: None,
            connected: false,
            back: Vec::new(),
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
        if path != self.path {
            self.filter.clear();
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

    fn visible(&self) -> Vec<Item> {
        let f = self.filter.to_lowercase();
        self.items
            .iter()
            .filter(|i| f.is_empty() || i.name.to_lowercase().contains(&f))
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
    if path.is_empty() {
        // The drives, for going up from the top of one.
        return Ok(('A'..='Z')
            .map(|d| format!("{d}:\\"))
            .filter(|d| Path::new(d).exists())
            .map(|d| Item {
                name: d,
                is_dir: true,
                size: 0,
                mtime: None,
            })
            .collect());
    }
    let mut items = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        items.push(Item {
            name: entry.file_name().to_string_lossy().into_owned(),
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
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct Settings {
    panes: Vec<SavedPane>,
    /// The last folder on each server, by host.
    remote: HashMap<String, String>,
}

fn settings_path() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("APPDATA")?)
            .join("rdrop")
            .join("settings.json"),
    )
}

fn load_settings() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn home() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into())
}

// ---- the app ----

enum Dialog {
    NewFolder(u64, String),
    Rename(u64, String, String),
    Delete(u64, Vec<String>),
}

enum Drag {
    /// Rows dragged out of a pane, to copy into another.
    Rows { from: u64, names: Vec<String> },
    /// A pane dragged by its header, to put somewhere else.
    Pane(u64),
    /// A server (or this computer) dragged from the list on the left.
    Place(Place),
}

enum PaneAct {
    Open(String),
    Back,
    Pick(Place),
    Split(bool),
    Close,
    NewFolder,
    Rename(String),
    Delete(Vec<String>),
    CopyTo(Vec<String>),
    CopyPaths(Vec<String>),
    Reveal(String),
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
    offset: i64,
    adopted: bool,
    next_id: u64,
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
            status: String::new(),
            settings,
            offset: local_offset(),
            adopted: false,
            next_id: 0,
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
                },
                SavedPane {
                    place: host,
                    path: String::new(),
                },
            ];
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
            ids.push(app.add_pane(ctx, place, s.path.clone()));
        }
        app.layout = row(&ids);
        app.focus = ids.first().copied().unwrap_or(0);
        app.previous = app.panes.get(1).map_or(app.focus, |p| p.id);
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
    fn set_place(&mut self, ctx: &Context, id: u64, place: Place, path: Option<String>) {
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
        if pane.is_local() {
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
                    Answer::Lost(e) => lost = Some(e),
                }
            }
            if let Some(e) = lost {
                pane.connected = false;
                pane.loading = false;
                pane.error = Some(e);
                pane.conn = None;
            }
        }
        for (id, path) in lists {
            self.open(id, path);
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

    fn delete(&mut self, id: u64, names: Vec<String>) {
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

    fn keys(&mut self, ctx: &Context) {
        if self.dialog.is_some() || ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let id = self.focus;
        let Some(pane) = self.pane(id) else { return };
        let names = pane.selected_names();
        let up = pane.up();
        let pressed = |k: Key, m: Modifiers| ctx.input_mut(|i| i.consume_key(m, k));
        if pressed(Key::F5, Modifiers::NONE) && !names.is_empty() {
            if let Some(to) = self.other_pane(id) {
                self.copy(id, &names, to, None);
            }
        }
        if pressed(Key::F7, Modifiers::NONE) {
            self.dialog = Some(Dialog::NewFolder(id, String::new()));
            self.focus_dialog = true;
        }
        if pressed(Key::F2, Modifiers::NONE) {
            if let Some(name) = names.first() {
                self.dialog = Some(Dialog::Rename(id, name.clone(), name.clone()));
                self.focus_dialog = true;
            }
        }
        if pressed(Key::Delete, Modifiers::NONE) && !names.is_empty() {
            self.dialog = Some(Dialog::Delete(id, names.clone()));
        }
        if pressed(Key::Backspace, Modifiers::NONE) || pressed(Key::ArrowUp, Modifiers::ALT) {
            if let Some(up) = up {
                self.go(id, up);
            }
        }
        if pressed(Key::ArrowLeft, Modifiers::ALT) {
            if let Some(back) = self.pane_mut(id).and_then(|p| p.back.pop()) {
                self.open(id, back);
            }
        }
        if pressed(Key::Tab, Modifiers::NONE) {
            let ids = self.layout.panes();
            if let Some(at) = ids.iter().position(|p| *p == id) {
                self.set_focus(ids[(at + 1) % ids.len()]);
            }
        }
        if pressed(Key::A, Modifiers::COMMAND) {
            if let Some(p) = self.pane_mut(id) {
                p.selected = p.items.iter().map(|i| i.name.clone()).collect();
            }
        }
        if pressed(Key::R, Modifiers::COMMAND) {
            self.refresh(id);
        }
        if pressed(Key::Enter, Modifiers::NONE) {
            if let [name] = names.as_slice() {
                let pane = self.pane(id).unwrap();
                if pane.items.iter().any(|i| &i.name == name && i.is_dir) {
                    let path = pane.child(name);
                    self.go(id, path);
                }
            }
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
    fn place_row(&mut self, ui: &mut Ui, ctx: &Context, place: Place) {
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
            Place::Server(host) => (icons::REMOTE, host.as_str()),
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
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        Panel::left("places")
            .exact_size(214.0)
            .resizable(false)
            .frame(Frame::NONE.fill(theme::WINDOW_BG()).inner_margin(Margin { left: 12, right: 10, top: 6, bottom: 10 }))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.label(RichText::new("PLACES").size(11.5).strong().color(theme::COMMENT()));
                ui.add_space(4.0);
                self.place_row(ui, &ctx, Place::Local);
                ui.add_space(10.0);
                ui.label(RichText::new("SERVERS").size(11.5).strong().color(theme::COMMENT()));
                ui.add_space(4.0);
                if self.hosts.is_empty() {
                    let text = if self.hosts_rx.is_some() { "Reading ~/.ssh/config…" } else { "No hosts in ~/.ssh/config" };
                    ui.label(RichText::new(text).size(12.0).color(theme::COMMENT()));
                }
                for host in self.hosts.clone() {
                    self.place_row(ui, &ctx, Place::Server(host));
                }
                ui.add_space(10.0);
                let edit = ui.add(TextEdit::singleline(&mut self.host_input).hint_text("Other: user@host").desired_width(f32::INFINITY));
                if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.host_input.trim().is_empty() {
                    let host = self.host_input.trim().to_string();
                    self.set_place(&ctx, self.focus, Place::Server(host), None);
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.label(
                        RichText::new("Drag rows onto another pane to copy · F5 copies to the pane used before · F2 rename · F7 new folder · Del delete · Tab next pane · Backspace up")
                            .size(11.0)
                            .color(theme::COMMENT()),
                    );
                });
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
                        ui.label(RichText::new("Ready").size(12.0).color(theme::COMMENT()));
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
        let width = 520.0;
        let mut close = false;
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
                        });
                    });
                    ui.add_space(4.0);
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
                                State::Done => format!("Done  ·  {}  ·  {}/s  ·  into {}", size_text(job.total), size_text(job.speed() as u64), job.to_dir),
                                State::Failed(e) => format!("Failed: {e}"),
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
            PaneAct::NewFolder => {
                self.dialog = Some(Dialog::NewFolder(id, String::new()));
                self.focus_dialog = true;
            }
            PaneAct::Rename(name) => {
                self.dialog = Some(Dialog::Rename(id, name.clone(), name));
                self.focus_dialog = true;
            }
            PaneAct::Delete(names) => self.dialog = Some(Dialog::Delete(id, names)),
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

    fn pane_ui(&mut self, ui: &mut Ui, id: u64, rect: Rect, acts: &mut Vec<(u64, PaneAct)>) {
        let focused = self.focus == id;
        let several = self.panes.len() > 1;
        let hosts = self.hosts.clone();
        let offset = self.offset;
        let dragging_rows = matches!(self.drag, Some(Drag::Rows { .. }));
        if ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_pressed()) {
            self.set_focus(id);
        }
        let Some(pane) = self.pane_mut(id) else {
            return;
        };
        let border = if focused {
            theme::ACCENT().gamma_multiply(0.6)
        } else {
            theme::LINE()
        };
        ui.painter().rect(
            rect,
            10,
            theme::PANEL_BG(),
            Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let mut act = |a: PaneAct| acts.push((id, a));

        // The header: what this pane shows, a handle to move it by, and what it can do.
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H));
        let handle = ui.interact(header, ui.id().with("handle"), Sense::click_and_drag());
        if handle.drag_started() {
            act(PaneAct::StartPane);
        }
        if handle.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        let mut head = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(header.shrink2(vec2(10.0, 4.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let (icon, title) = match &pane.place {
            Some(Place::Local) => (icons::FOLDER_ROOT, "This computer".to_string()),
            Some(Place::Server(host)) => (icons::REMOTE, host.clone()),
            None => (icons::ADD, "Pick a place".to_string()),
        };
        let dot = match &pane.place {
            Some(Place::Server(_)) if pane.connected => GOOD,
            Some(Place::Server(_)) => theme::COMMENT(),
            _ => theme::COMMENT(),
        };
        head.label(RichText::new(icon).color(dot));
        // Picking where the pane looks.
        let title: String = if title.chars().count() > 22 {
            title.chars().take(20).chain(['…']).collect()
        } else {
            title
        };
        head.menu_button(
            RichText::new(format!("{title}  {}", icons::CHEVRON_DOWN))
                .size(13.0)
                .strong(),
            |ui| {
                if ui
                    .button(format!("{}  This computer", icons::FOLDER_ROOT))
                    .clicked()
                {
                    act(PaneAct::Pick(Place::Local));
                }
                ui.separator();
                for host in &hosts {
                    if ui.button(format!("{}  {host}", icons::REMOTE)).clicked() {
                        act(PaneAct::Pick(Place::Server(host.clone())));
                    }
                }
            },
        );
        head.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if several
                && ui
                    .small_button(icons::CLOSE.to_string())
                    .on_hover_text("Close this pane")
                    .clicked()
            {
                act(PaneAct::Close);
            }
            let usable = pane.ready();
            // A narrow pane keeps its tools in a menu, so its name stays readable.
            if rect.width() < 480.0 {
                ui.menu_button("⋯", |ui| {
                    ui.add_enabled_ui(usable, |ui| {
                        let back = egui::Button::new("Back").shortcut_text("Alt+Left");
                        if ui.add_enabled(!pane.back.is_empty(), back).clicked() {
                            act(PaneAct::Back);
                        }
                        if ui
                            .add(egui::Button::new("Up").shortcut_text("Backspace"))
                            .clicked()
                        {
                            if let Some(up) = pane.up() {
                                act(PaneAct::Open(up));
                            }
                        }
                        if ui
                            .add(egui::Button::new("Refresh").shortcut_text("Ctrl+R"))
                            .clicked()
                        {
                            act(PaneAct::Open(pane.path.clone()));
                        }
                        if ui
                            .add(egui::Button::new("New folder…").shortcut_text("F7"))
                            .clicked()
                        {
                            act(PaneAct::NewFolder);
                        }
                    });
                    ui.separator();
                    if ui.button("Split right").clicked() {
                        act(PaneAct::Split(true));
                    }
                    if ui.button("Split down").clicked() {
                        act(PaneAct::Split(false));
                    }
                });
                return;
            }
            if ui.small_button("⬓").on_hover_text("Split down").clicked() {
                act(PaneAct::Split(false));
            }
            if ui.small_button("◫").on_hover_text("Split right").clicked() {
                act(PaneAct::Split(true));
            }
            ui.add_space(6.0);
            ui.add_enabled_ui(usable, |ui| {
                if ui
                    .small_button("+ Folder")
                    .on_hover_text("New folder (F7)")
                    .clicked()
                {
                    act(PaneAct::NewFolder);
                }
                if ui
                    .small_button("⟳")
                    .on_hover_text("Refresh (Ctrl+R)")
                    .clicked()
                {
                    act(PaneAct::Open(pane.path.clone()));
                }
                if ui
                    .small_button("↑")
                    .on_hover_text("Up (Backspace)")
                    .clicked()
                {
                    if let Some(up) = pane.up() {
                        act(PaneAct::Open(up));
                    }
                }
                if ui
                    .add_enabled(!pane.back.is_empty(), egui::Button::new("←").small())
                    .on_hover_text("Back (Alt+Left)")
                    .clicked()
                {
                    act(PaneAct::Back);
                }
            });
        });

        let body = Rect::from_min_max(rect.min + vec2(8.0, HEADER_H), rect.max - vec2(8.0, 8.0));
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(body)
                .layout(Layout::top_down(Align::Min)),
        );
        let ui = &mut ui;

        if !pane.ready() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                match &pane.place {
                    Some(Place::Server(host)) => {
                        if let Some(e) = &pane.error {
                            ui.label(RichText::new(e).size(12.5).color(BAD));
                            ui.add_space(8.0);
                            if ui.button("Try again").clicked() {
                                act(PaneAct::Pick(Place::Server(host.clone())));
                            }
                        } else {
                            ui.spinner();
                            ui.label(RichText::new(format!("Connecting to {host}…")).size(13.0).color(theme::COMMENT()));
                        }
                    }
                    _ => {
                        ui.label(RichText::new("Open a server here: pick one above, click one on the left, or drag one onto this pane.").size(13.0).color(theme::COMMENT()));
                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui.button(format!("{}  This computer", icons::FOLDER_ROOT)).clicked() {
                                act(PaneAct::Pick(Place::Local));
                            }
                            for host in hosts.iter().take(8) {
                                if ui.button(format!("{}  {host}", icons::REMOTE)).clicked() {
                                    act(PaneAct::Pick(Place::Server(host.clone())));
                                }
                            }
                        });
                    }
                }
            });
            return;
        }

        // Where it is: a path to type into.
        let edit = ui.add(
            TextEdit::singleline(&mut pane.path_edit)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );
        if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            act(PaneAct::Open(pane.path_edit.clone()));
        }
        ui.add_space(4.0);

        // Column headings, which sort.
        // Size and time make way for the name in a narrow pane.
        let full = ui.available_width();
        let size_w = if full > 330.0 { 84.0 } else { 0.0 };
        let time_w = if full > 480.0 { 140.0 } else { 0.0 };
        let widths = [full - size_w - time_w, size_w, time_w];
        let mut resort = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (i, (col, title)) in [
                (Col::Name, "Name"),
                (Col::Size, "Size"),
                (Col::Modified, "Modified"),
            ]
            .into_iter()
            .enumerate()
            {
                if widths[i] <= 0.0 {
                    continue;
                }
                let (r, resp) = ui.allocate_exact_size(vec2(widths[i], 20.0), Sense::click());
                let arrow = if pane.sort.0 == col {
                    if pane.sort.1 {
                        " ▲"
                    } else {
                        " ▼"
                    }
                } else {
                    ""
                };
                let (align, at) = if i == 0 {
                    (Align2::LEFT_CENTER, r.left_center() + vec2(28.0, 0.0))
                } else {
                    (Align2::RIGHT_CENTER, r.right_center() - vec2(10.0, 0.0))
                };
                ui.painter().text(
                    at,
                    align,
                    format!("{title}{arrow}"),
                    FontId::proportional(11.5),
                    theme::COMMENT(),
                );
                if resp.clicked() {
                    resort = Some(col);
                }
            }
        });
        if let Some(col) = resort {
            pane.sort = (
                col,
                if pane.sort.0 == col {
                    !pane.sort.1
                } else {
                    true
                },
            );
            pane.sort_items();
        }
        ui.painter()
            .hline(body.x_range(), ui.cursor().top(), (1.0, theme::LINE()));
        if let Some(e) = &pane.error {
            ui.label(RichText::new(e).size(12.5).color(BAD));
        }
        if pane.loading && pane.items.is_empty() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| ui.spinner());
        }

        let items = pane.visible();
        let mut clicked: Option<(usize, Modifiers)> = None;
        let mut opened: Option<usize> = None;
        let mut drag_from: Option<usize> = None;
        let mut menu: Option<(usize, u8)> = None;
        let mut empty_menu = false;
        let list = ScrollArea::vertical()
            .id_salt("rows")
            .auto_shrink(false)
            .show_rows(ui, ROW_H, items.len(), |ui, range| {
                for index in range {
                    let item = &items[index];
                    let selected = pane.selected.contains(&item.name);
                    let (r, resp) = ui.allocate_exact_size(
                        vec2(ui.available_width(), ROW_H),
                        Sense::click_and_drag(),
                    );
                    let fill = if selected {
                        if focused {
                            theme::ACCENT().gamma_multiply(0.22)
                        } else {
                            theme::tint(14)
                        }
                    } else if resp.hovered() && !dragging_rows {
                        theme::tint(7)
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(r, 6.0, fill);
                    let (glyph, color) = icons::for_entry(&item.name, item.is_dir, false);
                    let painter = ui.painter_at(r);
                    painter.text(
                        r.left_center() + vec2(8.0, 0.0),
                        Align2::LEFT_CENTER,
                        glyph,
                        FontId::proportional(14.0),
                        color,
                    );
                    let name_clip =
                        Rect::from_min_max(r.min, pos2(r.left() + widths[0] - 6.0, r.max.y))
                            .intersect(ui.clip_rect());
                    ui.painter().with_clip_rect(name_clip).text(
                        r.left_center() + vec2(28.0, 0.0),
                        Align2::LEFT_CENTER,
                        &item.name,
                        FontId::proportional(13.0),
                        theme::TEXT(),
                    );
                    if !item.is_dir && widths[1] > 0.0 {
                        painter.text(
                            pos2(r.left() + widths[0] + widths[1] - 10.0, r.center().y),
                            Align2::RIGHT_CENTER,
                            size_text(item.size),
                            FontId::proportional(12.0),
                            theme::MUTED(),
                        );
                    }
                    if widths[2] > 0.0 {
                        painter.text(
                            pos2(r.right() - 10.0, r.center().y),
                            Align2::RIGHT_CENTER,
                            time_text(item.mtime, offset),
                            FontId::proportional(12.0),
                            theme::MUTED(),
                        );
                    }
                    if resp.clicked() {
                        clicked = Some((index, ui.input(|i| i.modifiers)));
                    }
                    if resp.double_clicked() {
                        opened = Some(index);
                    }
                    if resp.drag_started() {
                        drag_from = Some(index);
                    }
                    resp.context_menu(|ui| {
                        if ui
                            .add(egui::Button::new("Copy to the other pane").shortcut_text("F5"))
                            .clicked()
                        {
                            menu = Some((index, 0));
                        }
                        ui.separator();
                        if ui
                            .add(egui::Button::new("Rename…").shortcut_text("F2"))
                            .clicked()
                        {
                            menu = Some((index, 1));
                        }
                        if ui
                            .add(egui::Button::new("Delete…").shortcut_text("Del"))
                            .clicked()
                        {
                            menu = Some((index, 2));
                        }
                        if ui.button("Copy path").clicked() {
                            menu = Some((index, 3));
                        }
                        if pane.place == Some(Place::Local)
                            && ui.button("Show in Explorer").clicked()
                        {
                            menu = Some((index, 4));
                        }
                    });
                }
            });
        // A right click on the empty part of the list.
        let rest = Rect::from_min_max(
            pos2(
                body.left(),
                list.content_size.y.min(list.inner_rect.height()) + list.inner_rect.top(),
            ),
            list.inner_rect.max,
        );
        if rest.height() > 4.0 {
            let resp = ui.interact(rest, ui.id().with("rest"), Sense::click());
            if resp.clicked() {
                pane.selected.clear();
            }
            resp.context_menu(|ui| {
                if ui.button("New folder…").clicked() {
                    empty_menu = true;
                }
            });
        }
        if empty_menu {
            act(PaneAct::NewFolder);
        }

        // Selecting, as in Explorer: a click picks one, Ctrl adds, Shift takes a range.
        if let Some((index, mods)) = clicked {
            let names: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
            if mods.shift {
                let from = pane
                    .anchor
                    .unwrap_or(index)
                    .min(names.len().saturating_sub(1));
                let (a, b) = (from.min(index), from.max(index));
                pane.selected = names[a..=b].iter().cloned().collect();
            } else if mods.command {
                if !pane.selected.remove(&names[index]) {
                    pane.selected.insert(names[index].clone());
                }
                pane.anchor = Some(index);
            } else {
                pane.selected = [names[index].clone()].into();
                pane.anchor = Some(index);
            }
        }
        if let Some(index) = opened {
            let item = &items[index];
            let path = pane.child(&item.name);
            if item.is_dir {
                act(PaneAct::Open(path));
            } else if pane.place == Some(Place::Local) {
                let _ = std::process::Command::new("explorer").arg(path).spawn();
            } else {
                act(PaneAct::CopyTo(vec![item.name.clone()]));
            }
        }
        if let Some(index) = drag_from.filter(|_| self_drag_free(dragging_rows)) {
            let name = items[index].name.clone();
            if !pane.selected.contains(&name) {
                pane.selected = [name].into();
            }
            act(PaneAct::StartRows(pane.selected_names()));
        }
        if let Some((index, what)) = menu {
            let name = items[index].name.clone();
            if !pane.selected.contains(&name) {
                pane.selected = [name.clone()].into();
            }
            let names = pane.selected_names();
            act(match what {
                0 => PaneAct::CopyTo(names),
                1 => PaneAct::Rename(name),
                2 => PaneAct::Delete(names),
                3 => PaneAct::CopyPaths(names),
                _ => PaneAct::Reveal(pane.child(&name)),
            });
        }

        // Typing narrows the list to names with what was typed; Esc clears it.
        if focused && ui.memory(|m| m.focused().is_none()) {
            let typed: String = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        Event::Text(t) => Some(t.clone()),
                        _ => None,
                    })
                    .collect()
            });
            pane.filter.push_str(&typed);
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                pane.filter.clear();
                pane.selected.clear();
            }
        }
        if !pane.filter.is_empty() {
            let at = rect.right_bottom() - vec2(14.0, 14.0);
            let galley = ui.painter().layout_no_wrap(
                format!("{}  {}", icons::SEARCH, pane.filter),
                FontId::proportional(12.5),
                theme::TEXT(),
            );
            let r = Rect::from_min_size(
                at - galley.size() - vec2(16.0, 8.0),
                galley.size() + vec2(16.0, 8.0),
            );
            ui.painter().rect_filled(r, 8.0, theme::tint(24));
            ui.painter()
                .galley(r.min + vec2(8.0, 4.0), galley, theme::TEXT());
        }
    }

    fn dialogs(&mut self, ctx: &Context) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut close = false;
        let mut confirm = false;
        let title = match dialog {
            Dialog::NewFolder(..) => "New folder".to_string(),
            Dialog::Rename(..) => "Rename".to_string(),
            Dialog::Delete(_, names) => match names.as_slice() {
                [one] => format!("Delete {one}?"),
                many => format!("Delete {} items?", many.len()),
            },
        };
        let focus = std::mem::take(&mut self.focus_dialog);
        let place = |id: u64| {
            self.panes
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.place.as_ref())
                .map_or("", Place::label)
                .to_string()
        };
        let delete_from = match dialog {
            Dialog::Delete(id, _) => place(*id),
            _ => String::new(),
        };
        egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(RichText::new(&title).size(16.0).strong());
            ui.add_space(8.0);
            match dialog {
                Dialog::NewFolder(_, name) | Dialog::Rename(_, _, name) => {
                    let edit = ui.add(TextEdit::singleline(name).desired_width(f32::INFINITY));
                    if focus {
                        edit.request_focus();
                    }
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        confirm = true;
                    }
                }
                Dialog::Delete(_, names) => {
                    let shown: Vec<&str> = names.iter().take(8).map(String::as_str).collect();
                    ui.label(RichText::new(shown.join("\n")).size(12.5).color(theme::COMMENT()));
                    if names.len() > 8 {
                        ui.label(RichText::new(format!("and {} more", names.len() - 8)).size(12.5).color(theme::COMMENT()));
                    }
                    ui.label(RichText::new(format!("They're removed from {delete_from}, folders with all they hold. This can't be undone.")).size(12.5));
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        confirm = true;
                    }
                }
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if matches!(dialog, Dialog::Delete(..)) { "Delete" } else { "OK" };
                if ui.button(label).clicked() {
                    confirm = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
        });
        if confirm {
            match self.dialog.take() {
                Some(Dialog::NewFolder(id, name)) if !name.trim().is_empty() => {
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
                Some(Dialog::Rename(id, old, new)) if !new.trim().is_empty() && new != old => {
                    let Some(pane) = self.pane(id) else { return };
                    let (from, to) = (pane.child(&old), pane.child(new.trim()));
                    if pane.is_local() {
                        if let Err(e) = std::fs::rename(&from, &to) {
                            self.status = e.to_string();
                        }
                        self.refresh(id);
                    } else if let Some(c) = &pane.conn {
                        c.ask(Ask::Rename(from, to));
                    }
                }
                Some(Dialog::Delete(id, names)) => self.delete(id, names),
                _ => {}
            }
        } else if close {
            self.dialog = None;
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
            })
            .collect();
        if let (Some(path), Ok(json)) = (settings_path(), serde_json::to_vec_pretty(&self.settings))
        {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let _ = std::fs::write(path, json);
        }
    }
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

/// A new drag of rows starts only when none is under way.
fn self_drag_free(dragging: bool) -> bool {
    !dragging
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !std::mem::replace(&mut self.adopted, true) {
            os::adopt_window(frame);
            os::set_backdrop(frame, None, true);
        }
        self.poll();
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
        self.dialogs(&ctx);
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
