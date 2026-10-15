//! Language servers: the programs editors use to understand code (completion, errors, go to
//! definition, rename and the rest). Each runs on the host next to the code, as a process
//! of its own, and speaks JSON-RPC over its input and output. This is the client side of
//! that conversation. It knows nothing of the interface: it turns the editor's questions
//! into messages, and what the servers say into `Event`s.

use crate::buffer::Pos;
use ride_proto::{Request, Response};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// A place as language servers count it: the line, and UTF-16 code units into the line.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct LPos {
    pub line: u32,
    pub ch: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextEdit {
    pub start: LPos,
    pub end: LPos,
    pub text: String,
}

/// Edits to make, file by file.
pub type WorkspaceEdit = Vec<(String, Vec<TextEdit>)>;

/// A problem a server found in a file.
#[derive(Clone, Debug)]
pub struct Diag {
    pub start: LPos,
    pub end: LPos,
    /// 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u8,
    pub message: String,
    pub source: String,
    /// As the server sent it: a quick fix asks about the problems it is for.
    raw: Value,
}

/// A completion the server offers.
#[derive(Clone, Debug, Default)]
pub struct Item {
    pub label: String,
    pub kind: u8,
    pub detail: String,
    pub doc: String,
    pub sort: String,
    pub filter: String,
    /// What goes in, and whether it is a snippet with places to fill in.
    pub insert: String,
    pub snippet: bool,
    /// What it replaces, when the server says.
    pub range: Option<(LPos, LPos)>,
    /// Edits elsewhere that come with it, such as an import at the top.
    pub extra: Vec<TextEdit>,
    pub(crate) raw: Value,
    pub resolved: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub path: String,
    pub start: LPos,
    pub end: LPos,
}

/// The function whose arguments are being typed.
#[derive(Clone, Debug, Default)]
pub struct Signature {
    pub label: String,
    /// The argument being typed, as a range of chars in `label`.
    pub active: Option<(usize, usize)>,
    pub doc: String,
}

/// A note the server puts in the code: the type it worked out for a name, or the name of
/// the parameter an argument is for.
#[derive(Clone, Debug, PartialEq)]
pub struct InlayHint {
    pub at: LPos,
    /// With a space before or after, when the server asks for one.
    pub label: String,
}

/// What the server says a name is, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemKind {
    Type,
    Param,
    Variable,
    Property,
    Constant,
    Func,
    Keyword,
}

/// A name the server coloured: its line, and where it is in UTF-16 units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SemToken {
    pub line: u32,
    pub start: u32,
    pub len: u32,
    pub kind: SemKind,
}

/// A quick fix or refactoring the server offers.
#[derive(Clone, Debug)]
pub struct CodeAction {
    pub title: String,
    pub preferred: bool,
    pub edit: Option<WorkspaceEdit>,
    pub command: Option<Value>,
    raw: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Starting,
    Ready,
    /// The program isn't on the host.
    Missing,
    Failed(String),
}

/// What an answer the client waits for is for. `tab` is the editor tab that asked.
#[derive(Clone, Debug)]
pub enum Pending {
    Initialize,
    Completion {
        tab: u64,
        at: Pos,
    },
    Resolve {
        tab: u64,
    },
    Hover {
        tab: u64,
        at: Pos,
    },
    Signature {
        tab: u64,
    },
    Definition,
    References {
        name: String,
    },
    Rename {
        name: String,
    },
    Actions {
        tab: u64,
    },
    ResolveAction,
    Format {
        tab: u64,
        revision: u64,
    },
    /// The problems in a file, for a server that waits to be asked.
    Diagnostics {
        path: String,
    },
    InlayHints {
        tab: u64,
        revision: u64,
    },
    Semantic {
        tab: u64,
        revision: u64,
    },
    Other,
}

/// What came of talking to a server, for the editor to act on.
#[derive(Debug)]
pub enum Event {
    Completion {
        tab: u64,
        at: Pos,
        items: Vec<Item>,
        incomplete: bool,
    },
    Resolved {
        tab: u64,
        item: Item,
    },
    Hover {
        tab: u64,
        at: Pos,
        text: String,
        range: Option<(LPos, LPos)>,
    },
    Signature {
        tab: u64,
        signature: Option<Signature>,
    },
    Definition {
        locations: Vec<Location>,
    },
    References {
        name: String,
        locations: Vec<Location>,
    },
    /// Files to change: a rename, a quick fix, or the server asking.
    Edit {
        edit: WorkspaceEdit,
        what: String,
    },
    Actions {
        tab: u64,
        actions: Vec<CodeAction>,
    },
    Format {
        tab: u64,
        revision: u64,
        edits: Vec<TextEdit>,
    },
    Diagnostics,
    InlayHints {
        tab: u64,
        revision: u64,
        hints: Vec<InlayHint>,
    },
    Semantic {
        tab: u64,
        revision: u64,
        tokens: Vec<SemToken>,
    },
    /// Something to tell the user.
    Message(String),
}

/// An open document, as the server knows it.
struct Doc {
    version: i32,
    revision: u64,
    text: String,
    /// When to ask for its problems, for a server that waits to be asked.
    pull_at: Option<f64>,
}

pub struct Server {
    pub name: String,
    pub proc: u64,
    pub state: State,
    next_id: i64,
    pending: HashMap<i64, Pending>,
    input: Vec<u8>,
    caps: Value,
    docs: HashMap<String, Doc>,
    stderr: String,
    /// Work it says it is doing (indexing, say), by token.
    progress: HashMap<String, String>,
    /// Files it has reported problems in.
    published: HashSet<String>,
}

impl Server {
    /// The characters after which it offers completions by itself (`.` and the like).
    pub fn completion_triggers(&self) -> Vec<char> {
        chars_of(&self.caps["completionProvider"]["triggerCharacters"])
    }

    pub fn signature_triggers(&self) -> Vec<char> {
        let provider = &self.caps["signatureHelpProvider"];
        if provider.is_null() {
            return Vec::new();
        }
        let mut chars = chars_of(&provider["triggerCharacters"]);
        chars.extend(chars_of(&provider["retriggerCharacters"]));
        if chars.is_empty() {
            chars = vec!['(', ','];
        }
        chars
    }

    pub fn can(&self, capability: &str) -> bool {
        let value = &self.caps[capability];
        !value.is_null() && *value != Value::Bool(false)
    }

    /// What it is busy with, for the status bar.
    pub fn busy(&self) -> Option<&str> {
        self.progress.values().next().map(String::as_str)
    }

    /// Whether it colours a whole file on request, in a way ride can read.
    pub fn colours(&self) -> bool {
        let provider = &self.caps["semanticTokensProvider"];
        let full = &provider["full"];
        provider["legend"]["tokenTypes"].is_array() && !full.is_null() && *full != false
    }

    fn resolves_completions(&self) -> bool {
        self.caps["completionProvider"]["resolveProvider"] == Value::Bool(true)
    }

    fn resolves_actions(&self) -> bool {
        self.caps["codeActionProvider"]["resolveProvider"] == Value::Bool(true)
    }

    /// Full text on every change, or only what changed.
    fn incremental(&self) -> bool {
        let sync = &self.caps["textDocumentSync"];
        let kind = if sync.is_object() {
            &sync["change"]
        } else {
            sync
        };
        kind.as_u64() == Some(2)
    }

    fn failed_to_start(&self, code: Option<i32>) -> State {
        let text = self.stderr.to_lowercase();
        let missing = code == Some(127)
            || text.contains("not found")
            || text.contains("is not recognized")
            || text.contains("no such file");
        if missing {
            return State::Missing;
        }
        let last = self
            .stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        State::Failed(if last.is_empty() {
            format!("exited with code {}", code.unwrap_or(-1))
        } else {
            last
        })
    }
}

fn chars_of(list: &Value) -> Vec<char> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str()?.chars().next())
        .collect()
}

