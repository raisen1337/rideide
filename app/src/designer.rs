//! MCP servers for Claude, and the Designer view.
//!
//! MCP servers set in Settings are handed to every Claude ride starts. One that listens on
//! this computer (a design tool, say) is reached from the server Claude runs on through an
//! SSH tunnel the other way round (`ssh -R`), so Claude there finds it at the same address.
//!
//! The Designer view is a set of design sessions, each a Claude chat with the Tribib design
//! editor's MCP tools and a Tribib document of its own, beside a live picture of the canvas,
//! read from Tribib on this computer.

use super::*;
use std::sync::mpsc::{self as channels};

/// One design session: a Claude chat that designs in a Tribib document.
pub(super) struct Design {
    pub(super) uid: u64,
    pub(super) name: String,
    pub(super) chat: Chat,
    pub(super) started_with: Option<String>,
    /// The Tribib document it works on: its id and name.
    pub(super) document: Option<(String, String)>,
}

/// What Claude is told in a design session.
fn design_prompt() -> String {
    "You are the designer in ride's Designer view. You design user interfaces on the Tribib \
canvas through the `tribib` MCP server (its tools are mcp__tribib__*). The user watches the \
canvas live beside this chat, so build in visual order and keep each build call small. Follow \
the server's own instructions: call get_editor_state first, read library_memory before \
designing, create with build, and look at your work with get_screenshot before saying it is \
done. Each message may start with a [Tribib document: …] line: make sure that document is the \
one the tools act on (select_document) before changing anything. The project the design is \
for is the current directory: read its code to match what exists, and when the user asks for \
the design as code, use get_code and write it into the project's files."
        .to_string()
}

/// An MCP server that listens on this computer, reached from the host through a tunnel.
pub(super) struct Tunnel {
    host: String,
    /// The port here, and the one it is reached at on the host.
    local: u16,
    remote: u16,
    child: Option<std::process::Child>,
    tries: u32,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The port of a URL on this computer (127.0.0.1 or localhost), if it is one.
fn local_port(url: &str) -> Option<u16> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split('/').next()?;
    let (host, port) = authority.rsplit_once(':')?;
    matches!(host, "127.0.0.1" | "localhost")
        .then(|| port.parse().ok())
        .flatten()
}

/// The same URL, at another port.
fn with_port(url: &str, port: u16) -> String {
    let scheme_end = url.find("://").map_or(0, |i| i + 3);
    let (scheme, rest) = url.split_at(scheme_end);
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    format!("{scheme}{host}:{port}{path}")
}

/// What `--mcp-config` gets: the servers by name, at the address Claude can reach.
fn mcp_json(servers: &[(String, String)]) -> Option<String> {
    if servers.is_empty() {
        return None;
    }
    let mut map = serde_json::Map::new();
    for (name, url) in servers {
        map.insert(
            name.clone(),
            serde_json::json!({ "type": "http", "url": url }),
        );
    }
    Some(serde_json::json!({ "mcpServers": map }).to_string())
}

// ---- the canvas, read from Tribib ----

/// What one look at Tribib found.
struct Look {
    documents: Vec<(String, String, bool)>,
    page: String,
    png: Option<Vec<u8>>,
}

/// The picture of the canvas, kept fresh while the Designer view is open.
pub(super) struct Canvas {
    texture: Option<egui::TextureHandle>,
    size: egui::Vec2,
    documents: Vec<(String, String, bool)>,
    page: String,
    error: Option<String>,
    /// A look under way, and when the last one ended.
    pending: Option<channels::Receiver<Result<Look, String>>>,
    last: f64,
    pub(super) live: bool,
    /// Fit the picture to the panel, or show it at its size.
    fit: bool,
    /// Whether a look takes a picture: not while Tribib itself is shown.
    shots: bool,
}

impl Default for Canvas {
    fn default() -> Self {
        Self {
            texture: None,
            size: egui::Vec2::ZERO,
            documents: Vec::new(),
            page: String::new(),
            error: None,
            pending: None,
            last: f64::NEG_INFINITY,
            live: true,
            fit: true,
            shots: true,
        }
    }
}

