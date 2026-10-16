//! How one file is copied once the queue has it: what happens when the destination is
//! already there (overwrite, skip, newer only, keep both), carrying on from where a stopped
//! transfer got to, large files over several connections at once, only the changed parts
//! of a file that is already there (as rsync does), and a checksum at both ends after.

use crate::sftp::{self, Session};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// What to do when a file being copied is already at its destination.
#[derive(Clone, Copy, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Conflict {
    #[default]
    Overwrite,
    Skip,
    /// Only when what is copied is newer than what is there.
    Newer,
    /// Both stay: the copy gets " (2)" in its name.
    KeepBoth,
}

impl Conflict {
    pub const ALL: [(Conflict, &'static str); 4] = [
        (Conflict::Overwrite, "Overwrite"),
        (Conflict::Newer, "Newer only"),
        (Conflict::Skip, "Skip"),
        (Conflict::KeepBoth, "Keep both"),
    ];
}

#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Options {
    pub conflict: Conflict,
    /// A SHA-256 of both ends after copying, where the server can make one.
    pub verify: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            conflict: Conflict::Overwrite,
            verify: true,
        }
    }
}

/// A file this large goes over several connections at once.
pub const PARALLEL_FROM: u64 = 64 * 1024 * 1024;
const PARTS: u64 = 4;
/// A file this large that is already there gets only its changed blocks.
pub const DELTA_FROM: u64 = 4 * 1024 * 1024;
const BLOCK: u64 = 1024 * 1024;
/// Smaller files aren't worth a checksum round trip.
pub const VERIFY_FROM: u64 = 256 * 1024;

pub type Step<'a> = &'a mut dyn FnMut(u64) -> bool;

/// What happened to a file.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Copied,
    /// Only the blocks that differed went, this many bytes.
    Delta(u64),
}

/// The size and modification time of what is at `path`, if anything is.
pub fn info(
    sessions: &mut HashMap<String, Session>,
    local: bool,
    host: &str,
    path: &str,
) -> Option<(u64, Option<u32>)> {
    if local {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as u32);
        Some((meta.len(), mtime))
    } else {
        let s = sessions.get_mut(host)?;
        let a = s.stat(path).ok()?;
        Some((a.size.unwrap_or(0), a.mtime))
    }
}

/// "name (2).ext", "name (3).ext"… the first that isn't taken.
pub fn free_name(path: &str, taken: &mut dyn FnMut(&str) -> bool) -> String {
    let (dir, file) = match path.rfind(['/', '\\']) {
        Some(at) => (&path[..=at], &path[at + 1..]),
        None => ("", path),
    };
    let (stem, ext) = match file.rfind('.') {
        Some(at) if at > 0 => (&file[..at], &file[at..]),
        _ => (file, ""),
    };
    (2..)
        .map(|n| format!("{dir}{stem} ({n}){ext}"))
        .find(|candidate| !taken(candidate))
        .unwrap()
}

// ---- checksums ----

pub fn local_sha256(path: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut f = File::open(path).ok()?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Some(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn remote_sha256(host: &str, path: &str) -> Option<String> {
    let out = sftp::run(host, &format!("sha256sum {}", sftp::quote(path)))?;
    let hash = out.split_whitespace().next()?.to_string();
    (hash.len() == 64).then_some(hash)
}

/// The MD5 of every 1 MB block of a file here.
fn local_blocks(path: &str) -> Option<Vec<String>> {
    use md5::{Digest, Md5};
    let mut f = File::open(path).ok()?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; BLOCK as usize];
    loop {
        let mut filled = 0;
        while filled < buf.len() {
            let n = f.read(&mut buf[filled..]).ok()?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        out.push(
            Md5::digest(&buf[..filled])
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        );
        if filled < buf.len() {
            break;
        }
    }
    Some(out)
}

/// The same for a file on the server, made there by perl (which nearly every Linux has),
/// so only the hashes cross the network.
fn remote_blocks(host: &str, path: &str) -> Option<Vec<String>> {
    let script = format!(
        "perl -MDigest::MD5=md5_hex -e 'open(F,\"<\",$ARGV[0]) or exit 1; binmode F; while(read(F,$b,{BLOCK})){{print md5_hex($b),\"\\n\"}}' {}",
        sftp::quote(path)
    );
    let out = sftp::run(host, &script)?;
    Some(
        out.lines()
            .map(str::to_string)
            .filter(|l| l.len() == 32)
            .collect(),
    )
}

fn changed_blocks(src: &[String], dest: &[String]) -> Vec<u64> {
    (0..src.len())
        .filter(|i| dest.get(*i) != Some(&src[*i]))
        .map(|i| i as u64)
        .collect()
}

// ---- writing a local file at a place in it ----

fn write_at(file: &File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        #[cfg(windows)]
        let n =
            std::os::windows::fs::FileExt::seek_write(file, &buf[done..], offset + done as u64)?;
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::write_at(file, &buf[done..], offset + done as u64)?;
        done += n;
    }
    Ok(())
}

struct At<'a> {
    file: &'a File,
    pos: u64,
}

