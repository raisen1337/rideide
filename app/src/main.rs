#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod buffer;
mod chat;
mod conn;
mod diff;
mod editor;
mod extras;
mod highlight;
mod icons;
mod iconsets;
mod nav;
mod outline;
mod palette;
mod panes;
mod preview;
mod sessions;
mod settings;
mod splash;
mod term;
mod theme;
mod themes;
mod tools;
mod update;

use buffer::{Buffer, Pos};
use chat::Chat;
use conn::{Conn, ConnEvent};
use editor::EditorState;
use eframe::egui::{
    self, text::LayoutJob, vec2, Align, Align2, CentralPanel, Color32, Context, FontId, Frame, Key,
    Layout, Margin, Modifiers, Panel, Pos2, RichText, ScrollArea, Sense, TextEdit, TextFormat,
    TextWrapMode, Ui, ViewportBuilder, ViewportId,
};
use ride_proto::{DirEntry, Request, Response, SearchHit, ServerMsg};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use term::Terminal;

fn main() -> eframe::Result {
    // A DirectComposition swapchain is what lets the window be see-through on Windows,
    // which the optional glass backdrop needs.
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .presentation_system = eframe::wgpu::Dx12SwapchainKind::DxgiFromVisual;
        // By default the GPU allocator reserves large blocks up front for speed. A text
        // UI needs little, so it is told to keep memory low instead.
        setup.device_descriptor = Arc::new(|_adapter| eframe::wgpu::DeviceDescriptor {
            label: Some("ride"),
            required_limits: eframe::wgpu::Limits {
                max_texture_dimension_2d: 8192,
                ..Default::default()
            },
            memory_hints: eframe::wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        });
    }
    let options = eframe::NativeOptions {
        // The window opens as the small start screen in the middle of the display, and
        // takes its remembered size and place when that is over (see `App::end_splash`).
        centered: true,
        persist_window: false,
        viewport: ViewportBuilder::default()
            .with_inner_size([splash::SIZE, splash::SIZE])
            .with_title("ride")
            .with_icon(egui::IconData {
                rgba: include_bytes!("../assets/icon-128.rgba").to_vec(),
                width: 128,
                height: 128,
            })
            // The system frame stays in the window's style and is hidden by `os::adopt_window`.
            .with_decorations(true)
            .with_transparent(true),
        wgpu_options,
        // Where eframe would keep it anyway, but found through APPDATA like the rest of
        // ride's data, so a copy started with another APPDATA has its own.
        persistence_path: std::env::var_os("APPDATA")
            .map(|dir| PathBuf::from(dir).join("ride").join("data").join("app.ron")),
        ..Default::default()
    };
    eframe::run_native("ride", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

/// The entry in the host list that opens folders on this computer instead of a server.
const LOCAL: &str = "This computer";

enum Link {
    Offline,
    Connecting {
        rx: Receiver<Result<Conn, String>>,
        status: Arc<Mutex<String>>,
    },
    Online(Conn),
}

/// What to do with the response to a one-shot request.
enum Pending {
    /// The listing of the folder that files copied from another host go into.
    Transfer {
        name: String,
        requests: Vec<Request>,
        dest: String,
    },
    Hello,
    /// The second file of a comparison; the first one's text is already in hand.
    Compare {
        left: String,
        left_text: String,
        right: String,
    },
    ListDir(String),
    Open {
        path: String,
        goto: Option<Pos>,
    },
    /// Re-read of an open file after Claude may have changed it.
    Reload {
        path: String,
    },
    Save {
        path: String,
        revision: u64,
    },
    /// A create/rename/delete; the directory is reloaded when it succeeds.
    FsOp {
        refresh: String,
    },
    /// A new empty file, opened once the server confirms it exists.
    Created {
        path: String,
        dir: String,
    },
    /// Files for an `@` mention, for the side chat or the agent with this uid.
    Mentions {
        query: String,
        agent: Option<u64>,
    },
    /// The current content of a file one of Claude's edits is being reverted in.
    Revert {
        change: chat::Change,
    },
    Reverted {
        change: chat::Change,
    },
    /// Files for the Ctrl+P finder.
    PaletteFiles {
        query: String,
    },
}

enum DirState {
    Loading,
    Loaded(Vec<DirEntry>),
    Failed(String),
}

struct Tab {
    /// Stable identity for the tab's own window when it is detached.
    uid: u64,
    path: String,
    buf: Buffer,
    view: EditorState,
    /// Set for images, audio and other files that aren't edited as text.
    preview: Option<preview::Preview>,
    /// The pane of the editor area it is in.
    pane: u32,
    /// Shown in its own OS window instead of the tab bar.
    detached: bool,
    /// Screen position for the window on the frame it is first shown.
    place_at: Option<Pos2>,
    float: Float,
    /// For autosave: the revision last seen, when it changed, and the revision last saved by it.
    edited: (u64, f64, u64),
}

struct Download {
    id: u64,
    name: String,
    bytes: u64,
    /// Feeds the worker thread that writes to disk; dropped to signal the end of the stream.
    tx: Option<Sender<Vec<u8>>>,
    result: Receiver<Result<String, String>>,
    outcome: Option<Result<String, String>>,
}

#[derive(Default)]
struct Search {
    query: String,
    regex: bool,
    case_sensitive: bool,
    id: Option<u64>,
    hits: Vec<SearchHit>,
    /// Let Claude find it, instead of matching text.
    ai: bool,
    ai_busy: bool,
}

enum PromptAction {
    NewFile(String),
    NewDir(String),
    Rename(String),
    /// Save the current agents as a team template under the typed name.
    SaveTeam,
}

enum Dialog {
    Prompt {
        title: String,
        value: String,
        action: PromptAction,
    },
    ConfirmDelete {
        path: String,
    },
    ConfirmClose(usize),
    /// Text to read and copy, such as a generated commit message.
    Text {
        title: String,
        text: String,
    },
    Schedule {
        prompt: String,
        minutes: String,
    },
}

enum TreeAction {
    Toggle(String),
    Open(String),
    Download(String, bool),
    Prompt(&'static str, String, PromptAction),
    Delete(String),
    Refresh(String),
    AddToChat(String),
    Upload(String),
}

struct TreeRow {
    depth: usize,
    path: String,
    name: String,
    is_dir: bool,
    /// A placeholder line ("loading…", an error) rather than an entry.
    note: bool,
}

/// Which edge of the editor area the terminal sits on.
#[derive(Clone, Copy, PartialEq)]
enum TermDock {
    Bottom,
    Top,
    Left,
    Right,
}

impl TermDock {
    const ALL: [(TermDock, &'static str); 4] = [
        (TermDock::Bottom, "bottom"),
        (TermDock::Top, "top"),
        (TermDock::Left, "left"),
        (TermDock::Right, "right"),
    ];

    fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(dock, _)| *dock == self)
            .map_or("bottom", |(_, name)| name)
    }

    fn from_name(name: &str) -> Self {
        Self::ALL
            .iter()
            .find(|(_, n)| *n == name)
            .map_or(TermDock::Bottom, |(dock, _)| *dock)
    }

    /// The part of `area` a terminal dragged over it would dock to, if the pointer is near an edge.
    fn zone_at(area: egui::Rect, pointer: Pos2) -> Option<(TermDock, egui::Rect)> {
        let (w, h) = (area.width() * 0.3, area.height() * 0.3);
        let zones = [
            (
                TermDock::Left,
                egui::Rect::from_min_size(area.min, vec2(w, area.height())),
            ),
            (
                TermDock::Right,
                egui::Rect::from_min_size(area.right_top() - vec2(w, 0.0), vec2(w, area.height())),
            ),
            (
                TermDock::Top,
                egui::Rect::from_min_size(area.min, vec2(area.width(), h)),
            ),
            (
                TermDock::Bottom,
                egui::Rect::from_min_size(area.left_bottom() - vec2(0.0, h), vec2(area.width(), h)),
            ),
        ];
        // Corners belong to two zones; the edge the pointer is closest to wins.
        zones
            .into_iter()
            .filter(|(_, zone)| zone.contains(pointer))
            .min_by(|(a, _), (b, _)| {
                let distance = |dock: &TermDock| match dock {
                    TermDock::Left => pointer.x - area.left(),
                    TermDock::Right => area.right() - pointer.x,
                    TermDock::Top => pointer.y - area.top(),
                    TermDock::Bottom => area.bottom() - pointer.y,
                };
                distance(a).total_cmp(&distance(b))
            })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    /// Files, editor, terminal and the side chat.
    Code,
    /// A team of Claude agents: the list on the left, the selected one's chat in the middle.
    Agents,
    /// Installed themes and the VS Code Marketplace, using the whole window.
    Themes,
    Settings,
}

/// One Claude Code session in the Agents view.
struct Agent {
    uid: u64,
    name: String,
    chat: Chat,
    /// The settings its process was started with.
    started_with: Option<String>,
    /// Messages that arrived while it was busy.
    inbox: std::collections::VecDeque<Mail>,
    /// Who gets this agent's answer when its current turn ends.
    reply_to: Option<u64>,
    /// The agent that created this one, for the team graph.
    parent: Option<u64>,
    /// Messages it has sent to teammates.
    sent: u32,
    /// What it is for, when it came from a team template.
    role: String,
}

struct Mail {
    from: u64,
    from_name: String,
    /// A request is answered automatically; an answer isn't, or two agents would talk forever.
    request: bool,
    text: String,
}

/// A `@@spawn` or `@@send` line an agent ended its turn with.
struct Directive {
    spawn: bool,
    name: String,
    model: String,
    text: String,
}

/// How many messages agents may exchange per user request before the team is paused.

fn team_prompt(name: &str) -> String {
    format!(
        "You are \"{name}\", one agent in a team working on the project in the current directory. \
Each of your turns starts with a [team: ...] line listing your teammates and their models.\n\
You can create teammates and talk to them. Put directives at the very end of your reply, each starting on its own line:\n\
@@spawn name=<short-name> model=<haiku|sonnet|opus> :: <complete, self-contained task>\n\
@@send <name> :: <message>\n\
A directive's text runs until the next directive or the end of your reply. A teammate knows nothing about your conversation, so give full context and exact file paths.\n\
Delegate when the work can be split up or done in parallel, and do small things yourself. Pick the cheapest model that can do the job well: haiku for lookups, summaries and mechanical edits; sonnet for ordinary coding; opus for hard design and debugging.\n\
After issuing directives, end your turn. Do not wait or poll: answers arrive later as messages starting with [from <name>]. When a message arrives from a teammate, your reply goes back to them automatically, so just answer.\n\
When the whole job is done, reply to the user with a short summary and no directives."
    )
}

fn agent_name(raw: &str) -> String {
    let name: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();
    if name.is_empty() {
        "agent".into()
    } else {
        name
    }
}

fn model_alias(raw: &str) -> &'static str {
    match raw.trim().to_lowercase().as_str() {
        "haiku" => "haiku",
        "sonnet" => "sonnet",
        "opus" => "opus",
        "fable" => "fable",
        _ => "",
    }
}

/// Splits an agent's reply into its prose and the directives it ends with.
fn parse_directives(text: &str) -> (String, Vec<Directive>) {
    let mut body = String::new();
    let mut directives: Vec<Directive> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let parsed = if let Some(rest) = trimmed.strip_prefix("@@spawn ") {
            rest.split_once("::").map(|(head, task)| {
                let field = |key: &str| {
                    head.split_whitespace()
                        .find_map(|w| w.strip_prefix(key))
                        .unwrap_or("")
                        .to_string()
                };
                Directive {
                    spawn: true,
                    name: agent_name(&field("name=")),
                    model: field("model="),
                    text: task.trim().to_string(),
                }
            })
        } else if let Some(rest) = trimmed.strip_prefix("@@send ") {
            rest.split_once("::").map(|(name, message)| Directive {
                spawn: false,
                name: agent_name(name),
                model: String::new(),
                text: message.trim().to_string(),
            })
        } else {
            None
        };
        match (parsed, directives.last_mut()) {
            (Some(directive), _) => directives.push(directive),
            (None, Some(current)) => {
                current.text.push('\n');
                current.text.push_str(line);
            }
            (None, None) => {
                body.push_str(line);
                body.push('\n');
            }
        }
    }
    (body.trim().to_string(), directives)
}

fn hash_of(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn link_request(link: &mut Link, req: Request) -> u64 {
    match link {
        Link::Online(conn) => conn.request(req),
        _ => 0,
    }
}

/// Makes sure the chat has a `claude` process running with its current settings.
fn ensure_proc(link: &mut Link, cwd: &str, chat: &mut Chat, started_with: &mut Option<String>) {
    let config = chat.config();
    if let Some(proc) = chat.proc.filter(|_| started_with.as_ref() != Some(&config)) {
        link_request(link, Request::ProcClose { proc });
        chat.stopped();
    }
    if chat.proc.is_none() {
        let id = link_request(
            link,
            Request::ProcOpen {
                command: chat.command(),
                cwd: cwd.to_string(),
            },
        );
        chat.proc = Some(id);
        *started_with = Some(config);
    }
}

#[derive(PartialEq)]
enum Side {
    Files,
    Changes,
    Git,
    Server,
    Outline,
}

struct App {
    link: Link,
    host: String,
    hosts: Vec<String>,
    /// Folder to reopen on the first connect, remembered from the last session.
    saved: Option<(String, String)>,
    /// A file named on the command line, opened once its folder is.
    open_on_connect: Option<String>,
    root: String,
    root_input: String,
    pending: HashMap<u64, Pending>,
    tree: HashMap<String, DirState>,
    expanded: HashSet<String>,
    tabs: Vec<Tab>,
    active: usize,
    next_uid: u64,
    side: Side,
    show_side: bool,
    search: Search,
    /// The folder search is shown above the file tree.
    show_search: bool,
    focus_search: bool,
    find: Option<String>,
    focus_find: bool,
    terms: Vec<Terminal>,
    active_term: usize,
    show_term: bool,
    term_dock: TermDock,
    /// The editor area of the last frame: where a dragged terminal can be dropped.
    work_area: egui::Rect,
    term_focused: bool,
    chat: Chat,
    show_chat: bool,
    chat_detached: bool,
    chat_place_at: Option<Pos2>,
    chat_float: Float,
    /// The settings the running `claude` process was started with.
    chat_proc_config: Option<String>,
    downloads: Vec<Download>,
    /// Where the last download went; the next save dialog starts there.
    download_dir: Option<PathBuf>,
    dialog: Option<Dialog>,
    status: String,
    view: View,
    agents: Vec<Agent>,
    active_agent: usize,
    /// Messages the agents may still exchange before the user has to step in.
    team_budget: u32,
    ai: ai::Ai,
    /// Hash of the chat state last written to disk, and when to look again.
    saved_state: u64,
    next_save: f64,
    themes: themes::Themes,
    palette: palette::Palette,
    settings_section: usize,
    git: tools::Git,
    monitor: tools::Monitor,
    /// Ports of the host made reachable on this computer.
    forwards: Vec<tools::Forward>,
    panes: panes::Panes,
    updater: update::Updater,
    /// What the outline side tab is narrowed to.
    outline_filter: String,
    /// Changelog entries to show once after an update: (version, text).
    whats_new: Vec<(String, String)>,
    /// The other connections that are open, besides the one shown.
    sessions: Vec<sessions::Session>,
    /// Which connection the one shown is.
    session_uid: u64,
    /// A connection that isn't the one shown is being served.
    background: bool,
    transfers: Vec<sessions::Transfer>,
    batches: Vec<sessions::Batch>,
    /// The connection whose files are shown under the tree, and whether that is hidden.
    beside: Option<u64>,
    hide_beside: bool,
    drag_file: Option<sessions::DragFile>,
    /// Where a dragged file can be let go: the place on screen, the connection, the folder.
    drop_targets: Vec<(egui::Rect, u64, String)>,
    /// An upload in progress.
    upload: Option<extras::Upload>,
    /// A folder to list again a moment from now (app time, path).
    refresh_later: Option<(f64, String)>,
    /// Tabs of the last session still to be reopened, and the host they belong to.
    restore: HashMap<String, extras::SavedTab>,
    restore_order: Vec<String>,
    restore_active: Option<String>,
    restore_host: String,
    /// The day of the last catch-up screen, so there is one a day.
    catchup_day: u64,
    /// Whether ride is the window in front; notifications are for when it isn't.
    focused: bool,
    /// The user's own team templates.
    teams: Vec<extras::Team>,
    /// A window size asked for from outside, used instead of the remembered one.
    forced_size: Option<[f32; 2]>,
    /// When the start screen appeared; `None` once it is over.
    splash: Option<f64>,
    /// The window's size and place when not maximized, and whether it is maximized,
    /// remembered between runs.
    window: Option<([f32; 2], [f32; 2])>,
    maximized: bool,
    /// How much larger the window comes out than the size asked for. The system frame is
    /// hidden but still counted when a size is set, so this much is taken off again.
    frame_extra: egui::Vec2,
    icon_sets: iconsets::IconSets,
}

/// Where chats, agents and scheduled tasks are kept between runs.
fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("ride").join("state.json"))
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn parent(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        // The folder above "C:/Users" is the drive root "C:/", slash included.
        Some(i) if trimmed[..i].ends_with(':') => &trimmed[..=i],
        Some(0) => "/",
        Some(i) => &trimmed[..i],
        None if trimmed.ends_with(':') => path,
        None => "/",
    }
}

fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("/")
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

/// Lets a blocking reader (the tar unpacker) consume chunks as they arrive from the network.
struct ChunkReader {
    rx: Receiver<Vec<u8>>,
    chunk: Vec<u8>,
    pos: usize,
}

impl Read for ChunkReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos == self.chunk.len() {
            match self.rx.recv() {
                Ok(chunk) => (self.chunk, self.pos) = (chunk, 0),
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.chunk.len() - self.pos);
        out[..n].copy_from_slice(&self.chunk[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn save_file(mut reader: ChunkReader, dest: &Path) -> Result<String, String> {
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    std::io::copy(&mut reader, &mut file).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    Ok(format!("Saved {}", dest.display()))
}

fn unpack_folder(reader: ChunkReader, dest: &Path) -> Result<String, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    let (mut done, mut skipped) = (0u32, 0u32);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        // Symlinks and names Windows can't represent fail here; the rest still gets extracted.
        match entry.unpack_in(dest) {
            Ok(true) => done += 1,
            _ => skipped += 1,
        }
    }
    let note = if skipped > 0 {
        format!(", {skipped} skipped")
    } else {
        String::new()
    };
    Ok(format!(
        "Downloaded {done} items into {}{note}",
        dest.display()
    ))
}

#[cfg(windows)]
mod os {
    #[repr(C)]
    #[derive(Default)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetCursorPos(point: *mut Point) -> i32;
        fn GetActiveWindow() -> isize;
        fn GetWindowRect(window: isize, rect: *mut Rect) -> i32;
        fn GetAsyncKeyState(key: i32) -> i16;
    }

    /// The cursor in physical screen pixels. egui stops hearing about the pointer
    /// once it leaves a window, hence the OS query.
    pub fn cursor() -> Option<(f32, f32)> {
        let mut point = Point::default();
        // SAFETY: the call only writes to the valid struct passed in.
        let ok = unsafe { GetCursorPos(&mut point) != 0 };
        ok.then_some((point.x as f32, point.y as f32))
    }

    /// The cursor position, if it is outside this app's active window.
    pub fn cursor_outside_window() -> Option<(f32, f32)> {
        let (x, y) = cursor()?;
        let mut rect = Rect::default();
        // SAFETY: the calls take no pointers except the valid struct that is written to.
        let ok = unsafe {
            let window = GetActiveWindow();
            window != 0 && GetWindowRect(window, &mut rect) != 0
        };
        let inside = (rect.left..rect.right).contains(&(x as i32))
            && (rect.top..rect.bottom).contains(&(y as i32));
        (ok && !inside).then_some((x, y))
    }

    pub fn left_button_down() -> bool {
        const VK_LBUTTON: i32 = 1;
        // SAFETY: a plain query with no pointers involved.
        unsafe { GetAsyncKeyState(VK_LBUTTON) < 0 }
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *const u8, name: *const u16) -> isize;
        fn SetInformationJobObject(job: isize, class: i32, info: *const u8, len: u32) -> i32;
        fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
    }

    /// Makes a process this app started end when the app does, however the app ends.
    /// Without it, a program that doesn't watch its input would be left running.
    pub fn end_with_app(child: &std::process::Child) {
        use std::os::windows::io::AsRawHandle;
        use std::sync::OnceLock;
        const EXTENDED_LIMIT_INFORMATION: i32 = 9;
        const KILL_ON_JOB_CLOSE: u32 = 0x2000;
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION is 144 bytes on 64-bit Windows, with the
        // limit flags 16 bytes in; everything else stays zero.
        #[repr(C, align(8))]
        struct Limits([u8; 144]);
        static JOB: OnceLock<isize> = OnceLock::new();
        let job = *JOB.get_or_init(|| {
            let mut limits = Limits([0; 144]);
            limits.0[16..20].copy_from_slice(&KILL_ON_JOB_CLOSE.to_le_bytes());
            // SAFETY: the pointers are null where allowed, or to the buffer above with
            // its true length. The job's handle is kept until the process ends, which
            // is what closes the job.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job != 0 {
                    SetInformationJobObject(
                        job,
                        EXTENDED_LIMIT_INFORMATION,
                        limits.0.as_ptr(),
                        144,
                    );
                }
                job
            }
        });
        if job != 0 {
            // SAFETY: both handles are valid: the job's forever, the child's while borrowed.
            unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as isize) };
        }
    }

    /// Whether Ctrl and V are held down.
    pub fn paste_keys_down() -> bool {
        const VK_CONTROL: i32 = 0x11;
        const VK_V: i32 = 0x56;
        // SAFETY: plain queries with no pointers involved.
        unsafe { GetAsyncKeyState(VK_CONTROL) < 0 && GetAsyncKeyState(VK_V) < 0 }
    }

    #[repr(C)]
    struct Margins {
        left: i32,
        right: i32,
        top: i32,
        bottom: i32,
    }

    type SubclassProc = unsafe extern "system" fn(isize, u32, usize, isize, usize, usize) -> isize;

    #[link(name = "user32")]
    extern "system" {
        fn IsZoomed(window: isize) -> i32;
        fn GetSystemMetrics(index: i32) -> i32;
        fn SetWindowPos(
            window: isize,
            after: isize,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            flags: u32,
        ) -> i32;
    }

    #[link(name = "comctl32")]
    extern "system" {
        fn SetWindowSubclass(window: isize, proc: SubclassProc, id: usize, data: usize) -> i32;
        fn DefSubclassProc(window: isize, message: u32, wparam: usize, lparam: isize) -> isize;
    }

    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmExtendFrameIntoClientArea(window: isize, margins: *const Margins) -> i32;
        fn DwmSetWindowAttribute(
            window: isize,
            attribute: u32,
            value: *const i32,
            size: u32,
        ) -> i32;
    }

    fn hwnd(window: &impl raw_window_handle::HasWindowHandle) -> Option<isize> {
        match window.window_handle().ok()?.as_raw() {
            raw_window_handle::RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
            _ => None,
        }
    }

    /// Removes the system title bar and borders from view while the window keeps them in
    /// its style. Windows then still treats it as a normal framed window: rounded corners,
    /// a shadow, snapping, and (what this is for) a blurred backdrop, which it only draws
    /// for framed windows.
    unsafe extern "system" fn hide_frame(
        window: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        _id: usize,
        _data: usize,
    ) -> isize {
        const WM_NCCALCSIZE: u32 = 0x0083;
        const SM_CXSIZEFRAME: i32 = 32;
        const SM_CXPADDEDBORDER: i32 = 92;
        if message == WM_NCCALCSIZE && wparam != 0 {
            // Answering 0 makes the client area cover the whole window. A maximized window
            // hangs over the screen edge by its frame's thickness, so that much is taken off.
            if IsZoomed(window) != 0 {
                let frame = GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER);
                // SAFETY: for this message, lparam points at NCCALCSIZE_PARAMS, which starts with the window's RECT.
                let rect = &mut *(lparam as *mut [i32; 4]);
                rect[0] += frame;
                rect[1] += frame;
                rect[2] -= frame;
                rect[3] -= frame;
            }
            return 0;
        }
        DefSubclassProc(window, message, wparam, lparam)
    }

    /// Takes over the window's frame (see `hide_frame`). Call once, on the window's own thread.
    pub fn adopt_window(window: &impl raw_window_handle::HasWindowHandle) -> bool {
        const SWP_FRAME_ONLY: u32 = 0x0020 | 0x0002 | 0x0001 | 0x0004 | 0x0010;
        let Some(window) = hwnd(window) else {
            return false;
        };
        // SAFETY: the handle is this process's live window, and the calls keep no pointers.
        unsafe {
            if SetWindowSubclass(window, hide_frame, 1, 0) == 0 {
                return false;
            }
            // The whole window counts as frame, so the backdrop can show anywhere in it.
            let margins = Margins {
                left: -1,
                right: -1,
                top: -1,
                bottom: -1,
            };
            DwmExtendFrameIntoClientArea(window, &margins);
            // Makes Windows recompute the frame now, through `hide_frame`.
            SetWindowPos(window, 0, 0, 0, 0, 0, SWP_FRAME_ONLY);
        }
        true
    }

    /// Turns the blurred backdrop behind the window on or off. `dark` picks its tint.
    pub fn set_backdrop(
        window: &impl raw_window_handle::HasWindowHandle,
        on: bool,
        dark: bool,
    ) -> bool {
        const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
        const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
        const BACKDROP_NONE: i32 = 1;
        const BACKDROP_ACRYLIC: i32 = 3;
        let Some(window) = hwnd(window) else {
            return false;
        };
        let set = |attribute: u32, value: i32| {
            // SAFETY: the value outlives the call and its size is given.
            unsafe { DwmSetWindowAttribute(window, attribute, &value, 4) == 0 }
        };
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark as i32);
        set(
            DWMWA_SYSTEMBACKDROP_TYPE,
            if on { BACKDROP_ACRYLIC } else { BACKDROP_NONE },
        )
    }
}

#[cfg(not(windows))]
mod os {
    pub fn cursor() -> Option<(f32, f32)> {
        None
    }

    pub fn cursor_outside_window() -> Option<(f32, f32)> {
        None
    }

    pub fn left_button_down() -> bool {
        false
    }

    pub fn paste_keys_down() -> bool {
        false
    }

    pub fn end_with_app(_child: &std::process::Child) {}

    pub fn adopt_window(_window: &impl raw_window_handle::HasWindowHandle) -> bool {
        false
    }

    pub fn set_backdrop(
        _window: &impl raw_window_handle::HasWindowHandle,
        _on: bool,
        _dark: bool,
    ) -> bool {
        false
    }
}

/// Tracks a detached window being moved, so dropping it on the main window docks it again.
#[derive(Default)]
struct Float {
    last_pos: Option<Pos2>,
    moved: bool,
    /// When the window appeared; its first placement doesn't count as the user moving it.
    born: Option<f64>,
}

impl Float {
    /// Call from inside the detached window. True when the user just dropped it
    /// on the top bar of the main window, whose screen rectangle is `main`.
    fn dropped_on_main(&mut self, ctx: &Context, main: Option<egui::Rect>) -> bool {
        let now = ctx.input(|i| i.time);
        let born = *self.born.get_or_insert(now);
        let Some(rect) = ctx.input(|i| i.viewport().outer_rect) else {
            return false;
        };
        if self.last_pos.is_some_and(|last| last != rect.min) && now - born > 1.0 {
            self.moved = true;
        }
        self.last_pos = Some(rect.min);
        if !self.moved || os::left_button_down() {
            return false;
        }
        self.moved = false;
        let (Some((x, y)), Some(main)) = (os::cursor(), main) else {
            return false;
        };
        let scale = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0);
        let top_bar = egui::Rect::from_min_size(main.min, vec2(main.width(), 96.0));
        top_bar.contains(Pos2::new(x / scale, y / scale))
    }
}

/// Lets a window without OS decorations be resized by its edges. Call it last in a frame,
/// so its cursor icon wins over the widgets'.
fn resize_edges(ctx: &Context) {
    use egui::{CursorIcon as Icon, ResizeDirection as Dir};
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
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
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
    }
}

/// Makes the bar this `ui` belongs to move the window when dragged and maximize it on a
/// double click. Call it before adding the bar's widgets, so they stay clickable on top.
fn title_drag(ui: &Ui) {
    let rect = ui.max_rect().expand2(vec2(12.0, 8.0));
    let resp = ui.interact(rect, ui.id().with("title_drag"), Sense::click_and_drag());
    if resp.double_clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    } else if resp.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

/// A minimize/maximize/close style button; `danger` turns it red on hover.
fn caption_button(ui: &mut Ui, icon: char, hint: &str, danger: bool) -> bool {
    ui.scope(|ui| {
        if danger {
            let red = Color32::from_rgb(0xc4, 0x2b, 0x1c);
            ui.visuals_mut().widgets.hovered.weak_bg_fill = red;
            ui.visuals_mut().widgets.active.weak_bg_fill = red;
        }
        let button = egui::Button::new(RichText::new(icon).size(13.0))
            .frame_when_inactive(false)
            .min_size(vec2(34.0, 26.0));
        ui.add(button).on_hover_text(hint).clicked()
    })
    .inner
}

/// Minimize, maximize and close for the main window, laid out right to left.
fn window_buttons(ui: &mut Ui) {
    use egui::ViewportCommand as Cmd;
    let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
    ui.spacing_mut().item_spacing.x = 2.0;
    if caption_button(ui, icons::WIN_CLOSE, "Close", true) {
        ui.ctx().send_viewport_cmd(Cmd::Close);
    }
    let (icon, hint) = if maximized {
        (icons::WIN_RESTORE, "Restore")
    } else {
        (icons::WIN_MAXIMIZE, "Maximize")
    };
    if caption_button(ui, icon, hint, false) {
        ui.ctx().send_viewport_cmd(Cmd::Maximized(!maximized));
    }
    if caption_button(ui, icons::WIN_MINIMIZE, "Minimize", false) {
        ui.ctx().send_viewport_cmd(Cmd::Minimized(true));
    }
}

/// The title bar of a detached window. Returns true when the window should dock back.
fn float_title_bar(ui: &mut Ui, title: LayoutJob) -> bool {
    let mut dock = false;
    Panel::top("float_title")
        .frame(Frame::NONE.fill(theme::WINDOW_BG()).inner_margin(Margin { left: 12, right: 4, top: 4, bottom: 4 }))
        .show_separator_line(false)
        .show(ui, |ui| {
            title_drag(ui);
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(title).selectable(false).truncate());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    dock |= caption_button(ui, icons::WIN_CLOSE, "Close this window; it returns to the main window", true);
                    if caption_button(ui, icons::WIN_MINIMIZE, "Minimize", false) {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    dock |= caption_button(
                        ui,
                        icons::DOCK,
                        "Dock back into the main window\n(or drop this window on the main window's top bar)",
                        false,
                    );
                });
            });
        });
    dock
}

/// While something is being dragged: where on the screen (in points) the cursor is,
/// if it has left the window. That is the cue to detach the dragged thing.
/// Whether the paste shortcut is being held. egui reports a paste only when the clipboard
/// holds text, so a pasted picture has to be noticed this way.
pub fn paste_keys_down() -> bool {
    os::paste_keys_down()
}

pub fn dragged_outside(ctx: &Context) -> Option<Pos2> {
    let (x, y) = os::cursor_outside_window()?;
    let scale = ctx
        .input(|i| i.viewport().native_pixels_per_point)
        .unwrap_or(1.0);
    // Offset so the new window's title bar lands under the cursor.
    Some(Pos2::new(x / scale - 120.0, y / scale - 16.0))
}

/// How many times `query` occurs in the buffer, ignoring case. Zero for huge files,
/// where counting on every frame would cost too much.
fn count_matches(buf: &Buffer, query: &str) -> usize {
    if query.is_empty() || buf.line_count() > 50_000 {
        return 0;
    }
    let needle = query.to_lowercase();
    (0..buf.line_count())
        .map(|i| buf.line(i).to_lowercase().matches(&needle).count())
        .sum()
}

/// The colour that stands for a model, so an agent's weight is visible at a glance.
fn model_color(model: &str) -> Color32 {
    match model {
        "opus" => theme::CLAUDE(),
        "sonnet" => theme::ACCENT(),
        "haiku" => theme::STRING(),
        "fable" => theme::KEYWORD(),
        _ => theme::COMMENT(),
    }
}

/// An agent's badge: its initial in a disc tinted by its model, with a ring that pulses while it works.
fn avatar(ui: &Ui, rect: egui::Rect, name: &str, model: &str, busy: bool) {
    let color = model_color(model);
    let (center, radius) = (rect.center(), rect.width() / 2.0);
    let painter = ui.painter();
    painter.circle_filled(center, radius, color.gamma_multiply(0.22));
    let ring = if busy {
        let pulse = 0.55 + 0.45 * (ui.input(|i| i.time) * 4.0).sin() as f32;
        color.gamma_multiply(pulse)
    } else {
        color.gamma_multiply(0.5)
    };
    painter.circle_stroke(center, radius - 0.75, (1.5, ring));
    let initial: String = name.chars().take(1).collect::<String>().to_uppercase();
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(radius * 0.95),
        theme::STRONG(),
    );
}

