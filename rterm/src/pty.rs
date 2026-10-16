//! A shell running in a pseudo console (ConPTY on Windows), with its output read on a
//! thread of its own.

use crate::profiles::Profile;
use eframe::egui;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

pub struct Pty {
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Box<dyn Write + Send>,
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
    /// A console Windows handed over: where resizes go, and what keeps it alive.
    #[allow(dead_code)]
    handed: Option<crate::handoff::Handoff>,
    pub output: Receiver<Vec<u8>>,
    /// The exit code, once the program has ended.
    exit: Arc<Mutex<Option<u32>>>,
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl Pty {
    pub fn spawn(
        profile: &Profile,
        cwd: &Path,
        (cols, rows): (u16, u16),
        ctx: &egui::Context,
    ) -> Result<Self, String> {
        let pair = native_pty_system()
            .openpty(size(cols, rows))
            .map_err(|e| e.to_string())?;
        let mut cmd = CommandBuilder::new(&profile.program);
        cmd.args(&profile.args);
        if cwd.is_dir() {
            cmd.cwd(cwd);
        }
        // The user's own environment, fresh from Windows, as Windows Terminal gives each
        // tab: not whatever started rterm, which may have been a tool with settings of its
        // own (Claude Code's NO_COLOR turned the colours off in every tab).
        if let Some(vars) = crate::userenv::fresh() {
            cmd.env_clear();
            for (name, value) in vars {
                cmd.env(name, value);
            }
        }
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "rterm");
        // Programs treat a terminal with WT_SESSION as Windows Terminal: colours, Unicode
        // symbols and modern sequences, all of which rterm draws as it does.
        cmd.env("WT_SESSION", session_id());
        for (key, value) in &profile.env {
            cmd.env(key, value);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("{}: {e}", profile.program.display()))?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

        let (tx, output) = channel();
        let repaint = ctx.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
        });
        // A pseudo console keeps its output open after the program ends, so the end is
        // learned by waiting on the program instead.
        let exit = Arc::new(Mutex::new(None));
        let ended = exit.clone();
        let repaint = ctx.clone();
        thread::spawn(move || {
            let code = child.wait().map_or(1, |status| status.exit_code());
            *ended.lock().unwrap() = Some(code);
            repaint.request_repaint();
        });
        Ok(Self {
            master: Some(pair.master),
            writer,
            killer: Some(killer),
            handed: None,
            output,
            exit,
        })
    }

    /// A console program Windows handed to rterm (see `handoff`), over its pipes.
    pub fn from_handoff(mut handoff: crate::handoff::Handoff, ctx: &egui::Context) -> Self {
        use std::os::windows::io::AsRawHandle;
        let writer = handoff
            .input
            .try_clone()
            .map(|f| Box::new(f) as Box<dyn Write + Send>)
            .unwrap_or_else(|_| Box::new(std::io::sink()));
        let mut reader = handoff.output.try_clone().ok();
        let (tx, output) = channel();
        let repaint = ctx.clone();
        thread::spawn(move || {
            let Some(reader) = reader.as_mut() else {
                return;
            };
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
        });
        let exit = Arc::new(Mutex::new(None));
        let ended = exit.clone();
        let repaint = ctx.clone();
        let client = handoff.client.as_raw_handle() as isize;
        // The handle stays open as long as the Pty holds the handoff; the thread only waits.
        let client = client as usize;
        thread::spawn(move || {
            let code = wait_for(client as isize);
            *ended.lock().unwrap() = Some(code);
            repaint.request_repaint();
        });
        handoff.title = handoff.title.trim().to_string();
        Self {
            master: None,
            writer,
            killer: None,
            handed: Some(handoff),
            output,
            exit,
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Some(master) = &self.master {
            let _ = master.resize(size(cols, rows));
        }
        // A handed-over console takes its size as Windows' pseudo consoles do: a resize
        // signal of three 16-bit numbers on its signal pipe.
        if let Some(h) = &self.handed {
            const PTY_SIGNAL_RESIZE_WINDOW: u16 = 8;
            let mut packet = Vec::with_capacity(6);
            for v in [PTY_SIGNAL_RESIZE_WINDOW, cols, rows] {
                packet.extend_from_slice(&v.to_le_bytes());
            }
            let _ = (&h.signal).write_all(&packet);
        }
    }

    pub fn exit_code(&self) -> Option<u32> {
        *self.exit.lock().unwrap()
    }
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

impl Drop for Pty {
    fn drop(&mut self) {
        if let Some(killer) = &mut self.killer {
            let _ = killer.kill();
        }
        // Letting go of a handed-over console's signal, reference and host closes it,
        // and Windows ends the program as it does when a console window closes.
    }
}

/// Waits for a process to end and gives its exit code.
fn wait_for(process: isize) -> u32 {
    #[link(name = "kernel32")]
    extern "system" {
        fn WaitForSingleObject(handle: isize, ms: u32) -> u32;
        fn GetExitCodeProcess(process: isize, code: *mut u32) -> i32;
    }
    let mut code = 1u32;
    unsafe {
        WaitForSingleObject(process, u32::MAX);
        GetExitCodeProcess(process, &mut code);
    }
    code
}