/// Calls a tool of an MCP server over HTTP, and returns what it answered.
fn call_tool(url: &str, tool: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": tool, "arguments": args },
    });
    // Straight to it: a system proxy has no business with a server on this computer.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into();
    let mut response = agent
        .post(url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .map_err(|e| e.to_string())?;
    let text = response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    // A stream of events, or plain JSON.
    let json = text
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .unwrap_or(&text);
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if let Some(error) = value.get("error") {
        return Err(error["message"].as_str().unwrap_or("failed").to_string());
    }
    if value["result"]["isError"] == true {
        let text = value["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("failed");
        return Err(text.to_string());
    }
    Ok(value["result"].clone())
}

/// Reads which documents are open, and a picture of the current page.
fn look(url: &str, select: Option<String>, shot: bool) -> Result<Look, String> {
    if let Some(document) = select {
        call_tool(
            url,
            "select_document",
            serde_json::json!({ "document": document }),
        )?;
    }
    let state = call_tool(url, "get_editor_state", serde_json::json!({}))?;
    let text = state["content"][0]["text"].as_str().unwrap_or("{}");
    let state: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let documents = state["documents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some((
                d["documentId"].as_str().or(d["id"].as_str())?.to_string(),
                d["name"].as_str().unwrap_or("Untitled").to_string(),
                d["active"].as_bool().unwrap_or(false),
            ))
        })
        .collect();
    let page = &state["active"]["page"];
    let png = match page["id"].as_str() {
        Some(id) if shot && page["children"].as_u64() != Some(0) => {
            let shot = call_tool(
                url,
                "get_screenshot",
                serde_json::json!({ "nodeId": id, "scale": 0.5 }),
            )?;
            shot["content"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|c| c["type"] == "image")
                .and_then(|c| c["data"].as_str())
                .and_then(|data| chat::base64_decode(data))
        }
        _ => None,
    };
    Ok(Look {
        documents,
        page: page["name"].as_str().unwrap_or("").to_string(),
        png,
    })
}

impl Canvas {
    /// Starts a look at Tribib, unless one is under way.
    fn refresh(&mut self, ctx: &Context, url: String, select: Option<String>) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = channels::channel();
        let ctx = ctx.clone();
        let shot = self.shots;
        std::thread::spawn(move || {
            let _ = tx.send(look(&url, select, shot));
            ctx.request_repaint();
        });
        self.pending = Some(rx);
    }

    fn take(&mut self, ctx: &Context, now: f64) {
        let Some(result) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        self.pending = None;
        self.last = now;
        match result {
            Ok(look) => {
                self.error = None;
                self.documents = look.documents;
                self.page = look.page;
                match look.png.and_then(|png| image::load_from_memory(&png).ok()) {
                    Some(picture) => {
                        let rgba = picture.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                        self.size = vec2(size[0] as f32, size[1] as f32);
                        match &mut self.texture {
                            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                            None => {
                                self.texture = Some(ctx.load_texture(
                                    "tribib_canvas",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ))
                            }
                        }
                    }
                    None if self.shots => self.texture = None,
                    None => {}
                }
            }
            Err(error) => self.error = Some(error),
        }
    }
}

/// Where Tribib's MCP server is: the one named so in Settings, or its usual address.
fn tribib_url() -> String {
    settings::with(|s| {
        s.mcp_servers
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case("tribib"))
            .map(|m| m.url.clone())
    })
    .unwrap_or_else(|| "http://127.0.0.1:3848/mcp".into())
}

/// Where Tribib's installer puts the app, for this user.
fn tribib_exe() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(
        PathBuf::from(base)
            .join("Programs")
            .join("tribib-desktop")
            .join("Tribib.exe"),
    )
}

fn tribib_installed() -> bool {
    tribib_exe().is_some_and(|exe| exe.exists())
}

/// Where Tribib publishes its Windows releases; its own app updates from here too.
const TRIBIB_RELEASES: &str = "https://s3.tribib.com/desktop/win";

/// Tribib being installed: bytes downloaded so far, of how many, and how it ended.
pub(super) struct TribibInstall {
    done: Arc<std::sync::atomic::AtomicU64>,
    total: Arc<std::sync::atomic::AtomicU64>,
    result: channels::Receiver<Result<(), String>>,
}

