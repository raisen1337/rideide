//! Watching a folder here and sending what changes in it to a folder on a server: saved
//! files go up a moment after they are written (once they stop changing), as a sync job in
//! the queue. Nothing is deleted on the server. Watches are kept across restarts.

use super::*;
use std::collections::BTreeMap;
use std::sync::mpsc::channel;

/// How often the folder is looked at, in seconds.
const EVERY: f64 = 1.5;

/// Paths that are never sent: tools' own folders and files being written.
fn ignored(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.split('/')
        .any(|part| matches!(part, ".git" | "node_modules" | "target" | ".idea" | ".vs"))
        || name.ends_with(".rdrop-part")
        || name.ends_with('~')
        || name.ends_with(".tmp")
        || name.ends_with(".swp")
        || name.starts_with(".#")
        || name.starts_with("~$")
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct WatchSpec {
    pub local: String,
    pub host: String,
    pub remote: String,
}

pub(super) struct Watch {
    pub spec: WatchSpec,
    /// What the folder held at the last look; empty until the first.
    seen: Option<BTreeMap<String, compare::Meta>>,
    /// Files that changed at the last look, sent once they stay the same for one more.
    settling: BTreeMap<String, compare::Meta>,
    rx: Option<std::sync::mpsc::Receiver<Result<BTreeMap<String, compare::Meta>, String>>>,
    at: f64,
    pub sent: usize,
    pub last: Option<String>,
    pub error: Option<String>,
}

impl Watch {
    pub fn new(spec: WatchSpec) -> Watch {
        Watch {
            spec,
            seen: None,
            settling: BTreeMap::new(),
            rx: None,
            at: f64::MIN,
            sent: 0,
            last: None,
            error: None,
        }
    }

    pub fn label(&self) -> String {
        let name = self
            .spec
            .local
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&self.spec.local);
        format!(
            "{name}  →  {}",
            Place::Server(self.spec.host.clone()).label()
        )
    }

    /// Looks again when it is time; gives the files to send now.
    fn tick(&mut self, ctx: &Context, now: f64) -> Vec<String> {
        if let Some(rx) = &self.rx {
            let Ok(result) = rx.try_recv() else {
                return Vec::new();
            };
            self.rx = None;
            let now_seen = match result {
                Ok(m) => {
                    self.error = None;
                    m
                }
                Err(e) => {
                    self.error = Some(e);
                    return Vec::new();
                }
            };
            let mut send = Vec::new();
            if let Some(before) = &self.seen {
                // Settled: the same as when it was seen changing.
                for (path, meta) in std::mem::take(&mut self.settling) {
                    match now_seen.get(&path) {
                        Some(m) if *m == meta => send.push(path),
                        Some(m) => {
                            self.settling.insert(path, *m);
                        }
                        None => {}
                    }
                }
                for (path, meta) in &now_seen {
                    if before.get(path) != Some(meta) && !ignored(path) && !send.contains(path) {
                        self.settling.insert(path.clone(), *meta);
                    }
                }
            }
            self.seen = Some(now_seen);
            return send;
        }
        if now - self.at >= EVERY {
            self.at = now;
            let (tx, rx) = channel();
            let root = self.spec.local.clone();
            let repaint = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(compare::listing(&Place::Local, &root));
                repaint.request_repaint();
            });
            self.rx = Some(rx);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(EVERY));
        Vec::new()
    }
}

impl App {
    pub(super) fn start_watch(&mut self, id: u64) {
        let Some(other) = self.other_pane(id) else {
            return;
        };
        let (Some(here), Some(there)) = (self.pane(id), self.pane(other)) else {
            return;
        };
        let (Some(Place::Local), Some(Place::Server(host))) = (&here.place, &there.place) else {
            self.status =
                "Watching needs a folder here in this pane and a server in the other".into();
            return;
        };
        if here.path.is_empty() || there.path.is_empty() {
            return;
        }
        let spec = WatchSpec {
            local: here.path.clone(),
            host: host.clone(),
            remote: there.path.clone(),
        };
        if self.watches.iter().any(|w| w.spec == spec) {
            return;
        }
        self.note = format!(
            "Watching {}: what changes there goes up to {}",
            spec.local, spec.remote
        );
        self.watches.push(Watch::new(spec));
        self.save();
    }

    pub(super) fn watch_tick(&mut self) {
        let ctx = self.ctx.clone();
        let now = self.now;
        for i in 0..self.watches.len() {
            let send = self.watches[i].tick(&ctx, now);
            if send.is_empty() {
                continue;
            }
            let w = &mut self.watches[i];
            w.sent += send.len();
            w.last = Some(if send.len() == 1 {
                send[0].clone()
            } else {
                format!("{} files", send.len())
            });
            let spec = w.spec.clone();
            let name = format!("Watch: {}", w.last.clone().unwrap_or_default());
            self.register(&spec.host);
            self.queue.add_sync(
                Place::Local,
                spec.local,
                Place::Server(spec.host),
                spec.remote,
                send,
                Vec::new(),
                name,
            );
        }
    }

    /// The watches, in the sidebar, each with a way to stop it.
    pub(super) fn watches_ui(&mut self, ui: &mut Ui) {
        if self.watches.is_empty() {
            return;
        }
        ui.add_space(10.0);
        ui.label(
            RichText::new("WATCHING")
                .size(11.5)
                .strong()
                .color(theme::COMMENT()),
        );
        ui.add_space(4.0);
        let mut stop = None;
        for (i, w) in self.watches.iter().enumerate() {
            Frame::NONE
                .fill(theme::tint(6))
                .corner_radius(8)
                .inner_margin(Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        let color = if w.error.is_some() { BAD } else { GOOD };
                        ui.label(RichText::new("●").size(9.0).color(color));
                        ui.add(
                            egui::Label::new(
                                RichText::new(w.label()).size(12.0).color(theme::TEXT()),
                            )
                            .truncate(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .small_button(icons::CLOSE.to_string())
                                .on_hover_text("Stop watching")
                                .clicked()
                            {
                                stop = Some(i);
                            }
                        });
                    });
                    let detail = match (&w.error, &w.last) {
                        (Some(e), _) => e.clone(),
                        (None, Some(last)) => format!("{} sent  ·  last {last}", w.sent),
                        (None, None) => format!("into {}", w.spec.remote),
                    };
                    ui.add(
                        egui::Label::new(RichText::new(detail).size(11.0).color(theme::COMMENT()))
                            .truncate(),
                    )
                    .on_hover_text(format!(
                        "{}\n→ {}:{}",
                        w.spec.local, w.spec.host, w.spec.remote
                    ));
                });
            ui.add_space(4.0);
        }
        if let Some(i) = stop {
            self.watches.remove(i);
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_files_are_left_alone() {
        assert!(ignored(".git/index"));
        assert!(ignored("web/node_modules/x/index.js"));
        assert!(ignored("notes.txt~"));
        assert!(ignored("big.iso.rdrop-part"));
        assert!(!ignored("src/main.rs"));
        assert!(!ignored(".env"));
    }
}
