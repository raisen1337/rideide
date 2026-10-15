//! Several connections at once. The app's own fields are the connection being looked at;
//! the others wait in `App::sessions` and are swapped in for a moment every frame, so what
//! arrives for them is handled by the very same code. Request ids are unique across
//! connections, so an answer can never be taken for another connection's.

use super::*;

/// Everything that belongs to one connection.
pub(super) struct Session {
    pub(super) uid: u64,
    pub(super) link: Link,
    pub(super) host: String,
    saved: Option<(String, String)>,
    open_on_connect: Option<String>,
    root: String,
    root_input: String,
    pending: HashMap<u64, Pending>,
    tree: HashMap<String, DirState>,
    expanded: HashSet<String>,
    tabs: Vec<Tab>,
    active: usize,
    panes: panes::Panes,
    terms: Vec<Terminal>,
    term_panes: panes::Panes,
    active_term: usize,
    search: Search,
    find: Option<String>,
    git: tools::Git,
    monitor: tools::Monitor,
    forwards: Vec<tools::Forward>,
    upload: Option<extras::Upload>,
    refresh_later: Option<(f64, String)>,
    restore: HashMap<String, extras::SavedTab>,
    restore_order: Vec<String>,
    restore_active: Option<String>,
    restore_host: String,
    lsp: lsp::Lsp,
}

impl Session {
    pub(super) fn new(uid: u64, host: String) -> Self {
        Self {
            uid,
            link: Link::Offline,
            host,
            saved: None,
            open_on_connect: None,
            root: String::new(),
            root_input: String::new(),
            pending: HashMap::new(),
            tree: HashMap::new(),
            expanded: HashSet::new(),
            tabs: Vec::new(),
            active: 0,
            panes: panes::Panes::default(),
            term_panes: panes::Panes::default(),
            terms: Vec::new(),
            active_term: 0,
            search: Search::default(),
            find: None,
            git: tools::Git::default(),
            monitor: tools::Monitor::default(),
            forwards: Vec::new(),
            upload: None,
            refresh_later: None,
            restore: HashMap::new(),
            restore_order: Vec::new(),
            restore_active: None,
            restore_host: String::new(),
            lsp: lsp::Lsp::default(),
        }
    }
}

/// A file or folder on its way from one connection to another.
pub(super) struct Transfer {
    /// The id of the download on the connection it comes from.
    id: u64,
    /// The connection it goes to, and the folder there.
    to: u64,
    dest: String,
    name: String,
    is_dir: bool,
    data: Vec<u8>,
}

/// Requests for a connection that isn't the one at hand: what a finished transfer writes.
pub(super) struct Batch {
    to: u64,
    dest: String,
    name: String,
    requests: Vec<Request>,
}

/// What is being dragged out of a file tree.
#[derive(Clone)]
pub(super) struct DragFile {
    pub(super) from: u64,
    pub(super) path: String,
    pub(super) is_dir: bool,
}

/// One file may be this large to go from host to host: it is held in memory on the way.
const MAX_TRANSFER: usize = 200 * 1024 * 1024;

/// Turns the gzipped tar a host sends for a folder into the requests that recreate it.
fn folder_requests(data: &[u8], dest: &str) -> Result<Vec<Request>, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(data));
    let mut requests = Vec::new();
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let relative = entry
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        // Nothing in the archive may name a place outside the folder it is unpacked into.
        if relative.starts_with('/') || relative.split('/').any(|part| part == "..") {
            continue;
        }
        let path = join(dest, relative.trim_end_matches('/'));
        match entry.header().entry_type() {
            tar::EntryType::Directory => requests.push(Request::CreateDir { path }),
            tar::EntryType::Regular => {
                let mut data = Vec::new();
                entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
                requests.push(Request::WriteFile { path, data });
            }
            _ => {}
        }
    }
    Ok(requests)
}