/// Downloads Tribib's latest installer from Tribib, checks it against the checksum Tribib
/// publishes with it, and runs it.
fn install_tribib(
    done: &std::sync::atomic::AtomicU64,
    total: &std::sync::atomic::AtomicU64,
) -> Result<(), String> {
    use sha2::{Digest, Sha512};
    use std::io::{Read, Write};
    use std::sync::atomic::Ordering;
    let manifest = ureq::get(&format!("{TRIBIB_RELEASES}/latest.yml"))
        .call()
        .map_err(|e| format!("couldn't reach Tribib: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    // The top-level `path:` and `sha512:` of electron-builder's manifest.
    let field = |key: &str| {
        manifest
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().trim_matches('\'').trim_matches('"').to_string())
    };
    let (Some(file), Some(checksum)) = (field("path:"), field("sha512:")) else {
        return Err("Tribib's release list isn't as expected".into());
    };
    if file.contains(['/', '\\']) || !file.ends_with(".exe") {
        return Err("Tribib's release list names an unexpected file".into());
    }
    let mut response = ureq::get(&format!("{TRIBIB_RELEASES}/{file}"))
        .call()
        .map_err(|e| format!("couldn't download Tribib: {e}"))?;
    if let Some(size) = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
    {
        total.store(size, Ordering::Relaxed);
    }
    let path = std::env::temp_dir().join(&file);
    let mut out = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut reader = response.body_mut().as_reader();
    let mut hasher = Sha512::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        out.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
    drop(out);
    if chat::base64(&hasher.finalize()) != checksum {
        let _ = std::fs::remove_file(&path);
        return Err(
            "the download doesn't match the checksum Tribib published; nothing was installed"
                .into(),
        );
    }
    // Tribib's own installer, quietly: it installs for this user only, as it does by hand.
    let status = std::process::Command::new(&path)
        .arg("/S")
        .status()
        .map_err(|e| format!("couldn't run Tribib's installer: {e}"))?;
    let _ = std::fs::remove_file(&path);
    if !status.success() || !tribib_installed() {
        return Err("Tribib's installer didn't finish".into());
    }
    Ok(())
}

/// Starts the Tribib app, if it is installed where its installer puts it.
fn open_tribib() -> Result<(), String> {
    let exe = tribib_exe().ok_or("no LOCALAPPDATA")?;
    if !exe.exists() {
        return Err("Tribib isn't installed where ride looks for it".into());
    }
    std::process::Command::new(exe)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

impl App {
    // ---- MCP servers ----

    /// Every frame: tunnels for the MCP servers on this computer, and the configuration
    /// Claude is started with.
    pub(super) fn mcp_tick(&mut self) {
        let servers: Vec<(String, String)> = settings::with(|s| {
            s.mcp_servers
                .iter()
                .filter(|m| m.on && !m.name.trim().is_empty() && !m.url.trim().is_empty())
                .map(|m| (mcp_name(&m.name), m.url.trim().to_string()))
                .collect()
        });
        let host = self.host.trim().to_string();
        // On this computer, or not connected: the servers are where they are.
        if !self.online() || chat::local_shell() || self.guest() {
            chat::set_mcp_config(mcp_json(&servers));
            return;
        }
        let mut reachable = Vec::new();
        for (name, url) in &servers {
            let Some(port) = local_port(url) else {
                reachable.push((name.clone(), url.clone()));
                continue;
            };
            if let Some(remote) = self.tunnel_for(&host, port) {
                reachable.push((name.clone(), with_port(url, remote)));
            }
        }
        // Tunnels no server needs any more go.
        let ports: Vec<u16> = servers.iter().filter_map(|(_, u)| local_port(u)).collect();
        self.tunnels
            .retain(|t| t.host == host && ports.contains(&t.local));
        chat::set_mcp_config(mcp_json(&reachable));
    }

    /// The port on the host at which a port of this computer is reached, once its tunnel
    /// is up; starts the tunnel when there is none.
    fn tunnel_for(&mut self, host: &str, port: u16) -> Option<u16> {
        let index = match self
            .tunnels
            .iter()
            .position(|t| t.host == host && t.local == port)
        {
            Some(index) => index,
            None => {
                self.tunnels.push(Tunnel {
                    host: host.to_string(),
                    local: port,
                    remote: port,
                    child: None,
                    tries: 0,
                });
                self.tunnels.len() - 1
            }
        };
        let tunnel = &mut self.tunnels[index];
        let exited = tunnel
            .child
            .as_mut()
            .is_some_and(|c| !matches!(c.try_wait(), Ok(None)));
        if tunnel.child.is_none() || exited {
            if tunnel.tries >= 4 {
                return None;
            }
            // The same port on the host first; another if that one is taken there.
            if tunnel.tries > 0 {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.subsec_nanos());
                tunnel.remote = 20000 + (nanos % 30000) as u16;
            }
            tunnel.tries += 1;
            tunnel.child = start_tunnel(host, tunnel.local, tunnel.remote).ok();
        }
        Some(tunnel.remote)
    }

    // ---- design sessions ----

    fn new_design(&mut self) -> usize {
        let n = self.designs.len() + 1;
        let mut chat = Chat::default();
        chat.title = format!("Design {n}");
        chat.attach = false;
        chat.mode = chat::Mode::Edit;
        chat.system = Some(design_prompt());
        chat.allowed_tools = "mcp__tribib".into();
        let uid = self.next_uid;
        self.next_uid += 1;
        let document = self
            .canvas
            .documents
            .iter()
            .find(|d| d.2)
            .map(|d| (d.0.clone(), d.1.clone()));
        self.designs.push(Design {
            uid,
            name: format!("Design {n}"),
            chat,
            started_with: None,
            document,
        });
        self.designs.len() - 1
    }

    pub(super) fn design_from_json(&mut self, v: &serde_json::Value) {
        let (Some(uid), Some(name)) = (v["uid"].as_u64(), v["name"].as_str()) else {
            return;
        };
        let mut chat = Chat::from_json(&v["chat"]);
        chat.system = Some(design_prompt());
        chat.allowed_tools = "mcp__tribib".into();
        let document = match (v["document"].as_str(), v["document_name"].as_str()) {
            (Some(id), name) => Some((id.to_string(), name.unwrap_or("").to_string())),
            _ => None,
        };
        self.designs.push(Design {
            uid,
            name: name.to_string(),
            chat,
            started_with: None,
            document,
        });
        self.next_uid = self.next_uid.max(uid + 1);
    }

    pub(super) fn designs_json(&self) -> serde_json::Value {
        self.designs
            .iter()
            .map(|d| {
                serde_json::json!({
                    "uid": d.uid,
                    "name": d.name,
                    "chat": d.chat.to_json(),
                    "document": d.document.as_ref().map(|x| x.0.clone()),
                    "document_name": d.document.as_ref().map(|x| x.1.clone()),
                })
            })
            .collect()
    }

    fn stop_design(&mut self, index: usize) {
        let design = &mut self.designs[index];
        if let Some(proc) = design.chat.proc {
            link_request(&mut self.link, Request::ProcClose { proc });
        }
        design.chat.stopped();
    }

    fn design_send(&mut self, index: usize, text: String) {
        if !self.online() {
            return;
        }
        let mentions = self.mention_context(&text);
        let design = &mut self.designs[index];
        ensure_proc(
            &mut self.link,
            &self.root,
            &mut design.chat,
            &mut design.started_with,
        );
        let mut context = match &design.document {
            Some((id, name)) => format!("[Tribib document: {name} ({id})]"),
            None => "[Tribib document: whichever is active]".to_string(),
        };
        if let Some(mentions) = mentions {
            context.push_str("\n\n");
            context.push_str(&mentions);
        }
        let data = design.chat.begin_turn(&text, Some(context));
        if let Some(proc) = design.chat.proc {
            link_request(&mut self.link, Request::ProcInput { proc, data });
        }
    }

    /// Feeds a process message to the design session it belongs to. False if none.
    pub(super) fn design_message(&mut self, id: u64, resp: &Response) -> bool {
        let Some(index) = self.designs.iter().position(|d| d.chat.proc == Some(id)) else {
            return false;
        };
        let chat = &mut self.designs[index].chat;
        match resp {
            Response::ProcOutput { data } => {
                if chat.on_stdout(data) {
                    let name = self.designs[index].name.clone();
                    let snippet = self.designs[index].chat.snippet();
                    self.notify(&format!("{name} finished"), &snippet);
                    // What it built shows at once.
                    self.canvas.last = f64::NEG_INFINITY;
                }
            }
            Response::ProcStderr { data } => chat.on_stderr(data),
            Response::ProcExit { code } => chat.on_exit(*code),
            Response::Error { message } => {
                chat.on_stderr(message.as_bytes());
                chat.on_exit(None);
            }
            _ => {}
        }
        true
    }

    // ---- the view ----

    /// The Designer view: the design sessions at the left, Tribib itself in the middle (its
    /// own window, held inside ride's), and the chat of the session at the right.
    pub(super) fn designer_view(&mut self, ui: &mut Ui, ctx: &Context) {
        if self.designs.is_empty() {
            self.new_design();
        }
        self.active_design = self.active_design.min(self.designs.len() - 1);
        self.canvas.take(ctx, self.now);
        let held = self.held.is_some();
        self.canvas.shots = !held;
        // Without Tribib in the middle, its picture is followed: every moment while Claude
        // works, less often otherwise. With it there, only its documents are looked at.
        let busy = self.designs.iter().any(|d| d.chat.busy);
        let every = match (&self.canvas.error, busy, held) {
            (Some(_), _, _) => 5.0,
            (None, _, true) => 4.0,
            (None, true, false) => 1.0,
            (None, false, false) => 2.5,
        };
        if self.canvas.live && self.now - self.canvas.last > every {
            self.canvas.refresh(ctx, tribib_url(), None);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((every / 2.0).min(1.0)));

        let size = ctx.content_rect().size();
        Panel::left("designs")
            .resizable(true)
            .default_size(210.0)
            .size_range(180.0..=(size.x * 0.25).max(180.0))
            .show_separator_line(true)
            .frame(theme::card(theme::glass()))
            .show(ui, |ui| self.designs_panel(ui));
        Panel::right("design_chat")
            .resizable(true)
            .default_size(420.0)
            .size_range(320.0..=(size.x * 0.5).max(320.0))
            .show_separator_line(true)
            .frame(theme::card(theme::surface()).inner_margin(Margin::same(12)))
            .show(ui, |ui| self.design_chat(ui));
        CentralPanel::default_margins()
            .frame(theme::card(theme::surface()).inner_margin(Margin::ZERO))
            .show(ui, |ui| self.tribib_area(ui));
    }

    /// The program held in the middle: Tribib, or another for trying it out (RIDE_EMBED).
    fn embed_program() -> String {
        std::env::var("RIDE_EMBED").unwrap_or_else(|_| "Tribib.exe".into())
    }

    /// The middle of the Designer: Tribib's own window, kept over this part of ride's.
    fn tribib_area(&mut self, ui: &mut Ui) {
        let mut pick = None;
        let mut give_back = false;
        let mut take_in = false;
        let index = self.active_design;
        Frame::NONE
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let chosen = self.designs[index]
                        .document
                        .as_ref()
                        .map_or("Choose a Tribib document".to_string(), |d| d.1.clone());
                    egui::ComboBox::from_id_salt("tribib_document")
                        .selected_text(chosen)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            if self.canvas.documents.is_empty() {
                                ui.label(
                                    RichText::new("No document open in Tribib")
                                        .color(theme::COMMENT()),
                                );
                            }
                            for (id, name, active) in &self.canvas.documents {
                                let label = if *active {
                                    format!("{name}  (in front)")
                                } else {
                                    name.clone()
                                };
                                let current = self.designs[index]
                                    .document
                                    .as_ref()
                                    .is_some_and(|d| &d.0 == id);
                                if ui.selectable_label(current, label).clicked() {
                                    pick = Some((id.clone(), name.clone()));
                                }
                            }
                        })
                        .response
                        .on_hover_text("The document Claude works on in this session");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.held.is_some() {
                            give_back =
                                icon_button(ui, icons::POP_OUT, "Give Tribib its own window back");
                        } else if self.embed_off {
                            take_in = ui
                                .button(format!("{}  Bring Tribib in", icons::DOCK))
                                .clicked();
                        }
                        let (dot, text) = match (&self.held, &self.canvas.error) {
                            (Some(_), _) => (theme::ANSI()[2], "Tribib"),
                            (None, Some(_)) => (theme::ANSI()[1], "Tribib isn't running"),
                            (None, None) if self.embed_off => {
                                (theme::COMMENT(), "Tribib in its own window")
                            }
                            (None, None) => (theme::ANSI()[3], "Looking for Tribib…"),
                        };
                        ui.label(RichText::new(text).small().color(theme::COMMENT()));
                        ui.label(RichText::new(icons::DOT.to_string()).size(9.0).color(dot));
                    });
                });
            });
        if let Some((id, name)) = pick {
            self.designs[index].document = Some((id.clone(), name));
            self.canvas.refresh(ui.ctx(), tribib_url(), Some(id));
        }
        if give_back {
            if let Some(held) = self.held.take() {
                crate::embed::release(&held);
            }
            self.embed_off = true;
        }
        if take_in {
            self.embed_off = false;
        }

        let area = ui.available_rect_before_wrap();
        ui.advance_cursor_after_rect(area);
        let parent = self.ride_window;
        // What lies over the middle hides Tribib: it would cover a dialog or the palette.
        // A list or menu of ride's that is open (the document list above, a chat menu)
        // would open behind it too.
        let menu_open = egui::Popup::is_any_open(ui.ctx());
        let covered = self.dialog.is_some() || self.palette.open || menu_open;
        let _ = parent;
        // Opening the Designer starts Tribib, if ride hasn't already.
        self.tribib_wanted = true;
        if let Some(held) = &self.held {
            let scale = ui.ctx().pixels_per_point();
            let r = area.shrink(1.0);
            let place = (
                (r.min.x * scale).round() as i32,
                (r.min.y * scale).round() as i32,
                (r.width() * scale).round() as i32,
                (r.height() * scale).round() as i32,
                !covered,
            );
            if self.embed_place != Some(place) {
                self.embed_place = Some(place);
                crate::embed::place(held, place.0, place.1, place.2, place.3, place.4);
            }
            return;
        }
        // Tribib isn't held: its picture, or what to do.
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(area.shrink(10.0)));
        if self.canvas.error.is_some() || !self.embed_off {
            self.tribib_setup(&mut inner);
            return;
        }
        self.canvas_picture(&mut inner);
    }

    /// Without Tribib in the middle: install it, or start it, and say what is going on.
    fn tribib_setup(&mut self, ui: &mut Ui) {
        use std::sync::atomic::Ordering;
        // An install under way, or just finished.
        if let Some(install) = &self.tribib_install {
            if let Ok(result) = install.result.try_recv() {
                self.tribib_install = None;
                match result {
                    Ok(()) => {
                        self.status = "Tribib is installed".into();
                        // Its installer opens it; it is taken in as it appears.
                        self.tribib_spawned = true;
                        self.tribib_started = self.now;
                    }
                    Err(error) => self.tribib_error = Some(error),
                }
            }
        }
        let installed = tribib_installed();
        ui.add_space(48.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(440.0);
            if let Some(install) = &self.tribib_install {
                let (done, total) = (
                    install.done.load(Ordering::Relaxed),
                    install.total.load(Ordering::Relaxed),
                );
                ui.label(RichText::new("Installing Tribib").size(17.0).color(theme::STRONG()));
                ui.add_space(8.0);
                let part = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                let text = if total > 0 && done >= total {
                    "Running Tribib's installer…".to_string()
                } else {
                    format!("Downloading from Tribib… {:.0} MB of {:.0} MB", done as f64 / 1e6, total as f64 / 1e6)
                };
                ui.add(egui::ProgressBar::new(part).desired_width(360.0));
                ui.label(RichText::new(text).small().color(theme::COMMENT()));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            } else if !installed {
                ui.label(RichText::new("Tribib isn't installed").size(17.0).color(theme::STRONG()));
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "The Designer works in Tribib, inside ride. ride downloads Tribib's latest installer from Tribib, checks it, and installs it for you. Tribib keeps itself up to date after that.",
                        )
                        .color(theme::COMMENT()),
                    )
                    .wrap(),
                );
                ui.add_space(12.0);
                let button = egui::Button::new(
                    RichText::new(format!("{}  Install Tribib", icons::DOWNLOAD)).color(Color32::WHITE),
                )
                .fill(theme::ACCENT().gamma_multiply(0.85));
                if ui.add(button).clicked() {
                    self.tribib_error = None;
                    let done = Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let total = Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let (tx, rx) = channels::channel();
                    let (d, t) = (done.clone(), total.clone());
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(install_tribib(&d, &t));
                        ctx.request_repaint();
                    });
                    self.tribib_install = Some(TribibInstall { done, total, result: rx });
                }
            } else {
                ui.spinner();
                ui.add_space(6.0);
                ui.label(RichText::new("Starting Tribib…").size(17.0).color(theme::STRONG()));
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Its window moves in here as soon as it opens. Sign in to Tribib once, in it, and it remembers you.",
                        )
                        .color(theme::COMMENT()),
                    )
                    .wrap(),
                );
                ui.add_space(10.0);
                if ui.button(format!("{}  Start it again", icons::PLAY)).clicked() {
                    self.tribib_spawned = true;
                    self.tribib_started = self.now;
                    if let Err(error) = open_tribib() {
                        self.tribib_error = Some(error);
                    }
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
            if let Some(error) = &self.tribib_error {
                ui.add_space(10.0);
                ui.add(
                    egui::Label::new(RichText::new(error).small().color(theme::ANSI()[1])).wrap(),
                );
            }
        });
    }

    /// Every frame: Tribib is ride's. ride starts it (as it opens, once Tribib is set up for
    /// Claude, or when the Designer is first opened), takes its window in from the first
    /// moment it appears, and keeps it hidden but in the Designer: it has no window or
    /// taskbar button of its own.
    pub(super) fn tribib_tick(&mut self) {
        let Some(parent) = self.ride_window else {
            return;
        };
        if self.embed_off {
            return;
        }
        if self
            .held
            .as_ref()
            .is_some_and(|h| !crate::embed::alive(h, parent))
        {
            self.held = None;
            self.embed_place = None;
        }
        if self.held.is_some() {
            return;
        }
        // Looked for often just after ride started it, so it is taken in before it is seen;
        // otherwise now and then: it walks every window.
        let starting = self.now - self.tribib_started < 30.0;
        let every = if starting { 0.1 } else { 1.0 };
        if self.now - self.embed_tried < every {
            return;
        }
        self.embed_tried = self.now;
        if let Some(window) = crate::embed::find(&Self::embed_program()) {
            self.held = crate::embed::hold(window, parent);
            self.embed_place = None;
            return;
        }
        let set_up = settings::with(|s| {
            s.mcp_servers
                .iter()
                .any(|m| m.on && m.name.eq_ignore_ascii_case("tribib"))
        });
        let wanted = self.tribib_wanted || set_up;
        if wanted && !self.tribib_spawned && tribib_installed() && self.tribib_install.is_none() {
            self.tribib_spawned = true;
            self.tribib_started = self.now;
            if let Err(error) = open_tribib() {
                self.tribib_error = Some(error);
            }
        }
    }

    /// Hides Tribib when the Designer isn't shown; ride's other views are under it.
    pub(super) fn designer_away(&mut self) {
        if let Some(held) = &self.held {
            if self.embed_place.is_some_and(|p| p.4) || self.embed_place.is_none() {
                crate::embed::place(held, 0, 0, 1, 1, false);
                self.embed_place = Some((0, 0, 1, 1, false));
            }
        }
    }

    /// As ride closes: a Tribib ride started closes with it, unseen; one that was open
    /// before gets its own window back.
    pub(super) fn designer_release(&mut self) {
        if let Some(held) = self.held.take() {
            if self.tribib_spawned {
                crate::embed::close(&held);
            } else {
                crate::embed::release(&held);
            }
        }
    }

    fn designs_panel(&mut self, ui: &mut Ui) {
        let mut add = false;
        let (mut select, mut remove) = (None, None);
        Frame::NONE
            .inner_margin(Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new("Designer").size(17.0).color(theme::STRONG()));
                        ui.label(
                            RichText::new("Claude designs in Tribib")
                                .small()
                                .color(theme::COMMENT()),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        add = icon_button(ui, icons::ADD, "New design");
                    });
                });
            });
        ui.add_space(4.0);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (index, design) in self.designs.iter().enumerate() {
                let chosen = index == self.active_design;
                let mut on_button = false;
                let row = Frame::NONE
                    .fill(if chosen {
                        theme::tint(18)
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(9)
                    .inner_margin(Margin::symmetric(10, 7))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, icons::CLOSE, "Remove this design session") {
                                    remove = Some(index);
                                    on_button = true;
                                }
                                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    let dot = if design.chat.busy { "● " } else { "" };
                                    ui.add(
                                        egui::Label::new(pieces(&[
                                            (dot, theme::ANSI()[2]),
                                            (&design.name, theme::STRONG()),
                                        ]))
                                        .selectable(false),
                                    );
                                    let document = design
                                        .document
                                        .as_ref()
                                        .map_or("No document chosen", |d| d.1.as_str());
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(document).small().color(theme::COMMENT()),
                                        )
                                        .selectable(false),
                                    );
                                });
                            });
                        });
                    })
                    .response;
                if row.contains_pointer() && !on_button && ui.input(|i| i.pointer.primary_clicked())
                {
                    select = Some(index);
                }
            }
        });
        if let Some(index) = remove {
            self.stop_design(index);
            self.designs.remove(index);
            self.active_design = self.active_design.min(self.designs.len().saturating_sub(1));
        } else if let Some(index) = select {
            self.active_design = index;
        }
        if add {
            self.active_design = self.new_design();
        }
    }

    fn design_chat(&mut self, ui: &mut Ui) {
        let index = self.active_design;
        let tribib = settings::with(|s| {
            s.mcp_servers
                .iter()
                .any(|m| m.on && m.name.eq_ignore_ascii_case("tribib"))
        });
        if !tribib {
            Frame::NONE
                .fill(theme::tint(8))
                .corner_radius(8)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(
                                "Claude needs Tribib's MCP server for this. Add it in Settings → Claude → MCP servers, or here:",
                            )
                            .color(theme::TEXT()),
                        );
                        if ui.button("Add Tribib").clicked() {
                            settings::change(|s| s.mcp_servers.push(settings::McpServer::tribib()));
                        }
                    });
                });
            ui.add_space(8.0);
        }
        let action = self.designs[index].chat.show(ui, None, false);
        match action {
            Some(chat::Action::Send(text)) => self.design_send(index, text),
            Some(chat::Action::Stop) => self.stop_design(index),
            Some(chat::Action::NewChat) => {
                self.stop_design(index);
                self.designs[index].chat.reset();
            }
            Some(chat::Action::Restart) => {
                if !self.designs[index].chat.busy {
                    self.stop_design(index);
                }
            }
            Some(chat::Action::FindFiles(query)) => self.find_files(query, None),
            Some(chat::Action::Wake) => {
                if self.online() {
                    let design = &mut self.designs[index];
                    ensure_proc(
                        &mut self.link,
                        &self.root,
                        &mut design.chat,
                        &mut design.started_with,
                    );
                }
            }
            _ => {}
        }
    }

    /// The picture of Tribib's canvas, for when Tribib is in its own window.
    fn canvas_picture(&mut self, ui: &mut Ui) {
        if let Some(error) = &self.canvas.error {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Tribib isn't answering").color(theme::STRONG()));
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{} — {error}", tribib_url()))
                            .small()
                            .color(theme::COMMENT()),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
                if ui.button("Open Tribib").clicked() {
                    if let Err(error) = open_tribib() {
                        self.status = error;
                    }
                }
            });
            return;
        }
        let Some(texture) = &self.canvas.texture else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                let text = if self.canvas.pending.is_some() && self.canvas.documents.is_empty() {
                    "Looking at Tribib…"
                } else {
                    "The page is empty. Ask Claude for a design and watch it appear here."
                };
                ui.label(RichText::new(text).color(theme::COMMENT()));
            });
            return;
        };
        if !self.canvas.page.is_empty() {
            ui.label(
                RichText::new(&self.canvas.page)
                    .small()
                    .color(theme::COMMENT()),
            );
        }
        let size = self.canvas.size;
        if self.canvas.fit {
            let room = ui.available_size();
            let scale = (room.x / size.x).min(room.y / size.y).min(2.0);
            let shown = size * scale;
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(texture.id(), shown))
                        .corner_radius(6),
                );
            });
        } else {
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(
                    texture.id(),
                    size * 2.0,
                )));
            });
        }
    }
}