/// The servers of one connection, and what they found.
#[derive(Default)]
pub struct Lsp {
    pub servers: Vec<Server>,
    /// Problems by file (by `key`).
    diags: HashMap<String, Vec<Diag>>,
    /// Bumped whenever `diags` changes.
    pub diag_gen: u64,
    /// Servers not to offer to install or restart again for now.
    pub dismissed: HashSet<String>,
    /// Servers being installed in a terminal; they are started again when that is done.
    pub installing: HashSet<String>,
    /// Bumped when a server says its hints or colours are out of date everywhere.
    pub refresh_gen: u64,
    out: Vec<Request>,
    root: String,
}

impl Lsp {
    pub fn server(&self, name: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.name == name)
    }

    fn server_mut(&mut self, name: &str) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.name == name)
    }

    pub fn ready(&self, name: &str) -> Option<&Server> {
        self.server(name).filter(|s| s.state == State::Ready)
    }

    pub fn diags_for(&self, path: &str) -> Option<&Vec<Diag>> {
        self.diags.get(&key(path))
    }

    /// Every problem found, by file.
    pub fn all_diags(&self) -> impl Iterator<Item = (&String, &Vec<Diag>)> {
        self.diags.iter()
    }

    /// The messages to hand to the connection.
    pub fn outgoing(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.out)
    }

    /// A server's process was started as `proc`: it is greeted.
    pub fn started(&mut self, name: &str, proc: u64, root: &str) {
        self.servers.retain(|s| s.name != name);
        self.root = root.to_string();
        self.servers.push(Server {
            name: name.to_string(),
            proc,
            state: State::Starting,
            next_id: 1,
            pending: HashMap::new(),
            input: Vec::new(),
            caps: Value::Null,
            docs: HashMap::new(),
            stderr: String::new(),
            progress: HashMap::new(),
            published: HashSet::new(),
        });
        let folder = root.trim_end_matches('/');
        let params = json!({
            "processId": null,
            "clientInfo": { "name": "ride", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": uri(folder),
            "rootPath": folder,
            "workspaceFolders": [{ "uri": uri(folder), "name": base_name(folder) }],
            "capabilities": capabilities(),
        });
        self.request(name, "initialize", params, Pending::Initialize);
    }

    /// Ends every server, for a new folder or a new connection.
    pub fn stop_all(&mut self) {
        for server in std::mem::take(&mut self.servers) {
            self.out.push(Request::ProcClose { proc: server.proc });
        }
        self.diags.clear();
        self.diag_gen += 1;
    }

    /// Forgets a server that stopped, so it can be started again.
    pub fn forget(&mut self, name: &str) {
        if let Some(index) = self.servers.iter().position(|s| s.name == name) {
            let server = self.servers.remove(index);
            self.out.push(Request::ProcClose { proc: server.proc });
            for path in server.published {
                self.diags.remove(&path);
            }
            self.diag_gen += 1;
        }
    }

    fn send(&mut self, name: &str, message: Value) {
        let Some(server) = self.server(name) else {
            return;
        };
        let body = message.to_string();
        let mut data = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        data.extend_from_slice(body.as_bytes());
        self.out.push(Request::ProcInput {
            proc: server.proc,
            data,
        });
    }

    pub fn request(&mut self, name: &str, method: &str, params: Value, pending: Pending) {
        let Some(server) = self.server_mut(name) else {
            return;
        };
        let id = server.next_id;
        server.next_id += 1;
        server.pending.insert(id, pending);
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.send(name, message);
    }

    pub fn notify(&mut self, name: &str, method: &str, params: Value) {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.send(name, message);
    }

    fn reply(&mut self, name: &str, id: Value, result: Value) {
        let message = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        self.send(name, message);
    }

    // ---- documents ----

    /// The editor revision the server last got of a file.
    pub fn synced(&self, name: &str, path: &str) -> Option<u64> {
        self.server(name)?.docs.get(path).map(|d| d.revision)
    }

    /// Tells the server what a file holds now: all of it the first time, after that what
    /// changed. `text` has `\n` line ends.
    pub fn sync(
        &mut self,
        name: &str,
        path: &str,
        language: &str,
        text: String,
        revision: u64,
        now: f64,
    ) {
        let Some(server) = self.server_mut(name).filter(|s| s.state == State::Ready) else {
            return;
        };
        let incremental = server.incremental();
        match server.docs.get_mut(path) {
            None => {
                server.docs.insert(
                    path.to_string(),
                    Doc {
                        version: 1,
                        revision,
                        text: text.clone(),
                        pull_at: Some(now),
                    },
                );
                let params = json!({ "textDocument": {
                    "uri": uri(path), "languageId": language, "version": 1, "text": text,
                }});
                self.notify(name, "textDocument/didOpen", params);
            }
            Some(doc) => {
                if doc.text == text {
                    doc.revision = revision;
                    return;
                }
                let change = if incremental {
                    let (start, end, inserted) = changed_region(&doc.text, &text);
                    json!({ "range": range_json(start, end), "text": inserted })
                } else {
                    json!({ "text": text })
                };
                doc.version += 1;
                doc.revision = revision;
                doc.text = text;
                // Asked a moment after typing stops, not on every key.
                doc.pull_at = Some(now + 0.3);
                let params = json!({
                    "textDocument": { "uri": uri(path), "version": doc.version },
                    "contentChanges": [change],
                });
                self.notify(name, "textDocument/didChange", params);
            }
        }
    }

    /// Asks the servers that wait to be asked for the problems in files changed a moment ago.
    pub fn tick(&mut self, now: f64) {
        let mut due = Vec::new();
        for server in &mut self.servers {
            if server.state != State::Ready || !server.can("diagnosticProvider") {
                continue;
            }
            for (path, doc) in &mut server.docs {
                if doc.pull_at.is_some_and(|at| at <= now) {
                    doc.pull_at = None;
                    due.push((server.name.clone(), path.clone()));
                }
            }
        }
        for (name, path) in due {
            let params = json!({ "textDocument": { "uri": uri(&path) } });
            self.request(
                &name,
                "textDocument/diagnostic",
                params,
                Pending::Diagnostics { path },
            );
        }
    }

    /// Whether anything waits for `tick`: the window has to wake for it.
    pub fn waiting(&self) -> bool {
        self.servers
            .iter()
            .any(|s| s.docs.values().any(|d| d.pull_at.is_some()))
    }

    fn set_diags(&mut self, name: &str, path: &str, diags: Vec<Diag>) {
        let path = key(path);
        if let Some(server) = self.server_mut(name) {
            server.published.insert(path.clone());
        }
        if diags.is_empty() {
            self.diags.remove(&path);
        } else {
            self.diags.insert(path, diags);
        }
        self.diag_gen += 1;
    }

    pub fn saved(&mut self, name: &str, path: &str) {
        if self.synced(name, path).is_some() {
            let params = json!({ "textDocument": { "uri": uri(path) } });
            self.notify(name, "textDocument/didSave", params);
        }
    }

    /// Closes the documents of `name` that aren't in `open` any more.
    pub fn close_others(&mut self, name: &str, open: &HashSet<&str>) {
        let Some(server) = self.server_mut(name) else {
            return;
        };
        let gone: Vec<String> = server
            .docs
            .keys()
            .filter(|p| !open.contains(p.as_str()))
            .cloned()
            .collect();
        for path in gone {
            if let Some(server) = self.server_mut(name) {
                server.docs.remove(&path);
            }
            let params = json!({ "textDocument": { "uri": uri(&path) } });
            self.notify(name, "textDocument/didClose", params);
        }
    }

    // ---- questions ----

    /// Asks for a quick fix's edits, when it came without them.
    pub fn resolve_action(&mut self, name: &str, action: &CodeAction) {
        self.request(
            name,
            "codeAction/resolve",
            action.raw.clone(),
            Pending::ResolveAction,
        );
    }

    pub fn resolve_completion(&mut self, name: &str, tab: u64, item: &Item) {
        if self.server(name).is_some_and(Server::resolves_completions) {
            self.request(
                name,
                "completionItem/resolve",
                item.raw.clone(),
                Pending::Resolve { tab },
            );
        }
    }

    /// Runs what a quick fix or the like names; the server then sends the edits.
    pub fn run_command(&mut self, name: &str, command: &Value) {
        let params = json!({
            "command": command["command"],
            "arguments": command.get("arguments").cloned().unwrap_or(json!([])),
        });
        self.request(name, "workspace/executeCommand", params, Pending::Other);
    }

    pub fn code_actions(
        &mut self,
        name: &str,
        tab: u64,
        path: &str,
        start: LPos,
        end: LPos,
        line: u32,
    ) {
        let diagnostics: Vec<Value> = self
            .diags
            .get(&key(path))
            .into_iter()
            .flatten()
            .filter(|d| d.start.line <= line && d.end.line >= line)
            .map(|d| d.raw.clone())
            .collect();
        let params = json!({
            "textDocument": { "uri": uri(path) },
            "range": range_json(start, end),
            "context": { "diagnostics": diagnostics, "triggerKind": 1 },
        });
        self.request(
            name,
            "textDocument/codeAction",
            params,
            Pending::Actions { tab },
        );
    }

    /// Asks a server something about a place in a file.
    pub fn at(&mut self, name: &str, method: &str, path: &str, at: LPos, pending: Pending) {
        let mut params = json!({
            "textDocument": { "uri": uri(path) },
            "position": { "line": at.line, "character": at.ch },
        });
        match &pending {
            Pending::References { .. } => {
                params["context"] = json!({ "includeDeclaration": true });
            }
            Pending::Rename { name } => params["newName"] = json!(name),
            Pending::Completion { .. } => params["context"] = json!({ "triggerKind": 1 }),
            _ => {}
        }
        self.request(name, method, params, pending);
    }

    /// Asks for the inlay hints of a whole file.
    pub fn inlay_hints(&mut self, name: &str, path: &str, tab: u64, revision: u64, lines: u32) {
        let params = json!({
            "textDocument": { "uri": uri(path) },
            "range": range_json(LPos::default(), LPos { line: lines, ch: 0 }),
        });
        let pending = Pending::InlayHints { tab, revision };
        self.request(name, "textDocument/inlayHint", params, pending);
    }

    /// Asks for the colours of a whole file.
    pub fn semantic_tokens(&mut self, name: &str, path: &str, tab: u64, revision: u64) {
        let params = json!({ "textDocument": { "uri": uri(path) } });
        let pending = Pending::Semantic { tab, revision };
        self.request(name, "textDocument/semanticTokens/full", params, pending);
    }

    /// Asks for completions typed after `trigger`, such as a `.`.
    pub fn complete(
        &mut self,
        name: &str,
        path: &str,
        at: LPos,
        trigger: Option<char>,
        pending: Pending,
    ) {
        let context = match trigger {
            Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string() }),
            None => json!({ "triggerKind": 1 }),
        };
        let params = json!({
            "textDocument": { "uri": uri(path) },
            "position": { "line": at.line, "character": at.ch },
            "context": context,
        });
        self.request(name, "textDocument/completion", params, pending);
    }

    pub fn format(
        &mut self,
        name: &str,
        path: &str,
        tab: u64,
        revision: u64,
        tab_size: usize,
        spaces: bool,
    ) {
        let params = json!({
            "textDocument": { "uri": uri(path) },
            "options": { "tabSize": tab_size, "insertSpaces": spaces },
        });
        self.request(
            name,
            "textDocument/formatting",
            params,
            Pending::Format { tab, revision },
        );
    }

    // ---- what comes back ----

    /// Takes what a server's process sent. `None` when the process isn't a server's.
    pub fn feed(&mut self, proc: u64, resp: &Response) -> Option<Vec<Event>> {
        let index = self.servers.iter().position(|s| s.proc == proc)?;
        let mut events = Vec::new();
        match resp {
            Response::ProcOutput { data } => {
                if let Some(trace) = std::env::var_os("RIDE_LSP_TRACE_FILE") {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(trace)
                    {
                        let _ = f.write_all(data);
                    }
                }
                self.servers[index].input.extend_from_slice(data);
                while let Some(message) = take_message(&mut self.servers[index].input) {
                    let name = self.servers[index].name.clone();
                    self.message(&name, message, &mut events);
                }
            }
            Response::ProcStderr { data } => {
                let stderr = &mut self.servers[index].stderr;
                stderr.push_str(&String::from_utf8_lossy(data));
                if stderr.len() > 8192 {
                    let cut = stderr.len() - 4096;
                    let cut = (cut..stderr.len())
                        .find(|&i| stderr.is_char_boundary(i))
                        .unwrap_or(0);
                    stderr.drain(..cut);
                }
            }
            Response::ProcExit { code } => self.ended(index, *code, &mut events),
            Response::Error { message } => {
                self.servers[index].stderr.push_str(message);
                self.ended(index, None, &mut events);
            }
            _ => {}
        }
        Some(events)
    }

    fn ended(&mut self, index: usize, code: Option<i32>, events: &mut Vec<Event>) {
        let server = &mut self.servers[index];
        server.state = match server.state {
            State::Starting => server.failed_to_start(code),
            _ => State::Failed(format!(
                "stopped{}",
                code.map(|c| format!(" (exit code {c})"))
                    .unwrap_or_default()
            )),
        };
        server.docs.clear();
        server.pending.clear();
        server.progress.clear();
        for path in std::mem::take(&mut server.published) {
            self.diags.remove(&path);
        }
        self.diag_gen += 1;
        events.push(Event::Diagnostics);
    }

    fn message(&mut self, name: &str, message: Value, events: &mut Vec<Event>) {
        let method = message["method"].as_str().map(str::to_string);
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => {
                self.server_request(name, &method, id, &message["params"], events)
            }
            (Some(method), None) => self.notification(name, &method, &message["params"], events),
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else { return };
                let Some(pending) = self.server_mut(name).and_then(|s| s.pending.remove(&id))
                else {
                    return;
                };
                match message.get("error") {
                    Some(error) => self.failed(name, pending, error, events),
                    None => self.answer(name, pending, &message["result"], events),
                }
            }
            (None, None) => {}
        }
    }

    fn failed(&mut self, name: &str, pending: Pending, error: &Value, events: &mut Vec<Event>) {
        let text = error["message"].as_str().unwrap_or("failed").to_string();
        match pending {
            Pending::Initialize => {
                if let Some(server) = self.server_mut(name) {
                    server.state = State::Failed(text);
                }
            }
            Pending::Rename { .. } => events.push(Event::Message(format!("Can't rename: {text}"))),
            Pending::Definition { .. } | Pending::References { .. } => {
                events.push(Event::Message(text))
            }
            Pending::Format { .. } => {
                events.push(Event::Message(format!("Couldn't format: {text}")))
            }
            _ => {}
        }
    }

    fn answer(&mut self, name: &str, pending: Pending, result: &Value, events: &mut Vec<Event>) {
        match pending {
            Pending::Initialize => {
                if let Some(server) = self.server_mut(name) {
                    server.caps = result["capabilities"].clone();
                    server.state = State::Ready;
                }
                self.notify(name, "initialized", json!({}));
            }
            Pending::Completion { tab, at } => {
                let (list, incomplete) = match result {
                    Value::Array(items) => (items.clone(), false),
                    Value::Object(_) => (
                        result["items"].as_array().cloned().unwrap_or_default(),
                        result["isIncomplete"].as_bool().unwrap_or(false),
                    ),
                    _ => (Vec::new(), false),
                };
                let defaults = &result["itemDefaults"];
                let items = list.iter().map(|v| parse_item(v, defaults)).collect();
                events.push(Event::Completion {
                    tab,
                    at,
                    items,
                    incomplete,
                });
            }
            Pending::Resolve { tab } => {
                let mut item = parse_item(result, &Value::Null);
                item.resolved = true;
                events.push(Event::Resolved { tab, item });
            }
            Pending::Hover { tab, at } => {
                let text = markup(&result["contents"]);
                if !text.trim().is_empty() {
                    events.push(Event::Hover {
                        tab,
                        at,
                        text,
                        range: parse_range(&result["range"]),
                    });
                }
            }
            Pending::Signature { tab } => events.push(Event::Signature {
                tab,
                signature: parse_signature(result),
            }),
            Pending::Definition => events.push(Event::Definition {
                locations: parse_locations(result),
            }),
            Pending::References { name } => events.push(Event::References {
                name,
                locations: parse_locations(result),
            }),
            Pending::Rename { name } => {
                let edit = parse_workspace_edit(result);
                if edit.is_empty() {
                    events.push(Event::Message("Nothing to rename here".into()));
                } else {
                    events.push(Event::Edit {
                        edit,
                        what: format!("Renamed to {name}"),
                    });
                }
            }
            Pending::Actions { tab } => {
                let actions = result
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| v.get("disabled").is_none())
                    .map(parse_action)
                    .collect();
                events.push(Event::Actions { tab, actions });
            }
            Pending::ResolveAction => {
                let action = parse_action(result);
                if let Some(edit) = action.edit {
                    events.push(Event::Edit {
                        edit,
                        what: action.title.clone(),
                    });
                }
                if let Some(command) = action.command {
                    self.run_command(name, &command);
                }
            }
            Pending::Format { tab, revision } => events.push(Event::Format {
                tab,
                revision,
                edits: parse_edits(result),
            }),
            Pending::Diagnostics { path } => {
                // "unchanged" leaves what was found before.
                if result["kind"] == "full" {
                    let diags = result["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(parse_diag)
                        .collect();
                    self.set_diags(name, &path, diags);
                    events.push(Event::Diagnostics);
                }
            }
            Pending::InlayHints { tab, revision } => {
                let hints = result
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(parse_hint)
                    .collect();
                events.push(Event::InlayHints {
                    tab,
                    revision,
                    hints,
                });
            }
            Pending::Semantic { tab, revision } => {
                let Some(server) = self.server(name) else {
                    return;
                };
                let legend = &server.caps["semanticTokensProvider"]["legend"];
                let tokens = decode_tokens(&result["data"], legend);
                events.push(Event::Semantic {
                    tab,
                    revision,
                    tokens,
                });
            }
            Pending::Other => {}
        }
    }

    /// Runs a quick fix: its edits, then its command; one without either is asked for them.
    pub fn apply_action(&mut self, name: &str, action: &CodeAction, events: &mut Vec<Event>) {
        if action.edit.is_none() && action.command.is_none() {
            if self.server(name).is_some_and(Server::resolves_actions) {
                self.resolve_action(name, action);
            }
            return;
        }
        if let Some(edit) = &action.edit {
            events.push(Event::Edit {
                edit: edit.clone(),
                what: action.title.clone(),
            });
        }
        if let Some(command) = &action.command {
            self.run_command(name, command);
        }
    }

    fn server_request(
        &mut self,
        name: &str,
        method: &str,
        id: Value,
        params: &Value,
        events: &mut Vec<Event>,
    ) {
        let result = match method {
            "workspace/configuration" => {
                let items = params["items"].as_array().cloned().unwrap_or_default();
                Value::Array(items.iter().map(|item| configuration(item)).collect())
            }
            "workspace/applyEdit" => {
                let edit = parse_workspace_edit(&params["edit"]);
                let what = params["label"]
                    .as_str()
                    .unwrap_or("Applied an edit")
                    .to_string();
                events.push(Event::Edit { edit, what });
                json!({ "applied": true })
            }
            "workspace/diagnostic/refresh" => {
                // Something changed that may change the problems in every open file.
                if let Some(server) = self.server_mut(name) {
                    for doc in server.docs.values_mut() {
                        doc.pull_at = Some(0.0);
                    }
                }
                Value::Null
            }
            "workspace/workspaceFolders" => {
                let folder = self.root.trim_end_matches('/').to_string();
                json!([{ "uri": uri(&folder), "name": base_name(&folder) }])
            }
            "workspace/semanticTokens/refresh" | "workspace/inlayHint/refresh" => {
                self.refresh_gen += 1;
                Value::Null
            }
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create"
            | "window/showMessageRequest"
            | "workspace/codeLens/refresh" => Value::Null,
            _ => {
                let message = json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": format!("{method} isn't supported") },
                });
                self.send(name, message);
                return;
            }
        };
        self.reply(name, id, result);
    }

    fn notification(&mut self, name: &str, method: &str, params: &Value, events: &mut Vec<Event>) {
        match method {
            "textDocument/publishDiagnostics" => {
                let Some(path) = params["uri"].as_str().and_then(path_of) else {
                    return;
                };
                let diags: Vec<Diag> = params["diagnostics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(parse_diag)
                    .collect();
                self.set_diags(name, &path, diags);
                events.push(Event::Diagnostics);
            }
            "$/progress" => {
                let token = params["token"].to_string();
                let value = &params["value"];
                let Some(server) = self.server_mut(name) else {
                    return;
                };
                match value["kind"].as_str() {
                    Some("end") => {
                        server.progress.remove(&token);
                    }
                    Some(kind @ ("begin" | "report")) => {
                        let title = if kind == "begin" {
                            value["title"].as_str().unwrap_or("").to_string()
                        } else {
                            server
                                .progress
                                .get(&token)
                                .and_then(|t| t.split(" · ").next())
                                .unwrap_or("")
                                .to_string()
                        };
                        let mut text = title;
                        if let Some(message) = value["message"].as_str().filter(|m| !m.is_empty()) {
                            text = format!("{text} · {message}");
                        }
                        if let Some(percent) = value["percentage"].as_u64() {
                            text = format!("{text} {percent}%");
                        }
                        server.progress.insert(token, text);
                    }
                    _ => {}
                }
            }
            "window/showMessage" => {
                // Errors and warnings only; the rest is chatter.
                if params["type"].as_u64().is_some_and(|t| t <= 2) {
                    let text = params["message"].as_str().unwrap_or("").trim();
                    if !text.is_empty() {
                        events.push(Event::Message(format!("{name}: {text}")));
                    }
                }
            }
            _ => {}
        }
    }
}

