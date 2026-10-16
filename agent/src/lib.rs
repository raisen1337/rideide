//! The backend of ride: file access, search, terminals and processes, driven by protocol
//! frames. On a remote host it runs as its own program over stdin/stdout; for a folder on
//! this computer the app runs the same code in-process over a pair of pipes.

mod claude_history;
#[cfg(unix)]
pub mod collab;
#[cfg(unix)]
mod host;
pub mod mcp;

use flate2::{write::GzEncoder, Compression};
use ignore::{WalkBuilder, WalkState};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use ride_proto::*;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

const MAX_SEARCH_HITS: usize = 5000;
const MAX_SEARCH_FILE: u64 = 8 * 1024 * 1024;
const CHUNK: usize = 256 * 1024;

type Out = SyncSender<ServerMsg>;
type Cancels = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;

struct Term {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

struct Proc {
    /// Closed (taken) when the app sends empty input, so the program sees the end of it.
    stdin: Option<ChildStdin>,
    child: Arc<Mutex<Child>>,
}

/// Serves requests read from `input`, writing responses to `output`, until `input` ends.
pub fn serve(mut input: impl Read, mut out: impl Write + Send + 'static) {
    let (tx, rx) = sync_channel::<ServerMsg>(64);
    thread::spawn(move || {
        if out.write_all(MAGIC).and_then(|_| out.flush()).is_err() {
            return;
        }
        for msg in rx {
            if write_frame(&mut out, &msg).is_err() {
                break;
            }
        }
    });

    #[cfg(unix)]
    let asks = host::listen(tx.clone());
    let cancels: Cancels = Arc::default();
    let mut terms: HashMap<u64, Term> = HashMap::new();
    let mut procs: HashMap<u64, Proc> = HashMap::new();

    while let Ok(Some(ClientMsg { id, req })) = read_frame(&mut input) {
        match req {
            Request::Hello => {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .map_or_else(|_| "/".into(), |home| slashes(&home));
                let _ = tx.send(ServerMsg {
                    id,
                    resp: Response::Hello { home },
                });
            }
            Request::ListDir { path } => reply(&tx, id, move || list_dir(&path)),
            Request::ReadFile { path } => reply(&tx, id, move || {
                fs::read(&path).map(|data| Response::File { data })
            }),
            Request::WriteFile { path, data } => reply(&tx, id, move || {
                // An upload sends the files of a folder without creating the folder first.
                if let Some(dir) = Path::new(&path).parent() {
                    fs::create_dir_all(dir)?;
                }
                fs::write(&path, data).map(|_| Response::Ok)
            }),
            Request::CreateFile { path } => reply(&tx, id, move || {
                fs::File::create_new(&path).map(|_| Response::Ok)
            }),
            Request::CreateDir { path } => reply(&tx, id, move || {
                fs::create_dir_all(&path).map(|_| Response::Ok)
            }),
            Request::Rename { from, to } => reply(&tx, id, move || {
                if Path::new(&to).exists() {
                    return Err(io::Error::other(format!("{to} already exists")));
                }
                fs::rename(&from, &to).map(|_| Response::Ok)
            }),
            Request::Delete { path } => reply(&tx, id, move || {
                if fs::symlink_metadata(&path)?.is_dir() {
                    fs::remove_dir_all(&path)?;
                } else {
                    fs::remove_file(&path)?;
                }
                Ok(Response::Ok)
            }),
            Request::ClaudeSessions { cwd } => reply(&tx, id, move || {
                Ok(Response::ClaudeSessions {
                    sessions: claude_history::sessions(&cwd),
                })
            }),
            Request::FindFiles { root, query } => {
                reply(&tx, id, move || Ok(find_files(&root, &query)))
            }
            Request::Search {
                root,
                query,
                regex,
                case_sensitive,
            } => {
                let cancel = register(&cancels, id);
                let (tx, cancels) = (tx.clone(), cancels.clone());
                thread::spawn(move || {
                    let resp = match search(id, &root, &query, regex, case_sensitive, &cancel, &tx)
                    {
                        Ok(()) => Response::Done,
                        Err(message) => Response::Error { message },
                    };
                    cancels.lock().unwrap().remove(&id);
                    let _ = tx.send(ServerMsg { id, resp });
                });
            }
            Request::Download { path } => {
                let cancel = register(&cancels, id);
                let (tx, cancels) = (tx.clone(), cancels.clone());
                thread::spawn(move || {
                    let resp = match download(id, &path, &cancel, &tx) {
                        Ok(()) => Response::Done,
                        Err(e) => Response::Error {
                            message: e.to_string(),
                        },
                    };
                    cancels.lock().unwrap().remove(&id);
                    let _ = tx.send(ServerMsg { id, resp });
                });
            }
            Request::Cancel { target } => {
                if let Some(flag) = cancels.lock().unwrap().get(&target) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            Request::TermOpen { cols, rows, cwd } => match open_term(id, cols, rows, &cwd, &tx) {
                Ok(term) => {
                    terms.insert(id, term);
                }
                Err(message) => {
                    let _ = tx.send(ServerMsg {
                        id,
                        resp: Response::Error { message },
                    });
                }
            },
            Request::TermInput { term, data } => {
                if let Some(t) = terms.get_mut(&term) {
                    let _ = t.writer.write_all(&data).and_then(|_| t.writer.flush());
                }
            }
            Request::TermResize { term, cols, rows } => {
                if let Some(t) = terms.get(&term) {
                    let _ = t.master.resize(pty_size(cols, rows));
                }
            }
            Request::TermClose { term } => {
                if let Some(mut t) = terms.remove(&term) {
                    let _ = t.killer.kill();
                }
            }
            Request::ProcOpen { command, cwd } => match open_proc(id, &command, &cwd, &tx) {
                Ok(proc) => {
                    procs.insert(id, proc);
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg {
                        id,
                        resp: Response::Error {
                            message: e.to_string(),
                        },
                    });
                }
            },
            Request::ProcInput { proc, data } => {
                if let Some(p) = procs.get_mut(&proc) {
                    if data.is_empty() {
                        p.stdin = None;
                    } else if let Some(stdin) = p.stdin.as_mut() {
                        let _ = stdin.write_all(&data).and_then(|_| stdin.flush());
                    }
                }
            }
            Request::ProcClose { proc } => {
                if let Some(p) = procs.remove(&proc) {
                    let _ = p.child.lock().unwrap().kill();
                }
            }
            #[cfg(unix)]
            Request::HostAnswer { token, text } => asks.answer(token, &text),
            #[cfg(not(unix))]
            Request::HostAnswer { .. } => {}
        }
    }

    // The app went away: take the shells down with us.
    for t in terms.values_mut() {
        let _ = t.killer.kill();
    }
    for p in procs.values() {
        let _ = p.child.lock().unwrap().kill();
    }
}

/// Paths are exchanged with forward slashes on every system; Windows accepts them too.
fn slashes(path: &str) -> String {
    if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_string()
    }
}