impl App {
    /// Exchanges the connection at hand with `other`.
    fn swap_session(&mut self, other: &mut Session) {
        use std::mem::swap;
        swap(&mut self.session_uid, &mut other.uid);
        swap(&mut self.link, &mut other.link);
        swap(&mut self.host, &mut other.host);
        swap(&mut self.saved, &mut other.saved);
        swap(&mut self.open_on_connect, &mut other.open_on_connect);
        swap(&mut self.root, &mut other.root);
        swap(&mut self.root_input, &mut other.root_input);
        swap(&mut self.pending, &mut other.pending);
        swap(&mut self.tree, &mut other.tree);
        swap(&mut self.expanded, &mut other.expanded);
        swap(&mut self.tabs, &mut other.tabs);
        swap(&mut self.active, &mut other.active);
        swap(&mut self.panes, &mut other.panes);
        swap(&mut self.terms, &mut other.terms);
        swap(&mut self.term_panes, &mut other.term_panes);
        swap(&mut self.active_term, &mut other.active_term);
        swap(&mut self.search, &mut other.search);
        swap(&mut self.find, &mut other.find);
        swap(&mut self.git, &mut other.git);
        swap(&mut self.monitor, &mut other.monitor);
        swap(&mut self.forwards, &mut other.forwards);
        swap(&mut self.upload, &mut other.upload);
        swap(&mut self.refresh_later, &mut other.refresh_later);
        swap(&mut self.restore, &mut other.restore);
        swap(&mut self.restore_order, &mut other.restore_order);
        swap(&mut self.restore_active, &mut other.restore_active);
        swap(&mut self.restore_host, &mut other.restore_host);
        swap(&mut self.lsp, &mut other.lsp);
        chat::set_local_shell(self.host.trim() == LOCAL);
        share::set_guest(share::is_session_id(&self.host));
    }

    /// Runs `work` with one of the waiting connections in the app's place.
    pub(super) fn with_session<R>(&mut self, index: usize, work: impl FnOnce(&mut Self) -> R) -> R {
        let mut session =
            std::mem::replace(&mut self.sessions[index], Session::new(0, String::new()));
        self.swap_session(&mut session);
        let was = std::mem::replace(&mut self.background, true);
        let result = work(self);
        self.background = was;
        self.swap_session(&mut session);
        self.sessions[index] = session;
        result
    }

    /// Lets the waiting connections handle what has arrived for them.
    pub(super) fn poll_sessions(&mut self, ctx: &Context) {
        for index in 0..self.sessions.len() {
            self.with_session(index, |app| {
                app.poll_link(ctx);
                app.poll_uploads(ctx);
            });
        }
        for batch in std::mem::take(&mut self.batches) {
            let send = |app: &mut Self| {
                let list = Request::ListDir {
                    path: batch.dest.clone(),
                };
                let (name, requests, dest) = (batch.name, batch.requests, batch.dest);
                app.request_then(
                    list,
                    Pending::Transfer {
                        name,
                        requests,
                        dest,
                    },
                );
            };
            if batch.to == self.session_uid {
                send(self);
            } else if let Some(index) = self.sessions.iter().position(|s| s.uid == batch.to) {
                self.with_session(index, send);
            }
        }
    }

    /// Claude's processes belong to the connection they were started on, so they stop
    /// when another connection takes its place.
    fn stop_claude(&mut self) {
        self.chat_stop();
        for index in 0..self.agents.len() {
            self.stop_agent(index);
        }
    }

    pub(super) fn switch_session(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        self.stop_claude();
        let mut session = self.sessions.remove(index);
        self.swap_session(&mut session);
        self.sessions.insert(index, session);
    }

    /// Opens another connection next to the ones there are; it becomes the one shown.
    pub(super) fn add_session(&mut self, ctx: &Context, host: String) {
        self.stop_claude();
        self.next_uid += 1;
        let mut session = Session::new(self.next_uid, host);
        self.swap_session(&mut session);
        self.sessions.push(session);
        self.connect(ctx);
    }