impl Write for At<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        write_at(self.file, buf, self.pos)?;
        self.pos += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn set_mtime_local(path: &str, mtime: Option<u32>) {
    if let Some(m) = mtime {
        let when = std::time::UNIX_EPOCH + Duration::from_secs(m as u64);
        if let Ok(f) = File::options().write(true).open(path) {
            let _ = f.set_modified(when);
        }
    }
}

fn finish_local(part: &str, dest: &str, mtime: Option<u32>) -> Result<(), String> {
    let _ = std::fs::remove_file(dest);
    std::fs::rename(part, dest).map_err(|e| e.to_string())?;
    set_mtime_local(dest, mtime);
    Ok(())
}

// ---- server to here ----

/// Down from a server: the changed blocks of a file that is here already, several
/// connections for a large one, or one stream that carries on where it stopped.
pub fn download(
    s: &mut Session,
    host: &str,
    src: &str,
    size: u64,
    mtime: Option<u32>,
    dest: &str,
    existing: Option<u64>,
    step: Step,
) -> Result<Outcome, String> {
    let part = format!("{dest}.rdrop-part");
    if let Some(have) = existing.filter(|h| *h >= DELTA_FROM && size >= DELTA_FROM) {
        let _ = have;
        if let (Some(theirs), Some(ours)) = (remote_blocks(host, src), local_blocks(dest)) {
            let changed = changed_blocks(&theirs, &ours);
            std::fs::copy(dest, &part).map_err(|e| e.to_string())?;
            let file = File::options()
                .write(true)
                .open(&part)
                .map_err(|e| e.to_string())?;
            let total = changed.len() as u64 * BLOCK;
            let mut sent = 0u64;
            for block in &changed {
                let data = s.read_at(src, block * BLOCK, BLOCK)?;
                write_at(&file, &data, block * BLOCK).map_err(|e| e.to_string())?;
                sent += data.len() as u64;
                if !step(size * sent / total.max(1)) {
                    return Err("Stopped".into());
                }
            }
            file.set_len(size).map_err(|e| e.to_string())?;
            drop(file);
            finish_local(&part, dest, mtime)?;
            return Ok(Outcome::Delta(sent));
        }
    }
    let resume = std::fs::metadata(&part)
        .map(|m| m.len())
        .ok()
        .filter(|l| *l > 0 && *l < size);
    if size >= PARALLEL_FROM && resume.is_none() {
        let file = File::create(&part).map_err(|e| e.to_string())?;
        file.set_len(size).map_err(|e| e.to_string())?;
        parallel(host, size, step, &|session, start, len, progress| {
            let mut out = At {
                file: &file,
                pos: start,
            };
            session.download_range(src, start, len, &mut out, progress)
        })?;
        drop(file);
        finish_local(&part, dest, mtime)?;
        return Ok(Outcome::Copied);
    }
    let start = resume.unwrap_or(0);
    let file = if start > 0 {
        File::options().append(true).open(&part)
    } else {
        File::create(&part)
    }
    .map_err(|e| e.to_string())?;
    let mut out = std::io::BufWriter::with_capacity(256 * 1024, file);
    s.download_range(src, start, u64::MAX, &mut out, |n| step(start + n))?;
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    finish_local(&part, dest, mtime)?;
    Ok(Outcome::Copied)
}

// ---- here to a server ----