/// A name Claude Code takes for an MCP server: letters, digits, - and _.
fn mcp_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `ssh -R`: the host's 127.0.0.1:`remote` reaches this computer's 127.0.0.1:`local`.
fn start_tunnel(host: &str, local: u16, remote: u16) -> Result<std::process::Child, String> {
    let mut command = std::process::Command::new("ssh");
    command
        .args([
            "-N",
            "-o",
            "BatchMode=yes",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=30",
            "-R",
            &format!("127.0.0.1:{remote}:127.0.0.1:{local}"),
            host,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let child = command.spawn().map_err(|e| e.to_string())?;
    os::end_with_app(&child);
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_servers_are_found_and_moved_to_another_port() {
        assert_eq!(local_port("http://127.0.0.1:3848/mcp"), Some(3848));
        assert_eq!(local_port("http://localhost:9000"), Some(9000));
        assert_eq!(local_port("https://mcp.example.com:443/x"), None);
        assert_eq!(
            with_port("http://127.0.0.1:3848/mcp", 23848),
            "http://127.0.0.1:23848/mcp"
        );
        let json = mcp_json(&[("tribib".into(), "http://127.0.0.1:3848/mcp".into())]).unwrap();
        assert_eq!(
            json,
            r#"{"mcpServers":{"tribib":{"type":"http","url":"http://127.0.0.1:3848/mcp"}}}"#
        );
        assert_eq!(mcp_name("my server!"), "my_server_");
    }
}
