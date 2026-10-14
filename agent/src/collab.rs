//! Live editing. Everyone editing on this host through ride reaches one broker over a
//! socket only this user can open: it keeps the shared text of each document, puts the
//! edits it receives in one order (transforming each past the ones it crossed), and
//! passes them, with the cursors, to the others.
//!
//! `ride-agent collab` is what the app runs: it joins the broker, starting it if there is
//! none, and passes JSON lines between its own stdin/stdout and the broker.

use ride_proto::ot::{FromBroker, Peer, TextOp, ToBroker};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Versioned, so ride versions that speak differently never share a broker.
fn socket_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".cache/ride/collab-v2.sock"))
}

/// `ride-agent collab`: the app's line to the broker.
pub fn connect() {
    let Some(path) = socket_path() else {
        eprintln!("no home folder");
        std::process::exit(1);
    };
    let stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(_) => {
            start_broker();
            let started = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(100));
                if let Ok(stream) = UnixStream::connect(&path) {
                    break stream;
                }
                if started.elapsed() > Duration::from_secs(5) {
                    eprintln!("the live editing broker didn't start");
                    std::process::exit(1);
                }
            }
        }
    };
    let mut to_broker = stream.try_clone().expect("socket");
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if to_broker
                .write_all(format!("{line}\n").as_bytes())
                .and_then(|()| to_broker.flush())
                .is_err()
            {
                break;
            }
        }
        // The app is gone.
        std::process::exit(0);
    });
    let mut stdout = std::io::stdout();
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if writeln!(stdout, "{line}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
    }
    std::process::exit(0);
}

fn start_broker() {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // Its own process group, so it outlives the editor that started it.
    let _ = std::process::Command::new(exe)
        .arg("collab-broker")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
}

struct Doc {
    text: String,
    history: Vec<TextOp>,
    members: HashMap<u64, Peer>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    clients: HashMap<u64, Sender<String>>,
    docs: HashMap<String, Doc>,
}

impl State {
    fn send(&self, to: u64, msg: &FromBroker) {
        if let (Some(tx), Ok(line)) = (self.clients.get(&to), serde_json::to_string(msg)) {
            let _ = tx.send(line);
        }
    }

    fn to_others(&self, doc: &str, but: u64, msg: &FromBroker) {
        let Some(d) = self.docs.get(doc) else {
            return;
        };
        for id in d.members.keys().filter(|&&id| id != but) {
            self.send(*id, msg);
        }
    }

    fn snapshot(&self, doc: &str, for_id: u64) -> Option<FromBroker> {
        let d = self.docs.get(doc)?;
        Some(FromBroker::Doc {
            doc: doc.to_string(),
            rev: d.history.len() as u64,
            text: d.text.clone(),
            peers: d
                .members
                .iter()
                .filter(|(id, _)| **id != for_id)
                .map(|(id, peer)| (*id, peer.clone()))
                .collect(),
        })
    }

    fn leave(&mut self, doc: &str, id: u64) {
        let Some(d) = self.docs.get_mut(doc) else {
            return;
        };
        if d.members.remove(&id).is_none() {
            return;
        }
        if d.members.is_empty() {
            self.docs.remove(doc);
        } else {
            self.to_others(
                doc,
                id,
                &FromBroker::Left {
                    doc: doc.to_string(),
                    id,
                },
            );
        }
    }

    fn handle(&mut self, id: u64, msg: ToBroker) {
        match msg {
            ToBroker::Join { doc, user, text } => {
                let d = self.docs.entry(doc.clone()).or_insert_with(|| Doc {
                    text,
                    history: Vec::new(),
                    members: HashMap::new(),
                });
                d.members.insert(id, user.clone());
                if let Some(snapshot) = self.snapshot(&doc, id) {
                    self.send(id, &snapshot);
                }
                self.to_others(
                    &doc,
                    id,
                    &FromBroker::Joined {
                        doc: doc.clone(),
                        id,
                        user,
                    },
                );
            }
            ToBroker::Leave { doc } => self.leave(&doc, id),
            ToBroker::Op { doc, rev, op, at } => {
                let Some(d) = self
                    .docs
                    .get_mut(&doc)
                    .filter(|d| d.members.contains_key(&id))
                else {
                    return;
                };
                // Past the edits it didn't know of, then onto the text.
                let (mut op, mut at) = (op, at);
                let mut ok = rev as usize <= d.history.len();
                if ok {
                    for done in &d.history[rev as usize..] {
                        match TextOp::transform(&op, done) {
                            Ok((mine, theirs)) => {
                                op = mine;
                                // The cursor was in the sender's text; the edit it didn't
                                // know of moves it along like the rest of that text.
                                at = at.map(|(a, b)| (theirs.map(a, false), theirs.map(b, false)));
                            }
                            Err(_) => {
                                ok = false;
                                break;
                            }
                        }
                    }
                }
                let applied = if ok { op.apply(&d.text).ok() } else { None };
                match applied {
                    Some(text) => {
                        d.text = text;
                        d.history.push(op.clone());
                        self.send(id, &FromBroker::Ack { doc: doc.clone() });
                        self.to_others(
                            &doc,
                            id,
                            &FromBroker::Op {
                                doc: doc.clone(),
                                op,
                                id,
                                at,
                            },
                        );
                    }
                    // Out of step: it gets the text as it is, to start again from.
                    None => {
                        if let Some(snapshot) = self.snapshot(&doc, id) {
                            self.send(id, &snapshot);
                        }
                    }
                }
            }
            ToBroker::Cursor { doc, at, anchor } => {
                self.to_others(
                    &doc,
                    id,
                    &FromBroker::Cursor {
                        doc: doc.clone(),
                        id,
                        at,
                        anchor,
                    },
                );
            }
            ToBroker::Saved { doc } => {
                self.to_others(
                    &doc,
                    id,
                    &FromBroker::Saved {
                        doc: doc.clone(),
                        id,
                    },
                );
            }
        }
    }
}

