//! Looking for files under a folder, by name or by what is in them. On a server the search
//! runs there (find and grep), so only the results cross the network; a server without a
//! shell is walked over SFTP instead, by name. Here, the folders are walked on a thread.
//! Results come in as they are found, as paths under the folder searched.

use crate::sftp::{self, Session};
use crate::transfer::Place;
use crate::Item;
use eframe::egui::Context;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// More than this many results stop the search: the query should be narrower.
pub const LIMIT: usize = 2000;
/// Files larger than this aren't read for their contents, here.
const READ_UP_TO: u64 = 32 * 1024 * 1024;

pub enum Found {
    Items(Vec<Item>),
    Done(Option<String>),
}

pub struct Search {
    pub query: String,
    pub contents: bool,
    pub rx: Receiver<Found>,
    pub stop: Arc<AtomicBool>,
    pub found: usize,
    pub running: bool,
    pub error: Option<String>,
    pub started: Instant,
    pub took: Option<Duration>,
}

impl Search {
    pub fn start(ctx: &Context, place: &Place, root: &str, query: &str, contents: bool) -> Search {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (place, root_owned, q, flag, repaint) = (
            place.clone(),
            root.to_string(),
            query.to_string(),
            stop.clone(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            let mut out = Out {
                tx,
                batch: Vec::new(),
                sent: Instant::now(),
                count: 0,
                stop: flag,
                repaint,
            };
            let result = match &place {
                Place::Local => local(&root_owned, &q, contents, &mut out),
                Place::Server(host) => remote(host, &root_owned, &q, contents, &mut out),
            };
            out.flush();
            let _ = out.tx.send(Found::Done(result.err()));
            out.repaint.request_repaint();
        });
        Search {
            query: query.to_string(),
            contents,
            rx,
            stop,
            found: 0,
            running: true,
            error: None,
            started: Instant::now(),
            took: None,
        }
    }

    /// What came in since last time; `None` when nothing did.
    pub fn poll(&mut self) -> Option<Vec<Item>> {
        let mut new = Vec::new();
        let mut any = false;
        for msg in self.rx.try_iter() {
            any = true;
            match msg {
                Found::Items(items) => new.extend(items),
                Found::Done(error) => {
                    self.running = false;
                    self.error = error;
                    self.took = Some(self.started.elapsed());
                }
            }
        }
        self.found += new.len();
        any.then_some(new)
    }

    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct Out {
    tx: Sender<Found>,
    batch: Vec<Item>,
    sent: Instant,
    count: usize,
    stop: Arc<AtomicBool>,
    repaint: Context,
}

impl Out {
    /// Takes a result; false once the search should end.
    fn push(&mut self, item: Item) -> bool {
        self.batch.push(item);
        self.count += 1;
        if self.sent.elapsed() > Duration::from_millis(150) {
            self.flush();
        }
        self.going()
    }

    fn going(&self) -> bool {
        self.count < LIMIT && !self.stop.load(Ordering::Relaxed)
    }

    fn flush(&mut self) {
        if !self.batch.is_empty() {
            let _ = self.tx.send(Found::Items(std::mem::take(&mut self.batch)));
            self.repaint.request_repaint();
        }
        self.sent = Instant::now();
    }
}

/// Matches as Explorer does: any part of the name, upper or lower case alike; `*` and `?`
/// work as wildcards when they are used.
pub fn name_matches(name: &str, query: &str) -> bool {
    let (name, query) = (name.to_lowercase(), query.to_lowercase());
    if query.contains(['*', '?']) {
        wildcard(name.as_bytes(), query.as_bytes())
    } else {
        name.contains(&query)
    }
}

fn wildcard(name: &[u8], pattern: &[u8]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            wildcard(name, &pattern[1..]) || (!name.is_empty() && wildcard(&name[1..], pattern))
        }
        (Some(b'?'), Some(_)) => wildcard(&name[1..], &pattern[1..]),
        (Some(p), Some(n)) if p == n => wildcard(&name[1..], &pattern[1..]),
        _ => false,
    }
}

/// Whether a file holds the text, upper or lower case alike; files that look binary don't.
fn holds(path: &std::path::Path, needle: &[u8]) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, file);
    let mut buf = vec![0u8; 256 * 1024];
    // What is kept from the last piece, so a match across two pieces is found.
    let keep = needle.len().saturating_sub(1);
    let mut carry: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        if first && buf[..n.min(8000)].contains(&0) {
            return false;
        }
        first = false;
        carry.extend(buf[..n].iter().map(u8::to_ascii_lowercase));
        if carry.windows(needle.len()).any(|w| w == needle) {
            return true;
        }
        let cut = carry.len().saturating_sub(keep);
        carry.drain(..cut);
    }
}

