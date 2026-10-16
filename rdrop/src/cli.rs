//! rdrop from a terminal or a script, with the same engine as the window: folders as one
//! stream, changed blocks only, resume, parallel parts and checksums.
//!
//!   rdrop ls   PLACE
//!   rdrop cp   SOURCE... DESTINATION_FOLDER   [--skip | --newer | --keep-both] [--no-verify]
//!   rdrop get  HOST:PATH... [LOCAL_FOLDER]
//!   rdrop put  LOCAL... HOST:FOLDER
//!   rdrop sync SOURCE_FOLDER DESTINATION_FOLDER [--delete] [--dry-run] [--force]
//!   rdrop rm   PLACE...
//!   rdrop mkdir PLACE...
//!
//! A place is a path here (C:\work, .\site, file.txt), `HOST:PATH` for a host from
//! ~/.ssh/config, or `site:NAME:PATH` for a site saved in rdrop. A path on a server that
//! doesn't start with / is under the home folder there.

use crate::compare;
use crate::engine::{Conflict, Options};
use crate::sftp::{self, Session};
use crate::sites;
use crate::transfer::{self, Place};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const COMMANDS: [&str; 9] = [
    "ls", "cp", "get", "put", "sync", "rm", "mkdir", "help", "--help",
];

/// Runs a command when the first argument names one; `None` starts the window.
pub fn run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first()?.as_str();
    if !COMMANDS.contains(&command) {
        return None;
    }
    attach_console();
    // Saved sites sign in with what rdrop remembers for them.
    for site in sites::load() {
        let key = site.key();
        sftp::register(
            &key,
            sftp::Target {
                args: site.ssh_args(),
                password: sites::password(&key),
            },
        );
    }
    let (flags, rest): (Vec<&String>, Vec<&String>) =
        args[1..].iter().partition(|a| a.starts_with("--"));
    let has = |f: &str| flags.iter().any(|a| *a == f);
    let rest: Vec<String> = rest.into_iter().cloned().collect();
    let result = match command {
        "ls" => ls(&rest),
        "cp" => cp(&rest, &has),
        "get" => {
            let mut rest = rest;
            if rest.len() == 1 || rest.last().is_some_and(|l| place(l).0 != Place::Local) {
                rest.push(".".into());
            }
            cp(&rest, &has)
        }
        "put" => cp(&rest, &has),
        "sync" => sync(&rest, &has),
        "rm" => rm(&rest),
        "mkdir" => mkdir(&rest),
        _ => {
            print!("{}", HELP);
            Ok(())
        }
    };
    let _ = std::io::stdout().flush();
    match result {
        Ok(()) => Some(0),
        Err(e) => {
            eprintln!("rdrop: {e}");
            Some(1)
        }
    }
}

const HELP: &str = "rdrop: files between this computer and your servers

  rdrop ls    PLACE                         list a folder
  rdrop cp    SOURCE... FOLDER              copy files and folders into FOLDER
                --skip | --newer | --keep-both   when a file is already there (default: overwrite)
                --no-verify                       don't check copies with SHA-256
  rdrop get   HOST:PATH... [FOLDER]         copy from a server to here (default: this folder)
  rdrop put   PATH... HOST:FOLDER           copy from here to a server
  rdrop sync  FROM TO                       make TO hold what FROM holds, newer files only
                --delete     also remove from TO what FROM doesn't have
                --force      replace files in TO even when they are newer
                --dry-run    say what would happen, change nothing
  rdrop rm    PLACE...                      delete, folders with what is in them
  rdrop mkdir PLACE...                      make folders, with the ones above them

A PLACE is a path here, HOST:PATH for a host in ~/.ssh/config, or site:NAME:PATH for a
site saved in rdrop. Copies go between any two: here and a server, or two servers.
";

/// The terminal rdrop was started from, when output isn't going somewhere already.
fn attach_console() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(pid: u32) -> i32;
            fn GetStdHandle(which: u32) -> isize;
        }
        // STD_OUTPUT_HANDLE; ATTACH_PARENT_PROCESS.
        let out = unsafe { GetStdHandle(-11i32 as u32) };
        if out == 0 || out == -1 {
            unsafe { AttachConsole(u32::MAX) };
        }
    }
}