pub fn upload(
    s: &mut Session,
    host: &str,
    src: &str,
    size: u64,
    mtime: Option<u32>,
    dest: &str,
    existing: Option<u64>,
    step: Step,
) -> Result<Outcome, String> {
    let part = format!("{dest}.rdrop-part");
    if existing.is_some_and(|h| h >= DELTA_FROM) && size >= DELTA_FROM {
        if let (Some(ours), Some(theirs)) = (local_blocks(src), remote_blocks(host, dest)) {
            // The copy on the server is patched beside the real one, then put in its place.
            if sftp::run(
                host,
                &format!("cp -p {} {}", sftp::quote(dest), sftp::quote(&part)),
            )
            .is_some()
            {
                let changed = changed_blocks(&ours, &theirs);
                let total = changed.len() as u64 * BLOCK;
                let mut file = File::open(src).map_err(|e| e.to_string())?;
                let mut sent = 0u64;
                for block in &changed {
                    file.seek(SeekFrom::Start(block * BLOCK))
                        .map_err(|e| e.to_string())?;
                    let mut chunk = (&mut file).take(BLOCK);
                    let before = sent;
                    s.upload_at(&part, block * BLOCK, false, &mut chunk, |n| {
                        sent = before + n;
                        step(size * sent / total.max(1))
                    })?;
                    sent = before + BLOCK.min(size - block * BLOCK);
                }
                s.set_size(&part, size)?;
                if let Some(m) = mtime {
                    let _ = s.set_mtime(&part, m);
                }
                s.replace(&part, dest)?;
                return Ok(Outcome::Delta(sent));
            }
        }
    }
    let resume = s
        .stat(&part)
        .ok()
        .and_then(|a| a.size)
        .filter(|l| *l > 0 && *l < size);
    if size >= PARALLEL_FROM && resume.is_none() {
        s.upload_at(&part, 0, true, &mut std::io::empty(), |_| true)?;
        parallel(host, size, step, &|session, start, len, progress| {
            let mut file = File::open(src).map_err(|e| e.to_string())?;
            file.seek(SeekFrom::Start(start))
                .map_err(|e| e.to_string())?;
            session.upload_at(&part, start, false, &mut file.take(len), progress)
        })?;
    } else {
        let start = resume.unwrap_or(0);
        let mut file = File::open(src).map_err(|e| format!("{src}: {e}"))?;
        file.seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        s.upload_at(&part, start, start == 0, &mut file, |n| step(start + n))?;
    }
    if let Some(m) = mtime {
        let _ = s.set_mtime(&part, m);
    }
    s.replace(&part, dest)?;
    Ok(Outcome::Copied)
}

/// A large file in `PARTS` pieces, each over a connection of its own, at the same time.
/// `step` hears how far all of them are together.
type Piece<'a> =
    &'a (dyn Fn(&mut Session, u64, u64, &mut dyn FnMut(u64) -> bool) -> Result<(), String> + Sync);

