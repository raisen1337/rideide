//! The queue of transfers: files and whole folders from any place to any other, this
//! computer or a server, server to server included. A few run at once, each on sessions of
//! its own, so browsing and starting more goes on while they run.

use crate::sftp::{self, Session};
use eframe::egui::Context;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// How many transfers run at the same time.
const WORKERS: usize = 3;

/// Where files are: this computer, or a server by its name in the SSH config.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Place {
    Local,
    Server(String),
}

impl Place {
    pub fn label(&self) -> &str {
        match self {
            Place::Local => "This computer",
            Place::Server(key) => key.strip_prefix("site:").unwrap_or(key),
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum State {
    Waiting,
    Running,
    Done,
    Failed(String),
    Stopped,
}

/// One thing asked to be copied, a file or a folder with all it holds.
pub struct Job {
    pub id: u64,
    pub from: Place,
    pub from_path: String,
    pub to: Place,
    pub to_dir: String,
    pub name: String,
    pub total: u64,
    pub done: u64,
    /// The file being copied inside a folder.
    pub current: String,
    pub state: State,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
    pub stop: Arc<AtomicBool>,
}

impl Job {
    /// Bytes a second since it started.
    pub fn speed(&self) -> f64 {
        let Some(started) = self.started else {
            return 0.0;
        };
        let end = self.finished.unwrap_or_else(Instant::now);
        self.done as f64 / end.duration_since(started).as_secs_f64().max(0.001)
    }

    pub fn active(&self) -> bool {
        matches!(self.state, State::Waiting | State::Running)
    }
}

pub type Jobs = Arc<Mutex<Vec<Job>>>;

pub struct Queue {
    pub jobs: Jobs,
    tx: Sender<u64>,
    /// Folders a finished job changed, to list again.
    pub changed: Receiver<(Place, String)>,
    next: u64,
}

impl Queue {
    pub fn new(ctx: &Context) -> Queue {
        let jobs: Jobs = Default::default();
        let (tx, rx) = channel::<u64>();
        let rx = Arc::new(Mutex::new(rx));
        let (changed_tx, changed) = channel();
        for _ in 0..WORKERS {
            let (jobs, rx, changed_tx, repaint) =
                (jobs.clone(), rx.clone(), changed_tx.clone(), ctx.clone());
            std::thread::spawn(move || worker(jobs, rx, changed_tx, repaint));
        }
        Queue {
            jobs,
            tx,
            changed,
            next: 0,
        }
    }

    /// Queues copying `from_path` (on `from`) into the folder `to_dir` (on `to`).
    pub fn add(&mut self, from: Place, from_path: String, to: Place, to_dir: String) {
        self.next += 1;
        let name = from_path
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&from_path)
            .to_string();
        self.jobs.lock().unwrap().push(Job {
            id: self.next,
            from,
            from_path,
            to,
            to_dir,
            name,
            total: 0,
            done: 0,
            current: String::new(),
            state: State::Waiting,
            started: None,
            finished: None,
            stop: Arc::new(AtomicBool::new(false)),
        });
        let _ = self.tx.send(self.next);
    }

    pub fn clear_finished(&self) {
        self.jobs.lock().unwrap().retain(Job::active);
    }

    pub fn stop_all(&self) {
        for job in self.jobs.lock().unwrap().iter() {
            job.stop.store(true, Ordering::Relaxed);
        }
    }
}

fn worker(
    jobs: Jobs,
    rx: Arc<Mutex<Receiver<u64>>>,
    changed: Sender<(Place, String)>,
    repaint: Context,
) {
    // Sessions stay open between jobs, one per server.
    let mut sessions: HashMap<String, Session> = HashMap::new();
    loop {
        let Ok(id) = rx.lock().unwrap().recv() else {
            return;
        };
        let update = |f: &mut dyn FnMut(&mut Job)| {
            if let Some(job) = jobs.lock().unwrap().iter_mut().find(|j| j.id == id) {
                f(job);
            }
        };
        let mut info = None;
        update(&mut |job| {
            if job.state == State::Waiting && !job.stop.load(Ordering::Relaxed) {
                job.state = State::Running;
                job.started = Some(Instant::now());
                info = Some((
                    job.from.clone(),
                    job.from_path.clone(),
                    job.to.clone(),
                    job.to_dir.clone(),
                    job.stop.clone(),
                ));
            } else if job.state == State::Waiting {
                job.state = State::Stopped;
            }
        });
        repaint.request_repaint();
        let Some((from, from_path, to, to_dir, stop)) = info else {
            continue;
        };
        let mut progress = |done: u64, current: &str| {
            update(&mut |job| {
                job.done = done;
                if job.current != current {
                    job.current = current.to_string();
                }
            });
            repaint.request_repaint();
            !stop.load(Ordering::Relaxed)
        };
        let mut total = |t: u64| update(&mut |job| job.total = t);
        let result = run(
            &mut sessions,
            &from,
            &from_path,
            &to,
            &to_dir,
            &mut progress,
            &mut total,
        );
        if let Err(e) = &result {
            // A session that broke is dropped, so the next job opens a fresh one.
            if e.contains("connection was lost") || e.contains("ssh") {
                for place in [&from, &to] {
                    if let Place::Server(host) = place {
                        sessions.remove(host);
                    }
                }
            }
        }
        update(&mut |job| {
            job.finished = Some(Instant::now());
            job.state = match &result {
                Ok(()) => State::Done,
                Err(_) if stop.load(Ordering::Relaxed) => State::Stopped,
                Err(e) => State::Failed(e.clone()),
            };
        });
        let _ = changed.send((to, to_dir));
        repaint.request_repaint();
    }
}

fn session<'a>(
    sessions: &'a mut HashMap<String, Session>,
    host: &str,
) -> Result<&'a mut Session, String> {
    if !sessions.contains_key(host) {
        sessions.insert(host.to_string(), Session::connect(host)?);
    }
    Ok(sessions.get_mut(host).unwrap())
}