/// Where an argument points: here, or a host and a path there.
pub fn place(arg: &str) -> (Place, String) {
    let drive =
        arg.len() >= 2 && arg.as_bytes()[1] == b':' && arg.as_bytes()[0].is_ascii_alphabetic();
    if let Some(rest) = arg.strip_prefix("site:") {
        let (name, path) = rest.split_once(':').unwrap_or((rest, ""));
        return (Place::Server(format!("site:{name}")), remote_path(path));
    }
    match arg.split_once(':') {
        Some((host, path)) if !drive && !host.is_empty() && !host.contains(['/', '\\']) => {
            (Place::Server(host.to_string()), remote_path(path))
        }
        _ => {
            let path = Path::new(arg);
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            };
            (Place::Local, clean(&full))
        }
    }
}

fn remote_path(path: &str) -> String {
    if path.is_empty() {
        ".".into()
    } else {
        path.to_string()
    }
}

/// `C:\a\.\b\..\c` as `C:\a\c`, without asking the disk.
fn clean(path: &Path) -> String {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.display().to_string()
}

fn session<'a>(
    sessions: &'a mut HashMap<String, Session>,
    host: &str,
) -> Result<&'a mut Session, String> {
    if !sessions.contains_key(host) {
        let s = Session::connect(host).map_err(|e| format!("{host}: {e}"))?;
        sessions.insert(host.to_string(), s);
    }
    Ok(sessions.get_mut(host).unwrap())
}

/// A server path made whole: `~`-relative ones start from the home folder there.
fn absolute(s: &mut Session, path: &str) -> Result<String, String> {
    if path.starts_with('/') {
        Ok(path.to_string())
    } else {
        s.realpath(path)
    }
}

fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < 4 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn ls(args: &[String]) -> Result<(), String> {
    let target = args.first().map_or(".", String::as_str);
    let (place, path) = place(target);
    let mut rows: Vec<(String, bool, u64, Option<u64>)> = Vec::new();
    match &place {
        Place::Local => {
            for e in std::fs::read_dir(&path)
                .map_err(|e| format!("{path}: {e}"))?
                .flatten()
            {
                let Ok(m) = e.metadata() else { continue };
                let t = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                rows.push((
                    e.file_name().to_string_lossy().into_owned(),
                    m.is_dir(),
                    m.len(),
                    t,
                ));
            }
        }
        Place::Server(host) => {
            let mut sessions = HashMap::new();
            let s = session(&mut sessions, host)?;
            let path = absolute(s, &path)?;
            for e in s.list(&path)? {
                rows.push((
                    e.name,
                    e.attrs.is_dir(),
                    e.attrs.size.unwrap_or(0),
                    e.attrs.mtime.map(u64::from),
                ));
            }
        }
    }
    rows.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    let mut out = std::io::stdout().lock();
    let offset = crate::local_offset();
    for (name, dir, bytes, mtime) in rows {
        let when = crate::time_text(mtime, offset);
        let shown = if dir { format!("{name}/") } else { name };
        let bytes = if dir { String::new() } else { size(bytes) };
        let _ = writeln!(out, "{when:<17} {bytes:>10}  {shown}");
    }
    Ok(())
}

/// A line on the terminal that rewrites itself: what is being copied and how far.
struct Meter {
    started: Instant,
    total: u64,
    shown: Instant,
    tty: bool,
}

impl Meter {
    fn new() -> Meter {
        Meter {
            started: Instant::now(),
            total: 0,
            shown: Instant::now(),
            // Only a terminal shows the line; in a log or a pipe it would be noise.
            tty: std::io::IsTerminal::is_terminal(&std::io::stderr()),
        }
    }

