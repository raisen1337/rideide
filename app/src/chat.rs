//! The Claude chat panel. It drives Claude Code on the remote host in its headless
//! stream-json mode, so it uses the login and settings Claude Code already has there.

use crate::{diff, icons, theme};
use eframe::egui::{
    self, Align, Color32, FontId, Frame, Key, Layout, Margin, Modifiers, Pos2, RichText,
    ScrollArea, Sense, Stroke, TextEdit, TextWrapMode, Ui,
};
use serde_json::{json, Value};

/// Claude Code is started through PowerShell on this computer, not a shell on a remote host.
static LOCAL_SHELL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_local_shell(local: bool) {
    LOCAL_SHELL.store(local, std::sync::atomic::Ordering::Relaxed);
}

pub fn local_shell() -> bool {
    LOCAL_SHELL.load(std::sync::atomic::Ordering::Relaxed)
}

/// The MCP servers every Claude is started with, as `--mcp-config` takes them.
static MCP_CONFIG: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_mcp_config(config: Option<String>) {
    if let Ok(mut current) = MCP_CONFIG.lock() {
        *current = config;
    }
}

fn mcp_config() -> Option<String> {
    MCP_CONFIG.lock().ok().and_then(|c| c.clone())
}

/// Standard base64, as MCP servers send pictures.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = text
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, b) in chunk.iter().enumerate() {
            n |= value(*b)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    /// Reads and answers; asks the user before it changes a file or runs a command.
    Ask,
    /// May edit files without asking; still asks before running commands.
    Edit,
    /// Claude Code's auto mode: edits, plus commands it judges safe.
    Auto,
}

impl Mode {
    fn flag(self) -> &'static str {
        match self {
            Mode::Ask => "default",
            Mode::Edit => "acceptEdits",
            Mode::Auto => "auto",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::Ask => "Ask",
            Mode::Edit => "Edit",
            Mode::Auto => "Auto",
        }
    }
}

enum Item {
    User(String),
    /// A message from another agent of the team.
    From {
        who: String,
        text: String,
    },
    Text(String),
    Tool {
        name: String,
        detail: String,
    },
    Error(String),
    /// An image sent along with the user's message that follows it.
    Image {
        name: String,
        /// Missing once the chat has been saved and loaded again.
        texture: Option<egui::TextureHandle>,
    },
    /// Claude Code's id for the user message just before it. Files can be put back to how
    /// they were when that message was sent.
    Checkpoint(String),
    /// Something ride itself has to say, such as files having been restored.
    Note(String),
    /// A subagent Claude started (its Task/Agent tool): what kind, what for, and the id
    /// its own messages carry.
    Agent {
        id: String,
        kind: String,
        task: String,
    },
}

/// What a subagent did, as it reports it: its messages and tools, and whether it's done.
#[derive(Default)]
struct Sub {
    items: Vec<Item>,
    done: bool,
}

/// Claude Code's tools that start a subagent.
fn starts_agent(tool: &str) -> bool {
    matches!(tool, "Task" | "Agent")
}

/// Something Claude wants to do and waits to be allowed.
struct Ask {
    id: String,
    tool: String,
    input: Value,
}

/// An image that goes out with the next message.
struct Attached {
    name: String,
    png: Vec<u8>,
    texture: egui::TextureHandle,
}

/// A message sent while Claude was still at work on the one before.
struct Queued {
    text: String,
    images: Vec<(String, egui::TextureHandle)>,
}

/// Claude looks at images at about this size at most; anything larger only costs more.
const IMAGE_SIDE: u32 = 1568;
const IMAGE_LIMIT: usize = 6;

fn edits_files(tool: &str) -> bool {
    matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

pub(crate) fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub enum Action {
    Send(String),
    Stop,
    NewChat,
    Login,
    /// The mode changed; the running process has to be restarted to pick it up.
    Restart,
    /// Pop the chat out into its own window, at this screen position if known.
    Detach(Option<Pos2>),
    /// Another conversation was opened from the history; the running process is stale.
    Switched,
    /// The user is typing an `@` mention; look up files matching this text.
    FindFiles(String),
    /// Start the process if it isn't running: something is waiting to be sent to it.
    Wake,
    /// The history was opened: list the conversations Claude Code had in a terminal.
    LoadHistory,
}

pub struct Chat {
    /// Id of the running `claude` process on the remote host.
    pub proc: Option<u64>,
    session: Option<String>,
    items: Vec<Item>,
    input: String,
    /// Bytes of a stdout line that hasn't ended yet.
    partial: Vec<u8>,
    stderr: String,
    pub busy: bool,
    pub mode: Mode,
    /// Model alias passed to Claude Code; empty for its default.
    pub model_choice: &'static str,
    /// Effort level passed to Claude Code; empty for its default.
    pub effort: &'static str,
    model: String,
    needs_login: bool,
    /// Tell Claude which file is open in the editor.
    pub attach: bool,
    /// The last item is a `Text` still receiving deltas.
    streaming: bool,
    /// Text of the current assistant message already arrived as deltas.
    got_delta: bool,
    /// Keep the detached chat window above other windows.
    pub pinned: bool,
    /// Name shown in the header.
    pub title: String,
    /// One of the agents in the Agents view, rather than the side chat.
    pub team_member: bool,
    /// Tools it may use without asking (Claude Code's `--allowedTools`), such as an MCP
    /// server's, by name.
    pub allowed_tools: String,
    /// Extra system prompt for Claude Code.
    pub system: Option<String>,
    /// Final text of the last finished turn, until someone takes it.
    last_result: Option<String>,
    /// Earlier conversations, newest first.
    history: Vec<Saved>,
    show_history: bool,
    /// Subagents' conversations, by the id of the tool call that started each.
    subs: std::collections::HashMap<String, Sub>,
    /// The subagent conversations open, innermost last; empty for the chat itself.
    viewing: Vec<String>,
    /// Conversations Claude Code had in this folder outside ride (in a terminal), newest
    /// first, with when each was last written; `None` until the host has listed them.
    cli: Option<Vec<(Saved, u64)>>,
    /// File edits Claude made here that succeeded, oldest first.
    pub changes: Vec<Change>,
    /// Edits Claude asked for whose outcome isn't known yet, by tool-use id.
    pending_changes: Vec<(String, Change)>,
    /// Cost of finished processes, and of the running one as last reported, in USD.
    past_cost: f64,
    proc_cost: f64,
    pub turns: u32,
    /// Files offered for the `@` mention being typed, and the mention they answer.
    suggestions: Vec<String>,
    asked: String,
    /// What Claude is waiting to be allowed to do.
    asks: Vec<Ask>,
    /// Lines for the process that aren't user messages: answers to its requests, and ours.
    outbox: Vec<Vec<u8>>,
    /// The user chose to stop being asked about file edits in this conversation.
    allow_edits: bool,
    /// A request that just arrived, until the app has told the user about it.
    new_ask: Option<String>,
    images: Vec<Attached>,
    /// Messages sent while Claude was busy, in order: Claude Code takes each in once it's
    /// done with what it is doing, and they join the transcript then.
    queued: Vec<Queued>,
    /// The next replayed user message is the one just sent, and names its checkpoint.
    expect_checkpoint: bool,
    /// The user message whose restore button was pressed and waits to be confirmed.
    confirm_rewind: Option<usize>,
    rewinding: bool,
    /// Files were put back; the app should read them again.
    rewound: bool,
    paste_down: bool,
}

/// One edit Claude made to a file, as it described it to its editing tool.
#[derive(Clone)]
pub struct Change {
    pub path: String,
    pub old: String,
    pub new: String,
    /// The file was written whole, so `old` is unknown rather than empty.
    pub whole_file: bool,
}

fn changes_from_tool(name: &str, input: &Value) -> Vec<Change> {
    let path = input["file_path"].as_str().unwrap_or("").to_string();
    let text = |v: &Value| v.as_str().unwrap_or("").to_string();
    match name {
        "Edit" => vec![Change {
            path,
            old: text(&input["old_string"]),
            new: text(&input["new_string"]),
            whole_file: false,
        }],
        "MultiEdit" => input["edits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|edit| Change {
                path: path.clone(),
                old: text(&edit["old_string"]),
                new: text(&edit["new_string"]),
                whole_file: false,
            })
            .collect(),
        "Write" => vec![Change {
            path,
            old: String::new(),
            new: text(&input["content"]),
            whole_file: true,
        }],
        _ => Vec::new(),
    }
}

/// A conversation kept in the history. Its Claude Code session can be resumed.
struct Saved {
    session: Option<String>,
    title: String,
    items: Vec<Item>,
}

const HISTORY_LIMIT: usize = 40;

fn item_to_json(item: &Item) -> Value {
    match item {
        Item::User(text) => json!({ "k": "user", "a": text }),
        Item::From { who, text } => json!({ "k": "from", "a": who, "b": text }),
        Item::Text(text) => json!({ "k": "text", "a": text }),
        Item::Tool { name, detail } => json!({ "k": "tool", "a": name, "b": detail }),
        Item::Error(text) => json!({ "k": "error", "a": text }),
        Item::Image { name, .. } => json!({ "k": "image", "a": name }),
        Item::Checkpoint(id) => json!({ "k": "cp", "a": id }),
        Item::Note(text) => json!({ "k": "note", "a": text }),
        Item::Agent { id, kind, task } => json!({ "k": "agent", "a": task, "b": kind, "c": id }),
    }
}

fn item_from_json(v: &Value) -> Option<Item> {
    let a = v["a"].as_str()?.to_string();
    let b = v["b"].as_str().unwrap_or("").to_string();
    Some(match v["k"].as_str()? {
        "user" => Item::User(a),
        "from" => Item::From { who: a, text: b },
        "text" => Item::Text(a),
        "tool" => Item::Tool { name: a, detail: b },
        "error" => Item::Error(a),
        "image" => Item::Image {
            name: a,
            texture: None,
        },
        "cp" => Item::Checkpoint(a),
        "note" => Item::Note(a),
        "agent" => Item::Agent {
            id: v["c"].as_str().unwrap_or("").to_string(),
            kind: b,
            task: a,
        },
        _ => return None,
    })
}

