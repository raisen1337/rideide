//! Live editing, the editor's side. On a Linux host, with live editing on and a GitHub
//! sign-in, every open file is shared through the host's broker (`ride-agent collab`):
//! the edits typed here go out as operations, the ones others make come in and are
//! merged, and their cursors show in the editor with their names.

use super::*;
use ride_proto::ot::{Client, FromBroker, Peer, TextOp, ToBroker};

/// The colours others' cursors get, in turn.
fn peer_color(id: u64) -> Color32 {
    const COLORS: [(u8, u8, u8); 6] = [
        (0xf4, 0x8f, 0xb1),
        (0x7e, 0xd3, 0x8f),
        (0xf6, 0xc1, 0x77),
        (0x8a, 0xb4, 0xf8),
        (0xc5, 0x8a, 0xf9),
        (0x6f, 0xd6, 0xd6),
    ];
    let (r, g, b) = COLORS[(id as usize) % COLORS.len()];
    Color32::from_rgb(r, g, b)
}

struct Other {
    peer: Peer,
    /// Their cursor and the other end of their selection, as offsets in the text.
    at: Option<(usize, usize)>,
}

struct Doc {
    client: Client,
    /// The shared text as it stands with this side's own edits: what the tab showed when
    /// last looked at.
    shadow: String,
    /// The broker has answered the join.
    joined: bool,
    others: HashMap<u64, Other>,
    revision_seen: u64,
    cursor_sent: (usize, usize),
}

#[derive(Default)]
pub(super) struct Live {
    proc: Option<u64>,
    partial: Vec<u8>,
    docs: HashMap<String, Doc>,
    /// The host the broker line was opened on; another host means another line.
    host: String,
    /// RIDE_LIVE_TYPE has been typed.
    typed: bool,
}

/// Others in a document, for the editor: name, colour, picture, and where they are.
pub(super) struct Mark {
    pub(super) name: String,
    pub(super) color: Color32,
    pub(super) avatar: Option<egui::TextureId>,
    pub(super) at: Option<(Pos, Pos)>,
}

impl App {
    fn live_send(&mut self, msg: &ToBroker) {
        let Some(proc) = self.live.proc else {
            return;
        };
        if let Ok(mut line) = serde_json::to_string(msg) {
            line.push('\n');
            self.request(Request::ProcInput {
                proc,
                data: line.into_bytes(),
            });
        }
    }

    fn live_wanted(&self) -> bool {
        settings::with(|s| s.live_editing)
            && self.github.me.is_some()
            && self.online()
            && !chat::local_shell()
    }

    /// Stops sharing: every document is left and the line to the broker closed.
    pub(super) fn live_stop(&mut self) {
        if let Some(proc) = self.live.proc.take() {
            self.request(Request::ProcClose { proc });
        }
        self.live.docs.clear();
        self.live.partial.clear();
        for tab in &mut self.tabs {
            tab.view.peers.clear();
        }
    }

    /// Once a frame: opens the line to the broker when needed, joins and leaves documents
    /// as tabs open and close, and sends what was typed.
    pub(super) fn live_tick(&mut self, ctx: &Context) {
        let host = self.host.trim().to_string();
        if !self.live_wanted() || (self.live.proc.is_some() && self.live.host != host) {
            if self.live.proc.is_some() {
                self.live_stop();
            }
            if !self.live_wanted() {
                return;
            }
        }
        if self.live.proc.is_none() {
            let proc = self.request(Request::ProcOpen {
                command: share::COLLAB.into(),
                cwd: self.root.clone(),
            });
            self.live.proc = Some(proc);
            self.live.host = host;
        }
        let Some(me) = self.github.me.clone() else {
            return;
        };
        let user = Peer {
            login: me.login.clone(),
            name: me.shown_name().to_string(),
            avatar: me.avatar_url.clone(),
        };
        // Files opened since: joined. Files closed: left.
        let open: Vec<String> = self
            .tabs
            .iter()
            .filter(|t| t.preview.is_none())
            .map(|t| t.path.clone())
            .collect();
        let gone: Vec<String> = self
            .live
            .docs
            .keys()
            .filter(|path| !open.contains(path))
            .cloned()
            .collect();
        for doc in gone {
            self.live.docs.remove(&doc);
            self.live_send(&ToBroker::Leave { doc });
        }
        for index in 0..self.tabs.len() {
            let tab = &self.tabs[index];
            if tab.preview.is_some() || self.live.docs.contains_key(&tab.path) {
                continue;
            }
            let (doc, text, revision) = (tab.path.clone(), tab.buf.plain_text(), tab.buf.revision);
            self.live.docs.insert(
                doc.clone(),
                Doc {
                    client: Client::new(0),
                    shadow: text.clone(),
                    joined: false,
                    others: HashMap::new(),
                    revision_seen: revision,
                    cursor_sent: (usize::MAX, usize::MAX),
                },
            );
            self.live_send(&ToBroker::Join {
                doc,
                user: user.clone(),
                text,
            });
        }
        // RIDE_LIVE_TYPE=TEXT types that at the end of the first shared file once joined,
        // to watch an edit arrive in another copy of ride.
        if let Ok(text) = std::env::var("RIDE_LIVE_TYPE") {
            let joined = self
                .tabs
                .iter()
                .position(|t| self.live.docs.get(&t.path).is_some_and(|d| d.joined));
            if let (Some(index), false) = (joined, self.live.typed) {
                self.live.typed = true;
                let buf = &mut self.tabs[index].buf;
                let end = Pos::new(buf.line_count() - 1, usize::MAX);
                buf.set_cursor(end, false);
                buf.insert(&text);
            }
        }
        for index in 0..self.tabs.len() {
            self.live_flush(index);
        }
        self.live_marks(ctx);
    }