type Progress<'a> = &'a mut dyn FnMut(u64, &str) -> bool;

/// A file to copy: its path at the source, its path under the copied item (with `/`), its
/// size and its modification time.
struct File {
    path: String,
    relative: String,
    size: u64,
    mtime: Option<u32>,
}

fn run(
    sessions: &mut HashMap<String, Session>,
    from: &Place,
    from_path: &str,
    to: &Place,
    to_dir: &str,
    progress: Progress,
    total: &mut dyn FnMut(u64),
) -> Result<(), String> {
    // What there is to copy, and the folders to make for it.
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    match from {
        Place::Local => walk_local(Path::new(from_path), "", &mut files, &mut dirs)
            .map_err(|e| e.to_string())?,
        Place::Server(host) => {
            let s = session(sessions, host)?;
            walk_remote(s, from_path, "", &mut files, &mut dirs)?;
        }
    }
    total(files.iter().map(|f| f.size).sum());
    let target = |relative: &str| match to {
        Place::Local => {
            let mut p = PathBuf::from(to_dir);
            for part in relative.split('/').filter(|s| !s.is_empty() && *s != "..") {
                p.push(part);
            }
            p.display().to_string()
        }
        Place::Server(_) => {
            let mut p = to_dir.trim_end_matches('/').to_string();
            for part in relative.split('/').filter(|s| !s.is_empty() && *s != "..") {
                p.push('/');
                p.push_str(part);
            }
            if p.is_empty() {
                "/".into()
            } else {
                p
            }
        }
    };
    for dir in &dirs {
        let path = target(dir);
        match to {
            Place::Local => std::fs::create_dir_all(&path).map_err(|e| format!("{path}: {e}"))?,
            Place::Server(host) => {
                let s = session(sessions, host)?;
                if s.mkdir(&path).is_err() && !s.stat(&path).is_ok_and(|a| a.is_dir()) {
                    return Err(format!("Couldn't create {path}"));
                }
            }
        }
    }
    let mut before = 0u64;
    for file in &files {
        let dest = target(&file.relative);
        let name = file
            .relative
            .rsplit('/')
            .next()
            .unwrap_or(&file.relative)
            .to_string();
        let mut step = |n: u64| progress(before + n, &name);
        copy_file(sessions, from, &file.path, to, &dest, file.mtime, &mut step)?;
        before += file.size;
        progress(before, &name);
    }
    Ok(())
}