fn first_line(text: &str, max: usize) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(max)
        .collect()
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            proc: None,
            session: None,
            items: Vec::new(),
            input: String::new(),
            partial: Vec::new(),
            stderr: String::new(),
            busy: false,
            mode: Mode::Ask,
            model_choice: "",
            effort: "",
            model: String::new(),
            needs_login: false,
            attach: true,
            streaming: false,
            got_delta: false,
            pinned: false,
            title: "Claude".into(),
            team_member: false,
            allowed_tools: String::new(),
            system: None,
            last_result: None,
            history: Vec::new(),
            show_history: false,
            subs: Default::default(),
            viewing: Vec::new(),
            cli: None,
            changes: Vec::new(),
            pending_changes: Vec::new(),
            past_cost: 0.0,
            proc_cost: 0.0,
            turns: 0,
            suggestions: Vec::new(),
            asked: String::new(),
            asks: Vec::new(),
            outbox: Vec::new(),
            allow_edits: false,
            new_ask: None,
            images: Vec::new(),
            queued: Vec::new(),
            expect_checkpoint: false,
            confirm_rewind: None,
            rewinding: false,
            rewound: false,
            paste_down: false,
        }
    }
}

fn looks_like_login_problem(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "/login",
        "invalid api key",
        "not logged in",
        "authentication_error",
        "oauth token",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn tool_detail(name: &str, input: &Value) -> String {
    let key = match name {
        "Bash" => "command",
        "Grep" | "Glob" => "pattern",
        "WebFetch" => "url",
        "WebSearch" => "query",
        "Task" | "Agent" => "description",
        _ => "file_path",
    };
    let text = input[key].as_str().unwrap_or("");
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

fn tool_icon(name: &str) -> char {
    match name {
        "Read" => icons::EYE,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => icons::EDIT,
        "Bash" => icons::TERMINAL,
        "Grep" | "Glob" => icons::SEARCH,
        "WebFetch" | "WebSearch" => icons::GLOBE,
        "TodoWrite" | "Task" | "Agent" => icons::TASKS,
        _ => icons::TOOLS,
    }
}

impl Chat {
    /// How the process is started, apart from the session. A change means a restart.
    pub fn config(&self) -> String {
        // Claude Code keeps what files looked like before each message only when asked to.
        let checkpoints = if local_shell() {
            "$env:CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING='true'; "
        } else {
            "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true exec "
        };
        // `--permission-prompt-tool stdio` makes it ask here before doing what its mode
        // doesn't allow outright; `--replay-user-messages` tells the id of each message.
        let mut cmd = format!(
            "{checkpoints}claude -p --input-format stream-json --output-format stream-json \
             --verbose --include-partial-messages --replay-user-messages \
             --permission-prompt-tool stdio --permission-mode {}",
            self.mode.flag()
        );
        if !self.model_choice.is_empty() {
            cmd.push_str(&format!(" --model {}", self.model_choice));
        }
        if !self.effort.is_empty() {
            cmd.push_str(&format!(" --effort {}", self.effort));
        }
        // The MCP servers set in Settings, and the tools that need no asking.
        if let Some(config) = mcp_config() {
            let quoted = if LOCAL_SHELL.load(std::sync::atomic::Ordering::Relaxed) {
                config.replace('\'', "''")
            } else {
                config.replace('\'', "'\\''")
            };
            cmd.push_str(&format!(" --mcp-config '{quoted}'"));
        }
        if !self.allowed_tools.is_empty() {
            cmd.push_str(&format!(" --allowedTools {}", self.allowed_tools));
        }
        if let Some(system) = &self.system {
            // Single-quoted for the shell that will run it. A POSIX shell closes and reopens
            // the quotes around an embedded one; PowerShell, used on this computer, doubles it.
            let quoted = if LOCAL_SHELL.load(std::sync::atomic::Ordering::Relaxed) {
                system.replace('\'', "''")
            } else {
                system.replace('\'', "'\\''")
            };
            cmd.push_str(&format!(" --append-system-prompt '{quoted}'"));
        }
        cmd
    }

    /// The shell command that starts Claude Code for this conversation.
    pub fn command(&self) -> String {
        let mut cmd = self.config();
        if let Some(session) = &self.session {
            // The id goes into a shell command line, so nothing but id characters may pass.
            let id: String = session
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            cmd.push_str(&format!(" --resume {id}"));
        }
        cmd
    }

    /// Records the user's turn and returns the line to write to the process.
    /// `context` is extra information for Claude that isn't shown in the transcript.
    pub fn begin_turn(&mut self, text: &str, context: Option<String>) -> Vec<u8> {
        // Sent while Claude is still at it: it goes out now and Claude Code takes it in
        // as soon as it's done with what it is doing; until then it shows as waiting.
        let queue = self.busy;
        let mut pictures = Vec::new();
        let mut shown = Vec::new();
        for image in std::mem::take(&mut self.images) {
            pictures.push(json!({
                "type": "image",
                "source": { "type": "base64", "media_type": "image/png", "data": base64(&image.png) },
            }));
            shown.push((image.name, image.texture));
        }
        if queue {
            self.queued.push(Queued {
                text: text.to_string(),
                images: shown,
            });
        } else {
            for (name, texture) in shown {
                self.items.push(Item::Image {
                    name,
                    texture: Some(texture),
                });
            }
            self.items.push(Item::User(text.to_string()));
            self.expect_checkpoint = true;
            self.confirm_rewind = None;
            self.busy = true;
            self.needs_login = false;
            self.streaming = false;
            self.stderr.clear();
        }
        let text = match context {
            Some(context) => format!("{context}\n\n{text}"),
            None => text.to_string(),
        };
        // A picture can go alone, but a text block may not be empty.
        let mut content = Vec::new();
        if !text.trim().is_empty() || pictures.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
        content.extend(pictures);
        let msg = json!({
            "type": "user",
            "message": { "role": "user", "content": content },
        });
        format!("{msg}\n").into_bytes()
    }

    /// Like `begin_turn`, for a message that comes from a teammate instead of the user.
    pub fn begin_turn_from(&mut self, who: &str, text: &str, roster: &str) -> Vec<u8> {
        self.items.push(Item::From {
            who: who.to_string(),
            text: text.to_string(),
        });
        self.expect_checkpoint = false;
        self.busy = true;
        self.streaming = false;
        self.stderr.clear();
        let text = format!("[team: {roster}]\n[from {who}]\n{text}");
        let msg = json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
        });
        format!("{msg}\n").into_bytes()
    }

    /// What this conversation has cost so far, in USD, as reported by Claude Code.
    pub fn cost(&self) -> f64 {
        self.past_cost + self.proc_cost
    }

    /// Appends text to the message being written, e.g. a mention picked elsewhere in the UI.
    pub fn add_to_input(&mut self, text: &str) {
        if !self.input.is_empty() && !self.input.ends_with(char::is_whitespace) {
            self.input.push(' ');
        }
        self.input.push_str(text);
        self.input.push(' ');
    }

    /// Answers a `FindFiles` action. Ignored if the user has typed on since.
    pub fn set_suggestions(&mut self, query: &str, paths: Vec<String>) {
        if self.asked.strip_prefix('@') == Some(query) {
            self.suggestions = paths;
        }
    }

    /// The `@word` at the end of the input, if one is being typed.
    fn mention_in_progress(&self) -> Option<&str> {
        let word = self.input.rsplit(char::is_whitespace).next()?;
        word.starts_with('@').then_some(word)
    }

    fn complete_mention(&mut self, replacement: &str) {
        if let Some(word) = self.mention_in_progress() {
            let keep = self.input.len() - word.len();
            self.input.truncate(keep);
            self.input.push_str(&format!("@{replacement} "));
        }
        self.suggestions.clear();
        self.asked.clear();
    }

    /// Lines to write to the process, other than user messages.
    pub fn take_outbox(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outbox)
    }

    /// What Claude has just asked to be allowed to do, told once.
    pub fn take_new_ask(&mut self) -> Option<String> {
        self.new_ask.take()
    }

    /// True once after files were put back to a checkpoint.
    pub fn take_rewound(&mut self) -> bool {
        std::mem::take(&mut self.rewound)
    }

    fn answer(&mut self, id: &str, allow: Option<&Value>) {
        let response = match allow {
            Some(input) => json!({ "behavior": "allow", "updatedInput": input }),
            None => json!({ "behavior": "deny", "message": "The user declined this action." }),
        };
        let msg = json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": id, "response": response },
        });
        self.outbox.push(format!("{msg}\n").into_bytes());
    }

    /// Adds a picture to the message being written.
    fn attach_image(&mut self, ctx: &egui::Context, name: String, picture: image::DynamicImage) {
        if self.images.len() >= IMAGE_LIMIT {
            return;
        }
        let picture = if picture.width().max(picture.height()) > IMAGE_SIDE {
            picture.resize(
                IMAGE_SIDE,
                IMAGE_SIDE,
                image::imageops::FilterType::Triangle,
            )
        } else {
            picture
        };
        let rgba = picture.to_rgba8();
        let mut png = Vec::new();
        let written = image::DynamicImage::ImageRgba8(rgba.clone())
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
        if written.is_err() {
            return;
        }
        let size = [rgba.width() as usize, rgba.height() as usize];
        let pixels = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
        let texture = ctx.load_texture(
            format!("chat-image-{}", png.len()),
            pixels,
            egui::TextureOptions::LINEAR,
        );
        self.images.push(Attached { name, png, texture });
    }

    /// Attaches the picture on the clipboard, if there is one.
    fn paste_image(&mut self, ctx: &egui::Context) -> bool {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return false;
        };
        let Ok(data) = clipboard.get_image() else {
            return false;
        };
        let (w, h) = (data.width as u32, data.height as u32);
        let Some(pixels) = image::RgbaImage::from_raw(w, h, data.bytes.into_owned()) else {
            return false;
        };
        self.attach_image(
            ctx,
            "pasted image".into(),
            image::DynamicImage::ImageRgba8(pixels),
        );
        true
    }

    fn pick_images(&mut self, ctx: &egui::Context) {
        let picked = rfd::FileDialog::new()
            .set_title("Attach images")
            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
            .pick_files();
        for path in picked.into_iter().flatten() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match std::fs::read(&path).map(|bytes| image::load_from_memory(&bytes)) {
                Ok(Ok(picture)) => self.attach_image(ctx, name, picture),
                _ => self.note_error(&format!("{name} isn't an image ride can read")),
            }
        }
    }

    /// The final text of the turn that just finished.
    pub fn take_result(&mut self) -> Option<String> {
        self.last_result.take()
    }

    pub fn note_error(&mut self, text: &str) {
        self.items.push(Item::Error(text.to_string()));
    }

    /// A plain-text outline of the last `max` things said here, for summaries.
    pub fn digest(&self, max: usize) -> String {
        Self::outline(&self.items, max)
    }

    fn outline(items: &[Item], max: usize) -> String {
        let clip = |text: &str| -> String {
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if flat.chars().count() > 420 {
                format!("{}…", flat.chars().take(420).collect::<String>())
            } else {
                flat
            }
        };
        let skip = items.len().saturating_sub(max);
        items[skip..]
            .iter()
            .map(|item| match item {
                Item::User(text) => format!("user: {}", clip(text)),
                Item::From { who, text } => format!("{who} (another agent): {}", clip(text)),
                Item::Text(text) => format!("claude: {}", clip(text)),
                Item::Tool { name, detail } => format!("claude used {name} {}", clip(detail)),
                Item::Error(text) => format!("error: {}", clip(text)),
                Item::Image { name, .. } => format!("user attached an image: {name}"),
                Item::Note(text) => format!("note: {}", clip(text)),
                Item::Agent { kind, task, .. } => {
                    format!("claude started a {kind} agent: {}", clip(task))
                }
                Item::Checkpoint(_) => String::new(),
            })
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The same outline for each of the `max` most recent earlier conversations.
    pub fn history_digest(&self, max: usize) -> Vec<(String, String)> {
        self.history
            .iter()
            .take(max)
            .map(|saved| (saved.title.clone(), Self::outline(&saved.items, 4)))
            .collect()
    }

    /// The last thing here is a request nothing has answered: an interrupted or unstarted turn.
    pub fn awaiting_answer(&self) -> bool {
        matches!(self.last_item(), Some(Item::User(_) | Item::From { .. }))
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The last thing said; a checkpoint is only a mark on the message before it.
    fn last_item(&self) -> Option<&Item> {
        self.items
            .iter()
            .rev()
            .find(|item| !matches!(item, Item::Checkpoint(_)))
    }

    /// A one-line summary of the latest activity, for lists.
    pub fn snippet(&self) -> String {
        if let Some(ask) = self.asks.first() {
            let detail = tool_detail(&ask.tool, &ask.input);
            return format!(
                "Waiting for your OK: {} {}",
                ask.tool,
                first_line(&detail, 50)
            );
        }
        match self.last_item() {
            Some(Item::User(text) | Item::Text(text) | Item::Error(text) | Item::Note(text)) => {
                first_line(text, 80)
            }
            Some(Item::Image { name, .. }) => name.clone(),
            Some(Item::Checkpoint(_)) => String::new(),
            Some(Item::From { who, text }) => format!("{who}: {}", first_line(text, 70)),
            Some(Item::Tool { name, detail }) => format!("{name} {}", first_line(detail, 70)),
            Some(Item::Agent { task, .. }) => format!("Agent: {}", first_line(task, 70)),
            None => String::new(),
        }
    }

    /// Moves the current conversation into the history.
    fn archive(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let title = self
            .items
            .iter()
            .find_map(|item| match item {
                Item::User(text) => Some(first_line(text, 70)),
                _ => None,
            })
            .unwrap_or_else(|| "Chat".into());
        let saved = Saved {
            session: self.session.take(),
            title,
            items: std::mem::take(&mut self.items),
        };
        self.history.insert(0, saved);
        self.history.truncate(HISTORY_LIMIT);
    }

    /// Starts a fresh conversation; the current one goes to the history.
    pub fn reset(&mut self) {
        self.archive();
        let fresh = Self {
            mode: self.mode,
            attach: self.attach,
            pinned: self.pinned,
            model_choice: self.model_choice,
            effort: self.effort,
            title: std::mem::take(&mut self.title),
            team_member: self.team_member,
            system: self.system.take(),
            history: std::mem::take(&mut self.history),
            changes: std::mem::take(&mut self.changes),
            past_cost: self.past_cost + self.proc_cost,
            turns: self.turns,
            ..Self::default()
        };
        *self = fresh;
    }

    fn open_saved(&mut self, index: usize) {
        let saved = self.history.remove(index);
        self.archive();
        self.session = saved.session;
        self.items = saved.items;
        self.needs_login = false;
        self.streaming = false;
    }

    /// Everything worth keeping between runs: the open conversation, the history, the settings.
    pub fn to_json(&self) -> Value {
        let history: Vec<Value> = self
            .history
            .iter()
            .map(|s| {
                json!({
                    "session": s.session,
                    "title": s.title,
                    "items": s.items.iter().map(item_to_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        // Old edits matter less and less; the newest are enough for the Changes panel.
        let skip = self.changes.len().saturating_sub(100);
        let changes: Vec<Value> = self.changes[skip..]
            .iter()
            .map(|c| json!({ "path": c.path, "old": c.old, "new": c.new, "whole": c.whole_file }))
            .collect();
        json!({
            "session": self.session,
            "items": self.items.iter().map(item_to_json).collect::<Vec<_>>(),
            "history": history,
            "title": self.title,
            "team_member": self.team_member,
            "mode": self.mode.flag(),
            "model": self.model_choice,
            "effort": self.effort,
            "system": self.system,
            "attach": self.attach,
            "turns": self.turns,
            "cost": self.cost(),
            "changes": changes,
            "subs": self
                .subs
                .iter()
                .map(|(id, sub)| {
                    let items: Vec<Value> = sub.items.iter().map(item_to_json).collect();
                    (id.clone(), json!({ "items": items, "done": sub.done }))
                })
                .collect::<serde_json::Map<String, Value>>(),
        })
    }

    /// Rebuilds a chat saved by `to_json`. Its process isn't running; the next message
    /// resumes the Claude Code session.
    pub fn from_json(v: &Value) -> Self {
        let known = |options: &[&'static str], value: &Value| {
            options
                .iter()
                .copied()
                .find(|o| Some(*o) == value.as_str())
                .unwrap_or("")
        };
        let mut chat = Self {
            session: v["session"].as_str().map(str::to_string),
            items: v["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(item_from_json)
                .collect(),
            title: v["title"].as_str().unwrap_or("Claude").to_string(),
            team_member: v["team_member"].as_bool().unwrap_or(false),
            mode: match v["mode"].as_str() {
                Some("acceptEdits") => Mode::Edit,
                Some("auto") => Mode::Auto,
                _ => Mode::Ask,
            },
            model_choice: known(&["fable", "opus", "sonnet", "haiku"], &v["model"]),
            effort: known(&["low", "medium", "high", "xhigh", "max"], &v["effort"]),
            system: v["system"].as_str().map(str::to_string),
            attach: v["attach"].as_bool().unwrap_or(true),
            turns: v["turns"].as_u64().unwrap_or(0) as u32,
            past_cost: v["cost"].as_f64().unwrap_or(0.0),
            changes: v["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| Change {
                    path: c["path"].as_str().unwrap_or("").to_string(),
                    old: c["old"].as_str().unwrap_or("").to_string(),
                    new: c["new"].as_str().unwrap_or("").to_string(),
                    whole_file: c["whole"].as_bool().unwrap_or(false),
                })
                .collect(),
            ..Self::default()
        };
        chat.load_history(&v["history"].to_string());
        for (id, sub) in v["subs"].as_object().into_iter().flatten() {
            let items = sub["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(item_from_json)
                .collect();
            let done = sub["done"].as_bool().unwrap_or(true);
            chat.subs.insert(id.clone(), Sub { items, done });
        }
        chat
    }

    pub fn load_history(&mut self, text: &str) {
        let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(text) else {
            return;
        };
        self.history = entries
            .iter()
            .map(|e| Saved {
                session: e["session"].as_str().map(str::to_string),
                title: e["title"].as_str().unwrap_or("Chat").to_string(),
                items: e["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(item_from_json)
                    .collect(),
            })
            .filter(|saved| !saved.items.is_empty())
            .take(HISTORY_LIMIT)
            .collect();
    }

    /// Opens the first subagent's conversation, as its card does (RIDE_CHAT_AGENT).
    pub fn open_first_agent(&mut self) {
        let first = self.items.iter().find_map(|item| match item {
            Item::Agent { id, .. } => Some(id.clone()),
            _ => None,
        });
        self.viewing.extend(first);
    }

    /// Shows the history, as its button does; the caller asks the host for the terminal's.
    pub fn open_history(&mut self) {
        self.show_history = true;
        self.cli = None;
    }

    /// The conversations Claude Code had in this folder, as the host lists them.
    pub fn set_cli_history(&mut self, sessions: Vec<ride_proto::ClaudeSession>) {
        use ride_proto::ClaudeMessage;
        let list = sessions
            .into_iter()
            .map(|session| {
                let items = session
                    .messages
                    .into_iter()
                    .map(|message| match message {
                        ClaudeMessage::User(text) => Item::User(text),
                        ClaudeMessage::Claude(text) => Item::Text(text),
                        ClaudeMessage::Tool { name, detail } => Item::Tool { name, detail },
                    })
                    .collect();
                let saved = Saved {
                    session: Some(session.id),
                    title: session.title,
                    items,
                };
                (saved, session.modified)
            })
            .collect();
        self.cli = Some(list);
    }

    fn open_cli(&mut self, index: usize) {
        let Some(list) = &mut self.cli else {
            return;
        };
        let (saved, _) = list.remove(index);
        self.archive();
        self.session = saved.session;
        self.items = saved.items;
        self.needs_login = false;
        self.streaming = false;
    }

    /// A message from inside a subagent: added to that subagent's conversation.
    fn on_sub_message(&mut self, parent: String, v: &Value) {
        match v["type"].as_str().unwrap_or("") {
            "assistant" => {
                let mut items = Vec::new();
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str().unwrap_or("") {
                        "text" => {
                            let text = block["text"].as_str().unwrap_or("");
                            if !text.trim().is_empty() {
                                items.push(Item::Text(text.to_string()));
                            }
                        }
                        "tool_use" => {
                            let name = block["name"].as_str().unwrap_or("tool").to_string();
                            let id = block["id"].as_str().unwrap_or("").to_string();
                            if starts_agent(&name) {
                                items.push(agent_item(&id, &block["input"]));
                                self.subs.entry(id).or_default();
                                continue;
                            }
                            // Its edits count in the Changes panel too.
                            for change in changes_from_tool(&name, &block["input"]) {
                                self.pending_changes.push((id.clone(), change));
                            }
                            let detail = tool_detail(&name, &block["input"]);
                            items.push(Item::Tool { name, detail });
                        }
                        _ => {}
                    }
                }
                self.subs.entry(parent).or_default().items.extend(items);
            }
            "user" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] != "tool_result" {
                        continue;
                    }
                    let id = block["tool_use_id"].as_str().unwrap_or("");
                    if let Some(sub) = self.subs.get_mut(id) {
                        sub.done = true;
                    }
                    let failed = block["is_error"].as_bool().unwrap_or(false);
                    let (done, waiting): (Vec<_>, Vec<_>) =
                        std::mem::take(&mut self.pending_changes)
                            .into_iter()
                            .partition(|(i, _)| i == id);
                    self.pending_changes = waiting;
                    if !failed {
                        self.changes
                            .extend(done.into_iter().map(|(_, change)| change));
                    }
                }
            }
            _ => {}
        }
    }

    /// A subagent's conversation, opened from its card: what it said and the tools it used.
    fn sub_view(&mut self, ui: &mut Ui, id: &str) -> Option<Action> {
        let (kind, task) = self
            .items
            .iter()
            .chain(self.subs.values().flat_map(|s| s.items.iter()))
            .find_map(|item| match item {
                Item::Agent { id: i, kind, task } if i == id => Some((kind.clone(), task.clone())),
                _ => None,
            })
            .unwrap_or_default();
        let mut back = false;
        ui.horizontal(|ui| {
            if icon_button(ui, icons::ARROW_LEFT, "Back") {
                back = true;
            }
            ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
            ui.label(RichText::new(icons::AGENTS).color(theme::CLAUDE()));
            let kind = if kind.is_empty() {
                "Agent"
            } else {
                kind.as_str()
            };
            ui.label(RichText::new(kind).strong());
            ui.label(RichText::new(&task).color(theme::COMMENT()));
        });
        ui.add_space(4.0);
        let busy = self.busy;
        let mut open = None;
        let sub = self.subs.get(id);
        ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                let items = sub.map(|s| s.items.as_slice()).unwrap_or_default();
                for (index, item) in items.iter().enumerate() {
                    match item {
                        Item::Text(text) => markdown(ui, text, 1_000_000 + index),
                        Item::Tool { name, detail } => tool_chip(ui, name, detail),
                        Item::Agent { id, kind, task } => {
                            let inner = self.subs.get(id);
                            let running = busy && !inner.is_some_and(|s| s.done);
                            let steps = inner.map_or(0, |s| s.items.len());
                            if agent_card(ui, kind, task, steps, running) {
                                open = Some(id.clone());
                            }
                        }
                        _ => {}
                    }
                }
                let done = sub.is_some_and(|s| s.done);
                if !done && busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Working…").color(theme::COMMENT()));
                    });
                } else if items.is_empty() {
                    ui.label(
                        RichText::new("Nothing from this agent was kept.").color(theme::COMMENT()),
                    );
                }
                ui.add_space(4.0);
            });
        if back {
            self.viewing.pop();
        }
        if let Some(id) = open {
            self.viewing.push(id);
        }
        None
    }

    fn history_view(&mut self, ui: &mut Ui) -> Option<Action> {
        let (mut open, mut delete, mut open_cli) = (None, None, None);
        ui.horizontal(|ui| {
            if icon_button(ui, icons::ARROW_LEFT, "Back to the chat") {
                self.show_history = false;
            }
            ui.label(RichText::new("History").strong().size(15.0));
        });
        ui.add_space(4.0);
        // Conversations already here (this one, or one in the list) aren't listed twice.
        let known: Vec<String> = self
            .history
            .iter()
            .filter_map(|s| s.session.clone())
            .chain(self.session.clone())
            .collect();
        let terminal: Vec<usize> = self
            .cli
            .iter()
            .flatten()
            .enumerate()
            .filter(|(_, (saved, _))| saved.session.as_ref().is_none_or(|id| !known.contains(id)))
            .map(|(index, _)| index)
            .collect();
        if self.history.is_empty() && terminal.is_empty() && self.cli.is_some() {
            ui.label(RichText::new("No earlier chats yet").color(theme::COMMENT()));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (index, saved) in self.history.iter().enumerate() {
                let row = Frame::NONE
                    .fill(theme::tint(7))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, icons::TRASH, "Remove from history") {
                                    delete = Some(index);
                                }
                                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                    ui.add(egui::Label::new(&saved.title).selectable(false));
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!(
                                                "{} messages",
                                                saved.items.len()
                                            ))
                                            .small()
                                            .color(theme::COMMENT()),
                                        )
                                        .selectable(false),
                                    );
                                });
                            });
                        });
                    })
                    .response;
                glow(ui, &row, 10);
                if delete.is_none()
                    && row.contains_pointer()
                    && ui.input(|i| i.pointer.primary_clicked())
                {
                    open = Some(index);
                }
            }
            match &self.cli {
                None => {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Looking for chats from the terminal…")
                                .small()
                                .color(theme::COMMENT()),
                        );
                    });
                }
                Some(list) if !terminal.is_empty() => {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("IN THE TERMINAL")
                            .small()
                            .strong()
                            .color(theme::COMMENT()),
                    );
                    ui.add_space(2.0);
                    for &index in &terminal {
                        let (saved, modified) = &list[index];
                        let row = Frame::NONE
                            .fill(theme::tint(7))
                            .corner_radius(10)
                            .inner_margin(Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(icons::TERMINAL).color(theme::COMMENT()),
                                        )
                                        .selectable(false),
                                    );
                                    ui.add(egui::Label::new(&saved.title).selectable(false));
                                });
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "{} messages · {}",
                                            saved.items.len(),
                                            ago(now.saturating_sub(*modified))
                                        ))
                                        .small()
                                        .color(theme::COMMENT()),
                                    )
                                    .selectable(false),
                                );
                            })
                            .response;
                        glow(ui, &row, 10);
                        if row.contains_pointer() && ui.input(|i| i.pointer.primary_clicked()) {
                            open_cli = Some(index);
                        }
                    }
                }
                Some(_) => {}
            }
        });
        if let Some(index) = open_cli {
            self.open_cli(index);
            self.show_history = false;
            return Some(Action::Switched);
        }
        if let Some(index) = delete {
            self.history.remove(index);
        } else if let Some(index) = open {
            self.open_saved(index);
            self.show_history = false;
            return Some(Action::Switched);
        }
        None
    }

    /// The process is gone because we stopped it; the session can still be resumed.
    /// Messages that were waiting when Claude stopped go back to the input box, so
    /// nothing typed is lost.
    fn unqueue(&mut self) {
        let queued = std::mem::take(&mut self.queued);
        if queued.is_empty() {
            return;
        }
        let mut text: Vec<String> = queued.iter().map(|q| q.text.clone()).collect();
        if !self.input.trim().is_empty() {
            text.push(std::mem::take(&mut self.input));
        }
        self.input = text.join("\n\n");
    }

    pub fn stopped(&mut self) {
        self.unqueue();
        self.past_cost += std::mem::take(&mut self.proc_cost);
        self.proc = None;
        self.busy = false;
        self.streaming = false;
        self.partial.clear();
        self.asks.clear();
        self.rewinding = false;
    }

    pub fn on_stderr(&mut self, data: &[u8]) {
        self.stderr.push_str(&String::from_utf8_lossy(data));
    }

    /// Feeds process output; returns true when Claude finished a turn.
    pub fn on_stdout(&mut self, data: &[u8]) -> bool {
        self.partial.extend_from_slice(data);
        let mut finished = false;
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            if let Ok(value) = serde_json::from_slice::<Value>(&line) {
                finished |= self.on_event(&value);
            }
        }
        finished
    }

    fn on_event(&mut self, v: &Value) -> bool {
        if let Some(session) = v["session_id"].as_str() {
            self.session = Some(session.to_string());
        }
        match v["type"].as_str().unwrap_or("") {
            "control_request" if v["request"]["subtype"] == "can_use_tool" => {
                let id = v["request_id"].as_str().unwrap_or("").to_string();
                let tool = v["request"]["tool_name"]
                    .as_str()
                    .unwrap_or("tool")
                    .to_string();
                let input = v["request"]["input"].clone();
                if self.allow_edits && edits_files(&tool) {
                    self.answer(&id, Some(&input));
                } else {
                    self.new_ask = Some(format!("{tool} {}", tool_detail(&tool, &input)));
                    self.asks.push(Ask { id, tool, input });
                }
                return false;
            }
            "control_cancel_request" => {
                self.asks
                    .retain(|ask| Some(ask.id.as_str()) != v["request_id"].as_str());
                return false;
            }
            // Our own answers are echoed back; only the reply to a restore matters.
            "control_response" => {
                let response = &v["response"];
                let ours = response["request_id"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("ride-rewind");
                if ours && self.rewinding {
                    self.rewinding = false;
                    let refused = response["subtype"] != "success"
                        || response["response"]["canRewind"] == false;
                    if refused {
                        let why = response["error"]
                            .as_str()
                            .or(response["response"]["error"].as_str())
                            .unwrap_or("Claude Code has no checkpoint for that message");
                        self.items
                            .push(Item::Error(format!("Couldn't restore the files: {why}")));
                    } else {
                        self.items.push(Item::Note(
                            "Files are back to how they were before that message. The conversation itself carries on from here.".into(),
                        ));
                        self.rewound = true;
                    }
                }
                return false;
            }
            _ => {}
        }
        // A subagent's messages make its own conversation, opened from its card.
        if let Some(parent) = v["parent_tool_use_id"].as_str() {
            self.on_sub_message(parent.to_string(), &v);
            return false;
        }
        match v["type"].as_str().unwrap_or("") {
            "system" => {
                if let Some(model) = v["model"].as_str() {
                    self.model = model.to_string();
                }
            }
            "stream_event" => {
                let event = &v["event"];
                if event["type"] == "content_block_delta" && event["delta"]["type"] == "text_delta"
                {
                    let text = event["delta"]["text"].as_str().unwrap_or("");
                    match self.items.last_mut() {
                        Some(Item::Text(current)) if self.streaming => current.push_str(text),
                        _ => self.items.push(Item::Text(text.to_string())),
                    }
                    self.streaming = true;
                    self.got_delta = true;
                } else if event["type"] == "content_block_stop" {
                    self.streaming = false;
                }
            }
            "assistant" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str().unwrap_or("") {
                        "text" if !self.got_delta => {
                            self.items
                                .push(Item::Text(block["text"].as_str().unwrap_or("").into()));
                        }
                        "tool_use" => {
                            let name = block["name"].as_str().unwrap_or("tool").to_string();
                            let id = block["id"].as_str().unwrap_or("").to_string();
                            if starts_agent(&name) {
                                self.items.push(agent_item(&id, &block["input"]));
                                self.subs.entry(id).or_default();
                                continue;
                            }
                            let detail = tool_detail(&name, &block["input"]);
                            for change in changes_from_tool(&name, &block["input"]) {
                                self.pending_changes.push((id.clone(), change));
                            }
                            self.items.push(Item::Tool { name, detail });
                        }
                        _ => {}
                    }
                }
                self.streaming = false;
                self.got_delta = false;
            }
            // Tool results: an edit counts as made only once its tool reports success.
            "user" => {
                // The message just sent comes back with its id, which is its checkpoint.
                let typed = v["message"]["content"]
                    .as_array()
                    .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "text"))
                    || v["message"]["content"].is_string();
                if v["isReplay"] == true && std::mem::take(&mut self.expect_checkpoint) {
                    let user = self.items.iter().rposition(|i| matches!(i, Item::User(_)));
                    if let (Some(index), Some(id)) = (user, v["uuid"].as_str()) {
                        self.items
                            .insert(index + 1, Item::Checkpoint(id.to_string()));
                    }
                } else if v["isReplay"] == true && typed && !self.queued.is_empty() {
                    // Claude Code took in a message that was waiting: it joins the
                    // transcript here, where Claude starts on it.
                    let queued = self.queued.remove(0);
                    for (name, texture) in queued.images {
                        self.items.push(Item::Image {
                            name,
                            texture: Some(texture),
                        });
                    }
                    self.items.push(Item::User(queued.text));
                    if let Some(id) = v["uuid"].as_str() {
                        self.items.push(Item::Checkpoint(id.to_string()));
                    }
                    self.busy = true;
                    self.streaming = false;
                    self.got_delta = false;
                }
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] != "tool_result" {
                        continue;
                    }
                    let id = block["tool_use_id"].as_str().unwrap_or("");
                    if let Some(sub) = self.subs.get_mut(id) {
                        sub.done = true;
                    }
                    let failed = block["is_error"].as_bool().unwrap_or(false);
                    let (done, waiting): (Vec<_>, Vec<_>) =
                        std::mem::take(&mut self.pending_changes)
                            .into_iter()
                            .partition(|(i, _)| i == id);
                    self.pending_changes = waiting;
                    if !failed {
                        self.changes
                            .extend(done.into_iter().map(|(_, change)| change));
                    }
                }
            }
            "result" => {
                self.turns += 1;
                if let Some(cost) = v["total_cost_usd"].as_f64() {
                    self.proc_cost = cost;
                }
                // With messages still waiting, Claude goes straight on to the next one.
                self.busy = !self.queued.is_empty();
                self.streaming = false;
                self.last_result = v["result"].as_str().map(str::to_string);
                if v["is_error"].as_bool().unwrap_or(false) {
                    let text = v["result"]
                        .as_str()
                        .unwrap_or("Claude reported an error")
                        .to_string();
                    self.needs_login |= looks_like_login_problem(&text);
                    self.items.push(Item::Error(text));
                }
                return true;
            }
            _ => {}
        }
        false
    }

    pub fn on_exit(&mut self, code: Option<i32>) {
        self.unqueue();
        self.past_cost += std::mem::take(&mut self.proc_cost);
        self.proc = None;
        self.partial.clear();
        self.asks.clear();
        self.rewinding = false;
        let interrupted = std::mem::take(&mut self.busy);
        self.streaming = false;
        let stderr = std::mem::take(&mut self.stderr);
        if code == Some(0) && !interrupted {
            return;
        }
        let text = if code == Some(127) || stderr.contains("command not found") {
            "Claude Code isn't installed on this host. Install it there, then try again."
                .to_string()
        } else if stderr.trim().is_empty() {
            format!("claude exited unexpectedly (code {code:?})")
        } else {
            stderr.trim().to_string()
        };
        self.needs_login |= looks_like_login_problem(&text);
        if text.contains("No conversation found") {
            // The saved session doesn't exist on this host (any more); start a fresh one next time.
            self.session = None;
            self.items.push(Item::Error(
                "This conversation can't be resumed on this host. Send your message again to continue in a new session.".into(),
            ));
            return;
        }
        self.items.push(Item::Error(text));
    }

    pub fn show(&mut self, ui: &mut Ui, open_file: Option<&str>, detached: bool) -> Option<Action> {
        if let Some(id) = self.viewing.last().cloned() {
            return self.sub_view(ui, &id);
        }
        if self.show_history {
            return self.history_view(ui);
        }
        let mut action = None;

        // The header drags the chat out of the window. Its handle goes in first, so the
        // buttons drawn over it afterwards get their clicks.
        let drag = (!detached && !self.team_member).then(|| {
            let rect = egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
            );
            ui.interact(rect, ui.id().with("chat_drag"), Sense::drag())
        });
        ui.horizontal(|ui| {
            let plain =
                |ui: &mut Ui, text: RichText| ui.add(egui::Label::new(text).selectable(false));
            // A detached chat already has its name in the window's title bar.
            // An agent's name is in the header above its chat; a detached chat's, in the title bar.
            if !detached && !self.team_member {
                let icon = if self.team_member {
                    icons::AGENTS
                } else {
                    icons::SPARKLE
                };
                plain(ui, RichText::new(icon).color(theme::CLAUDE()).size(16.0));
                plain(ui, RichText::new(&self.title).strong().size(15.0));
            }
            if !self.model.is_empty() {
                let model = self.model.trim_start_matches("claude-");
                plain(ui, RichText::new(model).small().color(theme::COMMENT()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if detached {
                    let pin = if self.pinned {
                        icons::PINNED
                    } else {
                        icons::PIN
                    };
                    if icon_button(ui, pin, "Keep on top of other windows") {
                        self.pinned = !self.pinned;
                    }
                } else if !self.team_member
                    && icon_button(
                        ui,
                        icons::POP_OUT,
                        "Open in its own window (or drag this header out of the window)",
                    )
                {
                    action = Some(Action::Detach(None));
                }
                if icon_button(ui, icons::ADD, "New chat") {
                    action = Some(Action::NewChat);
                }
                if !self.team_member && icon_button(ui, icons::HISTORY, "Earlier chats") {
                    self.show_history = true;
                    self.cli = None;
                    action = Some(Action::LoadHistory);
                }
            });
        });
        if let Some(drag) = drag {
            if drag.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                // Nothing reaches the window once the cursor is outside it, so keep polling.
                ui.ctx().request_repaint();
            }
            if drag.dragged() || drag.drag_stopped() {
                if let Some(screen_pos) = crate::dragged_outside(ui.ctx()) {
                    action = Some(Action::Detach(Some(screen_pos)));
                }
            }
        }
        ui.add_space(4.0);

        // The input box keeps its natural height at the bottom; the transcript takes the rest.
        egui::Panel::bottom(ui.id().with("chat_input_panel"))
            .frame(Frame::NONE.inner_margin(Margin {
                left: 0,
                right: 0,
                top: 8,
                bottom: 0,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                if let Some(a) = self.input_box(ui, open_file) {
                    action = Some(a);
                }
            });
        egui::CentralPanel::default_margins()
            .frame(Frame::NONE)
            .show(ui, |ui| {
                if let Some(a) = self.transcript(ui, open_file.is_some()) {
                    action = Some(a);
                }
            });
        action
    }

    fn welcome(&self, ui: &mut Ui, has_file: bool) -> Option<Action> {
        let mut action = None;
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.22).max(12.0));
            ui.label(RichText::new(icons::SPARKLE).size(34.0).color(theme::CLAUDE()));
            ui.add_space(6.0);
            ui.label(RichText::new("Ask Claude").size(19.0).strong());
            ui.label(
                RichText::new(if self.team_member {
                    "Give this agent a task. It can bring in teammates, and hands easy work to cheaper models."
                } else {
                    "Claude Code on this host, with the account logged in there"
                })
                    .small()
                    .color(theme::COMMENT()),
            );
            ui.add_space(14.0);
            let ideas: &[&str] = if self.team_member {
                &[]
            } else if has_file {
                &["Explain this file", "Find bugs in this file", "What does this project do?"]
            } else {
                &["What does this project do?", "Where should I start reading?"]
            };
            for idea in ideas {
                let chip = egui::Button::new(RichText::new(*idea).color(theme::MUTED()))
                    .fill(theme::tint(8))
                    .stroke(Stroke::new(1.0, theme::tint(20)))
                    .corner_radius(14);
                if ui.add(chip).clicked() {
                    action = Some(Action::Send(idea.to_string()));
                }
            }
        });
        action
    }

    fn transcript(&mut self, ui: &mut Ui, has_file: bool) -> Option<Action> {
        let mut action = None;
        if self.items.is_empty() {
            return self.welcome(ui, has_file);
        }
        // What the user chose on a card below, carried out once the transcript is drawn.
        let mut decision: Option<(usize, bool, bool)> = None;
        let mut rewind: Option<(usize, String)> = None;
        let mut confirm = self.confirm_rewind;
        let idle = !self.busy && !self.rewinding;
        let mut open_sub = None;
        ScrollArea::vertical().auto_shrink(false).stick_to_bottom(true).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            let width = ui.available_width();
            for (index, item) in self.items.iter().enumerate() {
                match item {
                    Item::User(text) => {
                        let checkpoint = match self.items.get(index + 1) {
                            Some(Item::Checkpoint(id)) => Some(id),
                            _ => None,
                        };
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            ui.set_max_width(width * 0.88);
                            let bubble = Frame::NONE
                                .fill(theme::tint(10))
                                .stroke(Stroke::new(1.0, theme::LINE()))
                                .corner_radius(8)
                                .inner_margin(Margin::symmetric(11, 7))
                                .show(ui, |ui| {
                                    bubble_text(ui, RichText::new(text.as_str()))
                                });
                            glow(ui, &bubble.response, 8);
                        });
                        if checkpoint.is_none() || !idle {
                            continue;
                        }
                        // On a line of its own under the message, so it never crowds the text.
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            if let Some(id) = checkpoint.filter(|_| idle) {
                                if confirm == Some(index) {
                                    if ui.small_button("Cancel").clicked() {
                                        confirm = None;
                                    }
                                    let restore = ui.small_button("Restore files").on_hover_text(
                                        "Undoes every file edit Claude made since this message was sent. Changes made by commands it ran stay.",
                                    );
                                    if restore.clicked() {
                                        rewind = Some((index, id.clone()));
                                    }
                                } else {
                                    let label = RichText::new(format!("{}  restore files", icons::DISCARD))
                                        .small()
                                        .color(theme::COMMENT());
                                    let button = egui::Button::new(label).frame_when_inactive(false);
                                    let hint = "Put files back to how they were before this message…";
                                    if ui.add(button).on_hover_text(hint).clicked() {
                                        confirm = Some(index);
                                    }
                                }
                            }
                        });
                    }
                    Item::Image { name, texture } => {
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| match texture {
                            Some(texture) => {
                                let size = texture.size_vec2();
                                let scale = (220.0 / size.x).min(160.0 / size.y).min(1.0);
                                ui.add(
                                    egui::Image::new(egui::load::SizedTexture::new(
                                        texture.id(),
                                        size * scale,
                                    ))
                                    .corner_radius(8),
                                )
                                .on_hover_text(name);
                            }
                            None => {
                                let chip = format!("{}  {name}", icons::IMAGE);
                                ui.label(RichText::new(chip).small().color(theme::COMMENT()));
                            }
                        });
                    }
                    Item::Checkpoint(_) => {}
                    Item::Note(text) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(icons::CHECK).color(theme::COMMENT()));
                            ui.label(RichText::new(text).small().color(theme::COMMENT()));
                        });
                    }
                    Item::From { who, text } => {
                        let card = Frame::NONE
                            .fill(theme::CLAUDE().gamma_multiply(0.10))
                            .stroke(Stroke::new(1.0, theme::CLAUDE().gamma_multiply(0.35)))
                            .corner_radius(12)
                            .inner_margin(Margin::symmetric(11, 8))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!("{}  from {who}", icons::AGENTS))
                                        .small()
                                        .strong()
                                        .color(theme::CLAUDE()),
                                );
                                markdown(ui, text, index);
                            });
                        glow(ui, &card.response, 12);
                    }
                    Item::Text(text) => markdown(ui, text, index),
                    Item::Agent { id, kind, task } => {
                        let sub = self.subs.get(id);
                        let running = self.busy && !sub.is_some_and(|s| s.done);
                        let steps = sub.map_or(0, |s| s.items.len());
                        if agent_card(ui, kind, task, steps, running) {
                            open_sub = Some(id.clone());
                        }
                    }
                    Item::Tool { name, detail } => {
                        let chip = Frame::NONE
                            .fill(theme::tint(7))
                            .stroke(Stroke::new(1.0, theme::tint(14)))
                            .corner_radius(9)
                            .inner_margin(Margin::symmetric(9, 5))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                                    ui.label(RichText::new(tool_icon(name)).color(theme::CLAUDE()));
                                    ui.label(RichText::new(name).small().strong());
                                    ui.label(RichText::new(detail).small().monospace().color(theme::COMMENT()));
                                });
                            });
                        glow(ui, &chip.response, 9);
                    }
                    Item::Error(text) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(icons::ERROR).color(theme::ANSI()[1]));
                            ui.label(RichText::new(text).color(theme::ANSI()[9]));
                        });
                    }
                }
            }
            if self.needs_login {
                ui.label(
                    RichText::new("Claude Code on this host isn't logged in. Logging in opens a terminal where you finish the sign-in with your Claude account.")
                        .small()
                        .color(theme::COMMENT()),
                );
                if ui.button(format!("{}  Log in to Claude", icons::SIGN_IN)).clicked() {
                    action = Some(Action::Login);
                }
            }
            for (index, ask) in self.asks.iter().enumerate() {
                if let Some((allow, always)) = ask_card(ui, ask, index) {
                    decision = Some((index, allow, always));
                }
            }
            if self.rewinding {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Restoring files…").color(theme::COMMENT()));
                });
            } else if self.busy && !self.streaming && self.asks.is_empty() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Working…").color(theme::COMMENT()));
                });
            }
            for queued in &self.queued {
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    ui.set_max_width(width * 0.88);
                    ui.vertical(|ui| {
                        Frame::NONE
                            .fill(theme::tint(6))
                            .stroke(Stroke::new(1.0, theme::tint(12)))
                            .corner_radius(8)
                            .inner_margin(Margin::symmetric(11, 7))
                            .show(ui, |ui| {
                                bubble_text(ui, RichText::new(&queued.text).color(theme::COMMENT()));
                            });
                        let note = match queued.images.len() {
                            0 => "queued: Claude reads it once it's done with this step".to_string(),
                            n => format!("queued, with {n} image(s): Claude reads it once it's done with this step"),
                        };
                        ui.label(RichText::new(note).small().color(theme::COMMENT()));
                    });
                });
            }
            ui.add_space(4.0);
        });
        self.confirm_rewind = confirm;
        if let Some(id) = open_sub {
            self.viewing.push(id);
        }
        if let Some((index, allow, always)) = decision {
            let ask = self.asks.remove(index);
            self.allow_edits |= always;
            self.answer(&ask.id, allow.then_some(&ask.input));
            // Edits of the same kind already waiting are covered by "always" too.
            if always {
                let (covered, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.asks)
                    .into_iter()
                    .partition(|ask| edits_files(&ask.tool));
                self.asks = waiting;
                for ask in covered {
                    self.answer(&ask.id, Some(&ask.input));
                }
            }
        }
        if let Some((index, id)) = rewind {
            self.confirm_rewind = None;
            self.rewinding = true;
            let msg = json!({
                "type": "control_request",
                "request_id": format!("ride-rewind-{index}"),
                "request": { "subtype": "rewind_files", "user_message_id": id },
            });
            self.outbox.push(format!("{msg}\n").into_bytes());
            if self.proc.is_none() {
                action = Some(Action::Wake);
            }
        }
        action
    }

    fn input_box(&mut self, ui: &mut Ui, open_file: Option<&str>) -> Option<Action> {
        let mut action = None;
        let id = ui.id().with("chat_input");
        let focused = ui.memory(|m| m.has_focus(id));
        // Enter sends and Shift+Enter breaks the line, so Enter is taken before the text box sees it.
        let enter = focused
            && ui.input_mut(|i| !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter));

        // `@` mentions: fixed entries plus files matching what follows the `@`.
        let mention = self.mention_in_progress().map(str::to_string);
        let mut offered: Vec<String> = Vec::new();
        if let Some(word) = &mention {
            let typed = word[1..].to_lowercase();
            let fixed = if self.team_member {
                &[][..]
            } else {
                &["selection", "terminal"][..]
            };
            offered.extend(
                fixed
                    .iter()
                    .filter(|f| f.starts_with(&typed))
                    .map(|f| f.to_string()),
            );
            offered.extend(self.suggestions.iter().cloned());
            if *word != self.asked {
                self.asked = word.clone();
                self.suggestions.clear();
                action = Some(Action::FindFiles(word[1..].to_string()));
            }
        } else {
            self.suggestions.clear();
            self.asked.clear();
        }
        // egui passes on pasted text only, so a picture on the clipboard is fetched here.
        let paste_keys = crate::paste_keys_down();
        if focused && paste_keys && !self.paste_down {
            self.paste_image(ui.ctx());
        }
        self.paste_down = paste_keys;
        let tab = focused
            && !offered.is_empty()
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab));
        let mut picked = tab.then(|| offered[0].clone());
        let hovered = ui.rect_contains_pointer(ui.max_rect());
        let border = if focused {
            theme::CLAUDE().gamma_multiply(0.75)
        } else if hovered {
            theme::tint(56)
        } else {
            theme::tint(24)
        };

        Frame::NONE
            .fill(theme::tint(8))
            .stroke(Stroke::new(1.0, border))
            .corner_radius(10)
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    if !offered.is_empty() {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        for (n, entry) in offered.iter().take(8).enumerate() {
                            let hint = if n == 0 { "   Tab" } else { "" };
                            let label =
                                RichText::new(format!("{} {entry}{hint}", icons::ATTACH)).small();
                            if ui.selectable_label(n == 0, label).clicked() {
                                picked = Some(entry.clone());
                            }
                        }
                        ui.separator();
                    }
                    if !self.images.is_empty() {
                        let mut remove = None;
                        ui.horizontal_wrapped(|ui| {
                            for (n, image) in self.images.iter().enumerate() {
                                let size = image.texture.size_vec2();
                                let scale = 44.0 / size.y;
                                let thumb = egui::Image::new(egui::load::SizedTexture::new(
                                    image.texture.id(),
                                    size * scale,
                                ))
                                .corner_radius(6)
                                .sense(Sense::click());
                                let hint = format!("{} — click to remove", image.name);
                                if ui.add(thumb).on_hover_text(hint).clicked() {
                                    remove = Some(n);
                                }
                            }
                        });
                        if let Some(n) = remove {
                            self.images.remove(n);
                        }
                    }
                    ui.add(
                        TextEdit::multiline(&mut self.input)
                            .id(id)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text("Ask Claude…  (@ to mention a file)")
                            .frame(Frame::NONE),
                    );
                    if let Some(path) = open_file {
                        let name = path.rsplit('/').next().unwrap_or(path);
                        let chip = egui::Button::selectable(
                            self.attach,
                            RichText::new(format!("{} {name}", icons::ATTACH)).small(),
                        );
                        if ui
                            .add(chip)
                            .on_hover_text("Tell Claude which file is open")
                            .clicked()
                        {
                            self.attach = !self.attach;
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let before = (self.mode, self.model_choice, self.effort);
                        egui::ComboBox::from_id_salt("chat_mode")
                            .selected_text(RichText::new(self.mode.label()).small())
                            .width(58.0)
                            .show_ui(ui, |ui| {
                                for (mode, hint) in [
                                    (
                                        Mode::Ask,
                                        "Asks you before it changes a file or runs a command",
                                    ),
                                    (
                                        Mode::Edit,
                                        "Edits files without asking; asks before running commands",
                                    ),
                                    (Mode::Auto, "May edit files and run commands it judges safe"),
                                ] {
                                    ui.selectable_value(&mut self.mode, mode, mode.label())
                                        .on_hover_text(hint);
                                }
                            });
                        picker(
                            ui,
                            "chat_model",
                            "Model",
                            &mut self.model_choice,
                            &[
                                ("", "Default"),
                                ("fable", "Fable"),
                                ("opus", "Opus"),
                                ("sonnet", "Sonnet"),
                                ("haiku", "Haiku"),
                            ],
                        );
                        picker(
                            ui,
                            "chat_effort",
                            "Effort",
                            &mut self.effort,
                            &[
                                ("", "Default"),
                                ("low", "Low"),
                                ("medium", "Medium"),
                                ("high", "High"),
                                ("xhigh", "Extra high"),
                                ("max", "Max"),
                            ],
                        );
                        if before != (self.mode, self.model_choice, self.effort) {
                            action = Some(Action::Restart);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let ready = !self.input.trim().is_empty() || !self.images.is_empty();
                            if self.busy {
                                let stop = egui::Button::new(
                                    RichText::new(icons::STOP).color(Color32::WHITE),
                                )
                                .fill(theme::ANSI()[1].gamma_multiply(0.7))
                                .corner_radius(14);
                                if ui.add(stop).on_hover_text("Stop").clicked() {
                                    action = Some(Action::Stop);
                                }
                                // Still at work: a message goes out now and waits its turn.
                                if ready {
                                    let send = egui::Button::new(
                                        RichText::new(icons::ARROW_UP).color(theme::EDITOR_BG()),
                                    )
                                    .fill(theme::TEXT())
                                    .corner_radius(14);
                                    let hint = "Send now (Enter): Claude reads it as soon as it's done with this step";
                                    if ui.add(send).on_hover_text(hint).clicked() || enter {
                                        action = Some(Action::Send(
                                            std::mem::take(&mut self.input).trim().to_string(),
                                        ));
                                    }
                                }
                            } else {
                                let ready =
                                    !self.input.trim().is_empty() || !self.images.is_empty();
                                // A plain light disc, as quiet as the rest of the theme.
                                let fill = if ready {
                                    theme::TEXT()
                                } else {
                                    theme::tint(18)
                                };
                                let send = egui::Button::new(
                                    RichText::new(icons::ARROW_UP).color(theme::EDITOR_BG()),
                                )
                                .fill(fill)
                                .corner_radius(14);
                                let clicked = ui.add(send).on_hover_text("Send (Enter)").clicked();
                                if (clicked || enter) && ready {
                                    action = Some(Action::Send(
                                        std::mem::take(&mut self.input).trim().to_string(),
                                    ));
                                }
                            }
                            if icon_button(
                                ui,
                                icons::IMAGE,
                                "Attach images (or paste one with Ctrl+V)",
                            ) {
                                self.pick_images(ui.ctx());
                            }
                        });
                    });
                });
            });
        if let Some(entry) = picked {
            self.complete_mention(&entry);
            ui.memory_mut(|m| m.request_focus(id));
        }
        action
    }
}