/// Runs a filesystem operation off the main loop so a slow disk can't stall terminal input.
fn reply(tx: &Out, id: u64, f: impl FnOnce() -> io::Result<Response> + Send + 'static) {
    let tx = tx.clone();
    thread::spawn(move || {
        let resp = f().unwrap_or_else(|e| Response::Error {
            message: e.to_string(),
        });
        let _ = tx.send(ServerMsg { id, resp });
    });
}

fn register(cancels: &Cancels, id: u64) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    cancels.lock().unwrap().insert(id, flag.clone());
    flag
}

fn list_dir(path: &str) -> io::Result<Response> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        // Follow symlinks so a link to a directory can be expanded in the tree.
        let meta = fs::metadata(entry.path()).or_else(|_| entry.metadata());
        let (is_dir, size) = meta.map(|m| (m.is_dir(), m.len())).unwrap_or((false, 0));
        entries.push(DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
            size,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(Response::Dir { entries })
}

/// File-name lookup for the chat's `@` mentions: shortest matching paths first.
fn find_files(root: &str, query: &str) -> Response {
    const SCAN_LIMIT: usize = 200_000;
    const RESULTS: usize = 40;
    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    let mut paths: Vec<String> = walker
        .flatten()
        .take(SCAN_LIMIT)
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|entry| {
            let path = slashes(&entry.path().strip_prefix(root).ok()?.to_string_lossy());
            let lower = path.to_lowercase();
            words.iter().all(|w| lower.contains(w)).then_some(path)
        })
        .collect();
    paths.sort_by_key(|p| p.len());
    paths.truncate(RESULTS);
    Response::Paths { paths }
}

fn search(
    id: u64,
    root: &str,
    query: &str,
    is_regex: bool,
    case_sensitive: bool,
    cancel: &AtomicBool,
    tx: &Out,
) -> Result<(), String> {
    let pattern = if is_regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    let re = regex::bytes::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| e.to_string())?;
    let found = AtomicUsize::new(0);

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build_parallel();
    walker.run(|| {
        let (re, found) = (&re, &found);
        Box::new(move |entry| {
            if cancel.load(Ordering::Relaxed) || found.load(Ordering::Relaxed) >= MAX_SEARCH_HITS {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else {
                return WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            if entry.metadata().map_or(true, |m| m.len() > MAX_SEARCH_FILE) {
                return WalkState::Continue;
            }
            let Ok(data) = fs::read(entry.path()) else {
                return WalkState::Continue;
            };
            let is_binary = data[..data.len().min(8192)].contains(&0);
            if is_binary || !re.is_match(&data) {
                return WalkState::Continue;
            }

            let path = slashes(&entry.path().to_string_lossy());
            let mut hits = Vec::new();
            for (n, line) in data.split(|b| *b == b'\n').enumerate() {
                let Some(m) = re.find(line) else { continue };
                let text = String::from_utf8_lossy(line);
                hits.push(SearchHit {
                    path: path.clone(),
                    line: n as u32,
                    col: String::from_utf8_lossy(&line[..m.start()]).chars().count() as u32,
                    text: text.trim_end().chars().take(400).collect(),
                });
            }
            found.fetch_add(hits.len(), Ordering::Relaxed);
            if tx
                .send(ServerMsg {
                    id,
                    resp: Response::SearchHits { hits },
                })
                .is_err()
            {
                return WalkState::Quit;
            }
            WalkState::Continue
        })
    });
    Ok(())
}

/// Buffers writes and ships them to the client as `Chunk` messages.
struct ChunkWriter<'a> {
    id: u64,
    buf: Vec<u8>,
    cancel: &'a AtomicBool,
    tx: &'a Out,
}