fn parallel(host: &str, size: u64, step: Step, piece: Piece) -> Result<(), String> {
    let done = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let part_len = size.div_ceil(PARTS);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..PARTS)
            .map(|i| {
                let (done, stop) = (&done, &stop);
                scope.spawn(move || -> Result<(), String> {
                    let start = i * part_len;
                    if start >= size {
                        return Ok(());
                    }
                    let len = part_len.min(size - start);
                    let mut session = Session::connect(host)?;
                    let mut last = 0u64;
                    piece(&mut session, start, len, &mut |n| {
                        done.fetch_add(n.saturating_sub(last), Ordering::Relaxed);
                        last = n;
                        !stop.load(Ordering::Relaxed)
                    })
                })
            })
            .collect();
        while handles.iter().any(|h| !h.is_finished()) {
            if !step(done.load(Ordering::Relaxed)) {
                stop.store(true, Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut first_error = None;
        for h in handles {
            if let Err(e) = h
                .join()
                .unwrap_or_else(|_| Err("A part of the transfer failed".into()))
            {
                first_error.get_or_insert(e);
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => {
                step(size);
                Ok(())
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_names_count_up() {
        let taken = ["a/report.pdf", "a/report (2).pdf"];
        let mut is = |p: &str| taken.contains(&p);
        assert_eq!(free_name("a/report.pdf", &mut is), "a/report (3).pdf");
        assert_eq!(
            free_name("C:\\x\\notes", &mut |_| false),
            "C:\\x\\notes (2)"
        );
        assert_eq!(free_name(".env", &mut |_| false), ".env (2)");
    }

    #[test]
    fn only_changed_blocks_go() {
        let a: Vec<String> = ["1", "2", "3", "4"].iter().map(|s| s.to_string()).collect();
        let b: Vec<String> = ["1", "x", "3"].iter().map(|s| s.to_string()).collect();
        assert_eq!(changed_blocks(&a, &b), [1, 3]);
    }

    /// RDROP_TEST_HOST=vespucci cargo test -p rdrop -- --ignored engine
    #[test]
    #[ignore]
    fn engine_on_a_real_server() {
        let Ok(host) = std::env::var("RDROP_TEST_HOST") else {
            return;
        };
        let id = std::process::id();
        let dir = format!("/tmp/rdrop-test-engine-{id}");
        let local = std::env::temp_dir().join(format!("rdrop-engine-{id}"));
        std::fs::create_dir_all(&local).unwrap();
        let path = |name: &str| local.join(name).display().to_string();
        let mut s = Session::connect(&host).unwrap();
        s.mkdir(&dir).unwrap();
        let same = |a: &str, b: &str| {
            assert_eq!(
                local_sha256(a),
                remote_sha256(&host, b),
                "{a} and {b} differ"
            );
        };

        // Large: over four connections at once.
        let size = PARALLEL_FROM + 3 * BLOCK + 12_345;
        let mut data: Vec<u8> = (0..size)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        std::fs::write(path("big.bin"), &data).unwrap();
        let remote = format!("{dir}/big.bin");
        let t = std::time::Instant::now();
        let out = upload(
            &mut s,
            &host,
            &path("big.bin"),
            size,
            Some(1_600_000_000),
            &remote,
            None,
            &mut |_| true,
        )
        .unwrap();
        eprintln!("parallel upload of {} MB: {:?}", size >> 20, t.elapsed());
        assert_eq!(out, Outcome::Copied);
        same(&path("big.bin"), &remote);
        assert_eq!(s.stat(&remote).unwrap().mtime, Some(1_600_000_000));

        // Two blocks changed: only they go up.
        data[5 * BLOCK as usize + 3] ^= 0xff;
        data[40 * BLOCK as usize] ^= 0xff;
        std::fs::write(path("big.bin"), &data).unwrap();
        let out = upload(
            &mut s,
            &host,
            &path("big.bin"),
            size,
            None,
            &remote,
            Some(size),
            &mut |_| true,
        )
        .unwrap();
        assert_eq!(out, Outcome::Delta(2 * BLOCK));
        same(&path("big.bin"), &remote);

        // Down in parallel, then down again as a delta over an older copy.
        let t = std::time::Instant::now();
        let out = download(
            &mut s,
            &host,
            &remote,
            size,
            None,
            &path("down.bin"),
            None,
            &mut |_| true,
        )
        .unwrap();
        eprintln!("parallel download of {} MB: {:?}", size >> 20, t.elapsed());
        assert_eq!(out, Outcome::Copied);
        same(&path("down.bin"), &remote);
        let mut old = data.clone();
        old[7 * BLOCK as usize] ^= 1;
        old.truncate(old.len() - 5000);
        std::fs::write(path("down.bin"), &old).unwrap();
        let out = download(
            &mut s,
            &host,
            &remote,
            size,
            None,
            &path("down.bin"),
            Some(old.len() as u64),
            &mut |_| true,
        )
        .unwrap();
        assert!(
            matches!(out, Outcome::Delta(n) if n <= 2 * BLOCK),
            "{out:?}"
        );
        same(&path("down.bin"), &remote);

        // A download that stopped carries on from its part.
        let small = 9 * BLOCK + 77;
        let piece = &data[..small as usize];
        std::fs::write(path("piece.bin"), piece).unwrap();
        let rpiece = format!("{dir}/piece.bin");
        s.upload(&rpiece, &mut &piece[..], |_| true).unwrap();
        std::fs::write(path("got.bin.rdrop-part"), &piece[..3 * BLOCK as usize]).unwrap();
        let mut first = None;
        download(
            &mut s,
            &host,
            &rpiece,
            small,
            None,
            &path("got.bin"),
            None,
            &mut |n| {
                first.get_or_insert(n);
                true
            },
        )
        .unwrap();
        assert!(first.unwrap() >= 3 * BLOCK, "it started over");
        assert_eq!(std::fs::read(path("got.bin")).unwrap(), piece);

        // And one up as well.
        let rgot = format!("{dir}/up.bin");
        s.upload(
            &format!("{rgot}.rdrop-part"),
            &mut &piece[..2 * BLOCK as usize],
            |_| true,
        )
        .unwrap();
        let mut first = None;
        upload(
            &mut s,
            &host,
            &path("piece.bin"),
            small,
            None,
            &rgot,
            None,
            &mut |n| {
                first.get_or_insert(n);
                true
            },
        )
        .unwrap();
        assert!(first.unwrap() >= 2 * BLOCK, "it started over");
        same(&path("piece.bin"), &rgot);
        assert!(s.stat(&format!("{rgot}.rdrop-part")).is_err());

        s.remove_all(&dir).unwrap();
        let _ = std::fs::remove_dir_all(local);
    }

    #[test]
    fn blocks_of_a_file_here() {
        let path = std::env::temp_dir().join(format!("rdrop-blocks-{}", std::process::id()));
        std::fs::write(&path, vec![7u8; (BLOCK * 2 + 10) as usize]).unwrap();
        let blocks = local_blocks(&path.display().to_string()).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], blocks[1]);
        let _ = std::fs::remove_file(path);
    }
}