/// Text made of differently coloured pieces, for labels that mix icons and names.
fn pieces(parts: &[(&str, Color32)]) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (text, color) in parts {
        job.append(
            text,
            0.0,
            TextFormat::simple(FontId::proportional(14.0), *color),
        );
    }
    job
}

fn icon_button(ui: &mut Ui, icon: char, hint: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(icon).size(13.0)).frame_when_inactive(false))
        .on_hover_text(hint)
        .clicked()
}

fn icon_toggle(ui: &mut Ui, on: bool, icon: char, hint: &str) -> bool {
    let color = if on { theme::TEXT() } else { theme::COMMENT() };
    ui.add(egui::Button::selectable(
        on,
        RichText::new(icon).size(16.0).color(color),
    ))
    .on_hover_text(hint)
    .clicked()
}

fn menu_item(ui: &mut Ui, icon: char, text: &str) -> bool {
    let clicked = ui.button(format!("{icon}   {text}")).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        settings::load();
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        let stored = |key: &str| {
            cc.storage
                .and_then(|s| s.get_string(key))
                .unwrap_or_default()
        };
        let mut hosts = conn::known_hosts();
        hosts.insert(0, LOCAL.to_string());
        let mut host = stored("host");
        if host.is_empty() {
            host = hosts.first().cloned().unwrap_or_default();
        }
        let saved = Some((stored("host"), stored("root"))).filter(|(_, root)| !root.is_empty());
        let download_dir = Some(PathBuf::from(stored("download_dir"))).filter(|p| p.is_dir());
        let theme_name = stored("theme");
        let glass = stored("glass") == "true";
        let icon_set = stored("icon_set");
        let term_dock = stored("term_dock");
        let was_connected = stored("connected") == "true";
        let glass_opacity = stored("glass_opacity").parse::<f32>().ok();
        let mut app = Self {
            link: Link::Offline,
            host,
            hosts,
            saved,
            open_on_connect: None,
            root: String::new(),
            root_input: String::new(),
            pending: HashMap::new(),
            tree: HashMap::new(),
            expanded: HashSet::new(),
            tabs: Vec::new(),
            active: 0,
            next_uid: 1,
            side: Side::Files,
            show_side: true,
            search: Search::default(),
            show_search: false,
            focus_search: false,
            find: None,
            focus_find: false,
            terms: Vec::new(),
            active_term: 0,
            show_term: true,
            term_dock: TermDock::from_name(&term_dock),
            work_area: egui::Rect::NOTHING,
            term_focused: false,
            chat: {
                // Carries over the history from before chats had their own state file.
                let mut chat = Chat::default();
                chat.load_history(&stored("chat_history"));
                chat
            },
            show_chat: stored("show_chat") != "false",
            chat_detached: false,
            chat_place_at: None,
            chat_float: Float::default(),
            chat_proc_config: None,
            downloads: Vec::new(),
            download_dir,
            dialog: None,
            status: String::new(),
            view: View::Code,
            agents: Vec::new(),
            active_agent: 0,
            team_budget: settings::with(|s| s.team_budget),
            ai: ai::Ai::default(),
            saved_state: 0,
            next_save: 0.0,
            themes: themes::Themes::default(),
            palette: palette::Palette::default(),
            settings_section: 0,
            git: tools::Git::default(),
            monitor: tools::Monitor::default(),
            forwards: Vec::new(),
            panes: panes::Panes::default(),
            updater: update::Updater::new(),
            outline_filter: String::new(),
            whats_new: {
                // Nothing stored: a fresh install has nothing to catch up on, while a ride
                // from before this screen existed was 0.1.0.
                let seen = match stored("seen_version") {
                    seen if !seen.is_empty() => seen,
                    _ if stored("host").is_empty() => update::VERSION.to_string(),
                    _ => "0.1.0".to_string(),
                };
                update::changes_since(&seen)
            },
            sessions: Vec::new(),
            session_uid: 0,
            background: false,
            transfers: Vec::new(),
            batches: Vec::new(),
            beside: None,
            hide_beside: false,
            drag_file: None,
            drop_targets: Vec::new(),
            upload: None,
            refresh_later: None,
            restore: HashMap::new(),
            restore_order: Vec::new(),
            restore_active: None,
            restore_host: String::new(),
            catchup_day: stored("catchup_day").parse().unwrap_or(0),
            focused: true,
            teams: extras::load_teams(),
            // RIDE_SIZE=WIDTHxHEIGHT opens the window at that size instead of the remembered
            // one, for checking the layout at a given resolution.
            forced_size: std::env::var("RIDE_SIZE").ok().and_then(|size| {
                let (w, h) = size.split_once('x')?;
                Some([w.parse().ok()?, h.parse().ok()?])
            }),
            // Set on the first frame, from the clock the animation runs on.
            splash: Some(-1.0),
            window: {
                let number = |key: &str| stored(key).parse::<f32>().ok();
                match (
                    number("win_w"),
                    number("win_h"),
                    number("win_x"),
                    number("win_y"),
                ) {
                    (Some(w), Some(h), Some(x), Some(y)) if w >= 300.0 && h >= 200.0 => {
                        Some(([w, h], [x, y]))
                    }
                    _ => None,
                }
            },
            maximized: stored("win_max") == "true",
            frame_extra: egui::Vec2::ZERO,
            icon_sets: iconsets::IconSets::default(),
        };
        app.load_state();
        // A first run has no chat yet; it starts in the mode the settings choose.
        if app.chat.is_empty() {
            app.chat.mode = settings::chat_mode();
        }
        // Lets a specific view be opened from outside, e.g. for screenshots.
        match std::env::var("RIDE_START_VIEW").as_deref() {
            Ok("themes") => app.view = View::Themes,
            Ok("settings") => app.view = View::Settings,
            _ => {}
        }
        // RIDE_HOST and RIDE_ROOT pick the host and folder, RIDE_SIDE the side panel.
        if let Ok(host) = std::env::var("RIDE_HOST") {
            app.saved = std::env::var("RIDE_ROOT")
                .ok()
                .map(|root| (host.clone(), root));
            app.host = host;
        }
        match std::env::var("RIDE_SIDE").as_deref() {
            Ok("git") => app.side = Side::Git,
            Ok("server") => app.side = Side::Server,
            Ok("outline") => app.side = Side::Outline,
            _ => {}
        }
        app.refresh_themes();
        if !theme_name.is_empty() {
            app.select_theme(&cc.egui_ctx, &theme_name);
        }
        // RIDE_GLASS forces the setting from outside, for checking the backdrop.
        app.themes.glass = match std::env::var("RIDE_GLASS").as_deref() {
            Ok("0") => false,
            Ok(_) => true,
            Err(_) => glass,
        };
        // A session that ended connected starts connected again, to the same host and folder.
        // RIDE_CONNECT forces it.
        if (was_connected
            && settings::with(|s| s.reconnect)
            && std::env::var_os("RIDE_NO_CONNECT").is_none())
            || std::env::var_os("RIDE_CONNECT").is_some()
        {
            app.connect(&cc.egui_ctx);
        }
        // A path on the command line ("Open with ride") opens that folder, or that file
        // in its folder, on this computer.
        let named = std::env::args().skip(1).find(|arg| !arg.starts_with("--"));
        if let Some(path) = named.filter(|path| Path::new(path).exists()) {
            let is_dir = Path::new(&path).is_dir();
            let path = path.replace('\\', "/");
            let folder = if is_dir {
                path.clone()
            } else {
                parent(&path).to_string()
            };
            app.host = LOCAL.to_string();
            app.saved = Some((LOCAL.to_string(), folder));
            app.open_on_connect = (!is_dir).then_some(path);
            app.view = View::Code;
            app.connect(&cc.egui_ctx);
        }
        app.refresh_icon_sets();
        if !icon_set.is_empty() {
            app.select_icon_set(&icon_set);
        }
        if let Some(opacity) = glass_opacity {
            app.themes.opacity = opacity.clamp(0.3, 0.97);
        }
        app.palette_from_env();
        if let Some(error) = update::Updater::take_last_error() {
            app.status = format!("The last update didn't install: {error}");
        }
        app
    }

    /// Turns the start screen's small window into the editor's: its remembered size and
    /// place, or a default one in the middle of the display.
    fn end_splash(&mut self, ctx: &Context) {
        use egui::ViewportCommand as Cmd;
        self.splash = None;
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(vec2(1920.0, 1080.0));
        let (size, pos) = match (self.forced_size, self.window) {
            (Some(size), _) => (vec2(size[0], size[1]), None),
            (None, Some((size, pos))) => (vec2(size[0], size[1]), Some(Pos2::new(pos[0], pos[1]))),
            (None, None) => (vec2(1500.0, 920.0).min(monitor - vec2(80.0, 100.0)), None),
        };
        // A place off every screen (a monitor that is gone, say) is not restored.
        let pos = pos
            .filter(|p| {
                p.x > -size.x + 100.0
                    && p.y > -40.0
                    && p.x < monitor.x * 3.0
                    && p.y < monitor.y * 3.0
            })
            .unwrap_or_else(|| ((monitor - size) / 2.0).max(vec2(0.0, 0.0)).to_pos2());
        ctx.send_viewport_cmd(Cmd::MinInnerSize(vec2(460.0, 320.0)));
        ctx.send_viewport_cmd(Cmd::InnerSize(size - self.frame_extra));
        ctx.send_viewport_cmd(Cmd::OuterPosition(pos));
        if self.maximized && self.forced_size.is_none() {
            ctx.send_viewport_cmd(Cmd::Maximized(true));
        }
        ctx.request_repaint();
    }

    /// Keeps track of where the window is, for the next start.
    fn remember_window(&mut self, ctx: &Context) {
        let (inner, outer, maximized, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.inner_rect,
                v.outer_rect,
                v.maximized.unwrap_or(false),
                v.minimized.unwrap_or(false),
            )
        });
        if minimized {
            return;
        }
        self.maximized = maximized;
        // While maximized, the size to come back to is the one from before.
        if let (false, Some(inner), Some(outer)) = (maximized, inner, outer) {
            if inner.width() >= 300.0 && inner.height() >= 200.0 {
                self.window = Some(([inner.width(), inner.height()], [outer.min.x, outer.min.y]));
            }
        }
    }

    // ---- saved state ----

    fn state_json(&self) -> String {
        let agents: Vec<serde_json::Value> = self
            .agents
            .iter()
            .map(|a| {
                serde_json::json!({
                    "uid": a.uid,
                    "name": a.name,
                    "parent": a.parent,
                    "sent": a.sent,
                    "role": a.role,
                    "chat": a.chat.to_json(),
                })
            })
            .collect();
        serde_json::json!({
            "chat": self.chat.to_json(),
            "agents": agents,
            "active_agent": self.active_agent,
            "scheduled": self.scheduled_json(),
            "session": self.session_json(),
        })
        .to_string()
    }

    fn load_state(&mut self) {
        let Some(text) = state_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        if v["chat"].is_object() {
            self.chat = Chat::from_json(&v["chat"]);
        }
        for a in v["agents"].as_array().into_iter().flatten() {
            let (Some(uid), Some(name)) = (a["uid"].as_u64(), a["name"].as_str()) else {
                continue;
            };
            self.agents.push(Agent {
                uid,
                name: name.to_string(),
                chat: Chat::from_json(&a["chat"]),
                started_with: None,
                inbox: Default::default(),
                reply_to: None,
                parent: a["parent"].as_u64(),
                sent: a["sent"].as_u64().unwrap_or(0) as u32,
                role: a["role"].as_str().unwrap_or("").to_string(),
            });
            self.next_uid = self.next_uid.max(uid + 1);
        }
        self.active_agent = v["active_agent"].as_u64().unwrap_or(0) as usize;
        self.load_scheduled(&v["scheduled"]);
        self.load_session(&v["session"]);
        self.saved_state = hash_of(&text);
    }

    /// Writes the chats to disk if they changed. Cheap enough to call often.
    fn write_state(&mut self) {
        let text = self.state_json();
        let hash = hash_of(&text);
        let Some(path) = state_path().filter(|_| hash != self.saved_state) else {
            return;
        };
        // Written beside the real file and renamed over it, so a crash can't leave half a file.
        let temp = path.with_extension("json.tmp");
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&temp, &text))
            .and_then(|_| std::fs::rename(&temp, &path));
        match written {
            Ok(()) => self.saved_state = hash,
            Err(error) => self.status = format!("Couldn't save chat history: {error}"),
        }
    }

    fn autosave(&mut self, ctx: &Context) {
        const EVERY: f64 = 3.0;
        let now = ctx.input(|i| i.time);
        if now >= self.next_save {
            self.next_save = now + EVERY;
            self.write_state();
        }
        // So the last change still gets saved if nothing else happens afterwards.
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(EVERY + 0.5));
    }

    fn online(&self) -> bool {
        matches!(self.link, Link::Online(_))
    }

    fn request(&mut self, req: Request) -> u64 {
        link_request(&mut self.link, req)
    }

    fn request_then(&mut self, req: Request, then: Pending) {
        if self.online() {
            let id = self.request(req);
            self.pending.insert(id, then);
        }
    }

    // ---- connection ----

    fn connect(&mut self, ctx: &Context) {
        let host = self.host.trim().to_string();
        if host.is_empty() {
            return;
        }
        self.disconnect();
        chat::set_local_shell(host == LOCAL);
        let (tx, rx) = channel();
        let status = Arc::new(Mutex::new(String::new()));
        let (ctx, thread_status) = (ctx.clone(), status.clone());
        thread::spawn(move || {
            let wake = ctx.clone();
            let result = if host == LOCAL {
                Conn::connect_local(move || wake.request_repaint())
            } else {
                Conn::connect(&host, &thread_status, move || wake.request_repaint())
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.link = Link::Connecting { rx, status };
        self.status.clear();
    }

    /// Drops everything tied to the connection. Open tabs stay, so unsaved edits aren't lost.
    fn disconnect(&mut self) {
        self.link = Link::Offline;
        self.forwards.clear();
        self.pending.clear();
        self.tree.clear();
        self.expanded.clear();
        self.terms.clear();
        self.search.id = None;
        // Claude's processes run on the connection shown; losing another one leaves them be.
        if !self.background {
            self.chat.stopped();
            for agent in &mut self.agents {
                agent.chat.stopped();
                agent.inbox.clear();
                agent.reply_to = None;
            }
        }
        for download in &mut self.downloads {
            download.tx = None;
        }
    }

    fn poll(&mut self, ctx: &Context) {
        self.poll_link(ctx);
        self.poll_uploads(ctx);
        self.poll_sessions(ctx);
        self.session_hooks(ctx);
        self.flush_chats();
        self.schedule_tick(ctx);
        self.focused = ctx.input(|i| i.focused);
        self.autosave(ctx);
        for download in &mut self.downloads {
            if let Ok(result) = download.result.try_recv() {
                // An error reported by the server explains more than the truncated stream it caused.
                download.outcome.get_or_insert(result);
            }
        }
    }

    /// Takes what has arrived on the connection at hand.
    fn poll_link(&mut self, ctx: &Context) {
        let mut connected = None;
        let mut events = Vec::new();
        match &mut self.link {
            Link::Offline => {}
            Link::Connecting { rx, .. } => connected = rx.try_recv().ok(),
            Link::Online(conn) => events.extend(conn.events.try_iter()),
        }
        match connected {
            Some(Ok(conn)) => {
                self.link = Link::Online(conn);
                self.request_then(Request::Hello, Pending::Hello);
            }
            Some(Err(error)) => {
                self.link = Link::Offline;
                self.status = format!("Connection failed: {error}");
            }
            None => {}
        }
        for event in events {
            match event {
                ConnEvent::Msg(msg) => self.handle(msg, ctx),
                ConnEvent::Closed(reason) => {
                    self.disconnect();
                    self.status = format!("Disconnected: {reason}");
                }
            }
        }
    }

    fn handle(&mut self, ServerMsg { id, resp }: ServerMsg, ctx: &Context) {
        if let Response::HostAsk { token, text } = &resp {
            let text = self.host_ask(text);
            self.request(Request::HostAnswer {
                token: *token,
                text,
            });
            return;
        }
        if self.job_message(id, &resp) || self.transfer_message(id, &resp) {
            return;
        }
        if let Some(index) = self.terms.iter().position(|t| t.id == id) {
            match resp {
                Response::TermOutput { data } => {
                    let mut reply = Vec::new();
                    self.terms[index].feed(&data, &mut reply, ctx);
                    if !reply.is_empty() {
                        self.request(Request::TermInput {
                            term: id,
                            data: reply,
                        });
                    }
                }
                _ => {
                    self.terms.remove(index);
                    self.active_term = self.active_term.min(self.terms.len().saturating_sub(1));
                    self.show_term &= !self.terms.is_empty();
                }
            }
            return;
        }

        if self.chat.proc == Some(id) {
            match resp {
                Response::ProcOutput { data } => {
                    if self.chat.on_stdout(&data) {
                        self.notify("Claude finished", &self.chat.snippet());
                        self.sync_after_claude();
                    }
                }
                Response::ProcStderr { data } => self.chat.on_stderr(&data),
                Response::ProcExit { code } => self.chat.on_exit(code),
                Response::Error { message } => {
                    self.chat.on_stderr(message.as_bytes());
                    self.chat.on_exit(None);
                }
                _ => {}
            }
            return;
        }

        if let Some(index) = self.agents.iter().position(|a| a.chat.proc == Some(id)) {
            let chat = &mut self.agents[index].chat;
            match resp {
                Response::ProcOutput { data } => {
                    if chat.on_stdout(&data) {
                        self.agent_turn_done(index);
                    }
                }
                Response::ProcStderr { data } => chat.on_stderr(&data),
                Response::ProcExit { code } => {
                    chat.on_exit(code);
                    self.agent_failed(index);
                }
                Response::Error { message } => {
                    chat.on_stderr(message.as_bytes());
                    chat.on_exit(None);
                    self.agent_failed(index);
                }
                _ => {}
            }
            return;
        }

        if self.search.id == Some(id) {
            match resp {
                Response::SearchHits { hits } => self.search.hits.extend(hits),
                Response::Error { message } => {
                    self.search.id = None;
                    self.status = format!("Search failed: {message}");
                }
                _ => self.search.id = None,
            }
            return;
        }

        if let Some(download) = self.downloads.iter_mut().find(|d| d.id == id) {
            match resp {
                Response::Chunk { data } => {
                    download.bytes += data.len() as u64;
                    if let Some(tx) = &download.tx {
                        let _ = tx.send(data);
                    }
                }
                Response::Done => download.tx = None,
                Response::Error { message } => {
                    download.tx = None;
                    download.outcome = Some(Err(message));
                }
                _ => {}
            }
            return;
        }

        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        match (pending, resp) {
            (Pending::Hello, Response::Hello { home }) => {
                let root = match self.saved.take() {
                    Some((host, root)) if host == self.host.trim() => root,
                    _ => home,
                };
                self.open_root(root);
                self.set_up_host();
                match self.open_on_connect.take() {
                    // Asked to open a particular file: straight to it.
                    Some(file) => self.open_file(file, None),
                    None => {
                        self.restore_session();
                        self.daily_catchup();
                    }
                }
                // RIDE_OPEN names more files to open, separated by semicolons; `path@LINE`
                // puts the cursor on a line.
                for item in std::env::var("RIDE_OPEN")
                    .iter()
                    .flat_map(|list| list.split(';'))
                {
                    let (path, line) = match item.rsplit_once('@') {
                        Some((path, line)) => (path, line.parse::<usize>().ok()),
                        None => (item, None),
                    };
                    let goto = line.map(|l| Pos::new(l.saturating_sub(1), 0));
                    self.open_file(path.replace('\\', "/"), goto);
                }
                // RIDE_CHAT_SEND sends a first message to the side chat, to look at its answer.
                // RIDE_NO_TERM starts with the terminal hidden, for screenshots.
                if std::env::var_os("RIDE_NO_TERM").is_some() {
                    self.show_term = false;
                }
                if let Ok(text) = std::env::var("RIDE_CHAT_SEND") {
                    self.show_chat = true;
                    self.chat_send(text);
                }
            }
            (
                Pending::Transfer {
                    name,
                    requests,
                    dest,
                },
                Response::Dir { entries },
            ) => {
                self.finish_transfer(ctx, name, requests, dest, &entries);
            }
            (Pending::ListDir(path), Response::Dir { entries }) => {
                self.tree.insert(path, DirState::Loaded(entries));
            }
            (Pending::ListDir(path), Response::Error { message }) => {
                self.tree.insert(path, DirState::Failed(message));
            }
            (Pending::Open { path, goto }, Response::File { data }) => {
                let (text, preview) = match preview::load(ctx, &path, data) {
                    Ok(preview::Loaded::Text(text)) => (text, None),
                    Ok(preview::Loaded::Preview(preview)) => (String::new(), Some(preview)),
                    Err(error) => {
                        self.status = format!("{}: {error}", file_name(&path));
                        return;
                    }
                };
                let mut tab = Tab {
                    uid: self.next_uid,
                    path,
                    buf: Buffer::new(&text),
                    view: EditorState::default(),
                    preview,
                    pane: self.focus_pane(),
                    detached: false,
                    place_at: None,
                    float: Float::default(),
                    edited: (0, 0.0, 0),
                };
                self.next_uid += 1;
                if let Some(pos) = goto {
                    tab.buf.set_cursor(pos, false);
                    tab.view.reveal_cursor = true;
                }
                tab.view.request_focus = true;
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
                self.restored_tab_opened(self.tabs.len() - 1);
                self.check_file(self.tabs.len() - 1);
            }
            (Pending::Reload { path }, Response::File { data }) => {
                let tab = self
                    .tabs
                    .iter_mut()
                    .find(|t| t.path == path && !t.buf.is_dirty());
                if let (Some(tab), Ok(text)) = (tab, String::from_utf8(data)) {
                    if tab.buf.text() != text {
                        let cursor = tab.buf.cursor;
                        tab.buf = Buffer::new(&text);
                        tab.buf.set_cursor(cursor, false);
                        tab.view = EditorState::default();
                    }
                }
                if let Some(index) = self.tabs.iter().position(|t| t.path == path) {
                    self.check_file(index);
                }
            }
            // The file may simply be gone; the tab keeps what it had.
            (Pending::Reload { .. }, _) => {}
            (Pending::Save { path, revision }, Response::Ok) => {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.path == path) {
                    tab.buf.mark_saved(revision);
                }
                if let Some(index) = self.tabs.iter().position(|t| t.path == path) {
                    self.check_file(index);
                }
                self.git_refresh();
                self.status = format!("Saved {path}");
            }
            (Pending::FsOp { refresh }, Response::Ok) => self.load_dir(&refresh),
            (Pending::Created { path, dir }, Response::Ok) => {
                self.load_dir(&dir);
                self.open_file(path, None);
            }
            (Pending::Mentions { query, agent }, Response::Paths { paths }) => {
                self.mentions_found(&query, agent, paths);
            }
            (Pending::Mentions { .. }, _) => {}
            (Pending::PaletteFiles { query }, Response::Paths { paths }) => {
                self.palette_files_found(&query, paths)
            }
            (Pending::PaletteFiles { .. }, _) => {}
            (
                Pending::Compare {
                    left,
                    left_text,
                    right,
                },
                Response::File { data },
            ) => {
                let right_text = String::from_utf8_lossy(&data);
                let lines = diff::lines(&left_text, &right_text);
                let title = format!("{} ↔ {}", file_name(&left), file_name(&right));
                self.push_preview_tab(title, preview::Preview::Diff(lines));
            }
            (Pending::Revert { change }, Response::File { data }) => {
                self.apply_revert(change, data)
            }
            (Pending::Reverted { change }, Response::Ok) => {
                self.status = format!("Reverted an edit in {}", file_name(&change.path));
                self.forget_change(&change);
                self.sync_after_claude();
            }
            (_, Response::Error { message }) => self.status = message,
            _ => {}
        }
    }

    // ---- files ----

    fn open_root(&mut self, root: String) {
        // "C:/" keeps its slash: "C:" alone means the drive's current folder, not its root.
        let root = root.replace('\\', "/");
        let root = if root.len() > 1 && !root.ends_with(":/") {
            root.trim_end_matches('/').to_string()
        } else {
            root
        };
        self.tree.clear();
        self.expanded.clear();
        self.root_input = root.clone();
        self.root = root.clone();
        self.load_dir(&root);
        self.git = tools::Git::default();
        self.monitor = tools::Monitor::default();
        self.git_refresh();
    }

    fn load_dir(&mut self, path: &str) {
        // Entries already shown stay until the fresh listing arrives, so a refresh doesn't flicker.
        self.tree
            .entry(path.to_string())
            .or_insert(DirState::Loading);
        self.request_then(
            Request::ListDir {
                path: path.to_string(),
            },
            Pending::ListDir(path.to_string()),
        );
    }

    fn open_file(&mut self, path: String, goto: Option<Pos>) {
        if let Some(index) = self.tabs.iter().position(|t| t.path == path) {
            let tab = &mut self.tabs[index];
            if !tab.detached {
                self.active = index;
            }
            if let Some(pos) = goto {
                tab.buf.set_cursor(pos, false);
                tab.view.reveal_cursor = true;
            }
            tab.view.request_focus = true;
            return;
        }
        // The whole file crosses the connection, so very large ones aren't opened.
        let limit = settings::with(|s| s.max_open_mb) as u64 * 1024 * 1024;
        let size = match self.tree.get(parent(&path)) {
            Some(DirState::Loaded(entries)) => entries
                .iter()
                .find(|e| e.name == file_name(&path))
                .map_or(0, |e| e.size),
            _ => 0,
        };
        if size > limit {
            self.status = format!(
                "{} is {} — too large to open; download it instead",
                file_name(&path),
                human_size(size)
            );
            return;
        }
        self.request_then(
            Request::ReadFile { path: path.clone() },
            Pending::Open { path, goto },
        );
    }

    fn save_tab(&mut self, index: usize) {
        self.tidy_before_save(index);
        // With format on save, the formatter's answer is what gets saved.
        if settings::with(|s| s.format_on_save) && self.format_tab(index, true) {
            return;
        }
        self.write_tab(index);
    }

    /// Writes a tab's text to its file as it is.
    fn write_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index).filter(|t| t.preview.is_none()) else {
            return;
        };
        if !self.online() {
            self.status = "Not connected, can't save".into();
            return;
        }
        let (path, revision) = (tab.path.clone(), tab.buf.revision);
        let data = tab.buf.text().into_bytes();
        self.request_then(
            Request::WriteFile {
                path: path.clone(),
                data,
            },
            Pending::Save { path, revision },
        );
    }

    fn close_tab(&mut self, index: usize, force: bool) {
        if !force && self.tabs[index].buf.is_dirty() {
            self.dialog = Some(Dialog::ConfirmClose(index));
            return;
        }
        self.tabs.remove(index);
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// The tab shown in the main window's editor: the active one, unless it lives in its own window.
    fn docked_active(&self) -> Option<usize> {
        match self.tabs.get(self.active) {
            Some(tab) if !tab.detached => Some(self.active),
            _ => self.tabs.iter().position(|t| !t.detached),
        }
    }

    /// Claude may have edited files: refresh open tabs without unsaved edits, and the tree.
    fn sync_after_claude(&mut self) {
        let clean: Vec<String> = self
            .tabs
            .iter()
            .filter(|t| !t.buf.is_dirty() && t.preview.is_none())
            .map(|t| t.path.clone())
            .collect();
        for path in clean {
            self.request_then(
                Request::ReadFile { path: path.clone() },
                Pending::Reload { path },
            );
        }
        let dirs: Vec<String> = std::iter::once(self.root.clone())
            .chain(self.expanded.iter().cloned())
            .collect();
        for dir in dirs {
            self.load_dir(&dir);
        }
        self.git_refresh();
    }

    fn start_download(&mut self, path: String, is_dir: bool) {
        let name = file_name(&path).to_string();
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = &self.download_dir {
            dialog = dialog.set_directory(dir);
        }
        let dest = if is_dir {
            dialog
                .set_title(format!("Download \"{name}\" into…"))
                .pick_folder()
        } else {
            dialog.set_file_name(&name).save_file()
        };
        let Some(dest) = dest else { return };
        self.download_dir = if is_dir {
            Some(dest.clone())
        } else {
            dest.parent().map(Path::to_path_buf)
        };

        let id = self.request(Request::Download { path });
        let (tx, rx) = channel();
        let (result_tx, result) = channel();
        thread::spawn(move || {
            let reader = ChunkReader {
                rx,
                chunk: Vec::new(),
                pos: 0,
            };
            let outcome = if is_dir {
                unpack_folder(reader, &dest)
            } else {
                save_file(reader, &dest)
            };
            let _ = result_tx.send(outcome);
        });
        self.downloads.push(Download {
            id,
            name,
            bytes: 0,
            tx: Some(tx),
            result,
            outcome: None,
        });
    }

    fn run_search(&mut self) {
        if let Some(id) = self.search.id.take() {
            self.request(Request::Cancel { target: id });
        }
        self.search.hits.clear();
        if self.search.query.is_empty() || !self.online() {
            return;
        }
        let id = self.request(Request::Search {
            root: self.root.clone(),
            query: self.search.query.clone(),
            regex: self.search.regex,
            case_sensitive: self.search.case_sensitive,
        });
        self.search.id = Some(id);
    }

    fn new_term(&mut self) {
        if !self.online() {
            return;
        }
        let (cols, rows) = self.terms.first().map_or((100, 24), |t| t.size());
        let id = self.request(Request::TermOpen {
            cols,
            rows,
            cwd: self.root.clone(),
        });
        self.terms.push(Terminal::new(id, cols, rows));
        self.active_term = self.terms.len() - 1;
        self.show_term = true;
    }

    fn find_next(&mut self, forward: bool, incremental: bool) {
        let (Some(query), Some(tab)) = (&self.find, self.tabs.get_mut(self.active)) else {
            return;
        };
        let (start, end) = tab.buf.selection();
        // While typing, keep matching from where the current match starts.
        let from = if forward && !incremental { end } else { start };
        if let Some((a, b)) = tab.buf.find(query, from, forward, false) {
            tab.buf.set_cursor(a, false);
            tab.buf.set_cursor(b, true);
            tab.view.reveal_cursor = true;
        }
    }

    // ---- chat ----

    /// Sends what the chats have queued for their processes: answers to permission
    /// requests, and requests to restore files. Also passes on what they have to report.
    fn flush_chats(&mut self) {
        let mut lines = Vec::new();
        let mut asked = Vec::new();
        let mut rewound = false;
        let chats = std::iter::once((&mut self.chat, "Claude".to_string())).chain(
            self.agents
                .iter_mut()
                .map(|a| (&mut a.chat, a.name.clone())),
        );
        for (chat, name) in chats {
            if let Some(proc) = chat.proc {
                lines.extend(chat.take_outbox().into_iter().map(|data| (proc, data)));
            }
            if let Some(what) = chat.take_new_ask() {
                asked.push((name, what));
            }
            rewound |= chat.take_rewound();
        }
        for (proc, data) in lines {
            self.request(Request::ProcInput { proc, data });
        }
        for (name, what) in asked {
            self.notify(&format!("{name} is waiting for your OK"), &what);
        }
        if rewound {
            self.sync_after_claude();
        }
    }

    /// Starts the side chat's process, if it isn't running with the current settings.
    fn chat_wake(&mut self) {
        if !self.online() {
            return;
        }
        // Mode, model and effort are fixed when the process starts.
        if self.chat.proc.is_some() && self.chat_proc_config != Some(self.chat.config()) {
            self.chat_stop();
        }
        if self.chat.proc.is_none() {
            let command = self.chat.command();
            let id = self.request(Request::ProcOpen {
                command,
                cwd: self.root.clone(),
            });
            self.chat.proc = Some(id);
            self.chat_proc_config = Some(self.chat.config());
        }
    }

    fn chat_send(&mut self, text: String) {
        if !self.online() {
            return;
        }
        self.chat_wake();
        let context = self
            .docked_active()
            .filter(|_| self.chat.attach)
            .map(|index| {
                let tab = &self.tabs[index];
                format!(
                    "[The user has {} open in their editor, with the cursor on line {}.]",
                    tab.path,
                    tab.buf.cursor.line + 1
                )
            });
        let context: Vec<String> = [context, self.mention_context(&text)]
            .into_iter()
            .flatten()
            .collect();
        let context = (!context.is_empty()).then(|| context.join("\n\n"));
        let data = self.chat.begin_turn(&text, context);
        if let Some(proc) = self.chat.proc {
            self.request(Request::ProcInput { proc, data });
        }
    }

    fn chat_stop(&mut self) {
        if let Some(proc) = self.chat.proc {
            self.request(Request::ProcClose { proc });
        }
        self.chat.stopped();
    }

    fn chat_ui(&mut self, ui: &mut Ui, detached: bool) {
        let open_file = self.docked_active().map(|i| self.tabs[i].path.clone());
        let Some(action) = self.chat.show(ui, open_file.as_deref(), detached) else {
            return;
        };
        match action {
            chat::Action::Send(text) => self.chat_send(text),
            chat::Action::Stop | chat::Action::Switched => self.chat_stop(),
            chat::Action::FindFiles(query) => self.find_files(query, None),
            chat::Action::Wake => self.chat_wake(),
            chat::Action::NewChat => {
                self.chat_stop();
                self.chat.reset();
            }
            chat::Action::Restart => {
                if !self.chat.busy {
                    self.chat_stop();
                }
            }
            chat::Action::Login => {
                self.new_term();
                if let Some(term) = self.terms.last() {
                    let term = term.id;
                    self.request(Request::TermInput {
                        term,
                        data: b"claude /login\r".to_vec(),
                    });
                }
            }
            chat::Action::Detach(pos) => {
                self.chat_detached = true;
                self.chat_place_at = pos;
                self.chat_float = Float::default();
            }
        }
    }

    // ---- agents ----

    /// "name (model)" for every agent, sent along with each message so agents know their team.
    fn roster(&self) -> String {
        let entries: Vec<String> = self
            .agents
            .iter()
            .map(|a| {
                let model = if a.chat.model_choice.is_empty() {
                    "default"
                } else {
                    a.chat.model_choice
                };
                format!("{} ({model})", a.name)
            })
            .collect();
        entries.join(", ")
    }

    fn new_agent(&mut self, name: &str, model: &'static str, mode: chat::Mode) -> usize {
        let base = agent_name(name);
        let mut name = base.clone();
        let mut n = 2;
        while self.agents.iter().any(|a| a.name == name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        let mut chat = Chat::default();
        chat.title = name.clone();
        chat.team_member = true;
        chat.attach = false;
        chat.model_choice = model;
        chat.mode = mode;
        chat.system = Some(team_prompt(&name));
        let uid = self.next_uid;
        self.next_uid += 1;
        self.agents.push(Agent {
            uid,
            name,
            chat,
            started_with: None,
            inbox: Default::default(),
            reply_to: None,
            parent: None,
            sent: 0,
            role: String::new(),
        });
        self.agents.len() - 1
    }

    fn agent_user_send(&mut self, index: usize, text: String) {
        if !self.online() {
            return;
        }
        self.team_budget = settings::with(|s| s.team_budget);
        let roster = self.roster();
        let mentions = self
            .mention_context(&text)
            .map(|m| format!("\n\n{m}"))
            .unwrap_or_default();
        let agent = &mut self.agents[index];
        agent.reply_to = None;
        ensure_proc(
            &mut self.link,
            &self.root,
            &mut agent.chat,
            &mut agent.started_with,
        );
        let data = agent
            .chat
            .begin_turn(&text, Some(format!("[team: {roster}]{mentions}")));
        if let Some(proc) = agent.chat.proc {
            link_request(&mut self.link, Request::ProcInput { proc, data });
        }
    }

    /// Hands a teammate's message to an agent, or queues it while the agent is busy.
    fn deliver(&mut self, to: usize, mail: Mail) {
        let roster = self.roster();
        let agent = &mut self.agents[to];
        if agent.chat.busy {
            agent.inbox.push_back(mail);
            return;
        }
        agent.reply_to = mail.request.then_some(mail.from);
        ensure_proc(
            &mut self.link,
            &self.root,
            &mut agent.chat,
            &mut agent.started_with,
        );
        let data = agent
            .chat
            .begin_turn_from(&mail.from_name, &mail.text, &roster);
        if let Some(proc) = agent.chat.proc {
            link_request(&mut self.link, Request::ProcInput { proc, data });
        }
    }

    /// An agent finished a turn: carry out its directives and pass its answer on.
    fn agent_turn_done(&mut self, index: usize) {
        let (uid, name, mode) = {
            let agent = &self.agents[index];
            (agent.uid, agent.name.clone(), agent.chat.mode)
        };
        let result = self.agents[index].chat.take_result().unwrap_or_default();
        let (body, directives) = parse_directives(&result);

        // A turn that ends without handing work on, and that nobody is waiting for, is news for the user.
        let delegated = !directives.is_empty();
        for directive in directives {
            if self.team_budget == 0 {
                self.agents[index].chat.note_error(
                    "The team reached its message limit for this request. Send a message to let it continue.",
                );
                break;
            }
            self.team_budget -= 1;
            let target = match self.agents.iter().position(|a| a.name == directive.name) {
                Some(target) => target,
                None if directive.spawn => {
                    let created =
                        self.new_agent(&directive.name, model_alias(&directive.model), mode);
                    self.agents[created].parent = Some(uid);
                    created
                }
                None => {
                    let note = format!(
                        "There is no agent named {}; the message was not sent.",
                        directive.name
                    );
                    self.agents[index].chat.note_error(&note);
                    continue;
                }
            };
            if target != index {
                let mail = Mail {
                    from: uid,
                    from_name: name.clone(),
                    request: true,
                    text: directive.text,
                };
                self.agents[index].sent += 1;
                self.deliver(target, mail);
            }
        }

        let reply_to = self.agents[index].reply_to.take();
        let requester = reply_to.and_then(|to| self.agents.iter().position(|a| a.uid == to));
        if !delegated && requester.is_none() {
            let title = format!("{} finished", self.agents[index].name);
            self.notify(&title, &self.agents[index].chat.snippet());
        }
        if let (Some(to), false) = (requester, body.is_empty()) {
            self.deliver(
                to,
                Mail {
                    from: uid,
                    from_name: name,
                    request: false,
                    text: body,
                },
            );
        }
        if let Some(mail) = self.agents[index].inbox.pop_front() {
            self.deliver(index, mail);
        }
        self.sync_after_claude();
    }

    /// An agent's process died; whoever is waiting for its answer has to hear about it.
    fn agent_failed(&mut self, index: usize) {
        let (uid, name) = (self.agents[index].uid, self.agents[index].name.clone());
        let reply_to = self.agents[index].reply_to.take();
        if let Some(to) = reply_to.and_then(|to| self.agents.iter().position(|a| a.uid == to)) {
            let text =
                "I stopped with an error before finishing; the task was not completed.".to_string();
            self.deliver(
                to,
                Mail {
                    from: uid,
                    from_name: name,
                    request: false,
                    text,
                },
            );
        }
    }

    fn stop_agent(&mut self, index: usize) {
        let agent = &mut self.agents[index];
        if let Some(proc) = agent.chat.proc {
            link_request(&mut self.link, Request::ProcClose { proc });
        }
        agent.chat.stopped();
        agent.inbox.clear();
        agent.reply_to = None;
    }

    /// The agents as a tree: each one under the agent that created it, with its depth.
    fn agent_order(&self) -> Vec<(usize, usize)> {
        fn visit(
            agents: &[Agent],
            parent: Option<u64>,
            depth: usize,
            out: &mut Vec<(usize, usize)>,
        ) {
            for (index, agent) in agents.iter().enumerate() {
                // An agent whose creator was removed is shown at the top level.
                let known = agent
                    .parent
                    .filter(|uid| agents.iter().any(|a| a.uid == *uid));
                if known == parent && depth < 12 {
                    out.push((index, depth));
                    visit(agents, Some(agent.uid), depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        visit(&self.agents, None, 0, &mut out);
        out
    }

    fn agents_panel(&mut self, ui: &mut Ui) {
        let (mut add, mut stop_all) = (false, false);
        let mut team_action = None;
        let working = self.agents.iter().filter(|a| a.chat.busy).count();
        let cost: f64 = self.agents.iter().map(|a| a.chat.cost()).sum();
        Frame::NONE
            .inner_margin(Margin {
                left: 6,
                right: 2,
                top: 4,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new("Team").size(17.0).color(theme::STRONG()));
                        let summary = format!(
                            "{} agents  ·  {working} working  ·  ${cost:.2}",
                            self.agents.len()
                        );
                        ui.label(RichText::new(summary).small().color(theme::COMMENT()));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        add = icon_button(ui, icons::ADD, "New agent");
                        stop_all = icon_button(ui, icons::STOP, "Stop all agents");
                        team_action = self.teams_menu(ui);
                    });
                });
            });
        egui::Panel::bottom("scheduled_tasks")
            .frame(Frame::NONE.inner_margin(Margin {
                left: 6,
                right: 2,
                top: 8,
                bottom: 2,
            }))
            .show(ui, |ui| self.scheduled_section(ui));

        let (mut select, mut remove) = (None, None);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (index, depth) in self.agent_order() {
                let agent = &self.agents[index];
                let selected = index == self.active_agent;
                let mut on_button = false;
                let indent = (depth.min(6) * 16) as i8;
                let row = Frame::NONE
                    .fill(if selected {
                        theme::tint(18)
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(9)
                    .inner_margin(Margin {
                        left: 8 + indent,
                        right: 4,
                        top: 6,
                        bottom: 6,
                    })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                        ui.horizontal(|ui| {
                            let (badge, _) =
                                ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                            avatar(
                                ui,
                                badge,
                                &agent.name,
                                agent.chat.model_choice,
                                agent.chat.busy,
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, icons::CLOSE, "Remove agent") {
                                    remove = Some(index);
                                    on_button = true;
                                }
                                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        let plain = |ui: &mut Ui, text: RichText| {
                                            ui.add(egui::Label::new(text).selectable(false))
                                        };
                                        plain(
                                            ui,
                                            RichText::new(&agent.name).color(theme::STRONG()),
                                        );
                                        let model = if agent.chat.model_choice.is_empty() {
                                            "default"
                                        } else {
                                            agent.chat.model_choice
                                        };
                                        plain(
                                            ui,
                                            RichText::new(model)
                                                .small()
                                                .color(model_color(agent.chat.model_choice)),
                                        );
                                        if !agent.inbox.is_empty() {
                                            plain(
                                                ui,
                                                RichText::new(format!(
                                                    "{} queued",
                                                    agent.inbox.len()
                                                ))
                                                .small()
                                                .color(theme::COMMENT()),
                                            );
                                        }
                                    });
                                    // What it is doing, or the last thing that happened in its chat.
                                    let snippet = agent.chat.snippet();
                                    let activity = match (agent.chat.busy, snippet.is_empty()) {
                                        (true, true) => "Working…".to_string(),
                                        (true, false) => format!("Working · {snippet}"),
                                        (false, true) => "Waiting for a task".to_string(),
                                        (false, false) => snippet,
                                    };
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(activity).small().color(theme::COMMENT()),
                                        )
                                        .selectable(false),
                                    );
                                });
                            });
                        });
                    })
                    .response;
                if row.contains_pointer() {
                    ui.painter().rect_stroke(
                        row.rect,
                        9,
                        (1.0, theme::tint(44)),
                        egui::StrokeKind::Inside,
                    );
                    if !on_button && ui.input(|i| i.pointer.primary_clicked()) {
                        select = Some(index);
                    }
                }
            }
        });
        // The avatars of working agents pulse, which needs frames.
        if working > 0 {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(120));
        }

        if let Some(index) = remove {
            self.stop_agent(index);
            self.agents.remove(index);
            self.active_agent = self.active_agent.min(self.agents.len().saturating_sub(1));
        } else if let Some(index) = select {
            self.active_agent = index;
        }
        if stop_all {
            for index in 0..self.agents.len() {
                self.stop_agent(index);
            }
        }
        if add {
            let mode = self.agents.first().map_or(chat::Mode::Ask, |a| a.chat.mode);
            self.active_agent = self.new_agent("agent", "", mode);
        }
        if let Some(action) = team_action {
            self.team_action(action);
        }
    }

    fn agent_center(&mut self, ui: &mut Ui) {
        if self.agents.is_empty() {
            self.new_agent("lead", "", settings::chat_mode());
        }
        let index = self.active_agent.min(self.agents.len() - 1);
        self.active_agent = index;
        if self.team_header(ui) {
            self.team_view(ui);
            return;
        }
        // The chat reads best as a column, not stretched across a wide window.
        let width = ui.available_width().min(860.0);
        let pad = ((ui.available_width() - width) / 2.0).max(0.0) as i8;
        let action = Frame::NONE
            .inner_margin(Margin {
                left: pad.max(12),
                right: pad.max(12),
                top: 10,
                bottom: 10,
            })
            .show(ui, |ui| self.agents[index].chat.show(ui, None, false))
            .inner;
        match action {
            Some(chat::Action::Send(text)) => self.agent_user_send(index, text),
            Some(chat::Action::Stop) => self.stop_agent(index),
            Some(chat::Action::NewChat) => {
                self.stop_agent(index);
                self.agents[index].chat.reset();
            }
            Some(chat::Action::Restart) => {
                if !self.agents[index].chat.busy {
                    self.stop_agent(index);
                }
            }
            Some(chat::Action::Login) => {
                self.view = View::Code;
                self.new_term();
                if let Some(term) = self.terms.last() {
                    let term = term.id;
                    self.request(Request::TermInput {
                        term,
                        data: b"claude /login\r".to_vec(),
                    });
                }
            }
            Some(chat::Action::FindFiles(query)) => {
                let uid = self.agents[index].uid;
                self.find_files(query, Some(uid));
            }
            Some(chat::Action::Wake) => {
                if self.online() {
                    let agent = &mut self.agents[index];
                    ensure_proc(
                        &mut self.link,
                        &self.root,
                        &mut agent.chat,
                        &mut agent.started_with,
                    );
                }
            }
            Some(chat::Action::Detach(_) | chat::Action::Switched) | None => {}
        }
    }

    // ---- ui ----

    fn shortcuts(&mut self, ctx: &Context) {
        let pressed = |m: Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(m, key));
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::P) {
            self.open_palette(false);
        }
        if pressed(Modifiers::COMMAND, Key::Comma) {
            self.view = if self.view == View::Settings {
                View::Code
            } else {
                View::Settings
            };
        }
        if pressed(Modifiers::COMMAND, Key::Backtick) {
            self.show_term = !(self.show_term && self.term_focused);
            if let Some(term) = self.terms.get_mut(self.active_term) {
                term.request_focus = self.show_term;
            }
            if !self.show_term {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.view.request_focus = true;
                }
            }
        }
        // The rest belong to the shell while the terminal has the keyboard (Ctrl+W, Ctrl+F…).
        if self.term_focused {
            return;
        }
        if self.online() && pressed(Modifiers::COMMAND, Key::P) {
            self.open_palette(true);
        }
        let active = self.docked_active();
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::O) {
            self.open_palette_with("@");
        }
        if pressed(Modifiers::SHIFT | Modifiers::ALT, Key::F) {
            if let Some(index) = active {
                self.format_document(index);
            }
        }
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::F) {
            self.toggle_search();
        } else if pressed(Modifiers::COMMAND, Key::F) {
            if let Some(index) = active {
                let selected = self.tabs[index].buf.selected_text();
                let seed = if selected.is_empty() || selected.contains('\n') {
                    self.find.take().unwrap_or_default()
                } else {
                    selected
                };
                self.find = Some(seed);
                self.focus_find = true;
            }
        }
        if pressed(Modifiers::COMMAND, Key::S) {
            if let Some(index) = active {
                self.save_tab(index);
            }
        }
        if pressed(Modifiers::COMMAND, Key::W) {
            if let Some(index) = active {
                self.close_tab(index, false);
            }
        }
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        title_drag(ui);
        // What is shown depends on the room there is. Whatever is dropped on a narrow
        // window stays reachable through the command palette.
        let width = ui.available_width();
        let (labels, brand, folder, toggles) = (
            width >= 1150.0,
            width >= 820.0,
            width >= 700.0,
            width >= 540.0,
        );
        let right_width = 3.0 * 36.0 + 20.0 + if toggles { 4.0 * 32.0 } else { 32.0 };

        ui.horizontal(|ui| {
            // The left part gets exactly what the right part leaves, and is clipped to it,
            // so the two can never run into each other.
            let left = vec2(
                (ui.available_width() - right_width).max(60.0),
                ui.spacing().interact_size.y + 6.0,
            );
            ui.allocate_ui_with_layout(left, Layout::left_to_right(Align::Center), |ui| {
                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                if brand {
                    ui.add(
                        egui::Label::new(RichText::new("ride").strong().color(theme::COMMENT()))
                            .selectable(false),
                    );
                    ui.add_space(6.0);
                }
                let views_width = if labels { 4.0 * 98.0 } else { 4.0 * 38.0 };
                match &self.link {
                    Link::Offline => {
                        ui.label(
                            RichText::new(icons::REMOTE)
                                .size(16.0)
                                .color(theme::COMMENT()),
                        );
                        let room = (ui.available_width() - views_width - 150.0).clamp(70.0, 200.0);
                        let edit = ui.add(
                            TextEdit::singleline(&mut self.host)
                                .desired_width(room)
                                .hint_text("host or user@host"),
                        );
                        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if !self.hosts.is_empty() {
                            ui.menu_button(RichText::new(icons::CHEVRON_DOWN), |ui| {
                                for host in &self.hosts {
                                    if menu_item(ui, icons::REMOTE, host) {
                                        self.host = host.clone();
                                    }
                                }
                            });
                        }
                        let text = if labels {
                            format!("{}  Connect", icons::PLUG)
                        } else {
                            icons::PLUG.to_string()
                        };
                        let connect = egui::Button::new(RichText::new(text).color(Color32::WHITE))
                            .fill(theme::ACCENT().gamma_multiply(0.55));
                        if ui.add(connect).on_hover_text("Connect").clicked() || enter {
                            self.connect(ui.ctx());
                        }
                    }
                    Link::Connecting { status, .. } => {
                        ui.spinner();
                        let text = format!("{}: {}", self.host, status.lock().unwrap());
                        ui.add(egui::Label::new(text).truncate());
                        if ui.button("Cancel").clicked() {
                            self.disconnect();
                        }
                    }
                    Link::Online(_) => {
                        let more = match self.sessions.len() {
                            0 => String::new(),
                            n => format!("  +{n}"),
                        };
                        let name = pieces(&[
                            (&format!("{}  ", icons::REMOTE), theme::STRING()),
                            (self.host.trim(), theme::TEXT()),
                            (&more, theme::COMMENT()),
                        ]);
                        let chip = egui::Button::new(name)
                            .fill(theme::STRING().gamma_multiply(0.16))
                            .corner_radius(9);
                        let hosts_before = self.sessions.len();
                        egui::containers::menu::MenuButton::from_button(chip)
                            .ui(ui, |ui| self.sessions_menu(ui));
                        // The menu may have closed this connection or shown another.
                        if !self.online() || self.sessions.len() != hosts_before {
                            return;
                        }
                        if icon_button(ui, icons::DISCONNECT, "Disconnect") {
                            self.close_session();
                            return;
                        }
                        if folder {
                            ui.add_space(6.0);
                            ui.label(RichText::new(icons::FOLDER_ROOT).color(theme::COMMENT()));
                            // The folder box takes what the view buttons after it don't need.
                            let room =
                                (ui.available_width() - views_width - 20.0).clamp(60.0, 380.0);
                            let edit = ui.add(
                                TextEdit::singleline(&mut self.root_input).desired_width(room),
                            );
                            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                self.open_root(self.root_input.trim().to_string());
                            }
                        }
                    }
                }
                ui.add_space(8.0);
                let views = [
                    (View::Code, icons::CODE, "Code", true),
                    (View::Agents, icons::AGENTS, "Agents", self.online()),
                    (View::Themes, icons::PALETTE, "Themes", true),
                    (View::Settings, icons::GEAR, "Settings", true),
                ];
                for (view, icon, label, available) in views {
                    let text = if labels {
                        format!("{icon}  {label}")
                    } else {
                        icon.to_string()
                    };
                    let button = egui::Button::selectable(self.view == view, text);
                    if available && ui.add(button).on_hover_text(label).clicked() {
                        self.view = view;
                    }
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                window_buttons(ui);
                ui.add_space(10.0);
                ui.spacing_mut().item_spacing.x = 6.0;
                if icon_button(ui, icons::SEARCH, "Command palette (Ctrl+Shift+P)") {
                    self.open_palette(false);
                }
                if toggles {
                    if icon_toggle(ui, self.show_chat, icons::SPARKLE, "Claude chat") {
                        self.show_chat = !self.show_chat;
                    }
                    if icon_toggle(ui, self.show_term, icons::TERMINAL, "Terminal (Ctrl+`)") {
                        self.show_term = !self.show_term;
                    }
                    if icon_toggle(ui, self.show_side, icons::FILES, "Files and search") {
                        self.show_side = !self.show_side;
                    }
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let mut cancel = None;
        let mut dismiss = None;
        ui.horizontal(|ui| {
            for (index, download) in self.downloads.iter().enumerate() {
                match &download.outcome {
                    None => {
                        ui.spinner();
                        ui.label(
                            RichText::new(format!(
                                "{} — {}",
                                download.name,
                                human_size(download.bytes)
                            ))
                            .small(),
                        );
                        if icon_button(ui, icons::CLOSE, "Cancel download") {
                            cancel = Some(download.id);
                        }
                    }
                    Some(outcome) => {
                        let (icon, text, color) = match outcome {
                            Ok(text) => (icons::CHECK, text.clone(), theme::STRING()),
                            Err(e) => (
                                icons::ERROR,
                                format!("Download of {} failed: {e}", download.name),
                                theme::ANSI()[1],
                            ),
                        };
                        ui.label(
                            RichText::new(format!("{icon}  {text}"))
                                .small()
                                .color(color),
                        );
                        if icon_button(ui, icons::CLOSE, "Dismiss") {
                            dismiss = Some(index);
                        }
                    }
                }
                ui.separator();
            }
            let room = (ui.available_width() - 110.0).max(0.0);
            ui.allocate_ui_with_layout(
                vec2(room, ui.available_height()),
                Layout::left_to_right(Align::Center),
                |ui| {
                    let status = RichText::new(&self.status).small().color(theme::COMMENT());
                    ui.add(egui::Label::new(status).truncate());
                },
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(version) = self.updater.ready().map(str::to_string) {
                    let text = format!("{}  Restart to update to {version}", icons::REFRESH);
                    let label = RichText::new(text).small().color(theme::ACCENT());
                    let label = egui::Label::new(label).sense(Sense::click());
                    let hint =
                        "Otherwise it installs when you close ride. Open tabs and chats come back.";
                    if ui.add(label).on_hover_text(hint).clicked() && self.updater.install(true) {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    ui.add_space(8.0);
                }
                if let Some(index) = self.docked_active() {
                    let c = self.tabs[index].buf.cursor;
                    ui.label(
                        RichText::new(format!("Ln {}, Col {}", c.line + 1, c.col + 1))
                            .small()
                            .color(theme::COMMENT()),
                    );
                    if let Some((line, message)) = self.tabs[index].view.diags.first().cloned() {
                        let text = format!("{}  line {}", icons::ERROR, line + 1);
                        let label = RichText::new(text).small().color(theme::ANSI()[1]);
                        let label = egui::Label::new(label).sense(Sense::click());
                        if ui.add(label).on_hover_text(message).clicked() {
                            let tab = &mut self.tabs[index];
                            tab.buf.set_cursor(Pos::new(line, 0), false);
                            tab.view.reveal_cursor = true;
                        }
                    }
                }
            });
        });
        if let Some(id) = cancel {
            self.request(Request::Cancel { target: id });
        }
        if let Some(index) = dismiss {
            self.downloads.remove(index);
        }
    }

    fn side_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if icon_toggle(ui, self.side == Side::Files, icons::FILES, "Files") {
                self.side = Side::Files;
            }
            if icon_toggle(
                ui,
                self.side == Side::Changes,
                icons::DIFF,
                "What Claude changed",
            ) {
                self.side = Side::Changes;
            }
            if icon_toggle(ui, self.side == Side::Git, icons::SOURCE_CONTROL, "Git") {
                self.side = Side::Git;
                self.git_refresh();
            }
            if icon_toggle(
                ui,
                self.side == Side::Server,
                icons::PULSE,
                "Server monitor",
            ) {
                self.side = Side::Server;
            }
            if icon_toggle(
                ui,
                self.side == Side::Outline,
                icons::METHOD,
                "Outline: functions and types in this file (Ctrl+Shift+O)",
            ) {
                self.side = Side::Outline;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_toggle(
                    ui,
                    self.show_search,
                    icons::SEARCH,
                    "Search in folder (Ctrl+Shift+F)",
                ) {
                    self.toggle_search();
                }
            });
        });
        ui.add_space(2.0);
        ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);
        match self.side {
            Side::Files => {
                if self.show_search {
                    self.search_section(ui);
                }
                if !self.sessions.is_empty() && !self.hide_beside {
                    let height = ui.available_height();
                    Panel::bottom("other_tree")
                        .resizable(true)
                        .default_size(height * 0.45)
                        .size_range(80.0..=(height - 80.0).max(80.0))
                        .show_separator_line(true)
                        .frame(Frame::NONE.inner_margin(Margin::symmetric(0, 6)))
                        .show(ui, |ui| self.other_tree(ui));
                }
                self.file_tree(ui);
                self.tree_drag(ui);
            }
            Side::Changes => self.changes_panel(ui),
            Side::Git => self.git_panel(ui),
            Side::Server => self.monitor_panel(ui),
            Side::Outline => self.outline_panel(ui),
        }
    }

    /// Shows or hides the folder search above the file tree.
    fn toggle_search(&mut self) {
        self.show_search = !self.show_search;
        if self.show_search {
            self.view = View::Code;
            self.side = Side::Files;
            self.show_side = true;
            self.focus_search = true;
        }
    }

    fn push_rows(&self, dir: &str, depth: usize, rows: &mut Vec<TreeRow>) {
        let note = |text: String| TreeRow {
            depth,
            path: String::new(),
            name: text,
            is_dir: false,
            note: true,
        };
        match self.tree.get(dir) {
            Some(DirState::Loaded(entries)) => {
                for entry in entries {
                    let path = join(dir, &entry.name);
                    let open = entry.is_dir && self.expanded.contains(&path);
                    rows.push(TreeRow {
                        depth,
                        path: path.clone(),
                        name: entry.name.clone(),
                        is_dir: entry.is_dir,
                        note: false,
                    });
                    if open {
                        self.push_rows(&path, depth + 1, rows);
                    }
                }
            }
            Some(DirState::Loading) => rows.push(note("loading…".into())),
            Some(DirState::Failed(error)) => rows.push(note(error.clone())),
            None => {}
        }
    }

    fn file_tree(&mut self, ui: &mut Ui) {
        let mut action = None;
        let root = self.root.clone();

        let header = ui
            .horizontal(|ui| {
                ui.label(
                    RichText::new(file_name(&root).to_uppercase())
                        .small()
                        .strong()
                        .color(theme::COMMENT()),
                )
                .on_hover_text(&root);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, icons::REFRESH, "Refresh") {
                        action = Some(TreeAction::Refresh(root.clone()));
                    }
                    if icon_button(ui, icons::NEW_FOLDER, "New folder") {
                        let prompt = PromptAction::NewDir(root.clone());
                        action = Some(TreeAction::Prompt("New folder", String::new(), prompt));
                    }
                    if icon_button(ui, icons::NEW_FILE, "New file") {
                        let prompt = PromptAction::NewFile(root.clone());
                        action = Some(TreeAction::Prompt("New file", String::new(), prompt));
                    }
                });
            })
            .response;
        header.context_menu(|ui| dir_menu(ui, &root, &mut action));

        let mut rows = Vec::new();
        self.push_rows(&root, 0, &mut rows);
        // With another connection open, rows can be dragged to its tree and take drops from it.
        let several = !self.sessions.is_empty();
        let here = self.session_uid;
        let mut targets = Vec::new();
        let mut dragged = None;
        let active_path = self.docked_active().map(|i| self.tabs[i].path.as_str());
        let row_height = ui.text_style_height(&egui::TextStyle::Button) + 4.0;
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.spacing_mut().button_padding = vec2(6.0, 2.0);

        let tree_area = ScrollArea::both().auto_shrink(false).show_rows(
            ui,
            row_height,
            rows.len(),
            |ui, range| {
                for row in &rows[range] {
                    ui.horizontal(|ui| {
                        ui.add_space(row.depth as f32 * 14.0);
                        if row.note {
                            ui.weak(&row.name);
                            return;
                        }
                        let open = self.expanded.contains(&row.path);
                        let chevron = if open {
                            icons::CHEVRON_DOWN
                        } else {
                            icons::CHEVRON_RIGHT
                        };
                        // Files get an invisible chevron, so their icons line up with the folders'.
                        let chevron_color = if row.is_dir {
                            theme::COMMENT()
                        } else {
                            Color32::TRANSPARENT
                        };
                        let (icon, icon_color) = icons::for_entry(&row.name, row.is_dir, open);
                        // Changed and new files stand out by the colour of their name.
                        let name_color = self.git_color(&row.path, row.is_dir);
                        let label = pieces(&[
                            (&format!("{chevron} "), chevron_color),
                            (&format!("{icon}  "), icon_color),
                            (&row.name, name_color),
                        ]);
                        let selected = active_path == Some(row.path.as_str());
                        // An installed icon theme supplies an image; otherwise the glyph in `label`.
                        let resp = match self.entry_icon(ui.ctx(), &row.name, row.is_dir, open) {
                            Some(texture) => {
                                let size = vec2(16.0, 16.0);
                                let image =
                                    egui::Image::new(egui::load::SizedTexture::new(texture, size));
                                let chevron = pieces(&[(&chevron.to_string(), chevron_color)]);
                                let name = pieces(&[(&row.name, name_color)]);
                                ui.add(egui::Button::selectable(selected, (chevron, image, name)))
                            }
                            None => ui.selectable_label(selected, label),
                        };
                        // The row itself takes the drag: a separate widget on top would eat clicks.
                        let resp = if several {
                            resp.interact(Sense::click_and_drag())
                        } else {
                            resp
                        };
                        if resp.clicked() {
                            action = Some(if row.is_dir {
                                TreeAction::Toggle(row.path.clone())
                            } else {
                                TreeAction::Open(row.path.clone())
                            });
                        }
                        if several {
                            let folder = if row.is_dir {
                                row.path.clone()
                            } else {
                                parent(&row.path).to_string()
                            };
                            targets.push((resp.rect, here, folder));
                            if resp.drag_started() {
                                dragged = Some(sessions::DragFile {
                                    from: here,
                                    path: row.path.clone(),
                                    is_dir: row.is_dir,
                                });
                            }
                        }
                        resp.context_menu(|ui| {
                            if row.is_dir {
                                dir_menu(ui, &row.path, &mut action);
                                ui.separator();
                            } else if menu_item(ui, icons::DOWNLOAD, "Download…") {
                                action = Some(TreeAction::Download(row.path.clone(), false));
                            }
                            if menu_item(ui, icons::SPARKLE, "Add to chat") {
                                action = Some(TreeAction::AddToChat(row.path.clone()));
                            }
                            if menu_item(ui, icons::EDIT, "Rename…") {
                                let prompt = PromptAction::Rename(row.path.clone());
                                action =
                                    Some(TreeAction::Prompt("Rename", row.name.clone(), prompt));
                            }
                            if menu_item(ui, icons::TRASH, "Delete…") {
                                action = Some(TreeAction::Delete(row.path.clone()));
                            }
                            if menu_item(ui, icons::COPY, "Copy path") {
                                ui.ctx().copy_text(row.path.clone());
                            }
                        });
                    });
                }
            },
        );
        if several {
            // Let go over the empty part of the tree, a file lands in the open folder.
            targets.push((tree_area.inner_rect, here, root.clone()));
            self.drop_targets.extend(targets);
            if dragged.is_some() {
                self.drag_file = dragged;
            }
        }

        match action {
            Some(TreeAction::Toggle(path)) => {
                if !self.expanded.remove(&path) {
                    self.expanded.insert(path.clone());
                    self.load_dir(&path);
                }
            }
            Some(TreeAction::Open(path)) => self.open_file(path, None),
            Some(TreeAction::Download(path, is_dir)) => self.start_download(path, is_dir),
            Some(TreeAction::Prompt(title, value, action)) => {
                self.dialog = Some(Dialog::Prompt {
                    title: title.into(),
                    value,
                    action,
                });
            }
            Some(TreeAction::Delete(path)) if settings::with(|s| s.confirm_delete) => {
                self.dialog = Some(Dialog::ConfirmDelete { path });
            }
            Some(TreeAction::Delete(path)) => {
                let refresh = parent(&path).to_string();
                self.request_then(Request::Delete { path }, Pending::FsOp { refresh });
            }
            Some(TreeAction::Refresh(path)) => self.load_dir(&path),
            Some(TreeAction::AddToChat(path)) => self.add_path_to_chat(&path),
            Some(TreeAction::Upload(dir)) => self.pick_and_upload(dir),
            None => {}
        }
    }

    /// The folder search above the file tree: the query box, then its results.
    fn search_section(&mut self, ui: &mut Ui) {
        fn chip(ui: &mut Ui, on: &mut bool, text: &str, hint: &str) -> bool {
            let button = egui::Button::selectable(*on, RichText::new(text).small());
            let clicked = ui.add(button).on_hover_text(hint).clicked();
            if clicked {
                *on = !*on;
            }
            clicked
        }

        let mut run = false;
        Frame::NONE
            .fill(theme::tint(8))
            .stroke((1.0, theme::LINE()))
            .corner_radius(8)
            .inner_margin(Margin {
                left: 8,
                right: 4,
                top: 3,
                bottom: 3,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.label(RichText::new(icons::SEARCH).color(theme::COMMENT()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let sparkle = icons::SPARKLE.to_string();
                        chip(
                            ui,
                            &mut self.search.ai,
                            &sparkle,
                            "Describe what you are looking for and let Claude find it",
                        );
                        run |= chip(ui, &mut self.search.regex, ".*", "Regular expression");
                        run |= chip(ui, &mut self.search.case_sensitive, "Aa", "Match case");
                        let edit = ui.add(
                            TextEdit::singleline(&mut self.search.query)
                                .desired_width(f32::INFINITY)
                                .frame(Frame::NONE)
                                .hint_text("Search in folder"),
                        );
                        if std::mem::take(&mut self.focus_search) {
                            edit.request_focus();
                        }
                        run |= edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    });
                });
            });
        if run && self.search.query.trim().is_empty() {
            self.search.hits.clear();
        } else if run && self.search.ai {
            self.ai_search();
        } else if run {
            self.run_search();
        }

        let busy = self.search.id.is_some() || self.search.ai_busy;
        if !busy && self.search.hits.is_empty() {
            ui.add_space(4.0);
            return;
        }
        let mut clear = false;
        ui.horizontal(|ui| {
            if busy {
                ui.spinner();
            }
            ui.label(
                RichText::new(format!("{} results", self.search.hits.len()))
                    .small()
                    .color(theme::COMMENT()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                clear = icon_button(ui, icons::CLOSE, "Clear the results");
            });
        });

        // Hits arrive grouped per file; a header row goes wherever the file changes.
        let hits = &self.search.hits;
        let mut rows = Vec::with_capacity(hits.len());
        for (i, hit) in hits.iter().enumerate() {
            if i == 0 || hits[i - 1].path != hit.path {
                rows.push((i, true));
            }
            rows.push((i, false));
        }
        let prefix = format!("{}/", self.root.trim_end_matches('/'));
        let row_height = ui.text_style_height(&egui::TextStyle::Button) + 4.0;
        // The results get up to half the panel; the tree keeps the rest.
        let max_height = (ui.available_height() * 0.5).max(120.0);
        let mut open = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.spacing_mut().button_padding = vec2(6.0, 2.0);
            ScrollArea::both()
                .id_salt("search_results")
                .max_height(max_height)
                .auto_shrink([false, true])
                .show_rows(ui, row_height, rows.len(), |ui, range| {
                    for &(i, is_header) in &rows[range] {
                        let hit = &hits[i];
                        if is_header {
                            let shown = hit.path.strip_prefix(&prefix).unwrap_or(&hit.path);
                            let (icon, color) =
                                icons::for_entry(file_name(&hit.path), false, false);
                            ui.horizontal(|ui| {
                                ui.set_min_height(row_height);
                                ui.label(pieces(&[
                                    (&format!("{icon}  "), color),
                                    (shown, theme::TEXT()),
                                ]));
                            });
                        } else {
                            let label = pieces(&[
                                (&format!("    {:>4}  ", hit.line + 1), theme::LINE_NUMBER()),
                                (hit.text.trim_start(), theme::MUTED()),
                            ]);
                            if ui.selectable_label(false, label).clicked() {
                                open = Some((
                                    hit.path.clone(),
                                    Pos::new(hit.line as usize, hit.col as usize),
                                ));
                            }
                        }
                    }
                });
        });
        ui.separator();
        if let Some((path, pos)) = open {
            self.open_file(path, Some(pos));
        }
        if clear {
            if let Some(id) = self.search.id.take() {
                self.request(Request::Cancel { target: id });
            }
            self.search.hits.clear();
        }
    }

    fn term_panel(&mut self, ui: &mut Ui) {
        let mut close = None;
        Frame::NONE
            .inner_margin(Margin {
                left: 8,
                right: 8,
                top: 6,
                bottom: 2,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (index, term) in self.terms.iter().enumerate() {
                        let title = if term.title.is_empty() {
                            format!("Terminal {}", index + 1)
                        } else {
                            term.title.chars().take(32).collect()
                        };
                        let label = pieces(&[
                            (&format!("{}  ", icons::TERMINAL), theme::COMMENT()),
                            (&title, theme::TEXT()),
                        ]);
                        if ui
                            .selectable_label(index == self.active_term, label)
                            .clicked()
                        {
                            self.active_term = index;
                        }
                        if icon_button(ui, icons::CLOSE, "Close terminal") {
                            close = Some(index);
                        }
                    }
                    if icon_button(ui, icons::ADD, "New terminal") {
                        self.new_term();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.menu_button(RichText::new(icons::DOCK).size(13.0), |ui| {
                            for (dock, name) in TermDock::ALL {
                                let label = format!("Dock at the {name}");
                                if ui.selectable_label(self.term_dock == dock, label).clicked() {
                                    self.term_dock = dock;
                                    ui.close();
                                }
                            }
                        })
                        .response
                        .on_hover_text("Move the terminal (or drag this bar to an edge)");
                    });
                });
                // The bar itself is the handle: drag it toward an edge of the editor area.
                let bar = ui.min_rect();
                let drag = ui.interact(bar, ui.id().with("terminal_drag"), Sense::drag());
                let pointer = ui.input(|i| i.pointer.interact_pos());
                if let (true, Some(pointer)) = (drag.dragged() || drag.drag_stopped(), pointer) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    if let Some((dock, zone)) = TermDock::zone_at(self.work_area, pointer) {
                        let layer = egui::LayerId::new(
                            egui::Order::Foreground,
                            egui::Id::new("terminal_drop"),
                        );
                        let painter = ui.ctx().layer_painter(layer);
                        painter.rect(
                            zone.shrink(4.0),
                            8,
                            theme::ACCENT().gamma_multiply(0.18),
                            (1.5, theme::ACCENT()),
                            egui::StrokeKind::Inside,
                        );
                        if drag.drag_stopped() {
                            self.term_dock = dock;
                        }
                    }
                }
            });
        if let Some(index) = close {
            let id = self.terms.remove(index).id;
            self.request(Request::TermClose { term: id });
            self.active_term = self.active_term.min(self.terms.len().saturating_sub(1));
        }
        if self.terms.is_empty() {
            self.new_term();
        }
        let Some(term) = self.terms.get_mut(self.active_term) else {
            return;
        };
        let id = term.id;
        let out = Frame::NONE
            .inner_margin(Margin {
                left: 6,
                right: 6,
                top: 0,
                bottom: 6,
            })
            .show(ui, |ui| term.show(ui))
            .inner;
        self.term_focused = out.focused;
        if let Some((cols, rows)) = out.resize {
            self.request(Request::TermResize {
                term: id,
                cols,
                rows,
            });
        }
        if !out.input.is_empty() {
            self.request(Request::TermInput {
                term: id,
                data: out.input,
            });
        }
    }

    fn tab_bar(&mut self, ui: &mut Ui, pane: u32) {
        let mut close = None;
        let shown = self.pane_tab(pane);
        let mut split = None;
        Frame::NONE
            .inner_margin(Margin {
                left: 8,
                right: 8,
                top: 6,
                bottom: 4,
            })
            .show(ui, |ui| {
                ScrollArea::horizontal().id_salt("tabs").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for index in 0..self.tabs.len() {
                            let tab = &self.tabs[index];
                            if tab.detached || tab.pane != pane {
                                continue;
                            }
                            let uid = tab.uid;
                            let name = file_name(&tab.path);
                            let (icon, color) = icons::for_entry(name, false, false);
                            let dirty = if tab.buf.is_dirty() {
                                format!("  {}", icons::DOT)
                            } else {
                                String::new()
                            };
                            let label = pieces(&[
                                (&format!("{icon}  "), color),
                                (name, theme::TEXT()),
                                (&dirty, theme::ACCENT()),
                            ]);
                            // Draggable, so a tab can be pulled out of the window into its own.
                            let button = match self.entry_icon(ui.ctx(), name, false, false) {
                                Some(texture) => {
                                    let size = vec2(16.0, 16.0);
                                    let image = egui::Image::new(egui::load::SizedTexture::new(
                                        texture, size,
                                    ));
                                    let text =
                                        pieces(&[(name, theme::TEXT()), (&dirty, theme::ACCENT())]);
                                    egui::Button::selectable(Some(index) == shown, (image, text))
                                }
                                None => egui::Button::selectable(Some(index) == shown, label),
                            }
                            .sense(Sense::click_and_drag());
                            let resp = ui.add(button).on_hover_text(format!(
                                "{}\nDrag to an edge of the editor to split it, or out of the window to open it in its own",
                                tab.path
                            ));
                            if resp.clicked() {
                                self.active = index;
                                self.panes.shown.insert(pane, uid);
                            }
                            let mut detach = None;
                            if resp.dragged() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                // Nothing reaches the window once the cursor is outside it, so keep polling.
                                ui.ctx().request_repaint();
                                self.panes.drag = Some(uid);
                            }
                            if resp.drag_stopped() {
                                self.panes.dropped = true;
                            }
                            if resp.dragged() || resp.drag_stopped() {
                                detach = dragged_outside(ui.ctx());
                            }
                            resp.context_menu(|ui| {
                                if menu_item(ui, icons::DOCK, "Split right") {
                                    split = Some((index, panes::Zone::Right));
                                }
                                if menu_item(ui, icons::DOCK, "Split down") {
                                    split = Some((index, panes::Zone::Below));
                                }
                                if menu_item(ui, icons::POP_OUT, "Open in its own window") {
                                    detach = Some(Pos2::new(160.0, 120.0));
                                }
                            });
                            if let Some(pos) = detach {
                                let tab = &mut self.tabs[index];
                                tab.detached = true;
                                tab.place_at = Some(pos);
                                tab.float = Float::default();
                            }
                            if resp.middle_clicked()
                                || icon_button(ui, icons::CLOSE, "Close (Ctrl+W)")
                            {
                                close = Some(index);
                            }
                        }
                    });
                });
            });
        if let Some((index, zone)) = split {
            self.split_tab(index, zone);
        }
        if let Some(index) = close {
            self.close_tab(index, false);
        }
    }

    /// One pane of the editor area: its tabs and the file of the one that is shown. The
    /// find bar and the inline-edit bar belong to the pane that has the focus.
    fn editor_pane(&mut self, ui: &mut Ui, pane: u32, focused: bool) {
        if self.tabs.iter().any(|t| !t.detached && t.pane == pane) {
            self.tab_bar(ui, pane);
        }
        let Some(active) = self.pane_tab(pane) else {
            ui.centered_and_justified(|ui| {
                let hint = if self.online() {
                    "Open a file from the tree\n\nCtrl+S save · Ctrl+F find · Ctrl+Shift+F search in folder · Ctrl+` terminal"
                } else {
                    "Connect to a host to start"
                };
                ui.label(RichText::new(hint).color(theme::COMMENT()));
            });
            return;
        };
        if focused {
            self.active = active;
        }

        if focused && self.find.is_some() {
            let mut step = None;
            let mut done = false;
            let query = self.find.clone().unwrap_or_default();
            let matches = count_matches(&self.tabs[active].buf, &query);
            Frame::NONE
                .inner_margin(Margin {
                    left: 10,
                    right: 12,
                    top: 4,
                    bottom: 2,
                })
                .show(ui, |ui| {
                    // A compact box at the right, like the find widget of most editors.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        Frame::NONE
                            .fill(theme::tint(10))
                            .stroke((1.0, theme::tint(34)))
                            .corner_radius(9)
                            .inner_margin(Margin {
                                left: 10,
                                right: 4,
                                top: 2,
                                bottom: 2,
                            })
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                // Laid out right to left, so the first widget ends up rightmost.
                                done = icon_button(ui, icons::CLOSE, "Close (Esc)");
                                if icon_button(ui, icons::ARROW_DOWN, "Next (Enter)") {
                                    step = Some((true, false));
                                }
                                if icon_button(ui, icons::ARROW_UP, "Previous (Shift+Enter)") {
                                    step = Some((false, false));
                                }
                                let (text, color) = match (query.is_empty(), matches) {
                                    (true, _) => (String::new(), theme::COMMENT()),
                                    (false, 0) => ("No results".to_string(), theme::ANSI()[1]),
                                    (false, 1) => ("1 match".to_string(), theme::COMMENT()),
                                    (false, n) => (format!("{n} matches"), theme::COMMENT()),
                                };
                                ui.add_space(4.0);
                                ui.label(RichText::new(text).small().color(color));
                                ui.add_space(4.0);
                                let edit = ui.add(
                                    TextEdit::singleline(self.find.as_mut().unwrap())
                                        .desired_width(
                                            (ui.available_width() - 150.0).clamp(60.0, 230.0),
                                        )
                                        .frame(Frame::NONE)
                                        .hint_text("Find in file"),
                                );
                                ui.label(RichText::new(icons::SEARCH).color(theme::COMMENT()));
                                if std::mem::take(&mut self.focus_find) {
                                    edit.request_focus();
                                }
                                if edit.changed() {
                                    step = Some((true, true));
                                }
                                let (enter, shift, escape) = ui.input(|i| {
                                    (
                                        i.key_pressed(Key::Enter),
                                        i.modifiers.shift,
                                        i.key_pressed(Key::Escape),
                                    )
                                });
                                if edit.lost_focus() && enter {
                                    step = Some((!shift, false));
                                    edit.request_focus();
                                }
                                done |= escape;
                            });
                    });
                });
            if let Some((forward, incremental)) = step {
                self.find_next(forward, incremental);
            }
            if done {
                self.find = None;
                self.tabs[active].view.request_focus = true;
            }
        }

        if focused {
            self.inline_bar(ui, active);
        }
        let root = self.root.clone();
        let tab = &mut self.tabs[active];
        let action = Frame::NONE
            .inner_margin(Margin {
                left: 4,
                right: 4,
                top: 0,
                bottom: 6,
            })
            .show(ui, |ui| tab_body(ui, tab, &root))
            .inner;
        if let Some(action) = action {
            self.editor_action(active, action);
        }
    }

    /// Tabs and the chat that were pulled out of the main window, each in its own OS window.
    fn detached_windows(&mut self, ctx: &Context) {
        let main = ctx.input(|i| i.viewport().outer_rect);
        let mut any = false;

        for index in 0..self.tabs.len() {
            if !self.tabs[index].detached {
                continue;
            }
            any = true;
            let root = self.root.clone();
            let tab = &mut self.tabs[index];
            let name = file_name(&tab.path).to_string();
            let mut builder = ViewportBuilder::default()
                .with_title(format!("{name} — ride"))
                .with_inner_size([900.0, 640.0])
                .with_min_inner_size([320.0, 200.0])
                .with_decorations(false);
            if let Some(pos) = tab.place_at.take() {
                builder = builder.with_position(pos);
            }
            let id = ViewportId::from_hash_of(("tab", tab.uid));
            let (mut save, mut dock) = (false, false);
            let mut editor_action = None;
            ctx.show_viewport_immediate(id, builder, |ui, _| {
                let ctx = ui.ctx().clone();
                save = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S));
                dock = ctx.input(|i| i.viewport().close_requested())
                    || tab.float.dropped_on_main(&ctx, main);

                let (icon, color) = icons::for_entry(&name, false, false);
                let dirty = if tab.buf.is_dirty() {
                    format!("  {}", icons::DOT)
                } else {
                    String::new()
                };
                dock |= float_title_bar(
                    ui,
                    pieces(&[
                        (&format!("{icon}  "), color),
                        (&tab.path, theme::TEXT()),
                        (&dirty, theme::ACCENT()),
                    ]),
                );
                CentralPanel::default_margins()
                    .frame(Frame::NONE.fill(theme::EDITOR_BG()).inner_margin(Margin {
                        left: 4,
                        right: 4,
                        top: 2,
                        bottom: 4,
                    }))
                    .show(ui, |ui| editor_action = tab_body(ui, tab, &root));
                resize_edges(&ctx);
            });
            if save {
                self.save_tab(index);
            }
            if let Some(action) = editor_action {
                self.editor_action(index, action);
            }
            if dock {
                self.tabs[index].detached = false;
                self.active = index;
            }
        }

        if self.chat_detached && self.online() && self.show_chat {
            any = true;
            let level = if self.chat.pinned {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            };
            let mut builder = ViewportBuilder::default()
                .with_title("Claude — ride")
                .with_inner_size([440.0, 680.0])
                .with_min_inner_size([340.0, 300.0])
                .with_decorations(false)
                .with_window_level(level);
            if let Some(pos) = self.chat_place_at.take() {
                builder = builder.with_position(pos);
            }
            ctx.show_viewport_immediate(ViewportId::from_hash_of("chat"), builder, |ui, _| {
                let ctx = ui.ctx().clone();
                let mut dock = ctx.input(|i| i.viewport().close_requested())
                    || self.chat_float.dropped_on_main(&ctx, main);
                dock |= float_title_bar(
                    ui,
                    pieces(&[
                        (&format!("{}  ", icons::SPARKLE), theme::CLAUDE()),
                        ("Claude", theme::TEXT()),
                    ]),
                );
                CentralPanel::default_margins()
                    .frame(
                        Frame::NONE
                            .fill(theme::PANEL_BG())
                            .inner_margin(Margin::same(10)),
                    )
                    .show(ui, |ui| self.chat_ui(ui, true));
                resize_edges(&ctx);
                if dock {
                    self.chat_detached = false;
                }
            });
        }

        // Keeps noticing a detached window being dropped back, even while nothing else changes.
        if any {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn dialogs(&mut self, ctx: &Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let title = match &dialog {
            Dialog::Prompt { title, .. } => title.clone(),
            Dialog::ConfirmDelete { .. } => "Delete".into(),
            Dialog::ConfirmClose(_) => "Unsaved changes".into(),
            Dialog::Text { title, .. } => title.clone(),
            Dialog::Schedule { .. } => "Scheduled task".into(),
        };
        let (mut confirm, mut cancel) = (false, false);
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let enter = ui.input(|i| i.key_pressed(Key::Enter));
                cancel = ui.input(|i| i.key_pressed(Key::Escape));
                let confirm_label = match &mut dialog {
                    Dialog::Prompt { value, .. } => {
                        let edit = ui.add(TextEdit::singleline(value).desired_width(320.0));
                        if !edit.has_focus() && !edit.lost_focus() {
                            edit.request_focus();
                        }
                        confirm = edit.lost_focus() && enter;
                        "OK"
                    }
                    Dialog::ConfirmDelete { path } => {
                        ui.label(format!(
                            "Delete {path} from the server?\nThis cannot be undone."
                        ));
                        "Delete"
                    }
                    Dialog::ConfirmClose(index) => {
                        let name = self.tabs.get(*index).map_or("", |t| file_name(&t.path));
                        ui.label(format!("{name} has unsaved changes."));
                        "Close without saving"
                    }
                    Dialog::Text { text, .. } => {
                        ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                            ui.add(TextEdit::multiline(text).desired_width(480.0));
                        });
                        "Copy"
                    }
                    Dialog::Schedule { prompt, minutes } => {
                        ui.label("What should Claude do each time?");
                        ui.add(
                            TextEdit::multiline(prompt)
                                .desired_width(440.0)
                                .desired_rows(3),
                        );
                        ui.horizontal(|ui| {
                            ui.label("Every");
                            ui.add(TextEdit::singleline(minutes).desired_width(48.0));
                            ui.label("minutes, in read-only (Ask) mode");
                        });
                        "Schedule"
                    }
                };
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    confirm |= ui.button(confirm_label).clicked();
                    cancel |= ui.button("Cancel").clicked();
                });
            });

        if cancel && !confirm {
            return;
        }
        if !confirm {
            self.dialog = Some(dialog);
            return;
        }
        match dialog {
            Dialog::Prompt { value, action, .. } => {
                let name = value.trim();
                if name.is_empty() || name.contains('/') {
                    self.status = "Invalid name".into();
                    return;
                }
                match action {
                    PromptAction::NewFile(dir) => {
                        self.expanded.insert(dir.clone());
                        let path = join(&dir, name);
                        self.request_then(
                            Request::CreateFile { path: path.clone() },
                            Pending::Created { path, dir },
                        );
                    }
                    PromptAction::NewDir(dir) => {
                        self.expanded.insert(dir.clone());
                        self.request_then(
                            Request::CreateDir {
                                path: join(&dir, name),
                            },
                            Pending::FsOp { refresh: dir },
                        );
                    }
                    PromptAction::Rename(path) => {
                        let dir = parent(&path).to_string();
                        let to = join(&dir, name);
                        if let Some(tab) = self.tabs.iter_mut().find(|t| t.path == path) {
                            tab.path = to.clone();
                        }
                        self.request_then(
                            Request::Rename { from: path, to },
                            Pending::FsOp { refresh: dir },
                        );
                    }
                    PromptAction::SaveTeam => self.save_team(name),
                }
            }
            Dialog::ConfirmDelete { path } => {
                let refresh = parent(&path).to_string();
                self.request_then(Request::Delete { path }, Pending::FsOp { refresh });
            }
            Dialog::ConfirmClose(index) => {
                if index < self.tabs.len() {
                    self.close_tab(index, true);
                }
            }
            Dialog::Text { text, .. } => ctx.copy_text(text),
            Dialog::Schedule { prompt, minutes } => {
                let now = ctx.input(|i| i.time);
                self.add_scheduled(prompt, &minutes, now);
            }
        }
    }
}