/// `ride-agent collab-broker`: serves until nobody has been connected for a while.
pub fn broker() {
    let Some(path) = socket_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Two editors starting at once each start a broker: the first to take the socket
    // keeps it, and the other, finding it answering, leaves. A socket nobody answers on
    // is left over from a broker that is gone, and is taken over.
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(_) if UnixStream::connect(&path).is_ok() => return,
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            match UnixListener::bind(&path) {
                Ok(listener) => listener,
                Err(_) => return,
            }
        }
    };
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    let _ = listener.set_nonblocking(true);
    let state = Arc::new(Mutex::new(State::default()));
    let mut idle_since = Some(Instant::now());
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                serve(stream, state.clone());
                idle_since = None;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
        let empty = state.lock().unwrap().clients.is_empty();
        match (empty, idle_since) {
            (false, _) => idle_since = None,
            (true, None) => idle_since = Some(Instant::now()),
            (true, Some(since)) if since.elapsed() > Duration::from_secs(120) => break,
            _ => {}
        }
    }
    let _ = std::fs::remove_file(&path);
}

fn serve(stream: UnixStream, state: Arc<Mutex<State>>) {
    let (tx, rx) = channel::<String>();
    let id = {
        let mut s = state.lock().unwrap();
        s.next_id += 1;
        let id = s.next_id;
        s.clients.insert(id, tx);
        id
    };
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    std::thread::spawn(move || {
        for line in rx {
            if writer
                .write_all(format!("{line}\n").as_bytes())
                .and_then(|()| writer.flush())
                .is_err()
            {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if let Ok(msg) = serde_json::from_str::<ToBroker>(&line) {
                state.lock().unwrap().handle(id, msg);
            }
        }
        let mut s = state.lock().unwrap();
        let docs: Vec<String> = s.docs.keys().cloned().collect();
        for doc in docs {
            s.leave(&doc, id);
        }
        s.clients.remove(&id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    fn client(state: &mut State) -> (u64, Receiver<String>) {
        let (tx, rx) = channel();
        state.next_id += 1;
        state.clients.insert(state.next_id, tx);
        (state.next_id, rx)
    }

    fn read(rx: &Receiver<String>) -> Vec<FromBroker> {
        rx.try_iter()
            .map(|l| serde_json::from_str(&l).unwrap())
            .collect()
    }

    #[test]
    fn the_broker_shares_text_and_edits() {
        let mut s = State::default();
        let (a, rx_a) = client(&mut s);
        let (b, rx_b) = client(&mut s);
        let user = |login: &str| Peer {
            login: login.into(),
            ..Peer::default()
        };
        s.handle(
            a,
            ToBroker::Join {
                doc: "/f".into(),
                user: user("ana"),
                text: "one".into(),
            },
        );
        // The second to join gets the shared text, not its own.
        s.handle(
            b,
            ToBroker::Join {
                doc: "/f".into(),
                user: user("bo"),
                text: "stale".into(),
            },
        );
        assert!(
            matches!(&read(&rx_b)[0], FromBroker::Doc { text, peers, .. } if text == "one" && peers.len() == 1)
        );
        assert!(matches!(
            &read(&rx_a)[..],
            [FromBroker::Doc { .. }, FromBroker::Joined { .. }]
        ));

        // Both edit revision 0 at once; B's arrives second and is moved past A's.
        s.handle(
            a,
            ToBroker::Op {
                doc: "/f".into(),
                rev: 0,
                op: TextOp::diff("one", "one!"),
                at: Some((4, 4)),
            },
        );
        s.handle(
            b,
            ToBroker::Op {
                doc: "/f".into(),
                rev: 0,
                op: TextOp::diff("one", "the one"),
                at: Some((4, 4)),
            },
        );
        assert_eq!(s.docs["/f"].text, "the one!");
        // B's cursor, after "the " in its text, is still there in the shared one.
        assert!(matches!(
            &read(&rx_a)[..],
            [
                FromBroker::Ack { .. },
                FromBroker::Op {
                    at: Some((4, 4)),
                    ..
                }
            ]
        ));
        assert!(matches!(
            &read(&rx_b)[..],
            [FromBroker::Op { .. }, FromBroker::Ack { .. }]
        ));

        s.leave("/f", a);
        assert!(matches!(&read(&rx_b)[..], [FromBroker::Left { id, .. }] if *id == a));
        s.leave("/f", b);
        assert!(s.docs.is_empty());
    }
}
