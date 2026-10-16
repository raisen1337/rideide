//! The queue of transfers: files and whole folders from any place to any other, this
//! computer or a server, server to server included. A few run at once, each on sessions of
//! its own, so browsing and starting more goes on while they run.

use crate::bulk;
use crate::engine::{self, Conflict, Options, Outcome};
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
#[derive(Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
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
    /// A move (cut and paste): the original goes once the copy is done.
    pub moving: bool,
    /// What happened beyond copying: files skipped, blocks sent, checksums.
    pub detail: String,
    /// A sync: only these files (paths under `from_path`, with `/`) go, each to the same
    /// path under `to_dir`, over what is there; then these paths under `to_dir` go away.
    pub only: Vec<String>,
    pub delete: Vec<String>,
}

/// A job as it is kept on disk, to carry on after rdrop starts again.
#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    from: Place,
    from_path: String,
    to: Place,
    to_dir: String,
    moving: bool,
    #[serde(default)]
    name: String,
    #[serde(default)]
    only: Vec<String>,
    #[serde(default)]
    delete: Vec<String>,
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
    /// How conflicts are handled and whether copies are checked, for jobs that start.
    pub options: Arc<Mutex<Options>>,
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
        let options: Arc<Mutex<Options>> = Default::default();
        for _ in 0..WORKERS {
            let (jobs, rx, changed_tx, repaint, options) = (
                jobs.clone(),
                rx.clone(),
                changed_tx.clone(),
                ctx.clone(),
                options.clone(),
            );
            std::thread::spawn(move || worker(jobs, rx, changed_tx, repaint, options));
        }
        Queue {
            jobs,
            options,
            tx,
            changed,
            next: 0,
        }
    }

    /// Queues copying `from_path` (on `from`) into the folder `to_dir` (on `to`).
    pub fn add(&mut self, from: Place, from_path: String, to: Place, to_dir: String) -> u64 {
        self.push(from, from_path, to, to_dir, false, Default::default())
    }

    /// Queues moving `from_path` into `to_dir`: copied, then removed where it was.
    pub fn add_move(&mut self, from: Place, from_path: String, to: Place, to_dir: String) -> u64 {
        self.push(from, from_path, to, to_dir, true, Default::default())
    }

    /// Queues a sync: the files listed (under `from_root`) to the same places under
    /// `to_root`, then the paths in `delete` removed from under `to_root`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_sync(
        &mut self,
        from: Place,
        from_root: String,
        to: Place,
        to_root: String,
        only: Vec<String>,
        delete: Vec<String>,
        name: String,
    ) -> u64 {
        self.push(
            from,
            from_root,
            to,
            to_root,
            false,
            (only, delete, Some(name)),
        )
    }

    fn push(
        &mut self,
        from: Place,
        from_path: String,
        to: Place,
        to_dir: String,
        moving: bool,
        (only, delete, name): (Vec<String>, Vec<String>, Option<String>),
    ) -> u64 {
        self.next += 1;
        let name = name.unwrap_or_else(|| {
            from_path
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&from_path)
                .to_string()
        });
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
            moving,
            detail: String::new(),
            only,
            delete,
        });
        let _ = self.tx.send(self.next);
        self.next
    }

    /// Starts a stopped or failed job again; it carries on where it got to.
    pub fn resume(&self, id: u64) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs
            .iter_mut()
            .find(|j| j.id == id && matches!(j.state, State::Stopped | State::Failed(_)))
        {
            job.state = State::Waiting;
            job.stop = Arc::new(AtomicBool::new(false));
            job.done = 0;
            job.started = None;
            job.finished = None;
            let _ = self.tx.send(id);
        }
    }

    pub fn resume_all(&self) {
        let ids: Vec<u64> = self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| matches!(j.state, State::Stopped | State::Failed(_)))
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.resume(id);
        }
    }

    /// Keeps the jobs not done yet on disk, so they can carry on after a restart.
    pub fn save(&self, path: &Path) {
        let saved: Vec<Saved> = self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| j.state != State::Done)
            .map(|j| Saved {
                from: j.from.clone(),
                from_path: j.from_path.clone(),
                to: j.to.clone(),
                to_dir: j.to_dir.clone(),
                moving: j.moving,
                name: j.name.clone(),
                only: j.only.clone(),
                delete: j.delete.clone(),
            })
            .collect();
        if let Ok(json) = serde_json::to_vec_pretty(&saved) {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(path));
            let _ = std::fs::write(path, json);
        }
    }

    /// Brings back the jobs a last run didn't finish, stopped, to resume when wanted.
    pub fn load(&mut self, path: &Path) {
        let saved: Vec<Saved> = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        for s in saved {
            self.next += 1;
            let name = if s.name.is_empty() {
                s.from_path
                    .trim_end_matches(['/', '\\'])
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&s.from_path)
                    .to_string()
            } else {
                s.name
            };
            self.jobs.lock().unwrap().push(Job {
                id: self.next,
                from: s.from,
                from_path: s.from_path,
                to: s.to,
                to_dir: s.to_dir,
                name,
                total: 0,
                done: 0,
                current: String::new(),
                state: State::Stopped,
                started: None,
                finished: None,
                stop: Arc::new(AtomicBool::new(false)),
                moving: s.moving,
                detail: "Not finished last time".into(),
                only: s.only,
                delete: s.delete,
            });
        }
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
    options: Arc<Mutex<Options>>,
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
                    job.moving,
                    job.only.clone(),
                    job.delete.clone(),
                ));
            } else if job.state == State::Waiting {
                job.state = State::Stopped;
            }
        });
        repaint.request_repaint();
        let Some((from, from_path, to, to_dir, stop, moving, only, delete)) = info else {
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
        let opts = *options.lock().unwrap();
        let mut summary = Summary::default();
        let mut result = if only.is_empty() && delete.is_empty() {
            run_with(
                &mut sessions,
                &opts,
                &mut summary,
                &from,
                &from_path,
                &to,
                &to_dir,
                &mut progress,
                &mut total,
            )
        } else {
            sync(
                &mut sessions,
                &opts,
                &mut summary,
                &from,
                &from_path,
                &to,
                &to_dir,
                &only,
                &delete,
                &mut progress,
                &mut total,
            )
        };
        let detail = summary.text();
        update(&mut |job| job.detail = detail.clone());
        // A move removes the original only once the copy is whole.
        if moving && result.is_ok() {
            result = match &from {
                Place::Local => {
                    let p = Path::new(&from_path);
                    if p.is_dir() {
                        std::fs::remove_dir_all(p)
                    } else {
                        std::fs::remove_file(p)
                    }
                    .map_err(|e| format!("Copied, but the original couldn't be removed: {e}"))
                }
                Place::Server(host) => session(&mut sessions, host)
                    .and_then(|s| s.remove_all(&from_path))
                    .map_err(|e| format!("Copied, but the original couldn't be removed: {e}")),
            };
            let folder = match &from {
                Place::Local => Path::new(&from_path)
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                Place::Server(_) => sftp::parent(&from_path),
            };
            let _ = changed.send((from.clone(), folder));
        }
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

/// What a job did besides copying, for its line in the list.
#[derive(Default)]
pub struct Summary {
    skipped: usize,
    /// Files that went as their changed blocks, and the bytes those blocks came to.
    delta: usize,
    delta_bytes: u64,
    delta_of: u64,
    verified: usize,
    streamed: bool,
    deleted: usize,
}

impl Summary {
    pub(crate) fn text(&self) -> String {
        let mut parts = Vec::new();
        if self.streamed {
            parts.push("as one stream".to_string());
        }
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if self.delta > 0 {
            parts.push(format!(
                "only the changes: {} of {}",
                size(self.delta_bytes),
                size(self.delta_of)
            ));
        }
        if self.deleted > 0 {
            parts.push(format!("{} deleted", self.deleted));
        }
        if self.verified > 0 {
            parts.push(format!("{} checked by SHA-256", self.verified));
        }
        parts.join("  ·  ")
    }
}

fn size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

#[cfg(test)]
fn run(
    sessions: &mut HashMap<String, Session>,
    from: &Place,
    from_path: &str,
    to: &Place,
    to_dir: &str,
    progress: Progress,
    total: &mut dyn FnMut(u64),
) -> Result<(), String> {
    run_with(
        sessions,
        &Options::default(),
        &mut Summary::default(),
        from,
        from_path,
        to,
        to_dir,
        progress,
        total,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_with(
    sessions: &mut HashMap<String, Session>,
    opts: &Options,
    summary: &mut Summary,
    from: &Place,
    from_path: &str,
    to: &Place,
    to_dir: &str,
    progress: Progress,
    total: &mut dyn FnMut(u64),
) -> Result<(), String> {
    // A folder goes as one stream when the server has a shell with tar.
    let is_dir = match from {
        Place::Local => Path::new(from_path).is_dir(),
        Place::Server(host) => session(sessions, host)?.stat(from_path)?.is_dir(),
    };
    let name = from_path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(from_path)
        .to_string();
    let there = match to {
        Place::Local => Path::new(to_dir).join(&name).exists(),
        Place::Server(host) => session(sessions, host)?
            .stat(&sftp::join(to_dir, &name))
            .is_ok(),
    };
    if is_dir && (opts.conflict == Conflict::Overwrite || !there) {
        summary.streamed = true;
        match (from, to) {
            (Place::Local, Place::Server(host)) if bulk::can(host) => {
                let (mut files, mut dirs) = (Vec::new(), Vec::new());
                walk_local(Path::new(from_path), "", &mut files, &mut dirs)
                    .map_err(|e| e.to_string())?;
                total(files.iter().map(|f| f.size).sum());
                return bulk::upload(Path::new(from_path), host, to_dir, progress);
            }
            (Place::Server(host), Place::Local) if bulk::can(host) => {
                total(bulk::size(host, from_path));
                return bulk::download(host, from_path, Path::new(to_dir), progress);
            }
            (Place::Server(a), Place::Server(b)) if bulk::can(a) && bulk::can(b) => {
                let size = bulk::size(a, from_path);
                total(size);
                return bulk::between(a, from_path, b, to_dir, size, progress);
            }
            _ => summary.streamed = false,
        }
    }
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
    copy_files(
        sessions, opts, summary, from, to, to_dir, &files, &dirs, progress,
    )
}

/// A sync: the files listed go to the same paths under `to_root`, replacing what is there
/// (the comparison already chose them), then the paths in `delete` are removed there.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync(
    sessions: &mut HashMap<String, Session>,
    opts: &Options,
    summary: &mut Summary,
    from: &Place,
    from_root: &str,
    to: &Place,
    to_root: &str,
    only: &[String],
    delete: &[String],
    progress: Progress,
    total: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let mut files = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    for relative in only {
        let path = match from {
            Place::Local => {
                let mut p = PathBuf::from(from_root);
                p.extend(relative.split('/'));
                p.display().to_string()
            }
            Place::Server(_) => sftp::join(from_root, relative),
        };
        let (size, mtime) = match from {
            Place::Local => {
                let meta = std::fs::metadata(&path).map_err(|e| format!("{path}: {e}"))?;
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as u32);
                (meta.len(), mtime)
            }
            Place::Server(host) => {
                let a = session(sessions, host)?.stat(&path)?;
                (a.size.unwrap_or(0), a.mtime)
            }
        };
        if let Some((parent, _)) = relative.rsplit_once('/') {
            let mut acc = String::new();
            for part in parent.split('/') {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(part);
                if !dirs.contains(&acc) {
                    dirs.push(acc.clone());
                }
            }
        }
        files.push(File {
            path,
            relative: relative.clone(),
            size,
            mtime,
        });
    }
    total(files.iter().map(|f| f.size).sum());
    let opts = Options {
        conflict: Conflict::Overwrite,
        ..*opts
    };
    copy_files(
        sessions, &opts, summary, from, to, to_root, &files, &dirs, progress,
    )?;
    for relative in delete {
        match to {
            Place::Local => {
                let mut p = PathBuf::from(to_root);
                p.extend(relative.split('/'));
                let gone = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
                gone.map_err(|e| format!("{}: {e}", p.display()))?;
            }
            Place::Server(host) => {
                session(sessions, host)?.remove_all(&sftp::join(to_root, relative))?;
            }
        }
        summary.deleted += 1;
    }
    Ok(())
}

/// Copies files found under a source to the same relative paths under `to_dir`, making
/// the folders first, with the rules for what is already there.
#[allow(clippy::too_many_arguments)]
fn copy_files(
    sessions: &mut HashMap<String, Session>,
    opts: &Options,
    summary: &mut Summary,
    from: &Place,
    to: &Place,
    to_dir: &str,
    files: &[File],
    dirs: &[String],
    progress: Progress,
) -> Result<(), String> {
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
    for dir in dirs {
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
    let (dest_local, dest_host) = match to {
        Place::Local => (true, String::new()),
        Place::Server(h) => (false, h.clone()),
    };
    for file in files {
        let mut dest = target(&file.relative);
        let name = file
            .relative
            .rsplit('/')
            .next()
            .unwrap_or(&file.relative)
            .to_string();
        if !dest_local {
            session(sessions, &dest_host)?;
        }
        // What is there already decides what happens.
        let mut existing = engine::info(sessions, dest_local, &dest_host, &dest);
        if let Some((_, there_mtime)) = existing {
            let skip = match opts.conflict {
                Conflict::Skip => true,
                Conflict::Newer => there_mtime.unwrap_or(0) >= file.mtime.unwrap_or(0),
                Conflict::KeepBoth => {
                    dest = engine::free_name(&dest, &mut |p| {
                        engine::info(sessions, dest_local, &dest_host, p).is_some()
                    });
                    existing = None;
                    false
                }
                Conflict::Overwrite => false,
            };
            if skip {
                summary.skipped += 1;
                before += file.size;
                progress(before, &name);
                continue;
            }
        }
        let mut step = |n: u64| progress(before + n, &name);
        let outcome = match (from, to) {
            (Place::Server(host), Place::Local) => {
                let s = session(sessions, host)?;
                engine::download(
                    s,
                    host,
                    &file.path,
                    file.size,
                    file.mtime,
                    &dest,
                    existing.map(|e| e.0),
                    &mut step,
                )?
            }
            (Place::Local, Place::Server(host)) => {
                let s = session(sessions, host)?;
                engine::upload(
                    s,
                    host,
                    &file.path,
                    file.size,
                    file.mtime,
                    &dest,
                    existing.map(|e| e.0),
                    &mut step,
                )?
            }
            _ => {
                copy_file(sessions, from, &file.path, to, &dest, file.mtime, &mut step)?;
                Outcome::Copied
            }
        };
        if let Outcome::Delta(sent) = outcome {
            summary.delta += 1;
            summary.delta_bytes += sent;
            summary.delta_of += file.size;
        }
        // Both ends hashed, where a server takes part and can make the hash.
        let remote = *from != Place::Local || *to != Place::Local;
        if opts.verify && remote && file.size >= engine::VERIFY_FROM {
            let hash = |place: &Place, path: &str| match place {
                Place::Local => engine::local_sha256(path),
                Place::Server(h) if bulk::can(h) => engine::remote_sha256(h, path),
                Place::Server(_) => None,
            };
            if let (Some(a), Some(b)) = (hash(from, &file.path), hash(to, &dest)) {
                if a != b {
                    return Err(format!(
                        "{name}: the copy doesn't match the original (SHA-256 differs)"
                    ));
                }
                summary.verified += 1;
            }
        }
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
    fn a_move_copies_then_removes_the_original() {
        let base = std::env::temp_dir().join(format!("rdrop-move-{}", std::process::id()));
        let (src, dest) = (base.join("from"), base.join("to"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(src.join("note.txt"), "moved").unwrap();
        let mut queue = Queue::new(&Context::default());
        let id = queue.add_move(
            Place::Local,
            src.join("note.txt").display().to_string(),
            Place::Local,
            dest.display().to_string(),
        );
        let started = Instant::now();
        loop {
            let state = queue
                .jobs
                .lock()
                .unwrap()
                .iter()
                .find(|j| j.id == id)
                .map(|j| j.state.clone());
            if state == Some(State::Done) {
                break;
            }
            assert!(
                started.elapsed().as_secs() < 10,
                "the move didn't finish: {:?}",
                state.map(|s| matches!(s, State::Failed(_)))
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(
            std::fs::read_to_string(dest.join("note.txt")).unwrap(),
            "moved"
        );
        assert!(!src.join("note.txt").exists());
        let _ = std::fs::remove_dir_all(base);
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

    /// Many small files, as one stream and file by file, against a real server, by hand:
    /// RDROP_TEST_HOST (and RDROP_TEST_HOST2) name hosts from ~/.ssh/config.
    #[test]
    #[ignore]
    fn many_small_files_on_real_servers() {
        let Ok(host) = std::env::var("RDROP_TEST_HOST") else {
            return;
        };
        let other = std::env::var("RDROP_TEST_HOST2").unwrap_or(host.clone());
        let base = std::env::temp_dir().join(format!("rdrop-many-{}", std::process::id()));
        let src = base.join("modules");
        for i in 0..400 {
            let dir = src.join(format!("pkg{}", i % 40));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("file{i}.js")),
                format!(
                    "module.exports = {i};
"
                )
                .repeat(20),
            )
            .unwrap();
        }
        let remote = format!("/tmp/rdrop-test-many-{}", std::process::id());
        let server = Place::Server(host.clone());
        let mut s = Session::connect(&host).unwrap();
        s.mkdir(&remote).unwrap();
        let mut sessions = HashMap::new();
        let count = |s: &mut Session, dir: &str| {
            let mut files = Vec::new();
            let mut dirs = Vec::new();
            walk_remote(s, dir, "", &mut files, &mut dirs).unwrap();
            files.len()
        };

        // File by file over SFTP, then as one stream.
        bulk::set_can(&host, false);
        let started = Instant::now();
        s.mkdir(&format!("{remote}/sftp")).unwrap();
        run(
            &mut sessions,
            &Place::Local,
            &src.display().to_string(),
            &server,
            &format!("{remote}/sftp"),
            &mut |_, _| true,
            &mut |_| {},
        )
        .unwrap();
        let by_file = started.elapsed();
        bulk::set_can(&host, true);
        let started = Instant::now();
        run(
            &mut sessions,
            &Place::Local,
            &src.display().to_string(),
            &server,
            &format!("{remote}/tar"),
            &mut |_, _| true,
            &mut |_| {},
        )
        .unwrap();
        let as_stream = started.elapsed();
        eprintln!("400 small files up: file by file {by_file:?}, as one stream {as_stream:?}");
        assert_eq!(count(&mut s, &format!("{remote}/tar/modules")), 400);
        assert_eq!(count(&mut s, &format!("{remote}/sftp/modules")), 400);

        // Down again, as one stream.
        let back = base.join("back");
        std::fs::create_dir_all(&back).unwrap();
        let started = Instant::now();
        run(
            &mut sessions,
            &server,
            &format!("{remote}/tar/modules"),
            &Place::Local,
            &back.display().to_string(),
            &mut |_, _| true,
            &mut |_| {},
        )
        .unwrap();
        eprintln!(
            "400 small files down as one stream: {:?}",
            started.elapsed()
        );
        let text =
            std::fs::read_to_string(back.join("modules").join("pkg7").join("file7.js")).unwrap();
        assert_eq!(
            text,
            "module.exports = 7;
"
            .repeat(20)
        );

        // Server to server.
        bulk::set_can(&other, true);
        let mut t = Session::connect(&other).unwrap();
        let remote2 = format!("{remote}-to");
        let started = Instant::now();
        run(
            &mut sessions,
            &server,
            &format!("{remote}/tar/modules"),
            &Place::Server(other.clone()),
            &remote2,
            &mut |_, _| true,
            &mut |_| {},
        )
        .unwrap();
        eprintln!(
            "400 small files server to server as one stream: {:?}",
            started.elapsed()
        );
        assert_eq!(count(&mut t, &format!("{remote2}/modules")), 400);

        t.remove_all(&remote2).unwrap();
        s.remove_all(&remote).unwrap();
        let _ = std::fs::remove_dir_all(base);
    }

    /// Against real servers, by hand: RDROP_TEST_HOST (and RDROP_TEST_HOST2 for a copy
    /// between servers) name hosts from ~/.ssh/config. Works in /tmp there and cleans up.
    /// A sync's files go to the same paths, folders made on the way, and its deletions go.
    #[test]
    #[ignore]
    fn a_sync_on_a_real_server() {
        let Ok(host) = std::env::var("RDROP_TEST_HOST") else {
            return;
        };
        let id = std::process::id();
        let dir = format!("/tmp/rdrop-test-sync-{id}");
        let local = std::env::temp_dir().join(format!("rdrop-sync-{id}"));
        std::fs::create_dir_all(local.join("a").join("b")).unwrap();
        std::fs::write(local.join("top.txt"), "top").unwrap();
        std::fs::write(local.join("a").join("b").join("deep.txt"), "deep").unwrap();
        let mut s = Session::connect(&host).unwrap();
        s.mkdir(&dir).unwrap();
        s.upload(&format!("{dir}/extra.txt"), &mut &b"x"[..], |_| true)
            .unwrap();
        let mut sessions = HashMap::new();
        let mut summary = Summary::default();
        sync(
            &mut sessions,
            &Options::default(),
            &mut summary,
            &Place::Local,
            &local.display().to_string(),
            &Place::Server(host.clone()),
            &dir,
            &["top.txt".into(), "a/b/deep.txt".into()],
            &["extra.txt".into()],
            &mut |_, _| true,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            s.stat(&format!("{dir}/a/b/deep.txt")).unwrap().size,
            Some(4)
        );
        assert_eq!(s.stat(&format!("{dir}/top.txt")).unwrap().size, Some(3));
        assert!(s.stat(&format!("{dir}/extra.txt")).is_err());
        assert_eq!(summary.deleted, 1);
        s.remove_all(&dir).unwrap();
        let _ = std::fs::remove_dir_all(local);
    }

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
        // Parts of a file, as previews and a log's live view read them.
        let part = s
            .read_at(&format!("{dir}/sub/big.bin"), 100_000, 50_000)
            .unwrap();
        assert!(part[..] == data[100_000..150_000], "a part differs");
        let tail = s
            .read_at(&format!("{dir}/sub/big.bin"), data.len() as u64 - 777, 4096)
            .unwrap();
        assert!(tail[..] == data[data.len() - 777..], "the end differs");
        // Permissions.
        s.chmod(&format!("{dir}/small.txt"), 0o640).unwrap();
        assert_eq!(
            s.stat(&format!("{dir}/small.txt"))
                .unwrap()
                .permissions
                .unwrap()
                & 0o777,
            0o640
        );
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