/// A setting a server asks the editor for. Only TypeScript's inlay hints are set, as VS
/// Code users have them; anything else is left to the server's defaults.
fn configuration(item: &Value) -> Value {
    match item["section"].as_str().unwrap_or("") {
        "typescript" | "javascript" | "js/ts" => json!({
            "inlayHints": {
                "parameterNames": { "enabled": "all", "suppressWhenArgumentMatchesName": true },
                "parameterTypes": { "enabled": false },
                "variableTypes": { "enabled": true, "suppressWhenTypeMatchesName": true },
                "propertyDeclarationTypes": { "enabled": true },
                "functionLikeReturnTypes": { "enabled": true },
                "enumMemberValues": { "enabled": true },
            },
        }),
        _ => Value::Null,
    }
}

/// What ride tells servers it can do.
fn capabilities() -> Value {
    json!({
        "general": { "positionEncodings": ["utf-16"] },
        "workspace": {
            "applyEdit": true,
            "workspaceEdit": { "documentChanges": true },
            "configuration": true,
            "workspaceFolders": true,
            "didChangeConfiguration": { "dynamicRegistration": false },
            "diagnostics": { "refreshSupport": true },
            "semanticTokens": { "refreshSupport": true },
            "inlayHint": { "refreshSupport": true },
        },
        "window": { "workDoneProgress": true, "showMessage": {} },
        "textDocument": {
            "synchronization": { "didSave": true, "dynamicRegistration": false },
            "completion": {
                "completionItem": {
                    "snippetSupport": true,
                    "documentationFormat": ["markdown", "plaintext"],
                    "insertReplaceSupport": true,
                    "labelDetailsSupport": true,
                    "resolveSupport": { "properties": ["documentation", "detail", "additionalTextEdits"] },
                },
                "completionList": { "itemDefaults": ["editRange", "insertTextFormat", "data"] },
                "contextSupport": true,
            },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "signatureHelp": {
                "signatureInformation": {
                    "documentationFormat": ["markdown", "plaintext"],
                    "parameterInformation": { "labelOffsetSupport": true },
                    "activeParameterSupport": true,
                },
            },
            "definition": { "linkSupport": true },
            "references": {},
            "rename": { "prepareSupport": false },
            "formatting": {},
            "codeAction": {
                "codeActionLiteralSupport": { "codeActionKind": { "valueSet": [
                    "", "quickfix", "refactor", "refactor.extract", "refactor.inline",
                    "refactor.rewrite", "source", "source.organizeImports", "source.fixAll",
                ]}},
                "isPreferredSupport": true,
                "disabledSupport": true,
                "dataSupport": true,
                "resolveSupport": { "properties": ["edit"] },
            },
            "publishDiagnostics": { "relatedInformation": false, "versionSupport": false },
            "diagnostic": { "dynamicRegistration": false, "relatedDocumentSupport": false },
            "inlayHint": { "dynamicRegistration": false },
            "semanticTokens": {
                "dynamicRegistration": false,
                "requests": { "range": false, "full": { "delta": false } },
                "tokenTypes": [
                    "namespace", "type", "class", "enum", "interface", "struct", "typeParameter",
                    "parameter", "variable", "property", "enumMember", "event", "function",
                    "method", "macro", "keyword", "modifier", "comment", "string", "number",
                    "regexp", "operator", "decorator",
                ],
                "tokenModifiers": [
                    "declaration", "definition", "readonly", "static", "deprecated", "abstract",
                    "async", "modification", "documentation", "defaultLibrary",
                ],
                "formats": ["relative"],
                "overlappingTokenSupport": false,
                "multilineTokenSupport": false,
                "augmentsSyntaxTokens": true,
            },
        },
    })
}

