//! Folders moved as one stream instead of file by file: `tar` on the server, through the
//! same ssh as SFTP, packed and unpacked on the way. A folder of thousands of small files
//! (node_modules, a game server's resources) goes in seconds instead of minutes, because
//! SFTP pays a round trip for every file and this pays one for the folder. A server
//! without a shell or tar falls back to SFTP.

use crate::sftp;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::Mutex;

pub type Progress<'a> = &'a mut dyn FnMut(u64, &str) -> bool;

static CAN: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

/// Whether the server has a shell with tar and gzip, asked once.
pub fn can(key: &str) -> bool {
    if let Some(known) = CAN
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(key).copied())
    {
        return known;
    }
    let ok = sftp::run(
        key,
        "command -v tar >/dev/null && command -v gzip >/dev/null && echo rdrop-ok",
    )
    .is_some_and(|out| out.contains("rdrop-ok"));
    CAN.lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(key.to_string(), ok);
    ok
}

/// Says whether a server can stream folders, without asking it: for tests that compare
/// the two ways.
#[cfg(test)]
pub fn set_can(key: &str, can: bool) {
    CAN.lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(key.to_string(), can);
}

/// The size of a folder on the server, roughly, for the progress bar.
pub fn size(key: &str, path: &str) -> u64 {
    sftp::run(
        key,
        &format!("du -sk {} 2>/dev/null | cut -f1", sftp::quote(path)),
    )
    .and_then(|out| out.trim().parse::<u64>().ok())
    .map_or(0, |kb| kb * 1024)
}

fn split(path: &str) -> (String, String) {
    let parent = sftp::parent(path);
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string();
    (parent, name)
}

/// What ssh said when it stopped, for an error.
fn finish(mut child: Child, what: &str) -> Result<(), String> {
    let status = child.wait().map_err(|e| e.to_string())?;
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    if status.success() {
        return Ok(());
    }
    let last = err
        .lines()
        .filter(|l| !l.trim().is_empty())
        .last()
        .unwrap_or("")
        .trim()
        .to_string();
    if last.is_empty() {
        Err(format!("{what} stopped ({status})"))
    } else {
        Err(format!("{what}: {last}"))
    }
}

/// A folder on the server, unpacked into `local_dir` as it arrives.
pub fn download(key: &str, path: &str, local_dir: &Path, progress: Progress) -> Result<(), String> {
    let (parent, name) = split(path);
    let script = format!(
        "cd {} && tar -cf - ./{} | gzip -1",
        sftp::quote(&parent),
        sftp::quote(&name)
    );
    let mut child = sftp::shell(key, &script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start ssh: {e}"))?;
    let out = child.stdout.take().unwrap();
    let mut archive = tar::Archive::new(GzDecoder::new(BufReader::with_capacity(512 * 1024, out)));
    archive.set_preserve_mtime(true);
    let mut done = 0u64;
    let result = (|| -> Result<(), String> {
        for entry in archive.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.header().entry_type();
            // Links make no sense on Windows without special rights; they are left out.
            if kind.is_symlink() || kind.is_hard_link() {
                continue;
            }
            let size = entry.header().size().unwrap_or(0);
            let shown = entry
                .path()
                .map(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            entry
                .unpack_in(local_dir)
                .map_err(|e| format!("{shown}: {e}"))?;
            done += size;
            if !progress(done, &shown) {
                return Err("Stopped".into());
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return result;
    }
    finish(child, "The server's tar")
}

/// A folder here, packed as it is read and unpacked into `remote_dir` on the server.
pub fn upload(local: &Path, key: &str, remote_dir: &str, progress: Progress) -> Result<(), String> {
    let script = format!(
        "mkdir -p {0} && cd {0} && gzip -dc | tar -xf -",
        sftp::quote(remote_dir)
    );
    let mut child = sftp::shell(key, &script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start ssh: {e}"))?;
    let stdin = child.stdin.take().unwrap();
    let mut builder = tar::Builder::new(GzEncoder::new(
        std::io::BufWriter::with_capacity(512 * 1024, stdin),
        Compression::fast(),
    ));
    builder.follow_symlinks(false);
    let base = local.parent().unwrap_or(local);
    let mut done = 0u64;
    let result = (|| -> Result<(), String> {
        let mut stack = vec![local.to_path_buf()];
        while let Some(path) = stack.pop() {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let meta =
                std::fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if meta.is_dir() {
                builder
                    .append_dir(&relative, &path)
                    .map_err(|e| e.to_string())?;
                for entry in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
                    stack.push(entry.map_err(|e| e.to_string())?.path());
                }
            } else if meta.is_file() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let mut header = tar::Header::new_gnu();
                header.set_metadata(&meta);
                header.set_mode(0o644);
                header.set_size(meta.len());
                let file =
                    std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let mut counted = Counted {
                    inner: file,
                    seen: 0,
                    on: &mut |n| progress(done + n, &name),
                    stopped: false,
                };
                builder
                    .append_data(&mut header, &relative, &mut counted)
                    .map_err(|e| e.to_string())?;
                if counted.stopped {
                    return Err("Stopped".into());
                }
                done += meta.len();
            }
        }
        let gz = builder.into_inner().map_err(|e| e.to_string())?;
        let mut writer = gz.finish().map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return result;
    }
    finish(child, "The server's tar")
}

/// A folder from one server to another: what one packs, the other unpacks, through here.
pub fn between(
    from: &str,
    path: &str,
    to: &str,
    remote_dir: &str,
    total: u64,
    progress: Progress,
) -> Result<(), String> {
    let (parent, name) = split(path);
    let pack = format!(
        "cd {} && tar -cf - ./{} | gzip -1",
        sftp::quote(&parent),
        sftp::quote(&name)
    );
    let unpack = format!(
        "mkdir -p {0} && cd {0} && gzip -dc | tar -xf -",
        sftp::quote(remote_dir)
    );
    let mut reader = sftp::shell(from, &pack)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start ssh: {e}"))?;
    let mut writer = sftp::shell(to, &unpack)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start ssh: {e}"))?;
    let mut out = reader.stdout.take().unwrap();
    let mut input = writer.stdin.take().unwrap();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut moved = 0u64;
    let result = (|| -> Result<(), String> {
        loop {
            let n = out.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok(());
            }
            input
                .write_all(&buffer[..n])
                .map_err(|_| "The other server stopped taking the folder".to_string())?;
            moved += n as u64;
            // The stream is compressed; its size against the folder's is a fair guess of how far.
            let shown = (moved * 3).min(total.max(1));
            if !progress(shown, &name) {
                return Err("Stopped".into());
            }
        }
    })();
    drop(input);
    if result.is_err() {
        let _ = reader.kill();
        let _ = writer.kill();
        let _ = reader.wait();
        let _ = writer.wait();
        return result;
    }
    finish(reader, "Packing on the first server")?;
    finish(writer, "Unpacking on the second server")?;
    progress(total, &name);
    Ok(())
}

/// A reader that tells how far it has read, and can be stopped.
struct Counted<'a, R> {
    inner: R,
    seen: u64,
    on: &'a mut dyn FnMut(u64) -> bool,
    stopped: bool,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.stopped {
            return Err(std::io::Error::other("stopped"));
        }
        let n = self.inner.read(buf)?;
        self.seen += n as u64;
        if !(self.on)(self.seen) {
            self.stopped = true;
            return Err(std::io::Error::other("stopped"));
        }
        Ok(n)
    }
}