/// The card that asks whether Claude may do something. Returns (allow, always) once chosen.
fn ask_card(ui: &mut Ui, ask: &Ask, salt: usize) -> Option<(bool, bool)> {
    let mut choice = None;
    let text = |key: &str| ask.input[key].as_str().unwrap_or("");
    Frame::NONE
        .fill(theme::CLAUDE().gamma_multiply(0.08))
        .stroke(Stroke::new(1.0, theme::CLAUDE().gamma_multiply(0.55)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(11, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let title = match ask.tool.as_str() {
                "Bash" => "Claude wants to run a command".to_string(),
                "Write" => "Claude wants to write a file".to_string(),
                tool if edits_files(tool) => "Claude wants to edit a file".to_string(),
                tool => format!("Claude wants to use {tool}"),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(tool_icon(&ask.tool)).color(theme::CLAUDE()));
                ui.label(RichText::new(title).strong());
            });
            let code = |ui: &mut Ui, body: &str| {
                Frame::NONE
                    .fill(theme::tint(10))
                    .corner_radius(6)
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.style_mut().wrap_mode = Some(TextWrapMode::Wrap);
                        let shown: String = body.chars().take(3000).collect();
                        ui.label(RichText::new(shown).monospace().size(13.0));
                    });
            };
            match ask.tool.as_str() {
                "Bash" => {
                    code(ui, text("command"));
                    if !text("description").is_empty() {
                        ui.label(
                            RichText::new(text("description"))
                                .small()
                                .color(theme::COMMENT()),
                        );
                    }
                }
                tool if edits_files(tool) => {
                    ui.label(RichText::new(text("file_path")).small().monospace());
                    for (n, change) in changes_from_tool(tool, &ask.input).iter().enumerate() {
                        // A very large edit would take long to compare on every frame.
                        let lines = if change.old.len() + change.new.len() > 40_000 {
                            vec![(
                                diff::Op::Same,
                                format!("{} lines", change.new.lines().count()),
                            )]
                        } else if change.whole_file {
                            change
                                .new
                                .lines()
                                .take(60)
                                .map(|l| (diff::Op::Added, l.to_string()))
                                .collect()
                        } else {
                            diff::lines(&change.old, &change.new)
                        };
                        diff::show(ui, &lines, &format!("ask-{salt}-{n}"), 200.0);
                    }
                }
                _ => {
                    let input = ask.input.to_string();
                    if input.len() > 2 {
                        code(ui, &input);
                    }
                }
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let allow = egui::Button::new(RichText::new("Allow").color(theme::EDITOR_BG()))
                    .fill(theme::TEXT());
                if ui.add(allow).clicked() {
                    choice = Some((true, false));
                }
                if edits_files(&ask.tool)
                    && ui
                        .button("Always allow edits")
                        .on_hover_text("Stop asking about file edits in this conversation")
                        .clicked()
                {
                    choice = Some((true, true));
                }
                if ui.button("Deny").clicked() {
                    choice = Some((false, false));
                }
            });
        });
    choice
}