/// One whole message off the front of `input`, if it has all arrived.
fn take_message(input: &mut Vec<u8>) -> Option<Value> {
    loop {
        let end = input.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&input[..end]).to_lowercase();
        let length = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|n| n.trim().parse::<usize>().ok());
        let Some(length) = length else {
            // Not a header: something the server printed on its output. Skip it.
            input.drain(..end + 4);
            continue;
        };
        if input.len() < end + 4 + length {
            return None;
        }
        let body: Vec<u8> = input.drain(..end + 4 + length).skip(end + 4).collect();
        if let Ok(value) = serde_json::from_slice(&body) {
            return Some(value);
        }
    }
}

// ---- places, paths and text ----

/// The char column of a UTF-16 offset into `line`.
pub fn col_of(line: &str, ch: u32) -> usize {
    let mut units = 0;
    for (index, c) in line.chars().enumerate() {
        if units >= ch as usize {
            return index;
        }
        units += c.len_utf16();
    }
    line.chars().count()
}

/// The UTF-16 offset of a char column of `line`.
pub fn ch_of(line: &str, col: usize) -> u32 {
    line.chars().take(col).map(|c| c.len_utf16() as u32).sum()
}

pub fn lpos(line_text: &str, pos: Pos) -> LPos {
    LPos {
        line: pos.line as u32,
        ch: ch_of(line_text, pos.col),
    }
}