impl Write for ChunkWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.flush()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("cancelled"));
        }
        if !self.buf.is_empty() {
            let data = std::mem::take(&mut self.buf);
            self.tx
                .send(ServerMsg {
                    id: self.id,
                    resp: Response::Chunk { data },
                })
                .map_err(|_| io::Error::other("client gone"))?;
        }
        Ok(())
    }
}

fn download(id: u64, path: &str, cancel: &AtomicBool, tx: &Out) -> io::Result<()> {
    let path = Path::new(path).canonicalize()?;
    let meta = fs::metadata(&path)?;
    let name = path
        .file_name()
        .map_or("root".into(), |n| n.to_string_lossy().into_owned());
    let info = Response::DownloadInfo {
        is_dir: meta.is_dir(),
        name: name.clone(),
    };
    tx.send(ServerMsg { id, resp: info })
        .map_err(|_| io::Error::other("client gone"))?;

    let mut out = ChunkWriter {
        id,
        buf: Vec::with_capacity(CHUNK),
        cancel,
        tx,
    };
    if meta.is_dir() {
        let mut tar = tar::Builder::new(GzEncoder::new(out, Compression::fast()));
        tar.follow_symlinks(false);
        tar.append_dir_all(&name, &path)?;
        tar.into_inner()?.finish()?.flush()
    } else {
        io::copy(&mut fs::File::open(&path)?, &mut out)?;
        out.flush()
    }
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn open_term(id: u64, cols: u16, rows: u16, cwd: &str, tx: &Out) -> Result<Term, String> {
    let pair = native_pty_system()
        .openpty(pty_size(cols, rows))
        .map_err(|e| e.to_string())?;

    let mut cmd = if cfg!(windows) {
        let mut cmd = CommandBuilder::new("powershell.exe");
        cmd.arg("-NoLogo");
        cmd
    } else {
        let mut cmd =
            CommandBuilder::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()));
        // A login shell, so PATH additions from the user's profile (e.g. for `claude`) apply.
        cmd.arg("-l");
        cmd
    };
    if Path::new(cwd).is_dir() {
        cmd.cwd(cwd);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // On Windows, programs look for Windows Terminal's WT_SESSION before they use colours,
    // Unicode symbols and modern sequences; ride's terminal draws all of them.
    if cfg!(windows) {
        cmd.env("WT_SESSION", session_id());
    }

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    let tx = tx.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let resp = Response::TermOutput {
                        data: buf[..n].to_vec(),
                    };
                    if tx.send(ServerMsg { id, resp }).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = child.wait();
        let _ = tx.send(ServerMsg {
            id,
            resp: Response::TermExit,
        });
    });

    Ok(Term {
        master: pair.master,
        writer,
        killer,
    })
}

fn open_proc(id: u64, command: &str, cwd: &str, tx: &Out) -> io::Result<Proc> {
    let mut cmd = if cfg!(windows) {
        // PowerShell has no `exec`; the command simply runs as its child.
        let mut cmd = Command::new("powershell.exe");
        cmd.args([
            "-NoLogo",
            "-NoProfile",
            "-Command",
            command.strip_prefix("exec ").unwrap_or(command),
        ]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    } else {
        // Through a login shell, for the same PATH the terminal has.
        let mut cmd = Command::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()));
        cmd.arg("-lc").arg(command);
        cmd
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if Path::new(cwd).is_dir() {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn()?;
    let stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let child = Arc::new(Mutex::new(child));

    let err_tx = tx.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = stderr.read(&mut buf) {
            let resp = Response::ProcStderr {
                data: buf[..n].to_vec(),
            };
            if err_tx.send(ServerMsg { id, resp }).is_err() {
                break;
            }
        }
    });

    let (tx, waiter) = (tx.clone(), child.clone());
    thread::spawn(move || {
        let mut buf = [0u8; 64 * 1024];
        while let Ok(n @ 1..) = stdout.read(&mut buf) {
            let resp = Response::ProcOutput {
                data: buf[..n].to_vec(),
            };
            if tx.send(ServerMsg { id, resp }).is_err() {
                break;
            }
        }
        // Stdout closed, so the process is done or about to be.
        let code = waiter.lock().unwrap().wait().ok().and_then(|s| s.code());
        let _ = tx.send(ServerMsg {
            id,
            resp: Response::ProcExit { code },
        });
    });

    Ok(Proc {
        stdin: Some(stdin),
        child,
    })
}

/// A fresh id in the form Windows Terminal gives each of its sessions as WT_SESSION.
/// Programs on Windows look for it to know they may use colours, Unicode symbols and
/// modern escape sequences: Claude Code, for one, falls back to plain ASCII without it.
pub fn session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}
