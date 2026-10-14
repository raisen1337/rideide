//! Live sessions: the folder open on a connection, shared with people who have no way
//! onto that server. The host starts a session and gets an ID; a guest types the ID where
//! a host name would go, asks to come in, and once let in sees the folder and edits it
//! live with the host.
//!
//! The two copies of ride meet directly (iroh: an encrypted QUIC link between the two
//! computers, through a public relay only when no direct path can be found). A guest's
//! requests travel to the host's ride, which checks each one — files inside the shared
//! folder, and the line to the live-editing broker, nothing else — and passes it on to
//! its own connection. The guest never reaches the server itself.

use super::*;
use iroh::endpoint::{presets, RecvStream, SendStream};
use iroh::{Endpoint, EndpointId};
use ride_proto::ot::Peer;
use ride_proto::ClientMsg;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

/// Versioned, so copies of ride that speak differently never pair up.
const ALPN: &[u8] = b"ride/live-session/1";
/// The one command a guest may have run on the server: the line to the live-editing broker.
pub(super) const COLLAB: &str = "exec \"$HOME/.cache/ride/ride-agent\" collab";

#[derive(Serialize, Deserialize)]
enum Wire {
    /// Guest → host, first: who asks to come in.
    Knock {
        user: Peer,
    },
    /// Host → guest: let in, to this folder.
    Welcome {
        host: Peer,
        root: String,
    },
    Refused {
        reason: String,
    },
    Req(ClientMsg),
    Resp(ServerMsg),
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ride-live")
            .enable_all()
            .build()
            .expect("the network runtime starts")
    })
}

async fn send(stream: &mut SendStream, wire: &Wire) -> Result<(), String> {
    let bytes = postcard::to_allocvec(wire).map_err(|e| e.to_string())?;
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(&bytes).await.map_err(|e| e.to_string())
}

/// `None` once the stream has ended or sent something unreadable.
async fn recv(stream: &mut RecvStream) -> Option<Wire> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await.ok()?;
    let len = u32::from_le_bytes(len) as usize;
    if len > 256 * 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.ok()?;
    postcard::from_bytes(&buf).ok()
}

/// A session's ID: its endpoint's public key in lowercase base32, 52 letters and digits.
fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let (mut out, mut buffer, mut bits) = (String::new(), 0u64, 0u32);
    for &byte in bytes {
        buffer = (buffer << 8) | byte as u64;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn parse_id(text: &str) -> Option<EndpointId> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let text = text.to_ascii_lowercase();
    let ok = (text.len() == 52 && text.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7')))
        || (text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit()));
    if !ok {
        return None;
    }
    text.parse().ok()
}

/// Whether what was typed in the host box is a session's ID rather than a host.
pub(super) fn is_session_id(text: &str) -> bool {
    parse_id(text).is_some()
}

/// The host's name for each session joined, by ID, for the top bar.
fn names() -> &'static Mutex<HashMap<String, String>> {
    static NAMES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    NAMES.get_or_init(Default::default)
}

/// What the top bar shows for a joined session instead of its ID.
pub(super) fn label(host: &str) -> Option<String> {
    let host = host.trim();
    is_session_id(host).then(|| match names().lock().unwrap().get(host) {
        Some(name) => format!("{name}'s session"),
        None => "Live session".to_string(),
    })
}

/// Set while the connection shown is a session joined as a guest.
static GUEST: AtomicBool = AtomicBool::new(false);

pub(super) fn set_guest(guest: bool) {
    GUEST.store(guest, Ordering::Relaxed);
}

pub(super) fn guest() -> bool {
    GUEST.load(Ordering::Relaxed)
}

/// Who you are to others: your GitHub account, or else the nickname you chose.
pub(super) fn me(github: &github::GitHub) -> Option<Peer> {
    if let Some(me) = &github.me {
        return Some(Peer {
            login: me.login.clone(),
            name: me.shown_name().to_string(),
            avatar: me.avatar_url.clone(),
        });
    }
    let nickname = settings::with(|s| s.nickname.trim().to_string());
    (!nickname.is_empty()).then(|| Peer {
        login: nickname.clone(),
        name: nickname,
        avatar: String::new(),
    })
}