fn local(root: &str, query: &str, contents: bool, out: &mut Out) -> Result<(), String> {
    let needle: Vec<u8> = query.to_lowercase().into_bytes();
    let sep = std::path::MAIN_SEPARATOR;
    let mut stack = vec![(std::path::PathBuf::from(root), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !out.going() {
                return Ok(());
            }
            let Ok(meta) = entry.metadata() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}{sep}{name}")
            };
            let link = entry.file_type().is_ok_and(|t| t.is_symlink());
            let hit = if contents {
                meta.is_file() && meta.len() <= READ_UP_TO && holds(&entry.path(), &needle)
            } else {
                name_matches(&name, query)
            };
            if meta.is_dir() && !link {
                stack.push((entry.path(), relative.clone()));
            }
            if hit {
                let item = Item {
                    hidden: crate::winshell::is_hidden(&meta) || name.starts_with('.'),
                    perms: None,
                    link,
                    name: relative,
                    is_dir: meta.is_dir(),
                    size: if meta.is_dir() { 0 } else { meta.len() },
                    mtime: meta
                        .modified()
                        .ok()
                        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs()),
                };
                if !out.push(item) {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// The pattern `find -iname` takes for what was typed.
fn find_pattern(query: &str) -> String {
    if query.contains(['*', '?']) {
        query.to_string()
    } else {
        format!("*{}*", query.replace(['[', ']'], "?"))
    }
}

fn remote(
    host: &str,
    root: &str,
    query: &str,
    contents: bool,
    out: &mut Out,
) -> Result<(), String> {
    use std::io::BufRead;
    let q = sftp::quote;
    let script = if contents {
        // grep lists the files, stat says what they are; both stop at the limit.
        format!(
            "cd {} 2>/dev/null || exit 3; grep -rIlZ -i -F --exclude-dir=.git -- {} . 2>/dev/null | head -z -n {LIMIT} | xargs -0 -r stat --printf '%n\\t%s\\t%Y\\t%F\\t%a\\n' 2>/dev/null",
            q(root),
            q(query)
        )
    } else {
        format!(
            "cd {} 2>/dev/null || exit 3; find . -mindepth 1 -iname {} -printf '%P\\t%s\\t%T@\\t%y\\t%m\\n' 2>/dev/null | head -n {LIMIT}",
            q(root),
            q(&find_pattern(query))
        )
    };
    let mut child = sftp::shell(host, &script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().unwrap();
    let mut lines = 0;
    for line in std::io::BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        lines += 1;
        if let Some(item) = parse_line(&line) {
            if !out.push(item) {
                let _ = child.kill();
                break;
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    match status.code() {
        Some(3) => Err(format!("{root} can't be opened")),
        // ssh itself failed, or there is no shell: walk the folders over SFTP, by name.
        Some(255) | Some(126) | Some(127) if lines == 0 && !contents => walk(host, root, query, out),
        Some(255) | Some(126) | Some(127) if lines == 0 => {
            Err("This server doesn't let rdrop run commands, so files can't be searched by what is in them".into())
        }
        _ => Ok(()),
    }
}

/// One line of find's or stat's output: path, size, time, kind, permissions.
fn parse_line(line: &str) -> Option<Item> {
    let mut parts = line.split('\t');
    let path = parts.next()?;
    let path = path.strip_prefix("./").unwrap_or(path).to_string();
    let size: u64 = parts.next()?.parse().ok()?;
    let mtime = parts.next()?.split('.').next().and_then(|s| s.parse().ok());
    let kind = parts.next()?;
    let perms = parts.next().and_then(|p| u32::from_str_radix(p, 8).ok());
    let is_dir = kind == "d" || kind == "directory";
    let name = path.rsplit('/').next().unwrap_or(&path);
    Some(Item {
        hidden: name.starts_with('.'),
        perms,
        link: kind == "l" || kind == "symbolic link",
        is_dir,
        size: if is_dir { 0 } else { size },
        mtime,
        name: path,
    })
}

fn walk(host: &str, root: &str, query: &str, out: &mut Out) -> Result<(), String> {
    let mut s = Session::connect(host)?;
    let mut stack = vec![String::new()];
    while let Some(rel) = stack.pop() {
        let dir = if rel.is_empty() {
            root.to_string()
        } else {
            sftp::join(root, &rel)
        };
        let Ok(entries) = s.list(&dir) else { continue };
        for e in entries {
            if !out.going() {
                return Ok(());
            }
            let relative = if rel.is_empty() {
                e.name.clone()
            } else {
                format!("{rel}/{}", e.name)
            };
            if e.attrs.is_dir() && !e.link {
                stack.push(relative.clone());
            }
            if name_matches(&e.name, query) {
                let is_dir = e.attrs.is_dir();
                let item = Item {
                    hidden: e.name.starts_with('.'),
                    perms: e.attrs.permissions,
                    link: e.link,
                    is_dir,
                    size: if is_dir { 0 } else { e.attrs.size.unwrap_or(0) },
                    mtime: e.attrs.mtime.map(u64::from),
                    name: relative,
                };
                if !out.push(item) {
                    return Ok(());
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
    fn names_match_as_explorer_matches_them() {
        assert!(name_matches("Main.rs", "main"));
        assert!(name_matches("server.log.1", "*.log*"));
        assert!(name_matches("a.txt", "?.TXT"));
        assert!(!name_matches("a.txt", "*.rs"));
        assert!(!name_matches("ab.txt", "?.txt"));
    }

    #[test]
    fn find_and_stat_lines_read_back() {
        let a = parse_line("src/main.rs\t1234\t1700000000.5\tf\t644").unwrap();
        assert_eq!(
            (a.name.as_str(), a.size, a.mtime, a.is_dir, a.perms),
            ("src/main.rs", 1234, Some(1_700_000_000), false, Some(0o644))
        );
        let b = parse_line("./logs/.old\t4096\t1700000000\tdirectory\t755").unwrap();
        assert!(b.is_dir && b.hidden && b.name == "logs/.old" && b.size == 0);
    }

    #[test]
    fn contents_are_found_across_pieces() {
        let path = std::env::temp_dir().join(format!("rdrop-holds-{}", std::process::id()));
        let mut data = vec![b'x'; 256 * 1024 - 3];
        data.extend_from_slice(b"NeedLE here");
        std::fs::write(&path, &data).unwrap();
        assert!(holds(&path, b"needle"));
        assert!(!holds(&path, b"haystack"));
        std::fs::write(&path, b"\0\0binary needle").unwrap();
        assert!(!holds(&path, b"needle"));
        let _ = std::fs::remove_file(path);
    }
}