/// A small drop-down over fixed `(value, label)` options. The empty value shows as `title`.
fn picker(
    ui: &mut Ui,
    salt: &str,
    title: &str,
    current: &mut &'static str,
    options: &[(&'static str, &'static str)],
) {
    let shown = options
        .iter()
        .find(|(value, _)| value == current && !value.is_empty())
        .map_or(title, |(_, label)| label);
    egui::ComboBox::from_id_salt(salt)
        .selected_text(RichText::new(shown).small())
        .width(62.0)
        .show_ui(ui, |ui| {
            ui.label(RichText::new(title).small().color(theme::COMMENT()));
            for (value, label) in options {
                ui.selectable_value(current, *value, *label);
            }
        });
}

/// Brightens a card while the pointer is over it.
fn glow(ui: &Ui, resp: &egui::Response, radius: u8) {
    if resp.contains_pointer() {
        ui.painter().rect(
            resp.rect,
            radius,
            theme::tint(8),
            Stroke::new(1.0, theme::tint(60)),
            egui::StrokeKind::Inside,
        );
    }
}

fn icon_button(ui: &mut Ui, icon: char, hint: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(icon).size(13.0)).frame_when_inactive(false))
        .on_hover_text(hint)
        .clicked()
}

/// Renders the subset of Markdown Claude's answers mostly use: fenced code,
/// headings, bullets, **bold** and `inline code`.
pub(crate) fn markdown(ui: &mut Ui, text: &str, salt: usize) {
    // What an agent tells a teammate is shown in that teammate's chat; here a
    // one-line chip per directive is enough.
    let mut offset = 0;
    let mut directives = Vec::new();
    let mut prose_end = None;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("@@") {
            prose_end.get_or_insert(offset);
            directives.push(line.trim());
        }
        offset += line.len();
    }
    let text = &text[..prose_end.unwrap_or(text.len())];

    ui.vertical(|ui| crate::markdown::render(ui, text, salt, &code_block));
    for directive in directives {
        delegation_chip(ui, directive);
    }
}

