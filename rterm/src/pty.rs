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
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
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
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "rterm");
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
            master: pair.master,
            writer,
            killer,
            output,
            exit,
        })
    }

    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(size(cols, rows));
    }

    pub fn exit_code(&self) -> Option<u32> {
        *self.exit.lock().unwrap()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}
