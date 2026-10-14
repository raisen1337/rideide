//! The link to a remote host: uploads the agent if needed, runs it over `ssh`
//! and exchanges protocol frames with it.

use ride_proto::{read_frame, write_frame, ClientMsg, Request, ServerMsg, MAGIC};
use std::io::{BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

static AGENT: &[u8] = include_bytes!(env!("RIDE_AGENT_PATH"));

pub enum ConnEvent {
    Msg(ServerMsg),
    Closed(String),
}

pub struct Conn {
    /// The `ssh` process of a remote link; a local one has none.
    child: Option<Child>,
    tx: Sender<ClientMsg>,
    pub events: Receiver<ConnEvent>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }
}

/// The system `ssh`, so `~/.ssh/config` (aliases, keys, ProxyJump) applies as in a terminal.
fn ssh(host: &str) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "-T", host]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Runs a remote command to completion, feeding it `input`, and returns its stdout.
fn run(host: &str, remote: &str, input: &[u8]) -> Result<String, String> {
    let mut child = ssh(host)
        .arg(remote)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start ssh: {e}"))?;
    let mut stdin = child.stdin.take().unwrap();
    let write = stdin.write_all(input);
    drop(stdin);
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() || write.is_err() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("ssh exited with {}", out.status)
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn agent_hash() -> u64 {
    AGENT.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Skips whatever the remote shell printed before the agent started.
fn skip_to_magic(r: &mut impl Read) -> std::io::Result<bool> {
    let mut seen = Vec::new();
    let mut byte = [0u8; 1];
    while seen.len() < 1024 * 1024 {
        if r.read(&mut byte)? == 0 {
            return Ok(false);
        }
        seen.push(byte[0]);
        if seen.ends_with(MAGIC) {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Conn {
    /// Blocking; call from a worker thread. `status` receives progress text and
    /// `wake` is called whenever there is something new for the UI thread.
    pub fn connect(
        host: &str,
        status: &Mutex<String>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Conn, String> {
        let wake = Arc::new(wake);
        let say = |text: &str| {
            *status.lock().unwrap() = text.to_string();
            wake();
        };

        let path = format!("~/.cache/ride/agent-{:016x}", agent_hash());
        say("Connecting…");
        let probe = run(
            host,
            &format!("test -x {path} && echo RIDE_PRESENT || uname -m"),
            b"",
        )?;
        let probe = probe.lines().last().unwrap_or("").trim().to_string();
        if probe != "RIDE_PRESENT" {
            if probe != "x86_64" {
                return Err(format!(
                    "unsupported remote architecture: {probe:?} (only x86_64 Linux for now)"
                ));
            }
            say("Uploading agent…");
            // Old builds are removed so the cache holds a single agent.
            let install = format!(
                "mkdir -p ~/.cache/ride && rm -f ~/.cache/ride/agent-* && cat > {path}.tmp && chmod 700 {path}.tmp && mv {path}.tmp {path}"
            );
            run(host, &install, AGENT)?;
        }

        say("Starting agent…");
        let mut child = ssh(host)
            .arg(format!("exec {path}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start ssh: {e}"))?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::with_capacity(256 * 1024, child.stdout.take().unwrap());
        let mut stderr = child.stderr.take().unwrap();

        let errors = Arc::new(Mutex::new(String::new()));
        {
            let errors = errors.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n @ 1..) = stderr.read(&mut buf) {
                    errors
                        .lock()
                        .unwrap()
                        .push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            });
        }

        if !skip_to_magic(&mut stdout).unwrap_or(false) {
            let _ = child.kill();
            let _ = child.wait();
            let err = errors.lock().unwrap().trim().to_string();
            return Err(if err.is_empty() {
                "the agent did not start".into()
            } else {
                err
            });
        }

        let (tx, outgoing) = channel::<ClientMsg>();
        thread::spawn(move || {
            for msg in outgoing {
                if write_frame(&mut stdin, &msg).is_err() {
                    break;
                }
            }
        });

        let (event_tx, events) = channel();
        thread::spawn(move || {
            let reason = loop {
                match read_frame::<_, ServerMsg>(&mut stdout) {
                    Ok(Some(msg)) => {
                        if event_tx.send(ConnEvent::Msg(msg)).is_err() {
                            return;
                        }
                        wake();
                    }
                    Ok(None) => break "connection closed".to_string(),
                    Err(e) => break e.to_string(),
                }
            };
            let err = errors.lock().unwrap().trim().to_string();
            let _ = event_tx.send(ConnEvent::Closed(if err.is_empty() { reason } else { err }));
            wake();
        });

        Ok(Conn {
            child: Some(child),
            tx,
            events,
        })
    }

    /// A link to this computer instead of a remote host: the same backend, run in this
    /// process and reached over a pair of pipes.
    pub fn connect_local(wake: impl Fn() + Send + Sync + 'static) -> Result<Conn, String> {
        let (mut from_agent, to_app) = std::io::pipe().map_err(|e| e.to_string())?;
        let (from_app, mut to_agent) = std::io::pipe().map_err(|e| e.to_string())?;
        // It ends when `to_agent` is dropped, which happens when this `Conn` is.
        thread::spawn(move || ride_agent::serve(from_app, to_app));
        if !skip_to_magic(&mut from_agent).unwrap_or(false) {
            return Err("the local backend did not start".into());
        }

        let (tx, outgoing) = channel::<ClientMsg>();
        thread::spawn(move || {
            for msg in outgoing {
                if write_frame(&mut to_agent, &msg).is_err() {
                    break;
                }
            }
        });

        let (event_tx, events) = channel();
        thread::spawn(move || {
            let mut reader = BufReader::with_capacity(256 * 1024, from_agent);
            let reason = loop {
                match read_frame::<_, ServerMsg>(&mut reader) {
                    Ok(Some(msg)) => {
                        if event_tx.send(ConnEvent::Msg(msg)).is_err() {
                            return;
                        }
                        wake();
                    }
                    Ok(None) => break "closed".to_string(),
                    Err(e) => break e.to_string(),
                }
            };
            let _ = event_tx.send(ConnEvent::Closed(reason));
            wake();
        });

        Ok(Conn {
            child: None,
            tx,
            events,
        })
    }

    /// Sends a request and returns the id its responses will carry.
    pub fn request(&mut self, req: Request) -> u64 {
        // One counter for all connections: with several open at once, an id then names
        // one request among all of them.
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _ = self.tx.send(ClientMsg { id, req });
        id
    }
}

/// Host aliases from `~/.ssh/config`, for the connect box.
pub fn known_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return Vec::new();
    };
    let config = std::path::Path::new(&home).join(".ssh").join("config");
    let Ok(text) = std::fs::read_to_string(config) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let key = words.next()?;
            let alias = words.next()?;
            (key.eq_ignore_ascii_case("host") && !alias.contains(['*', '?']))
                .then(|| alias.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ride_proto::Response;
    use std::time::Duration;

    /// The backend run in-process on this computer: list a folder, write and read a file,
    /// find it by name and by content, then remove it.
    #[test]
    fn the_local_link_serves_files() {
        let dir = std::env::temp_dir().join(format!("ride-local-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.to_string_lossy().replace('\\', "/");
        let file = format!("{root}/note.txt");

        let mut conn = Conn::connect_local(|| {}).unwrap();
        let mut ask = |req: Request| -> Response {
            let id = conn.request(req);
            loop {
                match conn
                    .events
                    .recv_timeout(Duration::from_secs(10))
                    .expect("an answer in time")
                {
                    ConnEvent::Msg(msg) if msg.id == id => return msg.resp,
                    ConnEvent::Msg(_) => continue,
                    ConnEvent::Closed(reason) => panic!("link closed: {reason}"),
                }
            }
        };

        assert!(matches!(ask(Request::Hello), Response::Hello { home } if !home.contains('\\')));
        assert!(matches!(
            ask(Request::WriteFile {
                path: file.clone(),
                data: b"hello ride".to_vec()
            }),
            Response::Ok
        ));
        assert!(
            matches!(ask(Request::ReadFile { path: file.clone() }), Response::File { data } if data == b"hello ride")
        );
        let listed = ask(Request::ListDir { path: root.clone() });
        assert!(
            matches!(listed, Response::Dir { entries } if entries.iter().any(|e| e.name == "note.txt" && !e.is_dir))
        );
        let found = ask(Request::FindFiles {
            root: root.clone(),
            query: "note".into(),
        });
        assert!(matches!(found, Response::Paths { paths } if paths == ["note.txt"]));
        let search = Request::Search {
            root: root.clone(),
            query: "RIDE".into(),
            regex: false,
            case_sensitive: false,
        };
        let hits = ask(search);
        assert!(
            matches!(&hits, Response::SearchHits { hits } if hits.len() == 1 && hits[0].path == file && hits[0].col == 6),
            "{hits:?}"
        );
        assert!(matches!(ask(Request::Delete { path: file }), Response::Ok));
        let _ = std::fs::remove_dir_all(dir);

        // A program reading its input to the end (as a formatter does) finishes once
        // empty input closes it.
        let proc = conn.request(Request::ProcOpen {
            command: "$input | Sort-Object".into(),
            cwd: root.clone(),
        });
        conn.request(Request::ProcInput {
            proc,
            data: b"pear\napple\n".to_vec(),
        });
        conn.request(Request::ProcInput {
            proc,
            data: Vec::new(),
        });
        let mut out = Vec::new();
        loop {
            match conn
                .events
                .recv_timeout(Duration::from_secs(20))
                .expect("the program ends")
            {
                ConnEvent::Msg(msg) if msg.id == proc => match msg.resp {
                    Response::ProcOutput { data } => out.extend(data),
                    Response::ProcExit { .. } => break,
                    _ => {}
                },
                ConnEvent::Msg(_) => continue,
                ConnEvent::Closed(reason) => panic!("link closed: {reason}"),
            }
        }
        let out = String::from_utf8_lossy(&out);
        assert!(out.find("apple") < out.find("pear"), "{out:?}");
    }

    /// A terminal on this computer starts a shell and its output comes back.
    #[test]
    fn the_local_link_runs_a_terminal() {
        let mut conn = Conn::connect_local(|| {}).unwrap();
        let term = conn.request(Request::TermOpen {
            cols: 80,
            rows: 24,
            cwd: String::new(),
        });
        conn.request(Request::TermInput {
            term,
            data: b"echo ride-terminal-ok\r".to_vec(),
        });
        let mut output = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !output.contains("ride-terminal-ok\r\n") && std::time::Instant::now() < deadline {
            if let Ok(ConnEvent::Msg(msg)) = conn.events.recv_timeout(Duration::from_millis(500)) {
                if let Response::TermOutput { data } = msg.resp {
                    let text = String::from_utf8_lossy(&data);
                    // Windows' console asks where the cursor is before it goes on; the
                    // terminal widget answers that in the app, so the test has to as well.
                    if text.contains("\u{1b}[6n") {
                        conn.request(Request::TermInput {
                            term,
                            data: b"\x1b[1;1R".to_vec(),
                        });
                    }
                    output.push_str(&text);
                }
            }
        }
        conn.request(Request::TermClose { term });
        assert!(
            output.contains("ride-terminal-ok"),
            "terminal output was: {output:?}"
        );
    }
}