    /// Sends a tab's new edits and its cursor, if they changed.
    fn live_flush(&mut self, index: usize) {
        let tab = &self.tabs[index];
        let Some(doc) = self.live.docs.get_mut(&tab.path).filter(|d| d.joined) else {
            return;
        };
        let mut out = Vec::new();
        let mut rejoin = false;
        if doc.revision_seen != tab.buf.revision {
            doc.revision_seen = tab.buf.revision;
            let text = tab.buf.plain_text();
            if text != doc.shadow {
                let op = TextOp::diff(&doc.shadow, &text);
                doc.shadow = text;
                match doc.client.local(op) {
                    Ok(Some((rev, op))) => out.push(ToBroker::Op {
                        doc: tab.path.clone(),
                        rev,
                        op,
                    }),
                    Ok(None) => {}
                    Err(_) => rejoin = true,
                }
            }
        }
        if rejoin {
            out.push(self.live_rejoin_msg(index));
        }
        let tab = &self.tabs[index];
        if let Some(doc) = self.live.docs.get_mut(&tab.path) {
            let cursor = (
                tab.buf.offset_of(tab.buf.cursor),
                tab.buf.offset_of(tab.buf.anchor),
            );
            if cursor != doc.cursor_sent {
                doc.cursor_sent = cursor;
                out.push(ToBroker::Cursor {
                    doc: tab.path.clone(),
                    at: cursor.0,
                    anchor: cursor.1,
                });
            }
        }
        for msg in out {
            self.live_send(&msg);
        }
    }

    /// Out of step with the broker: join again, to get the text as it is.
    fn live_rejoin_msg(&mut self, index: usize) -> ToBroker {
        let tab = &self.tabs[index];
        let me = self.github.me.clone().unwrap_or_default();
        if let Some(doc) = self.live.docs.get_mut(&tab.path) {
            doc.joined = false;
        }
        ToBroker::Join {
            doc: tab.path.clone(),
            user: Peer {
                login: me.login.clone(),
                name: me.shown_name().to_string(),
                avatar: me.avatar_url,
            },
            text: tab.buf.plain_text(),
        }
    }