    fn show(&mut self, done: u64, current: &str) {
        if !self.tty || self.shown.elapsed().as_millis() < 120 {
            return;
        }
        self.shown = Instant::now();
        let secs = self.started.elapsed().as_secs_f64().max(0.001);
        let pct = if self.total > 0 {
            done as f64 * 100.0 / self.total as f64
        } else {
            0.0
        };
        let name: String = current.chars().take(28).collect();
        eprint!(
            "\r  {pct:5.1}%  {} of {}  {}/s  {name:<28}",
            size(done),
            size(self.total),
            size((done as f64 / secs) as u64)
        );
    }

    fn end(&self) {
        if self.tty {
            eprint!("\r{:<78}\r", "");
        }
    }
}

fn cp(args: &[String], has: &dyn Fn(&str) -> bool) -> Result<(), String> {
    let [sources @ .., dest] = args else {
        return Err("cp needs at least one source and a folder to copy into".into());
    };
    if sources.is_empty() {
        return Err("cp needs at least one source and a folder to copy into".into());
    }
    let opts = Options {
        conflict: if has("--skip") {
            Conflict::Skip
        } else if has("--newer") {
            Conflict::Newer
        } else if has("--keep-both") {
            Conflict::KeepBoth
        } else {
            Conflict::Overwrite
        },
        verify: !has("--no-verify"),
    };
    let mut sessions = HashMap::new();
    let (to, mut to_dir) = place(dest);
    if let Place::Server(host) = &to {
        let s = session(&mut sessions, host)?;
        to_dir = absolute(s, &to_dir)?;
        if !s.stat(&to_dir).is_ok_and(|a| a.is_dir()) {
            return Err(format!("{dest} isn't a folder"));
        }
    } else {
        std::fs::create_dir_all(&to_dir).map_err(|e| format!("{to_dir}: {e}"))?;
    }
    for source in sources {
        let (from, mut from_path) = place(source);
        if let Place::Server(host) = &from {
            from_path = absolute(session(&mut sessions, host)?, &from_path)?;
        }
        let started = Instant::now();
        let mut meter = Meter::new();
        let total = std::cell::Cell::new(0u64);
        let mut summary = transfer::Summary::default();
        let result = {
            let meter = std::cell::RefCell::new(&mut meter);
            transfer::run_with(
                &mut sessions,
                &opts,
                &mut summary,
                &from,
                &from_path,
                &to,
                &to_dir,
                &mut |done, current| {
                    meter.borrow_mut().show(done, current);
                    true
                },
                &mut |t| {
                    total.set(t);
                    meter.borrow_mut().total = t;
                },
            )
        };
        meter.end();
        result.map_err(|e| format!("{source}: {e}"))?;
        let secs = started.elapsed().as_secs_f64();
        let detail = summary.text();
        println!(
            "{source}  →  {dest}   {} in {secs:.1} s{}",
            size(total.get()),
            if detail.is_empty() {
                String::new()
            } else {
                format!("  ({detail})")
            }
        );
    }
    Ok(())
}