/// What asking for a nickname was for.
pub(super) enum AfterNickname {
    Share,
    /// Joining the session whose ID is in the host box.
    Join,
}

pub(super) fn peer_name(peer: &Peer) -> &str {
    if peer.name.trim().is_empty() {
        &peer.login
    } else {
        &peer.name
    }
}

/// Joins a session as a guest; blocking, for the connecting thread. The link it returns
/// carries the app's requests to the host and the answers back.
pub(super) fn join(
    text: &str,
    me: Peer,
    status: &Mutex<String>,
    wake: impl Fn() + Send + Sync + 'static,
) -> Result<Conn, String> {
    let id = parse_id(text).ok_or("That isn't a session ID")?;
    let wake = Arc::new(wake);
    let say = |message: &str| {
        *status.lock().unwrap() = message.to_string();
        wake();
    };
    say("Finding the session…");
    let rt = runtime();
    let (endpoint, conn, mut to_host, mut from_host) = rt.block_on(async {
        let endpoint = Endpoint::bind(presets::N0)
            .await
            .map_err(|e| format!("couldn't start the network: {e}"))?;
        let mut last = String::new();
        // A session that has just started may take a moment to be findable.
        for _ in 0..3 {
            match tokio::time::timeout(Duration::from_secs(20), endpoint.connect(id, ALPN)).await {
                Ok(Ok(conn)) => match conn.open_bi().await {
                    Ok((to, from)) => return Ok((endpoint, conn, to, from)),
                    Err(e) => last = e.to_string(),
                },
                Ok(Err(e)) => last = e.to_string(),
                Err(_) => last = "no answer".into(),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        endpoint.close().await;
        Err(format!(
            "The session wasn't found ({last}). Check the ID, and that the session is still on."
        ))
    })?;

    say("Waiting for the host to let you in…");
    let answer = rt.block_on(async {
        send(&mut to_host, &Wire::Knock { user: me }).await?;
        Ok::<_, String>(
            tokio::time::timeout(Duration::from_secs(600), recv(&mut from_host))
                .await
                .ok()
                .flatten(),
        )
    });
    let leave = |endpoint: Endpoint| {
        rt.spawn(async move { endpoint.close().await });
    };
    match answer {
        Ok(Some(Wire::Welcome { host, .. })) => {
            let key: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            names()
                .lock()
                .unwrap()
                .insert(key, peer_name(&host).to_string());
        }
        Ok(Some(Wire::Refused { reason })) => {
            leave(endpoint);
            return Err(reason);
        }
        Ok(_) => {
            leave(endpoint);
            return Err("The session ended before you were let in".into());
        }
        Err(error) => {
            leave(endpoint);
            return Err(error);
        }
    }

    let (tx, outgoing) = channel::<ClientMsg>();
    let (event_tx, events) = channel();
    let (to_net, mut from_app) = unbounded_channel::<ClientMsg>();
    thread::spawn(move || {
        for msg in outgoing {
            if to_net.send(msg).is_err() {
                break;
            }
        }
    });
    let closing = conn.clone();
    rt.spawn(async move {
        while let Some(msg) = from_app.recv().await {
            if send(&mut to_host, &Wire::Req(msg)).await.is_err() {
                break;
            }
        }
        // The app let go of the link: leave the session.
        closing.close(0u32.into(), b"left");
        endpoint.close().await;
    });
    rt.spawn(async move {
        let reason = loop {
            match recv(&mut from_host).await {
                Some(Wire::Resp(msg)) => {
                    if event_tx.send(ConnEvent::Msg(msg)).is_err() {
                        return;
                    }
                    wake();
                }
                Some(_) => {}
                None => break "the live session ended",
            }
        };
        let _ = event_tx.send(ConnEvent::Closed(reason.into()));
        wake();
        drop(conn);
    });
    Ok(Conn::from_channels(tx, events))
}

enum Event {
    /// The session is up, with its ID, or couldn't start.
    Ready(Result<String, String>),
    Knock {
        key: u64,
        user: Peer,
        tx: UnboundedSender<Wire>,
    },
    Req {
        key: u64,
        msg: ClientMsg,
    },
    Gone {
        key: u64,
    },
}

pub(super) struct Guest {
    key: u64,
    pub(super) user: Peer,
    tx: UnboundedSender<Wire>,
    /// Their processes on the server: their id for each, and the host's.
    procs: HashMap<u64, u64>,
}

/// A session this copy of ride is hosting.
pub(super) struct Share {
    /// Its ID, once it is up.
    pub(super) id: Option<String>,
    pub(super) error: String,
    /// The connection shared, and its folder.
    pub(super) uid: u64,
    pub(super) root: String,
    pub(super) waiting: Vec<Guest>,
    pub(super) guests: Vec<Guest>,
    /// The panel is shown.
    pub(super) open: bool,
    events: Receiver<Event>,
    endpoint: Arc<Mutex<Option<Endpoint>>>,
    stopped: Arc<AtomicBool>,
    /// Requests passed on for guests: the host's id, and whose and which request it was.
    routes: HashMap<u64, (u64, u64)>,
}

impl Drop for Share {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(endpoint) = self.endpoint.lock().unwrap().take() {
            runtime().spawn(async move { endpoint.close().await });
        }
    }
}

fn start(ctx: &Context, uid: u64, root: String) -> Share {
    let (events_tx, events) = channel();
    let endpoint_slot = Arc::new(Mutex::new(None));
    let stopped = Arc::new(AtomicBool::new(false));
    let (slot, stop, ctx) = (endpoint_slot.clone(), stopped.clone(), ctx.clone());
    runtime().spawn(async move {
        let endpoint = match Endpoint::builder(presets::N0)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
        {
            Ok(endpoint) => endpoint,
            Err(e) => {
                let _ = events_tx.send(Event::Ready(Err(e.to_string())));
                ctx.request_repaint();
                return;
            }
        };
        // Reachable through the relay, and published, before the ID is handed out.
        let _ = tokio::time::timeout(Duration::from_secs(10), endpoint.online()).await;
        if stop.load(Ordering::Relaxed) {
            endpoint.close().await;
            return;
        }
        *slot.lock().unwrap() = Some(endpoint.clone());
        let _ = events_tx.send(Event::Ready(Ok(encode(endpoint.id().as_bytes()))));
        ctx.request_repaint();
        let mut next = 0;
        while let Some(incoming) = endpoint.accept().await {
            next += 1;
            let (key, events_tx, ctx) = (next, events_tx.clone(), ctx.clone());
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else {
                    return;
                };
                let Ok((mut to_guest, mut from_guest)) = conn.accept_bi().await else {
                    return;
                };
                let Some(Wire::Knock { user }) = recv(&mut from_guest).await else {
                    return;
                };
                let (tx, mut rx) = unbounded_channel::<Wire>();
                if events_tx.send(Event::Knock { key, user, tx }).is_err() {
                    return;
                }
                ctx.request_repaint();
                let closing = conn.clone();
                tokio::spawn(async move {
                    while let Some(wire) = rx.recv().await {
                        let refused = matches!(wire, Wire::Refused { .. });
                        if send(&mut to_guest, &wire).await.is_err() || refused {
                            break;
                        }
                    }
                    // Refused, removed, or the session stopped: what was sent gets there
                    // before the link closes.
                    let _ = to_guest.finish();
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    closing.close(0u32.into(), b"bye");
                });
                while let Some(wire) = recv(&mut from_guest).await {
                    if let Wire::Req(msg) = wire {
                        if events_tx.send(Event::Req { key, msg }).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                }
                let _ = events_tx.send(Event::Gone { key });
                ctx.request_repaint();
            });
        }
    });
    Share {
        id: None,
        error: String::new(),
        uid,
        root,
        waiting: Vec::new(),
        guests: Vec::new(),
        open: true,
        events,
        endpoint: endpoint_slot,
        stopped,
        routes: HashMap::new(),
    }
}

