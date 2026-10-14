//! Lets programs on the host ask the ride app for something: its MCP server, mostly.
//!
//! The agent listens on a socket only its own user can reach. A program writes one line
//! there, the agent passes it to the app, and the app's answer comes back as one line.

use ride_proto::{Response, ServerMsg};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The connections that wait for the app's answer, by the token the app was given.
#[derive(Default)]
pub struct Asks {
    waiting: Mutex<HashMap<u64, UnixStream>>,
}

impl Asks {
    pub fn answer(&self, token: u64, text: &str) {
        if let Some(mut stream) = self.waiting.lock().unwrap().remove(&token) {
            let _ = writeln!(stream, "{}", text.replace('\n', " "));
        }
    }
}

/// Where the sockets of the agents running for this user are.
pub fn socket_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".cache/ride/sock"))
}

/// Starts listening, and tells the programs this agent starts where, through `RIDE_SOCK`.
pub fn listen(tx: SyncSender<ServerMsg>) -> Arc<Asks> {
    let asks = Arc::new(Asks::default());
    let Some(dir) = socket_dir() else {
        return asks;
    };
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    // Sockets of agents that are gone are left behind; nothing answers on those.
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if UnixStream::connect(entry.path()).is_err() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    // A name that stays the same from one version of the agent to the next, for
    // whatever is set up to run it, such as Claude Code's list of MCP servers.
    if let (Some(cache), Ok(exe)) = (dir.parent(), std::env::current_exe()) {
        let link = cache.join("ride-agent");
        let _ = std::fs::remove_file(&link);
        let _ = std::os::unix::fs::symlink(exe, link);
    }
    let path = dir.join(format!("{}.sock", std::process::id()));
    let Ok(listener) = UnixListener::bind(&path) else {
        return asks;
    };
    std::env::set_var("RIDE_SOCK", &path);

    let waiting = asks.clone();
    std::thread::spawn(move || {
        let mut token = 0;
        for stream in listener.incoming().flatten() {
            let Ok(reader) = stream.try_clone() else {
                continue;
            };
            let _ = reader.set_read_timeout(Some(Duration::from_secs(5)));
            let mut line = String::new();
            let read = BufReader::new(reader.take(64 * 1024)).read_line(&mut line);
            // Another agent checking whether this socket is alive says nothing.
            if read.is_err() || line.trim().is_empty() {
                continue;
            }
            token += 1;
            waiting.waiting.lock().unwrap().insert(token, stream);
            let text = line.trim().to_string();
            let msg = ServerMsg {
                id: 0,
                resp: Response::HostAsk { token, text },
            };
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    asks
}

/// Asks the app connected to this host, and waits for its one-line answer.
pub fn ask(line: &str) -> Option<String> {
    // The agent that started this program first, then any other that is running.
    let mut sockets: Vec<PathBuf> = std::env::var_os("RIDE_SOCK")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    let others = socket_dir().and_then(|dir| std::fs::read_dir(dir).ok());
    sockets.extend(others.into_iter().flatten().flatten().map(|e| e.path()));
    sockets.into_iter().find_map(|path| {
        let mut stream = UnixStream::connect(path).ok()?;
        writeln!(stream, "{line}").ok()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(25)))
            .ok()?;
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).ok()?;
        Some(answer.trim().to_string()).filter(|a| !a.is_empty())
    })
}