    /// Output of the line to the broker; false if the message isn't for it.
    pub(super) fn live_message(&mut self, id: u64, resp: &Response) -> bool {
        if self.live.proc != Some(id) {
            return false;
        }
        match resp {
            Response::ProcOutput { data } => {
                self.live.partial.extend_from_slice(data);
                while let Some(end) = self.live.partial.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = self.live.partial.drain(..=end).collect();
                    if let Ok(msg) = serde_json::from_slice::<FromBroker>(&line) {
                        self.live_receive(msg);
                    }
                }
            }
            Response::ProcExit { .. } | Response::Error { .. } => {
                // Gone (an old host without the broker, or it stopped): try again later.
                self.live.proc = None;
                self.live.docs.clear();
            }
            _ => {}
        }
        true
    }

    fn live_receive(&mut self, msg: FromBroker) {
        let doc_of = |msg: &FromBroker| match msg {
            FromBroker::Doc { doc, .. }
            | FromBroker::Ack { doc }
            | FromBroker::Op { doc, .. }
            | FromBroker::Joined { doc, .. }
            | FromBroker::Left { doc, .. }
            | FromBroker::Cursor { doc, .. }
            | FromBroker::Saved { doc, .. } => doc.clone(),
        };
        let path = doc_of(&msg);
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.path == path && t.preview.is_none())
        else {
            return;
        };
        // Whatever was typed since the last frame goes first, so the text matches.
        self.live_flush(index);
        let mut send = Vec::new();
        let tab = &mut self.tabs[index];
        let Some(doc) = self.live.docs.get_mut(&path) else {
            return;
        };
        match msg {
            FromBroker::Doc {
                rev, text, peers, ..
            } => {
                doc.client = Client::new(rev);
                doc.joined = true;
                doc.others = peers
                    .into_iter()
                    .map(|(id, peer)| (id, Other { peer, at: None }))
                    .collect();
                // The shared text is the one to edit; someone else may have changed it.
                if tab.buf.plain_text() != text {
                    tab.buf.apply_remote(&text);
                }
                doc.shadow = text;
                doc.revision_seen = tab.buf.revision;
                doc.cursor_sent = (usize::MAX, usize::MAX);
            }
            FromBroker::Ack { .. } => {
                if let Some((rev, op)) = doc.client.ack() {
                    send.push(ToBroker::Op {
                        doc: path.clone(),
                        rev,
                        op,
                    });
                }
            }
            FromBroker::Op { op, .. } => {
                let applied = doc
                    .client
                    .remote(op)
                    .and_then(|op| op.apply(&doc.shadow).map(|text| (op, text)));
                match applied {
                    Ok((op, text)) => {
                        tab.buf.apply_remote(&text);
                        doc.shadow = text;
                        doc.revision_seen = tab.buf.revision;
                        for other in doc.others.values_mut() {
                            if let Some((at, anchor)) = &mut other.at {
                                *at = op.map(*at, false);
                                *anchor = op.map(*anchor, false);
                            }
                        }
                    }
                    Err(_) => {
                        doc.joined = false;
                        let me = self.github.me.clone().unwrap_or_default();
                        send.push(ToBroker::Join {
                            doc: path.clone(),
                            user: Peer {
                                login: me.login.clone(),
                                name: me.shown_name().to_string(),
                                avatar: me.avatar_url,
                            },
                            text: tab.buf.plain_text(),
                        });
                    }
                }
            }
            FromBroker::Joined { id, user, .. } => {
                self.status = format!("{} opened {}", user.name, file_name(&path));
                doc.others.insert(
                    id,
                    Other {
                        peer: user,
                        at: None,
                    },
                );
            }
            FromBroker::Left { id, .. } => {
                doc.others.remove(&id);
            }
            FromBroker::Cursor { id, at, anchor, .. } => {
                if let Some(other) = doc.others.get_mut(&id) {
                    other.at = Some((at, anchor));
                }
            }
            FromBroker::Saved { id, .. } => {
                // The file on the host now holds the shared text; so does this tab, unless
                // something is still on its way.
                let settled =
                    doc.client.outstanding.is_none() && doc.shadow == tab.buf.plain_text();
                if settled {
                    tab.buf.mark_saved(tab.buf.revision);
                }
                if let Some(other) = doc.others.get(&id) {
                    self.status = format!("{} saved {}", other.peer.name, file_name(&path));
                }
            }
        }
        for msg in send {
            self.live_send(&msg);
        }
    }

    /// After a save here: the others' tabs are saved too.
    pub(super) fn live_saved(&mut self, path: &str) {
        if self.live.docs.get(path).is_some_and(|d| d.joined) {
            self.live_send(&ToBroker::Saved {
                doc: path.to_string(),
            });
        }
    }

    /// Others' names, colours, pictures and places, for each tab's editor.
    fn live_marks(&mut self, ctx: &Context) {
        for index in 0..self.tabs.len() {
            let path = self.tabs[index].path.clone();
            let Some(doc) = self.live.docs.get(&path) else {
                self.tabs[index].view.peers.clear();
                continue;
            };
            let mut ids: Vec<u64> = doc.others.keys().copied().collect();
            ids.sort();
            let others: Vec<(u64, Peer, Option<(usize, usize)>)> = ids
                .iter()
                .map(|id| (*id, doc.others[id].peer.clone(), doc.others[id].at))
                .collect();
            let marks = others
                .into_iter()
                .map(|(id, peer, at)| {
                    let buf = &self.tabs[index].buf;
                    Mark {
                        name: if peer.name.is_empty() {
                            peer.login.clone()
                        } else {
                            peer.name.clone()
                        },
                        color: peer_color(id),
                        avatar: self.github.avatar(ctx, &peer.avatar),
                        at: at.map(|(a, b)| (buf.pos_at(a), buf.pos_at(b))),
                    }
                })
                .collect();
            self.tabs[index].view.peers = marks;
        }
    }
}
