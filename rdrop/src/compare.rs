//! Comparing two folders, here or on servers, file by file (size and modification time),
//! and the sync that follows: what would be copied which way, and what deleted, shown
//! before anything happens, each line one that can be left out.

use super::*;
use std::collections::BTreeMap;
use std::sync::mpsc::channel;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Meta {
    pub size: u64,
    pub mtime: u64,
}

/// How a file differs between the two sides.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Kind {
    OnlyLeft,
    OnlyRight,
    LeftNewer,
    RightNewer,
    /// The same time, another size: no side can be said to be newer.
    Differs,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Direction {
    ToRight,
    ToLeft,
    Both,
}

/// What the sync does to one file.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Action {
    ToRight,
    ToLeft,
    DeleteRight,
    DeleteLeft,
}

pub(super) struct Row {
    pub path: String,
    pub kind: Kind,
    pub left: Option<Meta>,
    pub right: Option<Meta>,
}

/// Times this close are the same: FAT keeps two seconds, some copies round.
const SLACK: u64 = 2;

pub(super) fn diff(
    left: &BTreeMap<String, Meta>,
    right: &BTreeMap<String, Meta>,
) -> (Vec<Row>, usize) {
    let mut rows = Vec::new();
    let mut same = 0;
    for (path, l) in left {
        let kind = match right.get(path) {
            None => Some(Kind::OnlyLeft),
            Some(r) if l.mtime.abs_diff(r.mtime) <= SLACK => {
                (l.size != r.size).then_some(Kind::Differs)
            }
            Some(r) if l.mtime > r.mtime => Some(Kind::LeftNewer),
            Some(_) => Some(Kind::RightNewer),
        };
        match kind {
            Some(kind) => rows.push(Row {
                path: path.clone(),
                kind,
                left: Some(*l),
                right: right.get(path).copied(),
            }),
            None => same += 1,
        }
    }
    for (path, r) in right {
        if !left.contains_key(path) {
            rows.push(Row {
                path: path.clone(),
                kind: Kind::OnlyRight,
                left: None,
                right: Some(*r),
            });
        }
    }
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    (rows, same)
}

/// What a row turns into for a direction, and whether it is picked to begin with: a
/// newer file is never replaced by an older one unless asked for.
pub(super) fn plan(kind: Kind, dir: Direction, delete: bool) -> Option<(Action, bool)> {
    use Kind::*;
    match (dir, kind) {
        (Direction::ToRight, OnlyLeft | LeftNewer | Differs) => Some((Action::ToRight, true)),
        (Direction::ToRight, RightNewer) => Some((Action::ToRight, false)),
        (Direction::ToRight, OnlyRight) => delete.then_some((Action::DeleteRight, true)),
        (Direction::ToLeft, OnlyRight | RightNewer | Differs) => Some((Action::ToLeft, true)),
        (Direction::ToLeft, LeftNewer) => Some((Action::ToLeft, false)),
        (Direction::ToLeft, OnlyLeft) => delete.then_some((Action::DeleteLeft, true)),
        (Direction::Both, OnlyLeft | LeftNewer) => Some((Action::ToRight, true)),
        (Direction::Both, OnlyRight | RightNewer) => Some((Action::ToLeft, true)),
        (Direction::Both, Differs) => Some((Action::ToRight, false)),
    }
}

fn reason(kind: Kind) -> &'static str {
    match kind {
        Kind::OnlyLeft => "only on the left",
        Kind::OnlyRight => "only on the right",
        Kind::LeftNewer => "newer on the left",
        Kind::RightNewer => "newer on the right",
        Kind::Differs => "same time, other size",
    }
}

// ---- listing a whole folder ----