fn sync(args: &[String], has: &dyn Fn(&str) -> bool) -> Result<(), String> {
    let [from_arg, to_arg] = args else {
        return Err("sync needs the folder to copy from and the folder to update".into());
    };
    let mut sessions = HashMap::new();
    let (from, mut from_root) = place(from_arg);
    let (to, mut to_root) = place(to_arg);
    for (p, root) in [(&from, &mut from_root), (&to, &mut to_root)] {
        if let Place::Server(host) = p {
            *root = absolute(session(&mut sessions, host)?, root)?;
        }
    }
    if let Place::Local = to {
        std::fs::create_dir_all(&to_root).map_err(|e| format!("{to_root}: {e}"))?;
    }
    let left = compare::listing(&from, &from_root)?;
    let right = compare::listing(&to, &to_root).unwrap_or_default();
    let (rows, same) = compare::diff(&left, &right);
    let direction = compare::Direction::ToRight;
    let mut copy = Vec::new();
    let mut delete = Vec::new();
    for row in &rows {
        let Some((action, on)) = compare::plan(row.kind, direction, has("--delete")) else {
            continue;
        };
        if !on && !has("--force") {
            println!(
                "  keep    {}  (newer in {to_arg}; --force replaces it)",
                row.path
            );
            continue;
        }
        match action {
            compare::Action::DeleteRight => delete.push(row.path.clone()),
            _ => copy.push(row.path.clone()),
        }
    }
    let bytes: u64 = copy
        .iter()
        .filter_map(|p| left.get(p))
        .map(|m| m.size)
        .sum();
    for p in &copy {
        println!("  copy    {p}");
    }
    for p in &delete {
        println!("  delete  {p}");
    }
    println!(
        "{} to copy ({}), {} to delete, {same} the same",
        copy.len(),
        size(bytes),
        delete.len()
    );
    if has("--dry-run") || (copy.is_empty() && delete.is_empty()) {
        return Ok(());
    }
    let mut meter = Meter::new();
    meter.total = bytes;
    let mut summary = transfer::Summary::default();
    let started = Instant::now();
    let result = {
        let meter = std::cell::RefCell::new(&mut meter);
        transfer::sync(
            &mut sessions,
            &Options::default(),
            &mut summary,
            &from,
            &from_root,
            &to,
            &to_root,
            &copy,
            &delete,
            &mut |done, current| {
                meter.borrow_mut().show(done, current);
                true
            },
            &mut |_| {},
        )
    };
    meter.end();
    result?;
    let detail = summary.text();
    println!(
        "Done in {:.1} s{}",
        started.elapsed().as_secs_f64(),
        if detail.is_empty() {
            String::new()
        } else {
            format!("  ({detail})")
        }
    );
    Ok(())
}

fn rm(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("rm needs something to delete".into());
    }
    let mut sessions = HashMap::new();
    for arg in args {
        match place(arg) {
            (Place::Local, path) => {
                let p = Path::new(&path);
                if p.is_dir() {
                    std::fs::remove_dir_all(p)
                } else {
                    std::fs::remove_file(p)
                }
                .map_err(|e| format!("{arg}: {e}"))?
            }
            (Place::Server(host), path) => {
                let s = session(&mut sessions, &host)?;
                let path = absolute(s, &path)?;
                if path == "/" || path.trim_end_matches('/').is_empty() {
                    return Err("rdrop won't delete / on a server".into());
                }
                s.remove_all(&path).map_err(|e| format!("{arg}: {e}"))?;
            }
        }
        println!("deleted  {arg}");
    }
    Ok(())
}

fn mkdir(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("mkdir needs a folder to make".into());
    }
    let mut sessions = HashMap::new();
    for arg in args {
        match place(arg) {
            (Place::Local, path) => {
                std::fs::create_dir_all(&path).map_err(|e| format!("{arg}: {e}"))?
            }
            (Place::Server(host), path) => {
                let s = session(&mut sessions, &host)?;
                let path = absolute(s, &path).or_else(|_| {
                    // realpath of a folder that isn't there yet fails: start from home.
                    s.realpath(".").map(|home| sftp::join(&home, &path))
                })?;
                let mut at = String::new();
                for part in path.split('/').filter(|p| !p.is_empty()) {
                    at.push('/');
                    at.push_str(part);
                    if !s.stat(&at).is_ok_and(|a| a.is_dir()) {
                        s.mkdir(&at).map_err(|e| format!("{at}: {e}"))?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_read_as_written() {
        assert_eq!(
            place("vespucci:/var/www"),
            (Place::Server("vespucci".into()), "/var/www".into())
        );
        assert_eq!(place("vps2:"), (Place::Server("vps2".into()), ".".into()));
        assert_eq!(
            place("vps2:logs"),
            (Place::Server("vps2".into()), "logs".into())
        );
        assert_eq!(
            place("site:Prod:/srv"),
            (Place::Server("site:Prod".into()), "/srv".into())
        );
        assert_eq!(place(r"C:\work\..\site"), (Place::Local, r"C:\site".into()));
        assert_eq!(place("C:/x").0, Place::Local);
        assert_eq!(place(r".\a").0, Place::Local);
    }
}