fn copy_file(
    sessions: &mut HashMap<String, Session>,
    from: &Place,
    src: &str,
    to: &Place,
    dest: &str,
    mtime: Option<u32>,
    step: &mut dyn FnMut(u64) -> bool,
) -> Result<(), String> {
    match (from, to) {
        (Place::Local, Place::Local) => {
            let mut input = std::fs::File::open(src).map_err(|e| format!("{src}: {e}"))?;
            write_local(dest, mtime, |out| {
                let mut buffer = vec![0u8; 256 * 1024];
                let mut done = 0u64;
                loop {
                    let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
                    if n == 0 {
                        return Ok(());
                    }
                    out.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
                    done += n as u64;
                    if !step(done) {
                        return Err("Stopped".into());
                    }
                }
            })
        }
        (Place::Local, Place::Server(host)) => {
            let s = session(sessions, host)?;
            let mut input = std::fs::File::open(src).map_err(|e| format!("{src}: {e}"))?;
            s.upload(dest, &mut input, |n| step(n))?;
            if let Some(m) = mtime {
                let _ = s.set_mtime(dest, m);
            }
            Ok(())
        }
        (Place::Server(host), Place::Local) => {
            let s = session(sessions, host)?;
            write_local(dest, mtime, |out| s.download(src, out, |n| step(n)))
        }
        (Place::Server(a), Place::Server(b)) => {
            // Straight from one server to the other: what one session reads, the other writes,
            // a few chunks at a time through memory. The same server needs a second session.
            let mut extra;
            let (reader, writer): (&mut Session, &mut Session) = if a == b {
                extra = Session::connect(a)?;
                (&mut extra, session(sessions, b)?)
            } else {
                session(sessions, a)?;
                session(sessions, b)?;
                let [r, w] = sessions.get_disjoint_mut([a.as_str(), b.as_str()]);
                (r.unwrap(), w.unwrap())
            };
            let (tx, rx) = sync_channel::<Result<Vec<u8>, String>>(32);
            let stopped = AtomicBool::new(false);
            std::thread::scope(|scope| {
                let stopped = &stopped;
                scope.spawn(move || {
                    let mut out = PipeWriter(tx.clone());
                    let read = reader.download(src, &mut out, |_| !stopped.load(Ordering::Relaxed));
                    if let Err(e) = read {
                        let _ = tx.send(Err(e));
                    }
                });
                let mut input = PipeReader {
                    rx,
                    held: Vec::new(),
                    at: 0,
                };
                let written = writer.upload(dest, &mut input, |n| {
                    let go = step(n);
                    if !go {
                        stopped.store(true, Ordering::Relaxed);
                    }
                    go
                });
                if written.is_err() {
                    stopped.store(true, Ordering::Relaxed);
                    // Lets the reading side finish by draining what it still sends.
                    while input.rx.recv().is_ok() {}
                }
                written?;
                if let Some(m) = mtime {
                    let _ = writer.set_mtime(dest, m);
                }
                Ok(())
            })
        }
    }
}

/// Writes a local file through `fill`, beside its real name first so that a copy that stops
/// halfway leaves no half file, and keeps the original's modification time.
fn write_local(
    dest: &str,
    mtime: Option<u32>,
    fill: impl FnOnce(&mut dyn Write) -> Result<(), String>,
) -> Result<(), String> {
    let path = PathBuf::from(dest);
    let partial = PathBuf::from(format!("{dest}.rdrop-part"));
    let file =
        std::fs::File::create(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    let mut out = std::io::BufWriter::with_capacity(256 * 1024, file);
    let result = fill(&mut out).and_then(|()| out.flush().map_err(|e| e.to_string()));
    drop(out);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    if let Some(m) = mtime {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(m as u64);
        if let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(when);
        }
    }
    Ok(())
}