pub(super) fn listing(place: &Place, root: &str) -> Result<BTreeMap<String, Meta>, String> {
    let mut out = BTreeMap::new();
    match place {
        Place::Local => {
            let mut stack = vec![(PathBuf::from(root), String::new())];
            while let Some((dir, rel)) = stack.pop() {
                let entries =
                    std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                for entry in entries.flatten() {
                    let Ok(meta) = entry.metadata() else { continue };
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let relative = if rel.is_empty() {
                        name
                    } else {
                        format!("{rel}/{name}")
                    };
                    if entry.file_type().is_ok_and(|t| t.is_symlink()) {
                        continue;
                    }
                    if meta.is_dir() {
                        stack.push((entry.path(), relative));
                    } else {
                        let mtime = meta
                            .modified()
                            .ok()
                            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |d| d.as_secs());
                        out.insert(
                            relative,
                            Meta {
                                size: meta.len(),
                                mtime,
                            },
                        );
                    }
                }
            }
        }
        Place::Server(host) if bulk::can(host) => {
            let script = format!(
                "cd {} || exit 3; find . -mindepth 1 -type f -printf '%P\\t%s\\t%T@\\n' 2>/dev/null; true",
                sftp::quote(root)
            );
            let text =
                sftp::run(host, &script).ok_or_else(|| format!("{root} couldn't be listed"))?;
            for line in text.lines() {
                let mut parts = line.split('\t');
                let (Some(path), Some(size), Some(time)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                let (Ok(size), Some(Ok(mtime))) =
                    (size.parse(), time.split('.').next().map(str::parse))
                else {
                    continue;
                };
                out.insert(path.to_string(), Meta { size, mtime });
            }
        }
        Place::Server(host) => {
            let mut s = sftp::Session::connect(host)?;
            let mut stack = vec![String::new()];
            while let Some(rel) = stack.pop() {
                let dir = if rel.is_empty() {
                    root.to_string()
                } else {
                    sftp::join(root, &rel)
                };
                for e in s.list(&dir)? {
                    let relative = if rel.is_empty() {
                        e.name.clone()
                    } else {
                        format!("{rel}/{}", e.name)
                    };
                    if e.link {
                        continue;
                    }
                    if e.attrs.is_dir() {
                        stack.push(relative);
                    } else {
                        out.insert(
                            relative,
                            Meta {
                                size: e.attrs.size.unwrap_or(0),
                                mtime: e.attrs.mtime.map_or(0, u64::from),
                            },
                        );
                    }
                }
            }
        }
    }
    Ok(out)
}

// ---- the dialog ----

pub(super) struct Side {
    pub place: Place,
    pub root: String,
}

impl Side {
    /// A card with where the side is: the place above, the folder below.
    fn card(&self, ui: &mut Ui, width: f32) {
        Frame::NONE
            .fill(theme::tint(6))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(width - 24.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                let icon = match self.place {
                    Place::Local => ui::DESKTOP,
                    Place::Server(_) => icons::REMOTE,
                };
                ui.label(
                    RichText::new(format!("{icon}  {}", self.place.label()))
                        .size(12.0)
                        .color(theme::COMMENT()),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(&self.root)
                            .size(12.5)
                            .monospace()
                            .color(theme::TEXT()),
                    )
                    .truncate(),
                )
                .on_hover_text(&self.root);
            });
    }
    fn short(&self) -> String {
        let name = self
            .root
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("");
        if name.is_empty() {
            self.root.clone()
        } else {
            name.to_string()
        }
    }
}

type Listed = Result<(Vec<Row>, usize), String>;

pub(super) struct Compare {
    pub left: Side,
    pub right: Side,
    rx: Option<std::sync::mpsc::Receiver<Listed>>,
    rows: Vec<Row>,
    same: usize,
    error: Option<String>,
    direction: Direction,
    delete: bool,
    /// Lines whose tick was changed from what the plan picks.
    flipped: HashSet<String>,
    started: f64,
}

impl Compare {
    pub fn start(ctx: &Context, left: Side, right: Side, now: f64) -> Compare {
        let (tx, rx) = channel();
        let (lp, lr, rp, rr, repaint) = (
            left.place.clone(),
            left.root.clone(),
            right.place.clone(),
            right.root.clone(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            // Both sides at once: each may be a server a long way off.
            let other = std::thread::spawn(move || listing(&rp, &rr));
            let result = listing(&lp, &lr).and_then(|l| {
                let r = other
                    .join()
                    .unwrap_or_else(|_| Err("Listing failed".into()))?;
                Ok(diff(&l, &r))
            });
            let _ = tx.send(result);
            repaint.request_repaint();
        });
        Compare {
            left,
            right,
            rx: Some(rx),
            rows: Vec::new(),
            same: 0,
            error: None,
            direction: Direction::ToRight,
            delete: false,
            flipped: HashSet::new(),
            started: now,
        }
    }