/// A file path as a `file://` URI.
pub fn uri(path: &str) -> String {
    let mut out = String::from("file://");
    if !path.starts_with('/') {
        // A Windows path, "C:/…".
        out.push('/');
    }
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The path a `file://` URI names.
pub fn path_of(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                i += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    let path = String::from_utf8_lossy(&decoded).into_owned();
    // "/c:/Users" is the Windows path "C:/Users".
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        return Some(format!(
            "{}{}",
            (b[1] as char).to_ascii_uppercase(),
            &path[2..]
        ));
    }
    Some(path)
}

/// How a path is looked up. Windows paths are compared without regard to case: servers
/// there may send them back in lower case.
pub fn key(path: &str) -> String {
    let b = path.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

/// Whether two paths name the same file.
pub fn same_path(a: &str, b: &str) -> bool {
    key(a) == key(b)
}

fn base_name(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or(path)
}

/// The one region where `old` and `new` differ: where it starts and ends in `old`, and
/// what `new` has there.
fn changed_region(old: &str, new: &str) -> (LPos, LPos, String) {
    let prefix = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, a), b)| a != b)
        .map_or(old.len().min(new.len()), |((i, _), _)| i);
    // `prefix` is a byte index valid in both, since the text before it is the same.
    let (old_rest, new_rest) = (&old[prefix..], &new[prefix..]);
    let suffix = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>()
        .min(old_rest.len())
        .min(new_rest.len());
    let old_end = old.len() - suffix;
    let inserted = new[prefix..new.len() - suffix].to_string();
    (lpos_at(old, prefix), lpos_at(old, old_end), inserted)
}

/// The place of a byte index in `text`.
fn lpos_at(text: &str, byte: usize) -> LPos {
    let before = &text[..byte];
    let line = before.matches('\n').count() as u32;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    LPos {
        line,
        ch: before[start..].encode_utf16().count() as u32,
    }
}

fn range_json(start: LPos, end: LPos) -> Value {
    json!({
        "start": { "line": start.line, "character": start.ch },
        "end": { "line": end.line, "character": end.ch },
    })
}

fn parse_lpos(v: &Value) -> Option<LPos> {
    Some(LPos {
        line: v["line"].as_u64()? as u32,
        ch: v["character"].as_u64()? as u32,
    })
}

fn parse_range(v: &Value) -> Option<(LPos, LPos)> {
    Some((parse_lpos(&v["start"])?, parse_lpos(&v["end"])?))
}

fn parse_edits(v: &Value) -> Vec<TextEdit> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let (start, end) = parse_range(&e["range"])?;
            Some(TextEdit {
                start,
                end,
                text: e["newText"].as_str()?.to_string(),
            })
        })
        .collect()
}

fn parse_workspace_edit(v: &Value) -> WorkspaceEdit {
    let mut out: WorkspaceEdit = Vec::new();
    let mut add = |uri: &str, edits: Vec<TextEdit>| {
        let Some(path) = path_of(uri) else { return };
        match out.iter_mut().find(|(p, _)| *p == path) {
            Some((_, list)) => list.extend(edits),
            None => out.push((path, edits)),
        }
    };
    if let Some(changes) = v["changes"].as_object() {
        for (uri, edits) in changes {
            add(uri, parse_edits(edits));
        }
    }
    for change in v["documentChanges"].as_array().into_iter().flatten() {
        // Creating, renaming and deleting files is left out.
        if let Some(uri) = change["textDocument"]["uri"].as_str() {
            add(uri, parse_edits(&change["edits"]));
        }
    }
    out.retain(|(_, edits)| !edits.is_empty());
    out
}

fn parse_locations(v: &Value) -> Vec<Location> {
    let list: Vec<&Value> = match v {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![v],
        _ => Vec::new(),
    };
    list.into_iter()
        .filter_map(|l| {
            let (uri, range) = match l["targetUri"].as_str() {
                Some(uri) => {
                    let range = if l["targetSelectionRange"].is_object() {
                        &l["targetSelectionRange"]
                    } else {
                        &l["targetRange"]
                    };
                    (uri, range)
                }
                None => (l["uri"].as_str()?, &l["range"]),
            };
            let (start, end) = parse_range(range)?;
            Some(Location {
                path: path_of(uri)?,
                start,
                end,
            })
        })
        .collect()
}

fn parse_diag(v: &Value) -> Option<Diag> {
    let (start, end) = parse_range(&v["range"])?;
    Some(Diag {
        start,
        end,
        severity: v["severity"].as_u64().unwrap_or(1) as u8,
        message: v["message"].as_str()?.to_string(),
        source: v["source"].as_str().unwrap_or("").to_string(),
        raw: v.clone(),
    })
}