/// Whether a path is the shared folder or inside it. Paths with `.` or `..` in them are
/// refused outright rather than worked out.
fn inside(root: &str, path: &str) -> bool {
    let root = root.trim_end_matches('/');
    path.starts_with('/')
        && !path.split('/').any(|part| part == ".." || part == ".")
        && (root.is_empty() || path == root || path.starts_with(&format!("{root}/")))
}

/// The answers that more of the same request may follow.
fn streaming(resp: &Response) -> bool {
    matches!(
        resp,
        Response::SearchHits { .. }
            | Response::DownloadInfo { .. }
            | Response::Chunk { .. }
            | Response::ProcOutput { .. }
            | Response::ProcStderr { .. }
    )
}

impl App {
    /// The connection shown is a session joined as a guest.
    pub(super) fn guest(&self) -> bool {
        is_session_id(self.host.trim())
    }

    /// Asks for the name others will see, when there's no GitHub sign-in to take it from.
    pub(super) fn nickname_window(&mut self, ctx: &Context) {
        let Some((text, _)) = &mut self.nickname_ask else {
            return;
        };
        let (mut go, mut close) = (false, false);
        egui::Window::new("Choose a nickname")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 120.0])
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(
                    RichText::new(
                        "The name the others in the session see on your cursor. You can sign in with GitHub instead, in Settings → Account.",
                    )
                    .small()
                    .color(theme::COMMENT()),
                );
                ui.add_space(4.0);
                let edit = ui.add(
                    TextEdit::singleline(text)
                        .desired_width(f32::INFINITY)
                        .char_limit(32)
                        .hint_text("Nickname"),
                );
                edit.request_focus();
                let ok = !text.trim().is_empty();
                go = ok && edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.horizontal(|ui| {
                    if ui.add_enabled(ok, egui::Button::new("Continue")).clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        close = true;
                    }
                });
            });
        if close {
            self.nickname_ask = None;
        }
        if !go {
            return;
        }
        let Some((text, after)) = self.nickname_ask.take() else {
            return;
        };
        let nickname = text.trim().to_string();
        settings::change(|s| s.nickname = nickname);
        match after {
            AfterNickname::Share => self.share_start(ctx),
            AfterNickname::Join => self.connect(ctx),
        }
    }

    pub(super) fn share_start(&mut self, ctx: &Context) {
        if let Some(share) = &mut self.share {
            share.open = true;
            return;
        }
        let problem = if !self.online() {
            Some("Connect to a server first, then share its folder")
        } else if self.guest() {
            Some("This is someone else's session; only its host can share it")
        } else if chat::local_shell() {
            Some("Live sessions share a folder on a Linux server, not one on this computer")
        } else {
            None
        };
        if problem.is_none() && me(&self.github).is_none() {
            self.nickname_ask = Some((String::new(), AfterNickname::Share));
            return;
        }
        if let Some(problem) = problem {
            self.status = problem.into();
            return;
        }
        self.share = Some(start(ctx, self.session_uid, self.root.clone()));
        self.status = "Starting a live session…".into();
    }

    pub(super) fn share_stop(&mut self) {
        let Some(mut share) = self.share.take() else {
            return;
        };
        let uid = share.uid;
        for guest in share.guests.drain(..) {
            for host_proc in guest.procs.values() {
                self.share_request(uid, Request::ProcClose { proc: *host_proc });
            }
        }
        self.status = "The live session ended".into();
    }

    /// Sends a request on the connection with this uid, shown or not; 0 if it's gone.
    fn share_request(&mut self, uid: u64, req: Request) -> u64 {
        if uid == self.session_uid {
            self.request(req)
        } else if let Some(index) = self.sessions.iter().position(|s| s.uid == uid) {
            self.with_session(index, |app| app.request(req))
        } else {
            0
        }
    }

    fn share_drop_guest(&mut self, key: u64) -> Option<Peer> {
        let share = self.share.as_mut()?;
        let uid = share.uid;
        let guest = if let Some(i) = share.guests.iter().position(|g| g.key == key) {
            share.guests.remove(i)
        } else {
            let i = share.waiting.iter().position(|g| g.key == key)?;
            share.waiting.remove(i)
        };
        share.routes.retain(|_, (k, _)| *k != key);
        for host_proc in guest.procs.values() {
            self.share_request(uid, Request::ProcClose { proc: *host_proc });
        }
        Some(guest.user)
    }

    pub(super) fn share_admit(&mut self, key: u64, admit: bool) {
        let me = me(&self.github).unwrap_or_default();
        let Some(share) = self.share.as_mut() else {
            return;
        };
        let Some(index) = share.waiting.iter().position(|g| g.key == key) else {
            return;
        };
        let guest = share.waiting.remove(index);
        if admit {
            let _ = guest.tx.send(Wire::Welcome {
                host: me,
                root: share.root.clone(),
            });
            self.status = format!("{} joined the live session", peer_name(&guest.user));
            share.guests.push(guest);
        } else {
            let _ = guest.tx.send(Wire::Refused {
                reason: "The host didn't let you in".into(),
            });
        }
    }

    pub(super) fn share_remove(&mut self, key: u64) {
        if let Some(user) = self.share_drop_guest(key) {
            self.status = format!("{} was removed from the session", peer_name(&user));
        }
    }

    /// Once a frame: takes in what guests sent, and ends the session if its connection is gone.
    pub(super) fn share_tick(&mut self, ctx: &Context) {
        // RIDE_SHARE=FILE starts a session once a folder is open, writes its ID to FILE and
        // lets every guest in, to try sessions between two copies of ride.
        static STARTED: AtomicBool = AtomicBool::new(false);
        let hook = std::env::var("RIDE_SHARE").ok();
        if hook.is_some()
            && self.share.is_none()
            && self.online()
            && !self.root.is_empty()
            && !STARTED.swap(true, Ordering::Relaxed)
        {
            self.share_start(ctx);
        }
        // RIDE_SHARE_STOP=SECS then stops it that long after it started, as Stop does.
        static STARTED_AT: OnceLock<std::time::Instant> = OnceLock::new();
        if let (Some(_), Ok(secs)) = (&self.share, std::env::var("RIDE_SHARE_STOP")) {
            let since = STARTED_AT.get_or_init(std::time::Instant::now);
            if since.elapsed().as_secs() >= secs.parse::<u64>().unwrap_or(u64::MAX) {
                self.share_stop();
            }
        }
        let Some(share) = &self.share else {
            return;
        };
        let uid = share.uid;
        let shown = uid == self.session_uid && self.online();
        let waiting_alive = self
            .sessions
            .iter()
            .any(|s| s.uid == uid && matches!(s.link, Link::Online(_)));
        if !shown && !waiting_alive {
            self.share_stop();
            self.status = "The live session ended: its connection closed".into();
            return;
        }
        let events: Vec<Event> = share.events.try_iter().collect();
        for event in events {
            match event {
                Event::Ready(Ok(id)) => {
                    if let Some(file) = &hook {
                        let _ = std::fs::write(file, &id);
                    }
                    if let Some(share) = &mut self.share {
                        share.id = Some(id);
                    }
                    self.status = "Live session started: send its ID to who you want in".into();
                }
                Event::Ready(Err(error)) => {
                    if let Some(share) = &mut self.share {
                        share.error = error;
                    }
                }
                Event::Knock { key, user, tx } => {
                    let name = peer_name(&user).to_string();
                    if let Some(share) = &mut self.share {
                        share.waiting.push(Guest {
                            key,
                            user,
                            tx,
                            procs: HashMap::new(),
                        });
                        share.open = true;
                    }
                    if hook.is_some() {
                        self.share_admit(key, true);
                    } else {
                        self.notify("Live session", &format!("{name} asks to join"));
                    }
                }
                Event::Req { key, msg } => self.share_forward(key, msg),
                Event::Gone { key } => {
                    let joined = self
                        .share
                        .as_ref()
                        .is_some_and(|s| s.guests.iter().any(|g| g.key == key));
                    if let Some(user) = self.share_drop_guest(key) {
                        if joined {
                            self.status = format!("{} left the live session", peer_name(&user));
                        }
                    }
                }
            }
        }
    }

    /// A guest's request: checked, then passed on to the shared connection.
    fn share_forward(&mut self, key: u64, ClientMsg { id, req }: ClientMsg) {
        let Some(share) = &mut self.share else {
            return;
        };
        let (uid, root) = (share.uid, share.root.clone());
        let Some(guest) = share.guests.iter_mut().find(|g| g.key == key) else {
            return;
        };
        let tx = guest.tx.clone();
        let reply = |resp: Response| {
            let _ = tx.send(Wire::Resp(ServerMsg { id, resp }));
        };
        let refuse = |what: &str| Response::Error {
            message: format!("{what} isn't available in a live session"),
        };
        let within = |path: &str| inside(&root, path);
        let mut opens_proc = false;
        let mut answered = true;
        let req = match req {
            Request::Hello => {
                reply(Response::Hello { home: root });
                return;
            }
            Request::ListDir { path } if within(&path) => Request::ListDir { path },
            Request::ReadFile { path } if within(&path) => Request::ReadFile { path },
            Request::WriteFile { path, data } if within(&path) => Request::WriteFile { path, data },
            Request::CreateFile { path } if within(&path) => Request::CreateFile { path },
            Request::CreateDir { path } if within(&path) => Request::CreateDir { path },
            Request::Delete { path } if within(&path) && !inside(&path, &root) => {
                Request::Delete { path }
            }
            Request::Rename { from, to }
                if within(&from) && within(&to) && !inside(&from, &root) =>
            {
                Request::Rename { from, to }
            }
            Request::Search {
                root: folder,
                query,
                regex,
                case_sensitive,
            } if within(&folder) => Request::Search {
                root: folder,
                query,
                regex,
                case_sensitive,
            },
            Request::FindFiles {
                root: folder,
                query,
            } if within(&folder) => Request::FindFiles {
                root: folder,
                query,
            },
            Request::Download { path } if within(&path) => Request::Download { path },
            Request::Cancel { target } => {
                let found = share
                    .routes
                    .iter()
                    .find(|(_, route)| **route == (key, target))
                    .map(|(host_id, _)| *host_id);
                let Some(target) = found else {
                    return;
                };
                answered = false;
                Request::Cancel { target }
            }
            Request::ProcOpen { command, .. } if command == COLLAB => {
                opens_proc = true;
                Request::ProcOpen { command, cwd: root }
            }
            Request::ProcInput { proc, data } => {
                let Some(proc) = guest.procs.get(&proc).copied() else {
                    return;
                };
                answered = false;
                Request::ProcInput { proc, data }
            }
            Request::ProcClose { proc } => {
                let Some(proc) = guest.procs.remove(&proc) else {
                    return;
                };
                answered = false;
                Request::ProcClose { proc }
            }
            Request::TermOpen { .. } => return reply(refuse("A terminal")),
            Request::ProcOpen { .. } => return reply(refuse("Running commands")),
            Request::HostAnswer { .. }
            | Request::TermInput { .. }
            | Request::TermResize { .. }
            | Request::TermClose { .. } => return,
            Request::ClaudeSessions { .. } => return reply(refuse("Claude")),
            _ => {
                return reply(Response::Error {
                    message: "Only files inside the shared folder can be reached".into(),
                })
            }
        };
        let host_id = self.share_request(uid, req);
        let Some(share) = &mut self.share else {
            return;
        };
        if answered && host_id != 0 {
            share.routes.insert(host_id, (key, id));
        }
        if opens_proc {
            if let Some(guest) = share.guests.iter_mut().find(|g| g.key == key) {
                guest.procs.insert(id, host_id);
            }
        }
    }

    /// An answer from the server: passed to the guest who asked, and kept from the rest of
    /// the app. The others come back to be handled as usual.
    pub(super) fn share_take(&mut self, msg: ServerMsg) -> Option<ServerMsg> {
        let Some(share) = &mut self.share else {
            return Some(msg);
        };
        let Some(&(key, id)) = share.routes.get(&msg.id) else {
            return Some(msg);
        };
        if !streaming(&msg.resp) {
            share.routes.remove(&msg.id);
        }
        if let Some(guest) = share.guests.iter_mut().find(|g| g.key == key) {
            if matches!(msg.resp, Response::ProcExit { .. } | Response::Error { .. }) {
                guest.procs.retain(|_, host| *host != msg.id);
            }
            let _ = guest.tx.send(Wire::Resp(ServerMsg { id, resp: msg.resp }));
        }
        None
    }

    /// The panel of a hosted session: its ID, who asks to join, who is in, and stopping it.
    pub(super) fn share_window(&mut self, ctx: &Context) {
        let Some(share) = &self.share else {
            return;
        };
        if !share.open {
            return;
        }
        enum Act {
            Admit(u64, bool),
            Remove(u64),
            Stop,
            Hide,
        }
        let mut act = None;
        let people: Vec<(u64, Peer, bool)> = share
            .waiting
            .iter()
            .map(|g| (g.key, g.user.clone(), true))
            .chain(share.guests.iter().map(|g| (g.key, g.user.clone(), false)))
            .collect();
        let (id, error, root) = (share.id.clone(), share.error.clone(), share.root.clone());
        let avatars: Vec<Option<egui::TextureId>> = people
            .iter()
            .map(|(_, user, _)| self.github.avatar(ctx, &user.avatar))
            .collect();
        egui::Area::new(egui::Id::new("live_session_panel"))
            .order(egui::Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, [-12.0, -34.0])
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(ui.visuals().window_fill)
                    .stroke((1.0, theme::tint(34)))
                    .corner_radius(12)
                    .shadow(egui::Shadow {
                        offset: [0, 10],
                        blur: 30,
                        spread: 0,
                        color: Color32::from_black_alpha(120),
                    })
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Live session").strong().size(15.0));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, icons::CLOSE, "Hide (the session goes on)") {
                                    act = Some(Act::Hide);
                                }
                            });
                        });
                        ui.label(
                            RichText::new(format!("Sharing {root}"))
                                .small()
                                .color(theme::COMMENT()),
                        );
                        ui.add_space(8.0);
                        match &id {
                            Some(id) => {
                                ui.label(
                                    RichText::new(
                                        "Send this ID to the people you want in. They paste it in ride where a host name goes.",
                                    )
                                    .small()
                                    .color(theme::COMMENT()),
                                );
                                ui.add_space(4.0);
                                Frame::NONE
                                    .fill(theme::tint(10))
                                    .corner_radius(8)
                                    .inner_margin(Margin::symmetric(10, 8))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(id).monospace().color(theme::STRONG()),
                                            )
                                            .wrap(),
                                        );
                                    });
                                ui.add_space(4.0);
                                if ui.button(format!("{}  Copy ID", icons::COPY)).clicked() {
                                    ui.ctx().copy_text(id.clone());
                                }
                            }
                            None if !error.is_empty() => {
                                ui.label(
                                    RichText::new(format!("Couldn't start: {error}"))
                                        .color(theme::ANSI()[1]),
                                );
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(RichText::new("Starting…").color(theme::COMMENT()));
                                });
                            }
                        }
                        if !people.is_empty() {
                            ui.add_space(10.0);
                        }
                        for ((key, user, waiting), avatar) in people.iter().zip(&avatars) {
                            ui.horizontal(|ui| {
                                match avatar {
                                    Some(texture) => {
                                        ui.add(
                                            egui::Image::new(egui::load::SizedTexture::new(
                                                *texture,
                                                vec2(24.0, 24.0),
                                            ))
                                            .corner_radius(12),
                                        );
                                    }
                                    None => {
                                        let (rect, _) =
                                            ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                                        ui.painter().circle_filled(rect.center(), 12.0, theme::tint(30));
                                    }
                                }
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(peer_name(user)).color(theme::STRONG()));
                                    // A nickname is whatever they typed: it says so.
                                    let who = if user.avatar.is_empty() {
                                        "not signed in with GitHub".to_string()
                                    } else {
                                        format!("@{}", user.login)
                                    };
                                    let note = if *waiting {
                                        format!("{who} · asks to join")
                                    } else {
                                        who
                                    };
                                    ui.label(RichText::new(note).small().color(theme::COMMENT()));
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if *waiting {
                                        if ui.button("Refuse").clicked() {
                                            act = Some(Act::Admit(*key, false));
                                        }
                                        let let_in = egui::Button::new(
                                            RichText::new("Let in").color(Color32::WHITE),
                                        )
                                        .fill(theme::ACCENT().gamma_multiply(0.6));
                                        if ui.add(let_in).clicked() {
                                            act = Some(Act::Admit(*key, true));
                                        }
                                    } else if ui.button("Remove").clicked() {
                                        act = Some(Act::Remove(*key));
                                    }
                                });
                            });
                        }
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(
                                "Guests see and edit the files in this folder, and nothing else: no terminal, Git or Claude.",
                            )
                            .small()
                            .color(theme::COMMENT()),
                        );
                        ui.add_space(6.0);
                        if ui.button(format!("{}  Stop the session", icons::STOP)).clicked() {
                            act = Some(Act::Stop);
                        }
                    });
            });
        match act {
            Some(Act::Admit(key, admit)) => self.share_admit(key, admit),
            Some(Act::Remove(key)) => self.share_remove(key),
            Some(Act::Stop) => self.share_stop(),
            Some(Act::Hide) => {
                if let Some(share) = &mut self.share {
                    share.open = false;
                }
            }
            None => {}
        }
    }

    /// The bottom bar's mark of a live session: hosted (a click shows its panel) or joined.
    pub(super) fn share_pill(&mut self, ui: &mut Ui) {
        let text = match &self.share {
            Some(share) => match (&share.id, share.waiting.len(), share.guests.len()) {
                (None, _, _) => "Live · starting".to_string(),
                (_, 0, 0) => "Live · nobody yet".to_string(),
                (_, 0, n) => format!("Live · {n} in"),
                (_, w, _) => format!("Live · {w} waiting"),
            },
            None if self.guest() && self.online() => {
                label(&self.host).unwrap_or_else(|| "Live session".into())
            }
            None => return,
        };
        let pill = Frame::NONE
            .fill(theme::STRING().gamma_multiply(0.18))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(8, 1))
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{}  {text}", icons::BROADCAST))
                            .small()
                            .color(theme::STRING()),
                    )
                    .selectable(false),
                );
            })
            .response
            .interact(Sense::click());
        if let Some(share) = &mut self.share {
            if pill.clicked() {
                share.open = !share.open;
            }
            pill.on_hover_text("The live session you're hosting");
        } else {
            pill.on_hover_text("You're a guest in this live session");
        }
        ui.add_space(6.0);
    }

    /// The box for a session's ID, from the palette or the connections menu.
    pub(super) fn join_window(&mut self, ctx: &Context) {
        let Some(text) = &mut self.join_input else {
            return;
        };
        let (mut join, mut close) = (false, false);
        egui::Window::new("Join a live session")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 120.0])
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.label(
                    RichText::new("Paste the ID the host sent you.")
                        .small()
                        .color(theme::COMMENT()),
                );
                let edit = ui.add(
                    TextEdit::singleline(text)
                        .desired_width(f32::INFINITY)
                        .hint_text("Session ID")
                        .font(egui::TextStyle::Monospace),
                );
                edit.request_focus();
                let valid = is_session_id(text);
                join = valid && edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.horizontal(|ui| {
                    if ui.add_enabled(valid, egui::Button::new("Join")).clicked() {
                        join = true;
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        close = true;
                    }
                    if !valid && !text.trim().is_empty() {
                        ui.label(
                            RichText::new("That doesn't look like a session ID")
                                .small()
                                .color(theme::COMMENT()),
                        );
                    }
                });
            });
        if join {
            let id: String = self
                .join_input
                .take()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if self.online() || matches!(self.link, Link::Connecting { .. }) {
                self.add_session(ctx, id);
            } else {
                self.host = id;
                self.connect(ctx);
            }
        } else if close {
            self.join_input = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_read_back_as_the_key_they_came_from() {
        let key = iroh::SecretKey::generate().public();
        let id = encode(key.as_bytes());
        assert_eq!(id.len(), 52);
        assert_eq!(parse_id(&id), Some(key));
        assert_eq!(parse_id(&format!("  {}\n", id.to_uppercase())), Some(key));
        assert!(!is_session_id("vespucci"));
        assert!(!is_session_id("root@163.5.63.68"));
    }

    #[test]
    fn guests_stay_inside_the_shared_folder() {
        assert!(inside("/srv/site", "/srv/site"));
        assert!(inside("/srv/site", "/srv/site/a/b.js"));
        assert!(inside("/srv/site/", "/srv/site/a"));
        assert!(!inside("/srv/site", "/srv/site-old/a"));
        assert!(!inside("/srv/site", "/srv/site/../etc/passwd"));
        assert!(!inside("/srv/site", "/srv/site/./a"));
        assert!(!inside("/srv/site", "/etc/passwd"));
        assert!(!inside("/srv/site", "relative/path"));
        assert!(inside("/", "/anything"));
    }
}