/// "→ name · model" for a `@@spawn`/`@@send` line, without the message itself.
fn delegation_chip(ui: &mut Ui, directive: &str) {
    let head = directive.split("::").next().unwrap_or(directive);
    let spawn = head.starts_with("@@spawn");
    let field = |key: &str| head.split_whitespace().find_map(|w| w.strip_prefix(key));
    let name = if spawn {
        field("name=")
    } else {
        head.split_whitespace().nth(1)
    }
    .unwrap_or("…");
    let label = match field("model=").filter(|_| spawn) {
        Some(model) => format!("Delegated to {name} · {model}"),
        None if spawn => format!("Delegated to {name}"),
        None => format!("Message to {name}"),
    };
    let chip = Frame::NONE
        .fill(theme::CLAUDE().gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, theme::CLAUDE().gamma_multiply(0.35)))
        .corner_radius(9)
        .inner_margin(Margin::symmetric(9, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icons::AGENTS).color(theme::CLAUDE()));
                ui.label(RichText::new(label).small().strong());
            });
        });
    glow(ui, &chip.response, 9);
}

fn code_block(ui: &mut Ui, code: &str, salt: (usize, usize)) {
    let block = Frame::NONE
        .fill(Color32::from_black_alpha(90))
        .corner_radius(8)
        .inner_margin(Margin::same(9))
        .show(ui, |ui| {
            ScrollArea::horizontal().id_salt(salt).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);
                ui.label(RichText::new(code.trim_end()).monospace());
            });
        })
        .response;
    if !block.contains_pointer() {
        return;
    }
    glow(ui, &block, 8);
    // A copy button in the corner, drawn by hand so it doesn't disturb the layout.
    let rect = egui::Rect::from_min_size(
        egui::pos2(block.rect.right() - 30.0, block.rect.top() + 5.0),
        egui::vec2(24.0, 22.0),
    );
    let button = ui
        .interact(rect, ui.id().with(("copy", salt)), Sense::click())
        .on_hover_text("Copy");
    let fill = if button.hovered() {
        theme::tint(50)
    } else {
        theme::tint(26)
    };
    ui.painter().rect_filled(rect, 6, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icons::COPY,
        FontId::proportional(12.0),
        theme::TEXT(),
    );
    if button.clicked() {
        ui.ctx().copy_text(code.trim_end().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_survives_saving_and_loading() {
        let mut chat = Chat::default();
        chat.mode = Mode::Edit;
        chat.model_choice = "haiku";
        chat.begin_turn("first question", None);
        chat.on_stdout(br#"{"type":"assistant","session_id":"abc-123","message":{"content":[{"type":"text","text":"an answer"},{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/a.rs","old_string":"x","new_string":"y"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}
{"type":"result","result":"an answer","total_cost_usd":0.5}
"#);
        chat.reset();
        chat.begin_turn("second question", None);

        let saved = chat.to_json();
        let restored = Chat::from_json(&saved);
        assert_eq!(restored.to_json(), saved);
        assert_eq!(restored.items.len(), 1);
        assert_eq!(restored.history.len(), 1);
        assert_eq!(restored.history[0].session.as_deref(), Some("abc-123"));
        assert_eq!(restored.changes.len(), 1);
        assert_eq!(restored.model_choice, "haiku");
        assert!(restored.mode == Mode::Edit && !restored.busy && restored.proc.is_none());
    }

    /// A message sent while Claude is still answering waits, joins the transcript when
    /// Claude Code takes it in, and keeps Claude busy until it has been answered too.
    #[test]
    fn a_message_sent_while_busy_waits_its_turn() {
        let mut chat = Chat::default();
        chat.begin_turn("write about the sea", None);
        chat.on_stdout(br#"{"type":"user","uuid":"m1","isReplay":true,"message":{"content":[{"type":"text","text":"write about the sea"}]}}
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"The sea"}}}
"#);
        let line = chat.begin_turn("end with BANANA", None);
        assert!(!line.is_empty(), "it goes out at once");
        assert_eq!(chat.queued.len(), 1);
        assert!(
            matches!(chat.items.last(), Some(Item::Text(t)) if t == "The sea"),
            "the answer under way isn't split"
        );
        chat.on_stdout(br#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":" is calm."}}}
{"type":"result","result":"The sea is calm."}
"#);
        assert!(chat.busy, "the waiting message is next");
        assert!(matches!(chat.items.last(), Some(Item::Text(t)) if t == "The sea is calm."));
        chat.on_stdout(br#"{"type":"user","uuid":"m2","isReplay":true,"message":{"content":[{"type":"text","text":"end with BANANA"}]}}
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"BANANA"}}}
{"type":"result","result":"BANANA"}
"#);
        assert!(chat.queued.is_empty() && !chat.busy);
        let tail: Vec<String> = chat.items[chat.items.len() - 3..]
            .iter()
            .map(|item| match item {
                Item::User(t) => format!("user {t}"),
                Item::Checkpoint(id) => format!("checkpoint {id}"),
                Item::Text(t) => format!("claude {t}"),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(
            tail,
            ["user end with BANANA", "checkpoint m2", "claude BANANA"]
        );

        // Stopped with messages waiting: they come back to the input box.
        chat.begin_turn("one more", None);
        chat.begin_turn("and another", None);
        chat.stopped();
        assert!(chat.queued.is_empty());
        assert_eq!(chat.input, "and another");
    }

    /// A subagent Claude starts gets a card; its own messages make its conversation,
    /// which is done when its result comes back and survives saving.
    #[test]
    fn a_subagent_has_a_conversation_of_its_own() {
        let mut chat = Chat::default();
        chat.begin_turn("look around", None);
        chat.on_stdout(br#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Task","input":{"description":"Find the config","subagent_type":"Explore","prompt":"..."}}]}}
{"type":"assistant","parent_tool_use_id":"t1","message":{"content":[{"type":"text","text":"Looking in src."},{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/srv/a.lua"}}]}}
{"type":"user","parent_tool_use_id":"t1","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"..."}]}}
{"type":"assistant","parent_tool_use_id":"t1","message":{"content":[{"type":"text","text":"It's in config.lua."}]}}
"#);
        assert!(
            matches!(chat.items.last(), Some(Item::Agent { kind, task, .. }) if kind == "Explore" && task == "Find the config")
        );
        assert_eq!(chat.subs["t1"].items.len(), 3);
        assert!(!chat.subs["t1"].done);
        assert!(
            !chat
                .items
                .iter()
                .any(|i| matches!(i, Item::Text(t) if t.contains("Looking"))),
            "kept apart"
        );
        chat.on_stdout(br#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"It's in config.lua."}]}}
{"type":"result","result":"done"}
"#);
        assert!(chat.subs["t1"].done);
        let restored = Chat::from_json(&chat.to_json());
        assert_eq!(restored.subs["t1"].items.len(), 3);
        assert!(restored.subs["t1"].done);
        let mut restored = restored;
        restored.open_first_agent();
        assert_eq!(restored.viewing, ["t1"]);
    }

    #[test]
    fn a_permission_request_is_answered() {
        let mut chat = Chat::default();
        chat.begin_turn("change it", None);
        chat.on_stdout(br#"{"type":"user","uuid":"msg-1","isReplay":true,"message":{"content":[{"type":"text","text":"change it"}]}}
{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Edit","input":{"file_path":"/a.rs","old_string":"x","new_string":"y"}}}
{"type":"control_request","request_id":"r2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf /tmp/x"}}}
"#);
        assert!(matches!(&chat.items[1], Item::Checkpoint(id) if id == "msg-1"));
        assert!(chat.awaiting_answer(), "a checkpoint isn't an answer");
        assert_eq!(chat.asks.len(), 2);
        assert!(chat.snippet().starts_with("Waiting for your OK: Edit"));
        assert_eq!(chat.take_new_ask().as_deref(), Some("Bash rm -rf /tmp/x"));

        let ask = chat.asks.remove(1);
        chat.answer(&ask.id, None);
        let ask = chat.asks.remove(0);
        chat.answer(&ask.id, Some(&ask.input));
        let sent: Vec<Value> = chat
            .take_outbox()
            .iter()
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(sent[0]["response"]["request_id"], "r2");
        assert_eq!(sent[0]["response"]["response"]["behavior"], "deny");
        assert_eq!(sent[1]["response"]["response"]["behavior"], "allow");
        assert_eq!(
            sent[1]["response"]["response"]["updatedInput"]["new_string"],
            "y"
        );

        // Once edits are always allowed, they are answered without a card.
        chat.allow_edits = true;
        chat.on_stdout(br#"{"type":"control_request","request_id":"r3","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"/b.rs","content":"z"}}}
"#);
        assert!(chat.asks.is_empty());
        assert_eq!(chat.take_outbox().len(), 1);

        // The checkpoint is kept with the conversation.
        let restored = Chat::from_json(&chat.to_json());
        assert!(matches!(&restored.items[1], Item::Checkpoint(id) if id == "msg-1"));
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(
            base64_decode(&base64(b"hello world!?")).unwrap(),
            b"hello world!?"
        );
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}

/// How long ago, in a word or two.
fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// A message bubble's text: as wide as its longest line, up to the room there is, wrapped
/// beyond that, and read from the left even in a bubble placed at the right.
fn bubble_text(ui: &mut Ui, text: RichText) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let natural = ui
        .painter()
        .layout_no_wrap(text.text().to_string(), font, Color32::WHITE)
        .size()
        .x;
    let width = (natural + 1.0).min(ui.available_width());
    ui.allocate_ui_with_layout(egui::vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
        ui.set_max_width(width);
        ui.add(egui::Label::new(text).wrap());
    });
}

fn agent_item(id: &str, input: &Value) -> Item {
    Item::Agent {
        id: id.to_string(),
        kind: input["subagent_type"].as_str().unwrap_or("").to_string(),
        task: input["description"]
            .as_str()
            .or(input["prompt"].as_str())
            .map(|t| first_line(t, 120))
            .unwrap_or_default(),
    }
}

fn tool_chip(ui: &mut Ui, name: &str, detail: &str) {
    let chip = Frame::NONE
        .fill(theme::tint(7))
        .stroke(Stroke::new(1.0, theme::tint(14)))
        .corner_radius(9)
        .inner_margin(Margin::symmetric(9, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                ui.label(RichText::new(tool_icon(name)).color(theme::CLAUDE()));
                ui.label(RichText::new(name).small().strong());
                ui.label(
                    RichText::new(detail)
                        .small()
                        .monospace()
                        .color(theme::COMMENT()),
                );
            });
        });
    glow(ui, &chip.response, 9);
}

/// A subagent in the conversation: what it's for and how far it got. True when clicked,
/// to open its own conversation.
fn agent_card(ui: &mut Ui, kind: &str, task: &str, steps: usize, running: bool) -> bool {
    let card = Frame::NONE
        .fill(theme::CLAUDE().gamma_multiply(0.08))
        .stroke(Stroke::new(1.0, theme::CLAUDE().gamma_multiply(0.30)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                ui.label(RichText::new(icons::AGENTS).color(theme::CLAUDE()));
                let kind = if kind.is_empty() { "Agent" } else { kind };
                ui.label(RichText::new(kind).small().strong());
                ui.label(RichText::new(task).small());
            });
            ui.horizontal(|ui| {
                if running {
                    ui.spinner();
                }
                let state = match (running, steps) {
                    (true, 0) => "starting".to_string(),
                    (true, n) => format!("working · {n} steps so far"),
                    (false, n) => format!("done · {n} steps"),
                };
                ui.label(
                    RichText::new(format!("{state}  ·  open {}", icons::CHEVRON_RIGHT))
                        .small()
                        .color(theme::COMMENT()),
                );
            });
        })
        .response;
    glow(ui, &card, 10);
    let card = card.interact(Sense::click());
    if card.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    card.on_hover_text("Open this agent's conversation")
        .clicked()
}
