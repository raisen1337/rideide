//! An SFTP client (version 3 of the protocol, which every server speaks) over the system's
//! `ssh -s HOST sftp`. ssh does the connecting, so the user's ~/.ssh/config, keys, agent
//! and jump hosts all apply, and any server with SFTP works, whatever its system.

use std::io::{BufReader, BufWriter, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const INIT: u8 = 1;
const VERSION: u8 = 2;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const WRITE: u8 = 6;
const LSTAT: u8 = 7;
const SETSTAT: u8 = 9;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REMOVE: u8 = 13;
const MKDIR: u8 = 14;
const RMDIR: u8 = 15;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const RENAME: u8 = 18;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;

const FX_OK: u32 = 0;
const FX_EOF: u32 = 1;

const ATTR_SIZE: u32 = 0x1;
const ATTR_UIDGID: u32 = 0x2;
const ATTR_PERMISSIONS: u32 = 0x4;
const ATTR_ACMODTIME: u32 = 0x8;
const ATTR_EXTENDED: u32 = 0x8000_0000;

pub const OPEN_READ: u32 = 0x1;
pub const OPEN_WRITE: u32 = 0x2;
pub const OPEN_CREATE: u32 = 0x8;
pub const OPEN_TRUNCATE: u32 = 0x10;

/// The size of one read or write; servers take 32 KiB at the least.
pub const CHUNK: usize = 32 * 1024;
/// How many reads or writes are sent before waiting for the first answer.
const IN_FLIGHT: usize = 32;

#[derive(Clone, Debug, Default)]
pub struct Attrs {
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub mtime: Option<u32>,
    pub atime: Option<u32>,
}

impl Attrs {
    pub fn is_dir(&self) -> bool {
        self.permissions.is_some_and(|p| p & 0o170000 == 0o040000)
    }

    pub fn is_link(&self) -> bool {
        self.permissions.is_some_and(|p| p & 0o170000 == 0o120000)
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub attrs: Attrs,
}

pub type Result<T> = std::result::Result<T, String>;

// ---- encoding ----

struct Out(Vec<u8>);

impl Out {
    fn new(kind: u8, id: u32) -> Self {
        let mut out = Out(vec![0, 0, 0, 0, kind]);
        out.u32(id);
        out
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend(v.to_be_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend(v.to_be_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32);
        self.0.extend(v);
        self
    }
    fn str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }
    fn attrs(&mut self, a: &Attrs) -> &mut Self {
        let mut flags = 0;
        if a.size.is_some() {
            flags |= ATTR_SIZE;
        }
        if a.permissions.is_some() {
            flags |= ATTR_PERMISSIONS;
        }
        if a.mtime.is_some() {
            flags |= ATTR_ACMODTIME;
        }
        self.u32(flags);
        if let Some(size) = a.size {
            self.u64(size);
        }
        if let Some(p) = a.permissions {
            self.u32(p);
        }
        if let Some(m) = a.mtime {
            self.u32(a.atime.unwrap_or(m));
            self.u32(m);
        }
        self
    }
    fn finish(mut self) -> Vec<u8> {
        let len = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&len.to_be_bytes());
        self.0
    }
}

struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn u32(&mut self) -> Result<u32> {
        if self.0.len() < 4 {
            return Err("short packet".into());
        }
        let (head, rest) = self.0.split_at(4);
        self.0 = rest;
        Ok(u32::from_be_bytes(head.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok((self.u32()? as u64) << 32 | self.u32()? as u64)
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        if self.0.len() < len {
            return Err("short packet".into());
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(head)
    }
    fn string(&mut self) -> Result<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
    fn attrs(&mut self) -> Result<Attrs> {
        let flags = self.u32()?;
        let mut a = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            a.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            self.u32()?;
            self.u32()?;
        }
        if flags & ATTR_PERMISSIONS != 0 {
            a.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            a.atime = Some(self.u32()?);
            a.mtime = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            for _ in 0..self.u32()? {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(a)
    }
}

fn status_error(code: u32, message: &str) -> String {
    let what = match code {
        2 => "No such file or folder",
        3 => "Permission denied",
        4 => "The server couldn't do that",
        5 => "Bad message",
        6 => "No connection",
        7 => "The connection was lost",
        8 => "The server doesn't support that",
        _ => "Failed",
    };
    if message.is_empty() || message.eq_ignore_ascii_case(what) {
        what.to_string()
    } else {
        format!("{what}: {message}")
    }
}

// ---- the session ----

pub struct Session {
    child: Child,
    to: BufWriter<ChildStdin>,
    from: BufReader<ChildStdout>,
    next_id: u32,
}

/// Starts `ssh` without a console window and without asking anything: a host that needs a
/// password the user hasn't given ssh some other way fails rather than waits.
fn ssh(host: &str) -> Command {
    let mut command = Command::new("ssh");
    command.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=30",
        "-s",
        host,
        "sftp",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

impl Session {
    pub fn connect(host: &str) -> Result<Session> {
        let mut child = ssh(host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Couldn't start ssh: {e}"))?;
        let to = BufWriter::with_capacity(256 * 1024, child.stdin.take().unwrap());
        let from = BufReader::with_capacity(256 * 1024, child.stdout.take().unwrap());
        let mut session = Session {
            child,
            to,
            from,
            next_id: 1,
        };
        let mut init = Out(vec![0, 0, 0, 0, INIT]);
        init.u32(3);
        let started = session.send(init.finish()).and_then(|()| session.receive());
        match started {
            Ok((VERSION, _)) => Ok(session),
            Ok(_) => Err("The server answered something other than SFTP".into()),
            Err(_) => Err(session.ssh_error()),
        }
    }

    /// What ssh said on its way out, for a connection that didn't work.
    fn ssh_error(&mut self) -> String {
        let _ = self.child.kill();
        let mut text = String::new();
        if let Some(mut err) = self.child.stderr.take() {
            let _ = err.read_to_string(&mut text);
        }
        let text = text.trim();
        if text.contains("Permission denied") {
            "The server wouldn't let you in with your SSH keys (Permission denied). rdrop signs in the way `ssh` does with BatchMode, so it needs a key or agent rather than a password.".into()
        } else if text.is_empty() {
            "The connection closed before SFTP started".into()
        } else {
            text.lines().last().unwrap_or(text).to_string()
        }
    }

    fn id(&mut self) -> u32 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    fn send(&mut self, packet: Vec<u8>) -> Result<()> {
        self.to.write_all(&packet).map_err(|e| e.to_string())?;
        self.to.flush().map_err(|e| e.to_string())
    }

    /// Queues a packet without flushing, for pipelined requests.
    fn queue(&mut self, packet: Vec<u8>) -> Result<()> {
        self.to.write_all(&packet).map_err(|e| e.to_string())
    }

    fn receive(&mut self) -> Result<(u8, Vec<u8>)> {
        let mut len = [0u8; 4];
        self.from
            .read_exact(&mut len)
            .map_err(|_| "The connection was lost".to_string())?;
        let len = u32::from_be_bytes(len) as usize;
        if len == 0 || len > 64 * 1024 * 1024 {
            return Err("The server sent a broken packet".into());
        }
        let mut body = vec![0u8; len];
        self.from
            .read_exact(&mut body)
            .map_err(|_| "The connection was lost".to_string())?;
        let kind = body[0];
        Ok((kind, body[1..].to_vec()))
    }

    /// Sends a request and waits for its answer: (type, payload after the id).
    fn call(&mut self, packet: Vec<u8>) -> Result<(u8, Vec<u8>)> {
        self.send(packet)?;
        let (kind, body) = self.receive()?;
        Ok((kind, body.get(4..).unwrap_or_default().to_vec()))
    }

    fn expect_ok(&mut self, packet: Vec<u8>) -> Result<()> {
        let (kind, body) = self.call(packet)?;
        check_status(kind, &body)
    }

    fn handle(&mut self, packet: Vec<u8>) -> Result<Vec<u8>> {
        let (kind, body) = self.call(packet)?;
        if kind == HANDLE {
            Ok(In(&body).bytes()?.to_vec())
        } else {
            Err(check_status(kind, &body)
                .err()
                .unwrap_or_else(|| "Unexpected answer".into()))
        }
    }

    fn close(&mut self, handle: &[u8]) -> Result<()> {
        let id = self.id();
        let mut p = Out::new(CLOSE, id);
        p.bytes(handle);
        self.expect_ok(p.finish())
    }

    /// The full path of `path` on the server; "." is the folder a session starts in.
    pub fn realpath(&mut self, path: &str) -> Result<String> {
        let id = self.id();
        let mut p = Out::new(REALPATH, id);
        p.str(path);
        let (kind, body) = self.call(p.finish())?;
        if kind != NAME {
            return Err(check_status(kind, &body).err().unwrap_or_default());
        }
        let mut r = In(&body);
        r.u32()?;
        r.string()
    }

    pub fn list(&mut self, path: &str) -> Result<Vec<Entry>> {
        let id = self.id();
        let mut p = Out::new(OPENDIR, id);
        p.str(path);
        let handle = self.handle(p.finish())?;
        let mut entries = Vec::new();
        loop {
            let id = self.id();
            let mut p = Out::new(READDIR, id);
            p.bytes(&handle);
            let (kind, body) = self.call(p.finish())?;
            if kind == STATUS {
                let mut r = In(&body);
                let code = r.u32()?;
                if code == FX_EOF {
                    break;
                }
                let _ = self.close(&handle);
                return Err(status_error(code, &r.string().unwrap_or_default()));
            }
            if kind != NAME {
                break;
            }
            let mut r = In(&body);
            for _ in 0..r.u32()? {
                let name = r.string()?;
                r.bytes()?;
                let attrs = r.attrs()?;
                if name != "." && name != ".." {
                    entries.push(Entry { name, attrs });
                }
            }
        }
        let _ = self.close(&handle);
        // A link to a folder is shown as the folder it leads to.
        for entry in &mut entries {
            if entry.attrs.is_link() {
                if let Ok(target) = self.stat(&join(path, &entry.name)) {
                    if target.is_dir() {
                        entry.attrs.permissions = target.permissions;
                    }
                }
            }
        }
        Ok(entries)
    }

    pub fn stat(&mut self, path: &str) -> Result<Attrs> {
        self.stat_kind(STAT, path)
    }

    pub fn lstat(&mut self, path: &str) -> Result<Attrs> {
        self.stat_kind(LSTAT, path)
    }

    fn stat_kind(&mut self, kind: u8, path: &str) -> Result<Attrs> {
        let id = self.id();
        let mut p = Out::new(kind, id);
        p.str(path);
        let (answer, body) = self.call(p.finish())?;
        if answer == ATTRS {
            In(&body).attrs()
        } else {
            Err(check_status(answer, &body).err().unwrap_or_default())
        }
    }

    pub fn mkdir(&mut self, path: &str) -> Result<()> {
        let id = self.id();
        let mut p = Out::new(MKDIR, id);
        p.str(path).attrs(&Attrs::default());
        self.expect_ok(p.finish())
    }

    pub fn rmdir(&mut self, path: &str) -> Result<()> {
        let id = self.id();
        let mut p = Out::new(RMDIR, id);
        p.str(path);
        self.expect_ok(p.finish())
    }

    pub fn remove(&mut self, path: &str) -> Result<()> {
        let id = self.id();
        let mut p = Out::new(REMOVE, id);
        p.str(path);
        self.expect_ok(p.finish())
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let id = self.id();
        let mut p = Out::new(RENAME, id);
        p.str(from).str(to);
        self.expect_ok(p.finish())
    }

    /// Sets a file's modification time, so a copy keeps its original's.
    pub fn set_mtime(&mut self, path: &str, mtime: u32) -> Result<()> {
        let id = self.id();
        let mut p = Out::new(SETSTAT, id);
        let attrs = Attrs {
            mtime: Some(mtime),
            atime: Some(mtime),
            ..Attrs::default()
        };
        p.str(path).attrs(&attrs);
        self.expect_ok(p.finish())
    }

    /// Deletes a file, or a folder with everything in it.
    pub fn remove_all(&mut self, path: &str) -> Result<()> {
        let attrs = self.lstat(path)?;
        if !attrs.is_dir() {
            return self.remove(path);
        }
        for entry in self.list(path)? {
            let child = join(path, &entry.name);
            if entry.attrs.is_dir() && !self.lstat(&child)?.is_link() {
                self.remove_all(&child)?;
            } else {
                self.remove(&child)?;
            }
        }
        self.rmdir(path)
    }

    /// Reads a whole file into `out`, many chunks in flight at once. `progress` gets the
    /// bytes done so far and returns false to stop.
    pub fn download(
        &mut self,
        path: &str,
        out: &mut (impl Write + ?Sized),
        mut progress: impl FnMut(u64) -> bool,
    ) -> Result<()> {
        let id = self.id();
        let mut p = Out::new(OPEN, id);
        p.str(path).u32(OPEN_READ).attrs(&Attrs::default());
        let handle = self.handle(p.finish())?;
        let result = (|| {
            let mut chunk = CHUNK as u64;
            let mut next_offset = 0u64;
            let mut pending: std::collections::VecDeque<u64> = Default::default();
            let mut written = 0u64;
            loop {
                while pending.len() < IN_FLIGHT {
                    let id = self.id();
                    let mut p = Out::new(READ, id);
                    p.bytes(&handle).u64(next_offset).u32(chunk as u32);
                    self.queue(p.finish())?;
                    pending.push_back(next_offset);
                    next_offset += chunk;
                }
                self.to.flush().map_err(|e| e.to_string())?;
                let offset = pending.pop_front().unwrap_or(written);
                let (kind, body) = self.receive()?;
                let body = body.get(4..).unwrap_or_default();
                match kind {
                    DATA => {
                        let data = In(body).bytes()?;
                        if offset != written {
                            return Err("The server answered out of order".into());
                        }
                        out.write_all(data).map_err(|e| e.to_string())?;
                        written += data.len() as u64;
                        if !progress(written) {
                            return Err("Stopped".into());
                        }
                        // A server may answer with less than was asked without being at the
                        // end. The reads after this one would then leave a gap, so their
                        // answers are dropped and reading goes on from here, in chunks the
                        // size the server gives.
                        if (data.len() as u64) < chunk {
                            for _ in 0..pending.len() {
                                self.receive()?;
                            }
                            pending.clear();
                            next_offset = written;
                            if !data.is_empty() {
                                chunk = data.len() as u64;
                            }
                        }
                    }
                    STATUS => {
                        let mut r = In(body);
                        let code = r.u32()?;
                        for _ in 0..pending.len() {
                            self.receive()?;
                        }
                        if code == FX_EOF {
                            return Ok(());
                        }
                        return Err(status_error(code, &r.string().unwrap_or_default()));
                    }
                    _ => return Err("Unexpected answer".into()),
                }
            }
        })();
        let _ = self.close(&handle);
        result
    }

    /// Writes a whole file from `input`, many chunks in flight at once.
    pub fn upload(
        &mut self,
        path: &str,
        input: &mut (impl Read + ?Sized),
        mut progress: impl FnMut(u64) -> bool,
    ) -> Result<()> {
        let id = self.id();
        let mut p = Out::new(OPEN, id);
        p.str(path)
            .u32(OPEN_WRITE | OPEN_CREATE | OPEN_TRUNCATE)
            .attrs(&Attrs::default());
        let handle = self.handle(p.finish())?;
        let result = (|| {
            let mut offset = 0u64;
            let mut pending = 0usize;
            let mut buffer = vec![0u8; CHUNK];
            let mut done = false;
            loop {
                while !done && pending < IN_FLIGHT {
                    let mut filled = 0;
                    while filled < CHUNK {
                        let n = input
                            .read(&mut buffer[filled..])
                            .map_err(|e| e.to_string())?;
                        if n == 0 {
                            break;
                        }
                        filled += n;
                    }
                    if filled == 0 {
                        done = true;
                        break;
                    }
                    let id = self.id();
                    let mut p = Out::new(WRITE, id);
                    p.bytes(&handle).u64(offset).bytes(&buffer[..filled]);
                    self.queue(p.finish())?;
                    offset += filled as u64;
                    pending += 1;
                    if filled < CHUNK {
                        done = true;
                    }
                }
                self.to.flush().map_err(|e| e.to_string())?;
                if pending == 0 {
                    break;
                }
                let (kind, body) = self.receive()?;
                pending -= 1;
                check_status(kind, body.get(4..).unwrap_or_default())?;
                if !progress(offset.saturating_sub((pending * CHUNK) as u64)) {
                    return Err("Stopped".into());
                }
            }
            Ok(())
        })();
        let closed = self.close(&handle);
        result.and(closed)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn check_status(kind: u8, body: &[u8]) -> Result<()> {
    if kind != STATUS {
        return Err("Unexpected answer".into());
    }
    let mut r = In(body);
    let code = r.u32()?;
    if code == FX_OK {
        Ok(())
    } else {
        Err(status_error(code, &r.string().unwrap_or_default()))
    }
}

/// A path on the server, joined with `/` whatever this computer uses.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The folder above a path on the server.
pub fn parent(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((up, _)) => up.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_round_trip() {
        let attrs = Attrs {
            size: Some(5),
            permissions: Some(0o040755),
            mtime: Some(1_700_000_000),
            atime: Some(1_700_000_000),
        };
        let mut p = Out::new(OPEN, 7);
        p.str("/srv/app").attrs(&attrs);
        let packet = p.finish();
        assert_eq!(
            u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize,
            packet.len() - 4
        );
        let mut r = In(&packet[5..]);
        assert_eq!(r.u32().unwrap(), 7);
        assert_eq!(r.string().unwrap(), "/srv/app");
        let back = r.attrs().unwrap();
        assert!(back.is_dir());
        assert_eq!(back.size, Some(5));
        assert_eq!(back.mtime, Some(1_700_000_000));
    }

    #[test]
    fn paths_on_the_server() {
        assert_eq!(join("/srv", "app"), "/srv/app");
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(parent("/srv/app"), "/srv");
        assert_eq!(parent("/srv"), "/");
        assert_eq!(parent("/"), "/");
    }
}