/// A tab's content: the editor for text, a preview for everything else.
fn tab_body(ui: &mut Ui, tab: &mut Tab, root: &str) -> Option<editor::Action> {
    if tab.preview.is_none() && settings::with(|s| s.breadcrumbs) {
        nav::breadcrumbs(ui, tab, root);
    }
    match &mut tab.preview {
        Some(preview) => {
            preview::show(ui, preview, file_name(&tab.path));
            None
        }
        None => {
            let lang = highlight::lang_for(&tab.path);
            editor::show(ui, &mut tab.buf, &mut tab.view, lang)
        }
    }
}

fn dir_menu(ui: &mut Ui, dir: &str, action: &mut Option<TreeAction>) {
    if menu_item(ui, icons::NEW_FILE, "New file…") {
        *action = Some(TreeAction::Prompt(
            "New file",
            String::new(),
            PromptAction::NewFile(dir.into()),
        ));
    }
    if menu_item(ui, icons::NEW_FOLDER, "New folder…") {
        *action = Some(TreeAction::Prompt(
            "New folder",
            String::new(),
            PromptAction::NewDir(dir.into()),
        ));
    }
    if menu_item(ui, icons::DOWNLOAD, "Download folder…") {
        *action = Some(TreeAction::Download(dir.into(), true));
    }
    if menu_item(ui, icons::ARROW_UP, "Upload files here…") {
        *action = Some(TreeAction::Upload(dir.into()));
    }
    if menu_item(ui, icons::REFRESH, "Refresh") {
        *action = Some(TreeAction::Refresh(dir.into()));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.shortcuts(&ctx);
        self.sync_glass(frame);
        self.apply_settings(&ctx);
        self.autosave_tick(&ctx);
        // The start screen has the window to itself until it is over.
        if self.splash.is_some() && !settings::with(|s| s.splash) {
            // No start screen: the window still opens small, so its frame is measured here
            // for the sizes set from now on, and it goes straight to its real size.
            let over = ctx.content_rect().size() - vec2(splash::SIZE, splash::SIZE);
            if over.max_elem() > 2.0 && over.max_elem() < 80.0 {
                self.frame_extra = over.max(egui::Vec2::ZERO);
            }
            self.end_splash(&ctx);
        }
        if let Some(started) = self.splash {
            let now = ctx.input(|i| i.time);
            let started = if started < 0.0 { now } else { started };
            self.splash = Some(started);
            // RIDE_SPLASH_HOLD=SECONDS freezes the start screen at that moment, to look at it.
            let held = std::env::var("RIDE_SPLASH_HOLD")
                .ok()
                .and_then(|v| v.parse().ok());
            let elapsed = held.unwrap_or((now - started) as f32);
            // An older version left a window state in the settings, which is restored
            // before any of this runs. For the first moments the window is put back to
            // the small square in the middle of the display.
            if now - started < 0.7 {
                use egui::ViewportCommand as Cmd;
                let side = vec2(splash::SIZE, splash::SIZE);
                let monitor = ctx
                    .input(|i| i.viewport().monitor_size)
                    .unwrap_or(vec2(1920.0, 1080.0));
                // Once the window is down to about the size asked for, what it has too much
                // of is the hidden frame; measured once, it is left out of every size set.
                let over = ctx.content_rect().size() - side + self.frame_extra;
                if self.frame_extra == egui::Vec2::ZERO
                    && over.max_elem() > 2.0
                    && over.max_elem() < 80.0
                {
                    self.frame_extra = over.max(egui::Vec2::ZERO);
                }
                ctx.send_viewport_cmd(Cmd::Maximized(false));
                ctx.send_viewport_cmd(Cmd::InnerSize(side - self.frame_extra));
                ctx.send_viewport_cmd(Cmd::OuterPosition(((monitor - side) / 2.0).to_pos2()));
            }
            if elapsed < splash::DURATION {
                splash::show(ui, elapsed);
                ctx.request_repaint();
                return;
            }
            self.end_splash(&ctx);
        }
        self.remember_window(&ctx);
        if let Some(line) = self.updater.tick(&ctx) {
            self.status = line;
        }
        // Installs started from the command palette finish outside the Themes view.
        self.poll_themes(&ctx);
        // An update found as ride opened: restart into it now. The tabs and chats are
        // saved on the way out and come back.
        if std::mem::take(&mut self.updater.apply_now) && self.updater.install(true) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // After an update, what changed comes before anything else.
        if !self.whats_new.is_empty() {
            self.whats_new_screen(ui);
            return;
        }
        // The catch-up screen covers everything until it is read or skipped.
        if self.catching_up() {
            self.catchup_screen(ui);
            return;
        }
        // With glass on, the panes' own see-through fills are all that is painted.
        if theme::glass_opacity().is_none() {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, theme::WINDOW_BG());
        }

        let online = self.online();
        let agents_view = online && self.view == View::Agents;
        // The editor comes first. A side panel shows only while the window is wide enough
        // to leave it room; the user's choice is kept for when it is widened again.
        let size = ctx.content_rect().size();
        let side = online && self.show_side && !agents_view && size.x >= 560.0;
        let chat_room = if side { 940.0 } else { 660.0 };
        let chat =
            online && self.show_chat && !self.chat_detached && !agents_view && size.x >= chat_room;
        Panel::top("top")
            .frame(Frame::NONE.fill(theme::chrome()).inner_margin(Margin {
                left: 12,
                right: 0,
                top: 3,
                bottom: 3,
            }))
            .show_separator_line(true)
            .show(ui, |ui| self.top_bar(ui));
        Panel::bottom("status")
            .frame(
                Frame::NONE
                    .fill(theme::chrome())
                    .inner_margin(Margin::symmetric(12, 3)),
            )
            .show_separator_line(true)
            .show(ui, |ui| self.status_bar(ui));
        if self.view == View::Themes || self.view == View::Settings {
            if self.view == View::Themes {
                self.themes_view(ui);
            } else {
                self.settings_view(ui);
            }
            self.term_focused = false;
            self.detached_windows(&ctx);
            self.dialogs(&ctx);
            self.agents_widget(&ctx);
            self.palette_window(&ctx);
            resize_edges(&ctx);
            return;
        }
        if agents_view && size.x >= 660.0 {
            Panel::left("agents")
                .resizable(true)
                .default_size(300.0)
                .size_range(200.0..=(size.x * 0.45).max(200.0))
                .show_separator_line(true)
                .frame(theme::card(theme::glass()))
                .show(ui, |ui| self.agents_panel(ui));
        }
        if side {
            Panel::left("side")
                .resizable(true)
                .default_size(280.0)
                .size_range(160.0..=(size.x * 0.45).max(160.0))
                .show_separator_line(true)
                .frame(theme::card(theme::glass()))
                .show(ui, |ui| self.side_panel(ui));
        }
        if chat {
            Panel::right("chat")
                .resizable(true)
                .default_size(400.0)
                .size_range(300.0..=(size.x * 0.5).max(300.0))
                .show_separator_line(true)
                .frame(theme::card(theme::glass()).inner_margin(Margin::same(10)))
                .show(ui, |ui| self.chat_ui(ui, false));
        }
        if online && self.show_term && !agents_view {
            // Each edge is its own panel, so each remembers its own size.
            // A terminal beside the editor needs width. When what the other panels leave
            // isn't enough for both, it goes underneath; otherwise the editor keeps its part.
            let room = ui.available_rect_before_wrap().width();
            let dock = match self.term_dock {
                TermDock::Left | TermDock::Right if room < 640.0 => TermDock::Bottom,
                dock => dock,
            };
            let (tall, wide) = ((size.y * 0.7).max(120.0), (room - 320.0).max(120.0));
            let panel = match dock {
                TermDock::Bottom => Panel::bottom("terminal")
                    .default_size(280.0)
                    .size_range(100.0..=tall),
                TermDock::Top => Panel::top("terminal_top")
                    .default_size(280.0)
                    .size_range(100.0..=tall),
                TermDock::Left => Panel::left("terminal_left")
                    .default_size(520.0)
                    .size_range(120.0..=wide),
                TermDock::Right => Panel::right("terminal_right")
                    .default_size(520.0)
                    .size_range(120.0..=wide),
            };
            // What is left for the editor and the terminal together, before the terminal takes its part.
            self.work_area = ui.available_rect_before_wrap();
            panel
                .resizable(true)
                .show_separator_line(true)
                .frame(theme::card(theme::surface()).inner_margin(Margin::ZERO))
                .show(ui, |ui| self.term_panel(ui));
        } else {
            self.term_focused = false;
        }
        CentralPanel::default_margins()
            .frame(theme::card(theme::surface()).inner_margin(Margin::ZERO))
            .show(ui, |ui| {
                if agents_view {
                    self.agent_center(ui);
                } else {
                    self.editor_area(ui);
                }
            });

        self.detached_windows(&ctx);
        self.dialogs(&ctx);
        if !agents_view {
            self.agents_widget(&ctx);
        }
        self.palette_window(&ctx);
        resize_edges(&ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Fully clear under glass, so the backdrop shows through the see-through panes.
        if theme::glass_opacity().is_some() {
            [0.0; 4]
        } else {
            theme::WINDOW_BG().to_normalized_gamma_f32()
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some((size, pos)) = self.window {
            for (key, value) in [
                ("win_w", size[0]),
                ("win_h", size[1]),
                ("win_x", pos[0]),
                ("win_y", pos[1]),
            ] {
                storage.set_string(key, value.to_string());
            }
        }
        storage.set_string("win_max", self.maximized.to_string());
        // The window state an older version stored; ride keeps its own now (above).
        storage.remove_string("window");
        storage.set_string("host", self.host.trim().to_string());
        storage.set_string("root", self.root.clone());
        storage.set_string("show_chat", self.show_chat.to_string());
        storage.set_string("theme", self.themes.current.clone());
        storage.set_string("glass", self.themes.glass.to_string());
        storage.set_string("icon_set", self.icon_sets.current.clone());
        storage.set_string("term_dock", self.term_dock.name().to_string());
        storage.set_string("connected", self.online().to_string());
        storage.set_string("catchup_day", self.catchup_day.to_string());
        // Seen once it has been closed; until then it shows again at the next start.
        if self.whats_new.is_empty() {
            storage.set_string("seen_version", update::VERSION.to_string());
        }
        storage.set_string("glass_opacity", self.themes.opacity.to_string());
        self.write_state();
        let dir = self
            .download_dir
            .as_ref()
            .map(|d| d.to_string_lossy().into_owned());
        storage.set_string("download_dir", dir.unwrap_or_default());
    }

    fn on_exit(&mut self) {
        // A downloaded update goes in as ride closes; the setup waits for it to be gone.
        if settings::with(|s| s.auto_update) {
            self.updater.install(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directives_are_split_from_the_reply() {
        let reply = "I'll split this up.\n\n@@spawn name=Docs-Writer model=haiku :: Summarise README.md\nin three bullets.\n@@send lead :: done with part one";
        let (body, directives) = parse_directives(reply);
        assert_eq!(body, "I'll split this up.");
        assert_eq!(directives.len(), 2);
        assert!(directives[0].spawn);
        assert_eq!(directives[0].name, "docs-writer");
        assert_eq!(model_alias(&directives[0].model), "haiku");
        assert_eq!(directives[0].text, "Summarise README.md\nin three bullets.");
        assert!(!directives[1].spawn);
        assert_eq!(directives[1].name, "lead");
        assert_eq!(directives[1].text, "done with part one");
    }

    #[test]
    fn a_reply_without_directives_is_all_body() {
        let (body, directives) = parse_directives("All done.\nNothing to delegate.");
        assert_eq!(body, "All done.\nNothing to delegate.");
        assert!(directives.is_empty());
    }
}
