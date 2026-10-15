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
fn look(url: &str, select: Option<String>) -> Result<Look, String> {
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
        Some(id) if page["children"].as_u64() != Some(0) => {
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
        std::thread::spawn(move || {
            let _ = tx.send(look(&url, select));
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
                    None => self.texture = None,
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

/// Starts the Tribib app, if it is installed where its installer puts it.
fn open_tribib() -> Result<(), String> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or("no LOCALAPPDATA")?;
    let exe = PathBuf::from(base)
        .join("Programs")
        .join("tribib-desktop")
        .join("Tribib.exe");
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

    /// The Designer view: the design sessions, the chat of the one chosen, and the canvas.
    pub(super) fn designer_view(&mut self, ui: &mut Ui, ctx: &Context) {
        if self.designs.is_empty() {
            self.new_design();
        }
        self.active_design = self.active_design.min(self.designs.len() - 1);
        self.canvas.take(ctx, self.now);
        // The canvas is looked at again every moment while Claude works, less often otherwise.
        let busy = self.designs.iter().any(|d| d.chat.busy);
        let every = match (&self.canvas.error, busy) {
            (Some(_), _) => 5.0,
            (None, true) => 1.0,
            (None, false) => 2.5,
        };
        if self.canvas.live && self.now - self.canvas.last > every {
            self.canvas.refresh(ctx, tribib_url(), None);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(every / 2.0));

        let size = ctx.content_rect().size();
        Panel::left("designs")
            .resizable(true)
            .default_size(240.0)
            .size_range(200.0..=(size.x * 0.3).max(200.0))
            .show_separator_line(true)
            .frame(theme::card(theme::glass()))
            .show(ui, |ui| self.designs_panel(ui));
        Panel::right("design_canvas")
            .resizable(true)
            .default_size(520.0)
            .size_range(380.0..=(size.x * 0.7).max(380.0))
            .show_separator_line(true)
            .frame(theme::card(theme::surface()).inner_margin(Margin::same(10)))
            .show(ui, |ui| self.canvas_panel(ui));
        CentralPanel::default_margins()
            .frame(theme::card(theme::surface()).inner_margin(Margin::same(12)))
            .show(ui, |ui| self.design_chat(ui));
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

    fn canvas_panel(&mut self, ui: &mut Ui) {
        let mut pick = None;
        let mut refresh = false;
        let index = self.active_design;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("CANVAS")
                    .small()
                    .strong()
                    .color(theme::COMMENT()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, icons::POP_OUT, "Open Tribib") {
                    if let Err(error) = open_tribib() {
                        self.status = error;
                    }
                }
                let fit = if self.canvas.fit { "Fit" } else { "100%" };
                if ui
                    .small_button(fit)
                    .on_hover_text("Fit the canvas to the panel, or show it at its size")
                    .clicked()
                {
                    self.canvas.fit = !self.canvas.fit;
                }
                if icon_button(ui, icons::REFRESH, "Look again") {
                    refresh = true;
                }
                let live = if self.canvas.live {
                    RichText::new(format!("{}  Live", icons::DOT))
                        .small()
                        .color(theme::ANSI()[2])
                } else {
                    RichText::new("Paused").small().color(theme::COMMENT())
                };
                if ui
                    .add(egui::Label::new(live).sense(Sense::click()))
                    .on_hover_text("Click to pause or resume following the canvas")
                    .clicked()
                {
                    self.canvas.live = !self.canvas.live;
                }
            });
        });
        ui.add_space(2.0);
        let chosen = self.designs[index]
            .document
            .as_ref()
            .map_or("Choose a Tribib document".to_string(), |d| d.1.clone());
        egui::ComboBox::from_id_salt("tribib_document")
            .selected_text(chosen)
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                if self.canvas.documents.is_empty() {
                    ui.label(RichText::new("No document open in Tribib").color(theme::COMMENT()));
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
            });
        ui.add_space(6.0);
        if let Some((id, name)) = pick {
            let index = self.active_design;
            self.designs[index].document = Some((id.clone(), name));
            self.canvas.refresh(ui.ctx(), tribib_url(), Some(id));
        } else if refresh {
            self.canvas.refresh(ui.ctx(), tribib_url(), None);
        }

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