struct PipeWriter(SyncSender<Result<Vec<u8>, String>>);

impl Write for PipeWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0
            .send(Ok(data.to_vec()))
            .map_err(|_| std::io::Error::other("the other server stopped taking data"))?;
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct PipeReader {
    rx: Receiver<Result<Vec<u8>, String>>,
    held: Vec<u8>,
    at: usize,
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.held.len() {
            match self.rx.recv() {
                Ok(Ok(data)) => {
                    self.held = data;
                    self.at = 0;
                }
                Ok(Err(e)) => return Err(std::io::Error::other(e)),
                // The reading side finished: the end of the file.
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.held.len() - self.at);
        buf[..n].copy_from_slice(&self.held[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

fn mtime_of(meta: &std::fs::Metadata) -> Option<u32> {
    let modified = meta.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as u32,
    )
}

/// Every file under `path` here, and the folders to make, parents first, all relative to
/// the folder `path` is in.
fn walk_local(
    path: &Path,
    under: &str,
    files: &mut Vec<File>,
    dirs: &mut Vec<String>,
) -> std::io::Result<()> {
    let meta = std::fs::metadata(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let relative = if under.is_empty() {
        name
    } else {
        format!("{under}/{name}")
    };
    if meta.is_dir() {
        dirs.push(relative.clone());
        for entry in std::fs::read_dir(path)? {
            walk_local(&entry?.path(), &relative, files, dirs)?;
        }
    } else {
        files.push(File {
            path: path.display().to_string(),
            relative,
            size: meta.len(),
            mtime: mtime_of(&meta),
        });
    }
    Ok(())
}

fn walk_remote(
    s: &mut Session,
    path: &str,
    under: &str,
    files: &mut Vec<File>,
    dirs: &mut Vec<String>,
) -> Result<(), String> {
    let attrs = s.stat(path)?;
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string();
    let relative = if under.is_empty() {
        name
    } else {
        format!("{under}/{name}")
    };
    if attrs.is_dir() {
        dirs.push(relative.clone());
        for entry in s.list(path)? {
            walk_remote(s, &sftp::join(path, &entry.name), &relative, files, dirs)?;
        }
    } else {
        files.push(File {
            path: path.to_string(),
            relative,
            size: attrs.size.unwrap_or(0),
            mtime: attrs.mtime,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp::Session;

    #[test]
    fn the_pipe_carries_bytes_and_errors() {
        let (tx, rx) = sync_channel(4);
        std::thread::spawn(move || {
            let mut w = PipeWriter(tx);
            w.write_all(b"hello ").unwrap();
            w.write_all(b"world").unwrap();
        });
        let mut r = PipeReader {
            rx,
            held: Vec::new(),
            at: 0,
        };
        let mut text = String::new();
        r.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello world");

        let (tx, rx) = sync_channel(4);
        tx.send(Ok(b"part".to_vec())).unwrap();
        tx.send(Err("lost".into())).unwrap();
        let mut r = PipeReader {
            rx,
            held: Vec::new(),
            at: 0,
        };
        let mut out = Vec::new();
        assert!(r.read_to_end(&mut out).is_err());
    }

    #[test]
    fn local_folders_copy_with_their_insides() {
        let base = std::env::temp_dir().join(format!("rdrop-test-{}", std::process::id()));
        let src = base.join("src").join("box");
        std::fs::create_dir_all(src.join("inner")).unwrap();
        std::fs::write(src.join("a.txt"), "aaa").unwrap();
        std::fs::write(src.join("inner").join("b.txt"), "bb").unwrap();
        let dest = base.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let mut sessions = HashMap::new();
        let mut seen = 0;
        run(
            &mut sessions,
            &Place::Local,
            &src.display().to_string(),
            &Place::Local,
            &dest.display().to_string(),
            &mut |done, _| {
                seen = done;
                true
            },
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("box").join("a.txt")).unwrap(),
            "aaa"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("box").join("inner").join("b.txt")).unwrap(),
            "bb"
        );
        assert_eq!(seen, 5);
        let _ = std::fs::remove_dir_all(base);
    }

    /// Against real servers, by hand: RDROP_TEST_HOST (and RDROP_TEST_HOST2 for a copy
    /// between servers) name hosts from ~/.ssh/config. Works in /tmp there and cleans up.
    #[test]
    #[ignore]
    fn on_real_servers() {
        let Ok(host) = std::env::var("RDROP_TEST_HOST") else {
            return;
        };
        let dir = format!("/tmp/rdrop-test-{}", std::process::id());
        let mut s = Session::connect(&host).unwrap();
        let home = s.realpath(".").unwrap();
        assert!(home.starts_with('/'));
        s.mkdir(&dir).unwrap();
        s.mkdir(&format!("{dir}/sub")).unwrap();
        // 3 MB and a bit, so many chunks are in flight and the last one is short.
        let data: Vec<u8> = (0..3_200_123u32).map(|i| (i * 7 % 251) as u8).collect();
        s.upload(&format!("{dir}/sub/big.bin"), &mut &data[..], |_| true)
            .unwrap();
        s.upload(&format!("{dir}/small.txt"), &mut &b"hello"[..], |_| true)
            .unwrap();
        let names: Vec<String> = s.list(&dir).unwrap().into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"sub".to_string()) && names.contains(&"small.txt".to_string()));
        let mut back = Vec::new();
        s.download(&format!("{dir}/sub/big.bin"), &mut back, |_| true)
            .unwrap();
        assert!(back == data, "the download differs from the upload");
        s.rename(&format!("{dir}/small.txt"), &format!("{dir}/renamed.txt"))
            .unwrap();
        assert_eq!(s.stat(&format!("{dir}/renamed.txt")).unwrap().size, Some(5));

        // A folder down to this computer and back up, through the queue's own code.
        let local = std::env::temp_dir().join(format!("rdrop-real-{}", std::process::id()));
        std::fs::create_dir_all(&local).unwrap();
        let mut sessions = HashMap::new();
        let server = Place::Server(host.clone());
        run(
            &mut sessions,
            &server,
            &format!("{dir}/sub"),
            &Place::Local,
            &local.display().to_string(),
            &mut |_, _| true,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            std::fs::read(local.join("sub").join("big.bin")).unwrap(),
            data
        );
        s.mkdir(&format!("{dir}/up")).unwrap();
        run(
            &mut sessions,
            &Place::Local,
            &local.join("sub").display().to_string(),
            &server,
            &format!("{dir}/up"),
            &mut |_, _| true,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            s.stat(&format!("{dir}/up/sub/big.bin")).unwrap().size,
            Some(data.len() as u64)
        );

        // Server to server: to the second host, or to the same one through a second session.
        let other = std::env::var("RDROP_TEST_HOST2").unwrap_or(host.clone());
        let mut t = Session::connect(&other).unwrap();
        let dir2 = format!("{dir}-to");
        t.mkdir(&dir2).unwrap();
        run(
            &mut sessions,
            &server,
            &format!("{dir}/sub"),
            &Place::Server(other.clone()),
            &dir2,
            &mut |_, _| true,
            &mut |_| {},
        )
        .unwrap();
        let mut copied = Vec::new();
        t.download(&format!("{dir2}/sub/big.bin"), &mut copied, |_| true)
            .unwrap();
        assert!(copied == data, "the server-to-server copy differs");

        t.remove_all(&dir2).unwrap();
        s.remove_all(&dir).unwrap();
        assert!(s.stat(&dir).is_err());
        let _ = std::fs::remove_dir_all(local);
    }
}
