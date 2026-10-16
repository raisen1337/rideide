//! The server side of the window: a session of its own on a thread, doing what the remote
//! pane asks (listing, new folders, renames, deletes) one thing at a time.

use crate::sftp::{self, Session};
use eframe::egui::Context;
use std::sync::mpsc::{channel, Receiver, Sender};

pub enum Ask {
    List(String),
    Mkdir(String),
    Rename(String, String),
    Delete(Vec<String>),
    /// Up to `limit` bytes of a file, from its start, or from its end with `tail`.
    Read {
        path: String,
        limit: u64,
        tail: bool,
    },
    Chmod(String, u32),
}

pub enum Answer {
    /// Connected; the folder the session starts in.
    Connected(String),
    Listed(String, Vec<sftp::Entry>),
    /// Something changed in this folder, which is worth listing again.
    Changed(String),
    Failed(String),
    /// What `Ask::Read` read, with the file's whole size.
    Read {
        path: String,
        data: Vec<u8>,
        size: u64,
    },
    /// The connection couldn't be made or was lost.
    Lost(String),
}

pub struct Remote {
    tx: Sender<Ask>,
    pub rx: Receiver<Answer>,
}

impl Remote {
    pub fn connect(host: &str, ctx: &Context) -> Remote {
        let (tx, asks) = channel::<Ask>();
        let (answers, rx) = channel();
        let (name, repaint) = (host.to_string(), ctx.clone());
        std::thread::spawn(move || {
            let send = |answer| {
                let _ = answers.send(answer);
                repaint.request_repaint();
            };
            let mut session = match Session::connect(&name) {
                Ok(s) => s,
                Err(error) => return send(Answer::Lost(error)),
            };
            let home = session.realpath(".").unwrap_or_else(|_| "/".into());
            send(Answer::Connected(home));
            for ask in asks {
                let answer = match ask {
                    Ask::List(path) => match session.list(&path) {
                        Ok(entries) => Answer::Listed(path, entries),
                        Err(e) => Answer::Failed(format!("{path}: {e}")),
                    },
                    Ask::Mkdir(path) => match session.mkdir(&path) {
                        Ok(()) => Answer::Changed(sftp::parent(&path)),
                        Err(e) => Answer::Failed(e),
                    },
                    Ask::Rename(from, to) => match session.rename(&from, &to) {
                        Ok(()) => Answer::Changed(sftp::parent(&to)),
                        Err(e) => Answer::Failed(e),
                    },
                    Ask::Read { path, limit, tail } => {
                        let read = session.stat(&path).and_then(|attrs| {
                            let size = attrs.size.unwrap_or(0);
                            let start = if tail { size.saturating_sub(limit) } else { 0 };
                            session
                                .read_at(&path, start, limit)
                                .map(|data| (data, size))
                        });
                        match read {
                            Ok((data, size)) => Answer::Read { path, data, size },
                            Err(e) => Answer::Failed(format!("{path}: {e}")),
                        }
                    }
                    Ask::Chmod(path, mode) => match session.chmod(&path, mode) {
                        Ok(()) => Answer::Changed(sftp::parent(&path)),
                        Err(e) => Answer::Failed(e),
                    },
                    Ask::Delete(paths) => {
                        let folder = paths.first().map(|p| sftp::parent(p)).unwrap_or_default();
                        let failed = paths.iter().find_map(|p| session.remove_all(p).err());
                        match failed {
                            Some(e) => Answer::Failed(e),
                            None => Answer::Changed(folder),
                        }
                    }
                };
                let lost =
                    matches!(&answer, Answer::Failed(e) if e.contains("connection was lost"));
                send(answer);
                if lost {
                    return send(Answer::Lost("The connection was lost".into()));
                }
            }
        });
        Remote { tx, rx }
    }

    pub fn ask(&self, ask: Ask) {
        let _ = self.tx.send(ask);
    }
}