    /// Each line's action and whether it is ticked, for the direction chosen.
    fn planned(&self) -> Vec<(usize, Action, bool)> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let (action, on) = plan(r.kind, self.direction, self.delete)?;
                Some((i, action, on ^ self.flipped.contains(&r.path)))
            })
            .collect()
    }
}

impl App {
    pub(super) fn compare_panes(&mut self, id: u64) {
        let Some(other) = self.other_pane(id) else {
            self.status = "Open a second pane to compare with".into();
            return;
        };
        // Left and right as they are on the screen.
        let order = self.layout.panes();
        let (a, b) =
            if order.iter().position(|p| *p == id) <= order.iter().position(|p| *p == other) {
                (id, other)
            } else {
                (other, id)
            };
        let side = |app: &App, id: u64| {
            app.pane(id)
                .filter(|p| p.ready() && !p.path.is_empty())
                .and_then(|p| {
                    Some(Side {
                        place: p.place.clone()?,
                        root: p.path.clone(),
                    })
                })
        };
        let (Some(left), Some(right)) = (side(self, a), side(self, b)) else {
            self.status = "Both panes need a folder open to compare them".into();
            return;
        };
        self.compare = Some(Compare::start(&self.ctx, left, right, self.now));
    }

    pub(super) fn compare_ui(&mut self, ctx: &Context) {
        let Some(c) = &mut self.compare else { return };
        if let Some(rx) = &c.rx {
            if let Ok(result) = rx.try_recv() {
                c.rx = None;
                match result {
                    Ok((rows, same)) => {
                        c.rows = rows;
                        c.same = same;
                    }
                    Err(e) => c.error = Some(e),
                }
            }
        }
        let loading = c.rx.is_some();
        let planned = c.planned();
        let picked: Vec<&(usize, Action, bool)> = planned.iter().filter(|p| p.2).collect();
        let copies = picked
            .iter()
            .filter(|p| matches!(p.1, Action::ToRight | Action::ToLeft))
            .count();
        let deletes = picked.len() - copies;
        let bytes: u64 = picked
            .iter()
            .map(|(i, a, _)| match a {
                Action::ToRight => c.rows[*i].left.map_or(0, |m| m.size),
                Action::ToLeft => c.rows[*i].right.map_or(0, |m| m.size),
                _ => 0,
            })
            .sum();
        let confirm = if loading {
            "Comparing…".to_string()
        } else if picked.is_empty() {
            "Nothing to do".to_string()
        } else if deletes > 0 {
            format!("Sync  ·  {copies} copied, {deletes} deleted")
        } else {
            format!("Sync {copies} file{}", if copies == 1 { "" } else { "s" })
        };
        let subtitle = "File by file, by size and time; nothing changes until you sync".to_string();
        let (left_name, right_name) = (c.left.short(), c.right.short());
        let now = self.now;
        let answer = ui::Dialog {
            id: "compare",
            icon: '\u{eae1}',
            title: "Compare folders",
            subtitle: &subtitle,
            confirm: &confirm,
            other: None,
            tone: if deletes > 0 { ui::Tone::Danger } else { ui::Tone::Normal },
            can: !loading && !picked.is_empty(),
            width: 820.0,
        }
        .show(ctx, |ui| {
            if loading {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("Listing both folders…  {:.0} s", now - c.started))
                            .size(12.5)
                            .color(theme::COMMENT()),
                    );
                });
                ui.add_space(30.0);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                return false;
            }
            if let Some(e) = &c.error {
                ui::callout(ui, ui::RED, |ui| {
                    ui.label(RichText::new(e).size(12.5).color(theme::TEXT()));
                });
                return false;
            }
                        ui.horizontal(|ui| {
                let w = (ui.available_width() - 34.0) / 2.0;
                c.left.card(ui, w);
                ui.label(RichText::new("⇄").size(16.0).color(theme::COMMENT()));
                c.right.card(ui, w);
            });
            // How the two differ, in a line.
            let count = |k: Kind| c.rows.iter().filter(|r| r.kind == k).count();
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let chip = |ui: &mut Ui, n: usize, text: &str, color: Color32| {
                    Frame::NONE
                        .fill(color.gamma_multiply(if n > 0 { 0.14 } else { 0.05 }))
                        .corner_radius(11)
                        .inner_margin(Margin::symmetric(10, 3))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("{n}  {text}"))
                                    .size(12.0)
                                    .color(if n > 0 { color } else { theme::COMMENT() }),
                            );
                        });
                };
                let accent = theme::ACCENT();
                chip(ui, count(Kind::OnlyLeft), "only left", accent);
                chip(ui, count(Kind::OnlyRight), "only right", accent);
                chip(ui, count(Kind::LeftNewer), "newer left", GOOD);
                chip(ui, count(Kind::RightNewer), "newer right", GOOD);
                chip(ui, count(Kind::Differs), "other size", ui::RED);
                chip(ui, c.same, "the same", theme::COMMENT());
            });
            if c.rows.is_empty() {
                ui.add_space(6.0);
                ui::callout(ui, GOOD, |ui| {
                    ui.label(
                        RichText::new(format!("The folders are the same: {} files match.", c.same))
                            .size(12.5)
                            .color(theme::TEXT()),
                    );
                });
                return false;
            }
            ui.horizontal(|ui| {
                let choices = [
                    (Direction::ToRight, format!("→  Update {right_name}")),
                    (Direction::ToLeft, format!("←  Update {left_name}")),
                    (Direction::Both, "⇄  Both ways".to_string()),
                ];
                let choices: Vec<(Direction, &str)> = choices.iter().map(|(d, s)| (*d, s.as_str())).collect();
                ui::segmented(ui, &mut c.direction, &choices);
            });
            if c.direction != Direction::Both {
                let target = if c.direction == Direction::ToRight { &right_name } else { &left_name };
                let other = if c.direction == Direction::ToRight { &left_name } else { &right_name };
                ui::switch(
                    ui,
                    &mut c.delete,
                    &format!("Delete from {target} what {other} doesn't have"),
                    "so it ends up an exact copy",
                );
            }
            // The lines: each with its tick, which way it goes and why.
            let row_h = 26.0;
            let list_h = (planned.len() as f32 * row_h).clamp(row_h * 3.0, 330.0);
            Frame::NONE
                .fill(theme::tint(4))
                .corner_radius(10)
                .stroke(Stroke::new(1.0, theme::LINE()))
                .inner_margin(Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ScrollArea::vertical()
                        .max_height(list_h)
                        .auto_shrink([false, true])
                        .show_rows(ui, row_h, planned.len(), |ui, range| {
                            for (i, action, on) in &planned[range] {
                                let row = &c.rows[*i];
                                ui.horizontal(|ui| {
                                    ui.set_height(row_h);
                                    let mut tick = *on;
                                    if ui.checkbox(&mut tick, "").changed() && !c.flipped.remove(&row.path) {
                                        c.flipped.insert(row.path.clone());
                                    }
                                    let (glyph, color) = match action {
                                        Action::ToRight => ("→", theme::ACCENT()),
                                        Action::ToLeft => ("←", theme::ACCENT()),
                                        Action::DeleteRight | Action::DeleteLeft => ("✕", ui::RED),
                                    };
                                    let color = if *on { color } else { theme::COMMENT() };
                                    ui.label(RichText::new(glyph).size(14.0).strong().color(color));
                                    ui.add_space(4.0);
                                    let size = match action {
                                        Action::ToRight | Action::DeleteRight if row.left.is_some() => row.left,
                                        Action::ToLeft | Action::DeleteLeft if row.right.is_some() => row.right,
                                        _ => row.left.or(row.right),
                                    }
                                    .map_or(0, |m| m.size);
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.add_sized(
                                            vec2(78.0, row_h),
                                            egui::Label::new(RichText::new(size_text(size)).size(12.0).color(theme::COMMENT())),
                                        );
                                        ui.add_sized(
                                            vec2(140.0, row_h),
                                            egui::Label::new(RichText::new(reason(row.kind)).size(12.0).color(theme::COMMENT())),
                                        );
                                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                            let text = RichText::new(&row.path).size(12.5).color(if *on { theme::TEXT() } else { theme::COMMENT() });
                                            ui.add(egui::Label::new(text).truncate());
                                        });
                                    });
                                });
                            }
                        });
                });
            ui.label(
                RichText::new(format!(
                    "{} of {} lines picked  ·  {} to copy  ·  times and sizes are compared, not contents",
                    picked.len(),
                    planned.len(),
                    size_text(bytes)
                ))
                .size(11.5)
                .color(theme::COMMENT()),
            );
            false
        });
        match answer {
            ui::Answer::Confirm => {
                let c = self.compare.take().unwrap();
                let mut lists: [Vec<String>; 4] = Default::default();
                for (i, action, on) in c.planned() {
                    if on {
                        lists[action as usize].push(c.rows[i].path.clone());
                    }
                }
                let [to_right, to_left, del_right, del_left] = lists;
                let (l, r) = (c.left.short(), c.right.short());
                if !to_right.is_empty() || !del_right.is_empty() {
                    self.queue.add_sync(
                        c.left.place.clone(),
                        c.left.root.clone(),
                        c.right.place.clone(),
                        c.right.root.clone(),
                        to_right,
                        del_right,
                        format!("Sync {l} → {r}"),
                    );
                }
                if !to_left.is_empty() || !del_left.is_empty() {
                    self.queue.add_sync(
                        c.right.place,
                        c.right.root,
                        c.left.place,
                        c.left.root,
                        to_left,
                        del_left,
                        format!("Sync {r} → {l}"),
                    );
                }
                self.show_jobs = true;
            }
            ui::Answer::Cancel => self.compare = None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(items: &[(&str, u64, u64)]) -> BTreeMap<String, Meta> {
        items
            .iter()
            .map(|(p, size, mtime)| {
                (
                    p.to_string(),
                    Meta {
                        size: *size,
                        mtime: *mtime,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn folders_differ_as_expected() {
        let left = map(&[
            ("a.txt", 1, 100),
            ("same.txt", 5, 50),
            ("new.txt", 2, 300),
            ("odd.bin", 9, 70),
            ("sub/x", 1, 1),
        ]);
        let right = map(&[
            ("same.txt", 5, 51),
            ("new.txt", 2, 200),
            ("odd.bin", 8, 70),
            ("b.txt", 3, 3),
        ]);
        let (rows, same) = diff(&left, &right);
        let kinds: Vec<(&str, Kind)> = rows.iter().map(|r| (r.path.as_str(), r.kind)).collect();
        assert_eq!(same, 1);
        assert_eq!(
            kinds,
            [
                ("a.txt", Kind::OnlyLeft),
                ("b.txt", Kind::OnlyRight),
                ("new.txt", Kind::LeftNewer),
                ("odd.bin", Kind::Differs),
                ("sub/x", Kind::OnlyLeft),
            ]
        );
    }

    #[test]
    fn a_sync_never_replaces_newer_files_unasked() {
        assert_eq!(
            plan(Kind::RightNewer, Direction::ToRight, false),
            Some((Action::ToRight, false))
        );
        assert_eq!(plan(Kind::OnlyRight, Direction::ToRight, false), None);
        assert_eq!(
            plan(Kind::OnlyRight, Direction::ToRight, true),
            Some((Action::DeleteRight, true))
        );
        assert_eq!(
            plan(Kind::RightNewer, Direction::Both, true),
            Some((Action::ToLeft, true))
        );
        assert_eq!(
            plan(Kind::Differs, Direction::Both, false),
            Some((Action::ToRight, false))
        );
    }
}