/// Hover and documentation text, as Markdown.
fn markup(v: &Value) -> String {
    match v {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(markup)
            .filter(|p| !p.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(_) => {
            let value = v["value"].as_str().unwrap_or("");
            match v["language"].as_str() {
                Some(language) => format!("```{language}\n{value}\n```"),
                None => value.to_string(),
            }
        }
        _ => String::new(),
    }
}

fn parse_item(v: &Value, defaults: &Value) -> Item {
    let label = v["label"].as_str().unwrap_or("").to_string();
    let mut detail = v["detail"].as_str().unwrap_or("").to_string();
    if detail.is_empty() {
        detail = v["labelDetails"]["description"]
            .as_str()
            .unwrap_or("")
            .to_string();
    }
    let format = v["insertTextFormat"]
        .as_u64()
        .or(defaults["insertTextFormat"].as_u64());
    let edit = &v["textEdit"];
    let (range, insert) = if edit.is_object() {
        let range = if edit["range"].is_object() {
            parse_range(&edit["range"])
        } else {
            parse_range(&edit["insert"])
        };
        (
            range,
            edit["newText"].as_str().unwrap_or(&label).to_string(),
        )
    } else {
        let edit_range = &defaults["editRange"];
        let range = if edit_range["start"].is_object() {
            parse_range(edit_range)
        } else {
            parse_range(&edit_range["insert"])
        };
        let text = v["textEditText"]
            .as_str()
            .or(v["insertText"].as_str())
            .unwrap_or(&label)
            .to_string();
        (range, text)
    };
    let mut raw = v.clone();
    if raw.get("data").is_none() && !defaults["data"].is_null() {
        raw["data"] = defaults["data"].clone();
    }
    Item {
        detail,
        doc: markup(&v["documentation"]),
        kind: v["kind"].as_u64().unwrap_or(0) as u8,
        sort: v["sortText"].as_str().unwrap_or(&label).to_string(),
        filter: v["filterText"].as_str().unwrap_or(&label).to_string(),
        insert,
        snippet: format == Some(2),
        range,
        extra: parse_edits(&v["additionalTextEdits"]),
        raw,
        resolved: false,
        label,
    }
}

fn parse_signature(v: &Value) -> Option<Signature> {
    let signatures = v["signatures"].as_array()?;
    let index = v["activeSignature"].as_u64().unwrap_or(0) as usize;
    let sig = signatures.get(index).or(signatures.first())?;
    let label = sig["label"].as_str()?.to_string();
    let param = sig["activeParameter"]
        .as_u64()
        .or(v["activeParameter"].as_u64())
        .unwrap_or(0) as usize;
    let active = sig["parameters"]
        .as_array()
        .and_then(|params| params.get(param))
        .and_then(|p| match &p["label"] {
            Value::String(text) => {
                let byte = label.find(text.as_str())?;
                let start = label[..byte].chars().count();
                Some((start, start + text.chars().count()))
            }
            Value::Array(offsets) => {
                let (a, b) = (offsets.first()?.as_u64()?, offsets.get(1)?.as_u64()?);
                Some((col_of(&label, a as u32), col_of(&label, b as u32)))
            }
            _ => None,
        });
    let mut doc = markup(&sig["documentation"]);
    let param_doc = sig["parameters"]
        .as_array()
        .and_then(|params| params.get(param))
        .map(|p| markup(&p["documentation"]))
        .unwrap_or_default();
    if !param_doc.trim().is_empty() {
        doc = if doc.trim().is_empty() {
            param_doc
        } else {
            format!("{param_doc}\n\n{doc}")
        };
    }
    Some(Signature { label, active, doc })
}

fn parse_hint(v: &Value) -> Option<InlayHint> {
    let at = parse_lpos(&v["position"])?;
    let mut label = match &v["label"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["value"].as_str()).collect(),
        _ => return None,
    };
    // A type spelled out in full can run across the screen.
    if label.chars().count() > 48 {
        label = label.chars().take(47).collect::<String>() + "…";
    }
    let label = label.replace(['\n', '\r'], " ");
    if label.trim().is_empty() {
        return None;
    }
    let left = if v["paddingLeft"] == true { " " } else { "" };
    let right = if v["paddingRight"] == true { " " } else { "" };
    Some(InlayHint {
        at,
        label: format!("{left}{label}{right}"),
    })
}

/// What a token type is, for its colour; `None` for those the syntax colours already
/// cover (strings, comments, numbers) and those ride doesn't know.
fn sem_kind(name: &str, readonly: bool) -> Option<SemKind> {
    Some(match name {
        "namespace" | "type" | "class" | "enum" | "interface" | "struct" | "typeParameter"
        | "builtinType" | "typeAlias" | "union" | "trait" => SemKind::Type,
        "parameter" | "selfParameter" => SemKind::Param,
        "variable" if readonly => SemKind::Constant,
        "variable" => SemKind::Variable,
        "property" | "field" if readonly => SemKind::Constant,
        "property" | "field" => SemKind::Property,
        "enumMember" | "constParameter" => SemKind::Constant,
        "function" | "method" | "macro" | "decorator" | "attribute" => SemKind::Func,
        "keyword" | "modifier" | "selfKeyword" | "selfTypeKeyword" => SemKind::Keyword,
        _ => return None,
    })
}

/// Reads the server's colours: groups of five numbers, each place given from the one before.
fn decode_tokens(data: &Value, legend: &Value) -> Vec<SemToken> {
    let names: Vec<&str> = legend["tokenTypes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| v.as_str().unwrap_or(""))
        .collect();
    let readonly_bit = legend["tokenModifiers"]
        .as_array()
        .and_then(|m| m.iter().position(|v| v == "readonly"))
        .map(|i| 1u64 << i);
    let numbers: Vec<u64> = data
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect();
    let (mut line, mut start) = (0u32, 0u32);
    let mut tokens = Vec::new();
    for group in numbers.chunks_exact(5) {
        let (delta_line, delta_start) = (group[0] as u32, group[1] as u32);
        if delta_line > 0 {
            line += delta_line;
            start = delta_start;
        } else {
            start += delta_start;
        }
        let name = names.get(group[3] as usize).copied().unwrap_or("");
        let readonly = readonly_bit.is_some_and(|bit| group[4] & bit != 0);
        if let Some(kind) = sem_kind(name, readonly) {
            tokens.push(SemToken {
                line,
                start,
                len: group[2] as u32,
                kind,
            });
        }
    }
    tokens
}

fn parse_action(v: &Value) -> CodeAction {
    // A bare command has a string `command`; a code action may carry a command object.
    let command = match &v["command"] {
        Value::String(_) => Some(v.clone()),
        Value::Object(_) => Some(v["command"].clone()),
        _ => None,
    };
    let edit = v.get("edit").map(parse_workspace_edit);
    CodeAction {
        title: v["title"].as_str().unwrap_or("").to_string(),
        preferred: v["isPreferred"].as_bool().unwrap_or(false),
        edit,
        command,
        raw: v.clone(),
    }
}

/// A snippet with its places filled in by their defaults, and the first place to fill in,
/// as a range of chars in the text.
pub fn expand_snippet(snippet: &str) -> (String, Option<(usize, usize)>) {
    let chars: Vec<char> = snippet.chars().collect();
    let (mut i, mut out, mut stops) = (0, String::new(), Vec::new());
    snippet_part(&chars, &mut i, &mut out, &mut stops, false);
    // $1 first, then the lowest number; $0 (the end) only when there is nothing else.
    let first = stops
        .iter()
        .filter(|(n, _, _)| *n > 0)
        .min_by_key(|(n, start, _)| (*n, *start))
        .or_else(|| stops.iter().find(|(n, _, _)| *n == 0))
        .map(|&(_, start, end)| (start, end));
    (out, first)
}

fn snippet_part(
    chars: &[char],
    i: &mut usize,
    out: &mut String,
    stops: &mut Vec<(u32, usize, usize)>,
    nested: bool,
) {
    let len = |out: &String| out.chars().count();
    while *i < chars.len() {
        let c = chars[*i];
        if c == '\\' && *i + 1 < chars.len() {
            out.push(chars[*i + 1]);
            *i += 2;
            continue;
        }
        if nested && c == '}' {
            return;
        }
        if c == '$' {
            match chars.get(*i + 1) {
                Some(d) if d.is_ascii_digit() => {
                    *i += 1;
                    let start = *i;
                    while *i < chars.len() && chars[*i].is_ascii_digit() {
                        *i += 1;
                    }
                    let n: u32 = chars[start..*i]
                        .iter()
                        .collect::<String>()
                        .parse()
                        .unwrap_or(0);
                    let at = len(out);
                    stops.push((n, at, at));
                    continue;
                }
                Some('{') => {
                    *i += 2;
                    let start = *i;
                    while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '_') {
                        *i += 1;
                    }
                    let name: String = chars[start..*i].iter().collect();
                    let begin = len(out);
                    match chars.get(*i) {
                        Some(':') => {
                            *i += 1;
                            snippet_part(chars, i, out, stops, true);
                        }
                        Some('|') => {
                            // A choice: the first one is taken.
                            *i += 1;
                            let mut first = true;
                            while *i < chars.len() && chars[*i] != '|' {
                                match chars[*i] {
                                    ',' => first = false,
                                    c if first => out.push(c),
                                    _ => {}
                                }
                                *i += 1;
                            }
                        }
                        _ => {}
                    }
                    while *i < chars.len() && chars[*i] != '}' {
                        *i += 1;
                    }
                    *i += 1;
                    if let Ok(n) = name.parse() {
                        stops.push((n, begin, len(out)));
                    }
                    continue;
                }
                Some(c) if c.is_alphabetic() || *c == '_' => {
                    // A variable such as $TM_FILENAME: left empty.
                    *i += 1;
                    while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '_') {
                        *i += 1;
                    }
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
        *i += 1;
    }
}

/// `text` with `edits` made in it: for files changed without being open.
pub fn apply_to_text(text: &str, edits: &[TextEdit]) -> String {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let offset = |at: LPos| -> usize {
        let Some(&start) = starts.get(at.line as usize) else {
            return text.len();
        };
        let end = starts
            .get(at.line as usize + 1)
            .map_or(text.len(), |next| next - 1);
        let line = text[start..end].trim_end_matches('\r');
        let col = col_of(line, at.ch);
        start + line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    };
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| (offset(e.start), offset(e.end), e.text.as_str()))
        .collect();
    spans.sort_by_key(|(a, b, _)| (*a, *b));
    let crlf = text.contains("\r\n");
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, new) in spans {
        if start < at {
            continue;
        }
        out.push_str(&text[at..start]);
        if crlf {
            out.push_str(&new.replace("\r\n", "\n").replace('\n', "\r\n"));
        } else {
            out.push_str(new);
        }
        at = end.max(start);
    }
    out.push_str(&text[at..]);
    out
}

