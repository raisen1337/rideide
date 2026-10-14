//! Wire protocol between the desktop app and the agent running on the remote host.
//!
//! Frames are `[u32 little-endian length][postcard payload]` over the agent's stdin/stdout.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Written by the agent before the first frame, so the client can skip anything
/// the remote shell's startup files printed.
pub const MAGIC: &[u8] = b"\0RIDE-AGENT-READY\0";

const MAX_FRAME: usize = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SearchHit {
    pub path: String,
    /// 0-based line and char column of the match start.
    pub line: u32,
    pub col: u32,
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Request {
    Hello,
    ListDir {
        path: String,
    },
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        data: Vec<u8>,
    },
    /// Creates an empty file; fails if the path already exists.
    CreateFile {
        path: String,
    },
    CreateDir {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Delete {
        path: String,
    },
    /// Streams `SearchHits` and ends with `Done`.
    Search {
        root: String,
        query: String,
        regex: bool,
        case_sensitive: bool,
    },
    /// Streams `DownloadInfo`, then `Chunk`s, and ends with `Done`.
    /// A file is sent raw, a directory as a gzipped tar.
    Download {
        path: String,
    },
    /// Lists files under `root` whose path contains every word of `query`.
    FindFiles {
        root: String,
        query: String,
    },
    /// Stops the streaming request with id `target`.
    Cancel {
        target: u64,
    },
    /// Opens a shell in a pty. The id of this request becomes the terminal id.
    TermOpen {
        cols: u16,
        rows: u16,
        cwd: String,
    },
    TermInput {
        term: u64,
        data: Vec<u8>,
    },
    TermResize {
        term: u64,
        cols: u16,
        rows: u16,
    },
    TermClose {
        term: u64,
    },
    /// Runs a shell command with piped stdio. The id of this request becomes the process id.
    ProcOpen {
        command: String,
        cwd: String,
    },
    /// Writes to the process's input; empty data closes it.
    ProcInput {
        proc: u64,
        data: Vec<u8>,
    },
    ProcClose {
        proc: u64,
    },
    /// The app's answer to a `HostAsk`.
    HostAnswer {
        token: u64,
        text: String,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Response {
    Hello {
        home: String,
    },
    Dir {
        entries: Vec<DirEntry>,
    },
    File {
        data: Vec<u8>,
    },
    Ok,
    SearchHits {
        hits: Vec<SearchHit>,
    },
    Paths {
        paths: Vec<String>,
    },
    DownloadInfo {
        is_dir: bool,
        name: String,
    },
    Chunk {
        data: Vec<u8>,
    },
    Done,
    TermOutput {
        data: Vec<u8>,
    },
    TermExit,
    ProcOutput {
        data: Vec<u8>,
    },
    ProcStderr {
        data: Vec<u8>,
    },
    ProcExit {
        code: Option<i32>,
    },
    Error {
        message: String,
    },
    /// A program on the host asks the app for something. Sent with id 0, on the agent's
    /// own initiative; `text` is one line of JSON and `token` names the asker.
    HostAsk {
        token: u64,
        text: String,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ClientMsg {
    pub id: u64,
    pub req: Request,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServerMsg {
    pub id: u64,
    pub resp: Response,
}

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let bytes = postcard::to_allocvec(msg).map_err(io::Error::other)?;
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Returns `Ok(None)` on a clean end of stream.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::other(format!("frame too large: {len} bytes")));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    postcard::from_bytes(&buf)
        .map(Some)
        .map_err(io::Error::other)
}