    /// Closes the connection shown. Another one, if there is one, takes its place.
    pub(super) fn close_session(&mut self) {
        self.stop_claude();
        self.disconnect();
        if !self.sessions.is_empty() {
            self.tabs.clear();
            let mut session = self.sessions.remove(0);
            self.swap_session(&mut session);
        }
    }

    /// For checking this without a mouse: `RIDE_SECOND=host|folder` opens a second
    /// connection once the first is up, and `RIDE_COPY=path` then copies that path of the
    /// first connection into the second one's folder.
    pub(super) fn session_hooks(&mut self, ctx: &Context) {
        use std::sync::atomic::{AtomicU8, Ordering};
        static STAGE: AtomicU8 = AtomicU8::new(0);
        let Ok(second) = std::env::var("RIDE_SECOND") else {
            return;
        };
        if !self.online() || self.root.is_empty() {
            return;
        }
        match STAGE.load(Ordering::Relaxed) {
            0 => {
                let (host, root) = second.split_once('|').unwrap_or((&second, ""));
                self.add_session(ctx, host.to_string());
                self.saved =
                    Some((host.to_string(), root.to_string())).filter(|_| !root.is_empty());
                STAGE.store(1, Ordering::Relaxed);
            }
            1 => {
                if let (Ok(path), Some(first)) = (std::env::var("RIDE_COPY"), self.sessions.first())
                {
                    let file = DragFile {
                        from: first.uid,
                        is_dir: Path::new(&path).is_dir(),
                        path: path.replace('\\', "/"),
                    };
                    self.start_transfer(file, self.session_uid, self.root.clone());
                }
                STAGE.store(2, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    // ---- files from host to host ----

    /// Copies a file or folder of one connection into a folder of another.
    fn start_transfer(&mut self, file: DragFile, to: u64, dest: String) {
        let name = file_name(&file.path).to_string();
        let ask = |app: &mut Self| {
            app.request(Request::Download {
                path: file.path.clone(),
            })
        };
        let id = if file.from == self.session_uid {
            ask(self)
        } else if let Some(index) = self.sessions.iter().position(|s| s.uid == file.from) {
            self.with_session(index, ask)
        } else {
            return;
        };
        self.status = format!("Copying {name}…");
        self.transfers.push(Transfer {
            id,
            to,
            dest,
            name,
            is_dir: file.is_dir,
            data: Vec::new(),
        });
    }

    /// Takes a message that belongs to a transfer. False if it is something else.
    pub(super) fn transfer_message(&mut self, id: u64, resp: &Response) -> bool {
        let Some(index) = self.transfers.iter().position(|t| t.id == id) else {
            return false;
        };
        match resp {
            Response::Chunk { data } => {
                let transfer = &mut self.transfers[index];
                transfer.data.extend_from_slice(data);
                if transfer.data.len() > MAX_TRANSFER {
                    let transfer = self.transfers.remove(index);
                    self.request(Request::Cancel { target: id });
                    self.status = format!(
                        "{} is over 200 MB, too large to copy between hosts this way",
                        transfer.name
                    );
                }
            }
            Response::Done => {
                let transfer = self.transfers.remove(index);
                let requests = if transfer.is_dir {
                    folder_requests(&transfer.data, &transfer.dest)
                } else {
                    let path = join(&transfer.dest, &transfer.name);
                    Ok(vec![Request::WriteFile {
                        path,
                        data: transfer.data,
                    }])
                };
                match requests {
                    Ok(requests) => self.batches.push(Batch {
                        to: transfer.to,
                        dest: transfer.dest,
                        name: transfer.name,
                        requests,
                    }),
                    Err(error) => self.status = format!("Couldn't copy {}: {error}", transfer.name),
                }
            }
            Response::Error { message } => {
                let transfer = self.transfers.remove(index);
                self.status = format!("Couldn't copy {}: {message}", transfer.name);
            }
            _ => {}
        }
        true
    }

    /// The listing of the folder a transfer goes into has arrived: write, unless something
    /// of that name is already there.
    pub(super) fn finish_transfer(
        &mut self,
        ctx: &Context,
        name: String,
        requests: Vec<Request>,
        dest: String,
        entries: &[DirEntry],
    ) {
        if entries.iter().any(|entry| entry.name == name) {
            self.status = format!(
                "{name} already exists in {dest} on {}; nothing was copied",
                self.host.trim()
            );
            return;
        }
        let files = requests
            .iter()
            .filter(|r| matches!(r, Request::WriteFile { .. }))
            .count();
        for request in requests {
            self.request(request);
        }
        self.status = format!(
            "Copied {name} ({files} files) to {} in {dest}",
            self.host.trim()
        );
        self.refresh_later = Some((ctx.input(|i| i.time) + 0.8, dest));
    }

    /// Called once a frame after the trees are drawn: shows what is being dragged, and
    /// starts a copy when it is let go over a folder of another connection.
    pub(super) fn tree_drag(&mut self, ui: &mut Ui) {
        self.last_drop_targets = self.drop_targets.clone();
        self.explorer_drag_hint(ui);
        let Some(file) = self.drag_file.clone() else {
            self.drop_targets.clear();
            return;
        };
        let pointer = ui.input(|i| i.pointer.interact_pos());
        // The innermost folder under the pointer: a row lies inside its tree's own area.
        let target = pointer.and_then(|p| {
            self.drop_targets
                .iter()
                .filter(|(rect, _, _)| rect.contains(p))
                .min_by(|a, b| a.0.area().total_cmp(&b.0.area()))
                .cloned()
        });
        let target = target.filter(|(_, to, _)| *to != file.from);
        if let Some(pointer) = pointer {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("tree_drag"));
            let painter = ui.ctx().layer_painter(layer);
            if let Some((rect, _, _)) = &target {
                painter.rect(
                    *rect,
                    4,
                    theme::ACCENT().gamma_multiply(0.18),
                    (1.0, theme::ACCENT()),
                    egui::StrokeKind::Inside,
                );
            }
            let label = format!("{}  {}", icons::COPY, file_name(&file.path));
            painter.text(
                pointer + vec2(14.0, 10.0),
                Align2::LEFT_TOP,
                label,
                FontId::proportional(13.0),
                theme::STRONG(),
            );
        }
        if !ui.input(|i| i.pointer.any_down()) {
            self.drag_file = None;
            if let Some((_, to, dest)) = target {
                self.start_transfer(file, to, dest);
            }
        }
        self.drop_targets.clear();
    }

    /// Where files dragged in from Explorer would go: the folder of the row under the
    /// cursor, or the open folder.
    pub(super) fn explorer_target(&self, ctx: &Context) -> String {
        let here = self.session_uid;
        crate::cursor_in_window(ctx)
            .and_then(|p| {
                self.last_drop_targets
                    .iter()
                    .filter(|(rect, to, _)| *to == here && rect.contains(p))
                    .min_by(|a, b| a.0.area().total_cmp(&b.0.area()))
                    .map(|(_, _, folder)| folder.clone())
            })
            .unwrap_or_else(|| self.root.clone())
    }

    /// While files from Explorer are over the window: the folder they would go into, lit up.
    fn explorer_drag_hint(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !hovering || !self.online() || self.background {
            return;
        }
        // Nothing else wakes the window while the files are carried around.
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
        let folder = self.explorer_target(&ctx);
        let here = self.session_uid;
        let rect = self
            .last_drop_targets
            .iter()
            .filter(|(_, to, f)| *to == here && *f == folder)
            .filter(|(rect, _, _)| crate::cursor_in_window(&ctx).is_some_and(|p| rect.contains(p)))
            .min_by(|a, b| a.0.area().total_cmp(&b.0.area()))
            .map(|(rect, _, _)| *rect);
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("explorer_drop"));
        let painter = ctx.layer_painter(layer);
        if let Some(rect) = rect {
            painter.rect(
                rect,
                4,
                theme::ACCENT().gamma_multiply(0.18),
                (1.0, theme::ACCENT()),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(pointer) = crate::cursor_in_window(&ctx) {
            let shown = if folder == self.root {
                file_name(&self.root).to_string()
            } else {
                folder
                    .strip_prefix(&format!("{}/", self.root.trim_end_matches('/')))
                    .unwrap_or(&folder)
                    .to_string()
            };
            let label = format!("{}  Upload into {shown}", icons::ARROW_UP);
            let galley = painter.layout_no_wrap(label, FontId::proportional(13.0), theme::STRONG());
            let at = pointer + vec2(16.0, 12.0);
            painter.rect_filled(
                egui::Rect::from_min_size(at - vec2(6.0, 4.0), galley.size() + vec2(12.0, 8.0)),
                6,
                theme::PANEL_BG(),
            );
            painter.galley(at, galley, theme::STRONG());
        }
    }

    /// The files of another connection, under the tree of the one shown. Files and folders
    /// can be dragged from one tree to the other.
    pub(super) fn other_tree(&mut self, ui: &mut Ui) {
        let index = self
            .sessions
            .iter()
            .position(|s| Some(s.uid) == self.beside)
            .unwrap_or(0);
        let uid = self.sessions[index].uid;
        let (rows, root, host, online) = self.with_session(index, |app| {
            let mut rows = Vec::new();
            let root = app.root.clone();
            app.push_rows(&root, 0, &mut rows);
            (rows, root, app.host.trim().to_string(), app.online())
        });

        enum Act {
            Toggle(String),
            Open(String),
            Show(u64),
            Switch,
            Hide,
        }
        let mut act = None;
        ui.horizontal(|ui| {
            let title = pieces(&[
                (&format!("{}  ", icons::REMOTE), theme::COMMENT()),
                (&host, theme::STRONG()),
            ]);
            ui.menu_button(title, |ui| {
                for session in &self.sessions {
                    if menu_item(ui, icons::REMOTE, session.host.trim()) {
                        act = Some(Act::Show(session.uid));
                    }
                }
                ui.separator();
                if menu_item(ui, icons::CLOSE, "Hide this tree") {
                    act = Some(Act::Hide);
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, icons::POP_OUT, "Work on this connection") {
                    act = Some(Act::Switch);
                }
            });
        });
        if !online {
            ui.label(
                RichText::new("Not connected")
                    .small()
                    .color(theme::COMMENT()),
            );
        }
        let row_height = ui.text_style_height(&egui::TextStyle::Button) + 4.0;
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.spacing_mut().button_padding = vec2(6.0, 2.0);
        let area = ScrollArea::both()
            .id_salt("other_tree")
            .auto_shrink(false)
            .show_rows(ui, row_height, rows.len(), |ui, range| {
                for row in &rows[range] {
                    ui.horizontal(|ui| {
                        ui.add_space(row.depth as f32 * 14.0);
                        if row.note {
                            ui.weak(&row.name);
                            return;
                        }
                        let open = row.is_dir && self.sessions[index].expanded.contains(&row.path);
                        let chevron = if open {
                            icons::CHEVRON_DOWN
                        } else {
                            icons::CHEVRON_RIGHT
                        };
                        let chevron_color = if row.is_dir {
                            theme::COMMENT()
                        } else {
                            Color32::TRANSPARENT
                        };
                        let (icon, icon_color) = icons::for_entry(&row.name, row.is_dir, open);
                        let label = pieces(&[
                            (&format!("{chevron} "), chevron_color),
                            (&format!("{icon}  "), icon_color),
                            (&row.name, theme::TEXT()),
                        ]);
                        // The row itself takes the drag: a separate widget on top would eat clicks.
                        let resp = ui
                            .selectable_label(false, label)
                            .interact(Sense::click_and_drag());
                        if resp.clicked() {
                            act = Some(if row.is_dir {
                                Act::Toggle(row.path.clone())
                            } else {
                                Act::Open(row.path.clone())
                            });
                        }
                        let folder = if row.is_dir {
                            row.path.clone()
                        } else {
                            parent(&row.path).to_string()
                        };
                        self.drop_targets.push((resp.rect, uid, folder));
                        if resp.drag_started() {
                            self.drag_file = Some(DragFile {
                                from: uid,
                                path: row.path.clone(),
                                is_dir: row.is_dir,
                            });
                        }
                    });
                }
            });
        // Let go over the empty part of the tree, a file lands in the open folder.
        self.drop_targets.push((area.inner_rect, uid, root));

        match act {
            Some(Act::Toggle(path)) => self.with_session(index, |app| {
                if !app.expanded.remove(&path) {
                    app.expanded.insert(path.clone());
                    app.load_dir(&path);
                }
            }),
            Some(Act::Open(path)) => {
                self.switch_session(index);
                self.open_file(path, None);
            }
            Some(Act::Show(uid)) => self.beside = Some(uid),
            Some(Act::Switch) => self.switch_session(index),
            Some(Act::Hide) => self.hide_beside = true,
            None => {}
        }
    }

    /// The menu behind the host's name in the top bar: the connections there are, and
    /// opening or closing one.
    pub(super) fn sessions_menu(&mut self, ui: &mut Ui) {
        let (mut switch, mut add, mut close) = (None, None, false);
        ui.label(
            RichText::new("CONNECTIONS")
                .small()
                .strong()
                .color(theme::COMMENT()),
        );
        let shown = |host: &str| share::label(host).unwrap_or_else(|| host.trim().to_string());
        let _ = ui.selectable_label(true, format!("{}   {}", icons::REMOTE, shown(&self.host)));
        for (index, session) in self.sessions.iter().enumerate() {
            if ui
                .selectable_label(
                    false,
                    format!("{}   {}", icons::REMOTE, shown(&session.host)),
                )
                .clicked()
            {
                switch = Some(index);
            }
        }
        ui.separator();
        ui.menu_button(format!("{}   Open another connection", icons::ADD), |ui| {
            for host in &self.hosts {
                if menu_item(ui, icons::REMOTE, host) {
                    add = Some(host.clone());
                }
            }
        });
        if !self.sessions.is_empty()
            && menu_item(ui, icons::EYE, "Show the other one's files beside these")
        {
            self.hide_beside = false;
            self.side = Side::Files;
            self.show_side = true;
        }
        let (mut host_session, mut join) = (false, false);
        if !self.guest() && !chat::local_shell() {
            let label = if self.share.is_some() {
                "Show the live session"
            } else {
                "Share this folder in a live session"
            };
            host_session = menu_item(ui, icons::BROADCAST, label);
        }
        join = menu_item(ui, icons::BROADCAST, "Join a live session…") || join;
        if menu_item(ui, icons::DISCONNECT, "Close this connection") {
            close = true;
        }
        if switch.is_some() || add.is_some() || close || host_session || join {
            ui.close();
        }
        if host_session {
            self.share_start(ui.ctx());
        }
        if join {
            self.join_input = Some(String::new());
        }
        if let Some(index) = switch {
            self.switch_session(index);
        }
        if let Some(host) = add {
            self.add_session(ui.ctx(), host);
        }
        if close {
            self.close_session();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_from_one_host_becomes_requests_for_another() {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut add = |path: &str, data: &[u8], kind: tar::EntryType| {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_entry_type(kind);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, data).unwrap();
        };
        add("site", b"", tar::EntryType::Directory);
        add("site/index.html", b"<p>hi</p>", tar::EntryType::Regular);
        add("site/css/a.css", b"p{}", tar::EntryType::Regular);
        let data = tar.into_inner().unwrap().finish().unwrap();

        let requests = folder_requests(&data, "/srv/www").unwrap();
        let seen: Vec<String> = requests
            .iter()
            .map(|request| match request {
                Request::CreateDir { path } => format!("dir {path}"),
                Request::WriteFile { path, data } => format!("file {path} {}", data.len()),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            seen,
            [
                "dir /srv/www/site",
                "file /srv/www/site/index.html 9",
                "file /srv/www/site/css/a.css 3"
            ]
        );
    }
}