/// The language id servers expect for a file.
pub fn language_id(path: &str) -> String {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
    match ext.as_str() {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        "rs" => "rust",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "sh" | "bash" => "shellscript",
        "yml" | "yaml" => "yaml",
        "md" => "markdown",
        "htm" | "html" => "html",
        "cs" => "csharp",
        "rb" => "ruby",
        "kt" | "kts" => "kotlin",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_read_whole_and_in_order() {
        let mut input = Vec::new();
        for body in [r#"{"id":1,"result":null}"#, r#"{"method":"x"}"#] {
            input.extend(format!("Content-Length: {}\r\n\r\n{body}", body.len()).bytes());
        }
        let half = input.split_off(30);
        assert!(take_message(&mut input).is_none());
        input.extend(half);
        assert_eq!(take_message(&mut input).unwrap()["id"], 1);
        assert_eq!(take_message(&mut input).unwrap()["method"], "x");
        assert!(input.is_empty());
    }

    #[test]
    fn colours_are_read_from_their_relative_places() {
        let legend = json!({
            "tokenTypes": ["variable", "parameter", "comment", "function"],
            "tokenModifiers": ["declaration", "readonly"],
        });
        // A parameter at 2:4, a constant 6 further on, a comment (left to the syntax
        // colours), then a function on line 5.
        let data = json!([2, 4, 3, 1, 0, 0, 6, 5, 0, 2, 0, 8, 4, 2, 0, 3, 1, 6, 3, 0]);
        let tokens = decode_tokens(&data, &legend);
        assert_eq!(
            tokens,
            vec![
                SemToken {
                    line: 2,
                    start: 4,
                    len: 3,
                    kind: SemKind::Param
                },
                SemToken {
                    line: 2,
                    start: 10,
                    len: 5,
                    kind: SemKind::Constant
                },
                SemToken {
                    line: 5,
                    start: 1,
                    len: 6,
                    kind: SemKind::Func
                },
            ]
        );
    }

    #[test]
    fn hints_take_either_label_and_their_padding() {
        let hint = parse_hint(&json!({
            "position": { "line": 1, "character": 5 },
            "label": [{ "value": "count" }, { "value": ":" }],
            "paddingRight": true,
        }))
        .unwrap();
        assert_eq!(hint.label, "count: ");
        assert_eq!(hint.at, LPos { line: 1, ch: 5 });
        let typed = parse_hint(&json!({
            "position": { "line": 0, "character": 3 }, "label": ": number", "kind": 1,
        }))
        .unwrap();
        assert_eq!(typed.label, ": number");
    }

    #[test]
    fn places_count_utf16_units() {
        let line = "a😀b";
        assert_eq!(ch_of(line, 2), 3);
        assert_eq!(col_of(line, 3), 2);
        assert_eq!(col_of(line, 99), 3);
    }

    #[test]
    fn uris_round_trip() {
        assert_eq!(uri("/root/my file.lua"), "file:///root/my%20file.lua");
        assert_eq!(
            path_of("file:///root/my%20file.lua").unwrap(),
            "/root/my file.lua"
        );
        assert_eq!(uri("C:/Users/a.ts"), "file:///C:/Users/a.ts");
        assert_eq!(path_of("file:///c%3A/Users/a.ts").unwrap(), "C:/Users/a.ts");
    }

    #[test]
    fn a_change_is_sent_as_the_region_that_changed() {
        let (start, end, text) = changed_region("let a = 1;\nfoo();", "let a = 12;\nfoo();");
        assert_eq!(
            (start, end, text.as_str()),
            (LPos { line: 0, ch: 9 }, LPos { line: 0, ch: 9 }, "2")
        );
        let (start, end, text) = changed_region("ab\ncd\nef", "ab\nef");
        assert_eq!(
            (start, end, text.as_str()),
            (LPos { line: 1, ch: 0 }, LPos { line: 2, ch: 0 }, "")
        );
        // A doubled letter: the region is still a valid one.
        let (start, end, text) = changed_region("aa", "aaa");
        assert_eq!(
            (start, end, text.as_str()),
            (LPos { line: 0, ch: 2 }, LPos { line: 0, ch: 2 }, "a")
        );
    }

    #[test]
    fn snippets_fill_in_their_defaults() {
        let (text, first) = expand_snippet("greet(${1:name}, ${2|a,b|})$0");
        assert_eq!(text, "greet(name, a)");
        assert_eq!(first, Some((6, 10)));
        let (text, first) = expand_snippet("fn $1() {\n\t$0\n}");
        assert_eq!(text, "fn () {\n\t\n}");
        assert_eq!(first, Some((3, 3)));
        assert_eq!(expand_snippet("a\\$b").0, "a$b");
        assert_eq!(expand_snippet("x${TM_FILENAME}y").0, "xy");
    }

    #[test]
    fn completions_take_their_edit_or_defaults() {
        let defaults = json!({ "editRange": { "start": { "line": 0, "character": 1 }, "end": { "line": 0, "character": 3 } } });
        let item = parse_item(&json!({ "label": "value", "kind": 6 }), &defaults);
        assert_eq!(item.insert, "value");
        assert_eq!(item.range.unwrap().0.ch, 1);
        let item = parse_item(
            &json!({ "label": "x", "textEdit": { "newText": "x()", "insert": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } }, "replace": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 4 } } } }),
            &Value::Null,
        );
        assert_eq!((item.insert.as_str(), item.range.unwrap().1.ch), ("x()", 1));
    }

    #[test]
    fn edits_are_made_in_text_on_disk() {
        let edit = |l1, c1, l2, c2, text: &str| TextEdit {
            start: LPos { line: l1, ch: c1 },
            end: LPos { line: l2, ch: c2 },
            text: text.into(),
        };
        let text = "local foo = 1\r\nprint(foo)\r\n";
        let edits = [
            edit(1, 6, 1, 9, "bar"),
            edit(0, 6, 0, 9, "bar"),
            edit(2, 0, 2, 0, "x\ny"),
        ];
        assert_eq!(
            apply_to_text(text, &edits),
            "local bar = 1\r\nprint(bar)\r\nx\r\ny"
        );
    }

    #[test]
    fn workspace_edits_come_from_either_form() {
        let edit = parse_workspace_edit(&json!({
            "documentChanges": [{ "textDocument": { "uri": "file:///a.ts", "version": 1 }, "edits": [
                { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } }, "newText": "b" }
            ]}],
            "changes": { "file:///c.ts": [] },
        }));
        assert_eq!(edit.len(), 1);
        assert_eq!(edit[0].0, "/a.ts");
    }

    /// Talks to a real language server: `RIDE_LSP_TEST` is the command that starts one for
    /// TypeScript (typescript-language-server --stdio). Run with `--ignored`.
    #[test]
    #[ignore]
    fn a_real_server_answers() {
        use std::io::{Read, Write};
        use std::process::{Command, Stdio};
        use std::sync::mpsc::channel;
        use std::time::{Duration, Instant};

        let command = std::env::var("RIDE_LSP_TEST").expect("RIDE_LSP_TEST names the server");
        // RIDE_LSP_TEST_DIR: where to put the test's project (TypeScript's server wants the
        // `typescript` package to be found from there).
        let base = std::env::var_os("RIDE_LSP_TEST_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("ride-lsp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.to_string_lossy().replace('\\', "/");
        let path = format!("{root}/a.ts");
        let text = "function greet(name: string): string {\n  return name;\n}\ngreet(\"x\");\nconst value = 1;\nconst wrong: number = \"a\";\nval";
        std::fs::write(&path, text).unwrap();
        std::fs::write(format!("{root}/tsconfig.json"), "{}").unwrap();

        let mut parts = command.split_whitespace();
        let mut child = Command::new(parts.next().unwrap())
            .args(parts)
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            while let Ok(n @ 1..) = stdout.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        let mut lsp = Lsp::default();
        let mut pump = |lsp: &mut Lsp, until: &mut dyn FnMut(&Lsp, &Event) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                lsp.tick(f64::MAX);
                for request in lsp.outgoing() {
                    if let Request::ProcInput { data, .. } = request {
                        stdin.write_all(&data).unwrap();
                        stdin.flush().unwrap();
                    }
                }
                assert!(Instant::now() < deadline, "no answer in time");
                let Ok(data) = rx.recv_timeout(Duration::from_millis(200)) else {
                    if until(lsp, &Event::Message(String::new())) {
                        return;
                    }
                    continue;
                };
                if std::env::var_os("RIDE_LSP_TRACE").is_some() {
                    eprintln!("<< {}", String::from_utf8_lossy(&data));
                }
                let events = lsp.feed(7, &Response::ProcOutput { data }).unwrap();
                for event in &events {
                    if until(lsp, event) {
                        return;
                    }
                }
                if until(lsp, &Event::Message(String::new())) {
                    return;
                }
            }
        };

        lsp.started("ts", 7, &root);
        eprintln!("stage 0");
        pump(&mut lsp, &mut |lsp, _| lsp.ready("ts").is_some());
        lsp.sync("ts", &path, "typescript", text.to_string(), 1, 0.0);

        // Problems: the string given to a number.
        eprintln!("stage 1");
        pump(&mut lsp, &mut |lsp, _| {
            lsp.diags_for(&path)
                .is_some_and(|d| d.iter().any(|d| d.start.line == 5))
        });

        // Completion after "val".
        lsp.complete(
            "ts",
            &path,
            LPos { line: 6, ch: 3 },
            None,
            Pending::Completion {
                tab: 1,
                at: Pos::new(6, 3),
            },
        );
        let mut labels = Vec::new();
        eprintln!("stage 2");
        pump(&mut lsp, &mut |_, e| match e {
            Event::Completion { items, .. } => {
                labels = items.iter().map(|i| i.label.clone()).collect();
                true
            }
            _ => false,
        });
        assert!(labels.iter().any(|l| l == "value"), "{labels:?}");

        // Hover over the call.
        lsp.at(
            "ts",
            "textDocument/hover",
            &path,
            LPos { line: 3, ch: 1 },
            Pending::Hover {
                tab: 1,
                at: Pos::new(3, 1),
            },
        );
        let mut hover = String::new();
        eprintln!("stage 3");
        pump(&mut lsp, &mut |_, e| match e {
            Event::Hover { text, .. } => {
                hover = text.clone();
                true
            }
            _ => false,
        });
        assert!(hover.contains("greet(name: string)"), "{hover}");

        // Definition of the call: the function on line 0.
        lsp.at(
            "ts",
            "textDocument/definition",
            &path,
            LPos { line: 3, ch: 1 },
            Pending::Definition,
        );
        let mut found = Vec::new();
        eprintln!("stage 4");
        pump(&mut lsp, &mut |_, e| match e {
            Event::Definition { locations, .. } => {
                found = locations.clone();
                true
            }
            _ => false,
        });
        assert_eq!(
            found.first().map(|l| (key(&l.path), l.start.line)),
            Some((key(&path), 0))
        );

        // References and rename of `greet`: the declaration and the call.
        lsp.at(
            "ts",
            "textDocument/references",
            &path,
            LPos { line: 0, ch: 10 },
            Pending::References {
                name: "greet".into(),
            },
        );
        let mut count = 0;
        eprintln!("stage 5");
        pump(&mut lsp, &mut |_, e| match e {
            Event::References { locations, .. } => {
                count = locations.len();
                true
            }
            _ => false,
        });
        assert_eq!(count, 2);
        lsp.at(
            "ts",
            "textDocument/rename",
            &path,
            LPos { line: 0, ch: 10 },
            Pending::Rename {
                name: "hello".into(),
            },
        );
        let mut edits = Vec::new();
        eprintln!("stage 6");
        pump(&mut lsp, &mut |_, e| match e {
            Event::Edit { edit, .. } => {
                edits = edit.clone();
                true
            }
            _ => false,
        });
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].1.len(), 2);
        assert!(edits[0].1.iter().all(|e| e.text == "hello"));

        // An edit sent as a change: the problem goes away.
        let fixed = text.replace("number = \"a\"", "string = \"a\"");
        lsp.sync("ts", &path, "typescript", fixed, 2, 0.0);
        eprintln!("stage 7");
        pump(&mut lsp, &mut |lsp, _| {
            !lsp.diags_for(&path)
                .is_some_and(|d| d.iter().any(|d| d.start.line == 5))
        });

        // Signature help inside the call.
        lsp.at(
            "ts",
            "textDocument/signatureHelp",
            &path,
            LPos { line: 3, ch: 6 },
            Pending::Signature { tab: 1 },
        );
        let mut signature = None;
        eprintln!("stage 8");
        pump(&mut lsp, &mut |_, e| match e {
            Event::Signature { signature: s, .. } => {
                signature = s.clone();
                true
            }
            _ => false,
        });
        let signature = signature.expect("a signature");
        let (a, b) = signature.active.unwrap();
        let param: String = signature.label.chars().skip(a).take(b - a).collect();
        assert_eq!(param, "name: string");

        // Colours: `name` in `return name;` is a parameter.
        if lsp.ready("ts").is_some_and(Server::colours) {
            lsp.semantic_tokens("ts", &path, 1, 2);
            let mut tokens = Vec::new();
            eprintln!("stage 9");
            pump(&mut lsp, &mut |_, e| match e {
                Event::Semantic { tokens: t, .. } => {
                    tokens = t.clone();
                    true
                }
                _ => false,
            });
            assert!(
                tokens
                    .iter()
                    .any(|t| t.line == 1 && t.start == 9 && t.kind == SemKind::Param),
                "{tokens:?}"
            );
        } else {
            eprintln!("the server gives no colours");
        }

        // Inlay hints, when the server gives them.
        if lsp.ready("ts").is_some_and(|s| s.can("inlayHintProvider")) {
            lsp.inlay_hints("ts", &path, 1, 2, 7);
            let mut hints = Vec::new();
            eprintln!("stage 10");
            pump(&mut lsp, &mut |_, e| match e {
                Event::InlayHints { hints: h, .. } => {
                    hints = h.clone();
                    true
                }
                _ => false,
            });
            // The name of the parameter `"x"` is given for, from the settings ride answers with.
            assert!(
                hints
                    .iter()
                    .any(|h| h.at == LPos { line: 3, ch: 6 } && h.label == "name: "),
                "{hints:?}"
            );
        } else {
            eprintln!("the server gives no inlay hints");
        }

        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
