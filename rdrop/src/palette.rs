//! The command palette (Ctrl+Shift+P or Ctrl+K): everything rdrop does, found by typing a
//! few letters of it, with its key beside it. A path typed in it is gone to.

use super::*;

/// What a line of the palette does.
#[derive(Clone)]
enum Run {
    Pane(PaneAct),
    Connect(Place),
    Go(Place, String),
    ShowJobs,
    ResumeAll,
    StopAll,
    ClearFinished,
    Conflict(engine::Conflict),
    Verify,
    NewSite,
    SelectAll,
    CopyToOther,
    FocusSearch(bool),
    NextPane,
}

struct Line {
    group: &'static str,
    title: String,
    keys: &'static str,
    run: Run,
}

pub(super) struct Palette {
    query: String,
    selected: usize,
    focus: bool,
}

impl Palette {
    pub fn with(query: String) -> Palette {
        Palette {
            query,
            selected: 0,
            focus: true,
        }
    }
}

/// How well `text` matches what was typed: its letters in order, better when they start
/// words or follow each other. `None` when they aren't all there.
pub(super) fn score(text: &str, query: &str) -> Option<i32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut total = 0;
    let mut at = 0;
    let mut last: Option<usize> = None;
    for q in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (at..text.len()).find(|&i| text[i] == q)?;
        let starts_word = found == 0 || !text[found - 1].is_alphanumeric();
        total += if last == Some(found.wrapping_sub(1)) {
            6
        } else {
            1
        };
        if starts_word {
            total += 8;
        }
        total -= (found - at).min(10) as i32 / 3;
        last = Some(found);
        at = found + 1;
    }
    Some(total - text.len() as i32 / 12)
}

impl App {
    fn palette_lines(&self) -> Vec<Line> {
        let mut lines = Vec::new();
        let mut add = |group, title: &str, keys, run| {
            lines.push(Line {
                group,
                title: title.to_string(),
                keys,
                run,
            })
        };
        let pane = self.pane(self.focus);
        let selected = pane.map(|p| p.selected_names()).unwrap_or_default();
        let first = selected.first().cloned();
        let local = pane.is_some_and(|p| p.is_local());

        add(
            "Pane",
            "Search in this folder",
            "Ctrl+F",
            Run::FocusSearch(false),
        );
        add("Pane", "Search inside files", "", Run::FocusSearch(true));
        add(
            "Pane",
            "New folder",
            "Ctrl+Shift+N",
            Run::Pane(PaneAct::NewFolder),
        );
        add("Pane", "Paste", "Ctrl+V", Run::Pane(PaneAct::Paste));
        add("Pane", "Refresh", "Ctrl+R", Run::Pane(PaneAct::Refresh));
        add("Pane", "Select all", "Ctrl+A", Run::SelectAll);
        add(
            "Pane",
            "Show or hide hidden items",
            "Ctrl+H",
            Run::Pane(PaneAct::ToggleHidden),
        );
        add(
            "Pane",
            "View as details",
            "",
            Run::Pane(PaneAct::View(View::Details)),
        );
        add(
            "Pane",
            "View as large icons",
            "",
            Run::Pane(PaneAct::View(View::Icons)),
        );
        add("Pane", "Go back", "Alt+Left", Run::Pane(PaneAct::Back));
        add("Pane", "Split right", "", Run::Pane(PaneAct::Split(true)));
        add("Pane", "Split down", "", Run::Pane(PaneAct::Split(false)));
        add("Pane", "Close this pane", "", Run::Pane(PaneAct::Close));
        add("Pane", "Next pane", "Tab", Run::NextPane);
        add(
            "Pane",
            "Compare with the other pane",
            "",
            Run::Pane(PaneAct::Compare),
        );
        if local {
            add(
                "Pane",
                "Watch this folder and upload changes",
                "",
                Run::Pane(PaneAct::Watch),
            );
        }
        if !selected.is_empty() {
            add(
                "Selection",
                "Copy to the other pane",
                "F5",
                Run::CopyToOther,
            );
            add(
                "Selection",
                "Copy",
                "Ctrl+C",
                Run::Pane(PaneAct::Clip(selected.clone(), false)),
            );
            add(
                "Selection",
                "Cut",
                "Ctrl+X",
                Run::Pane(PaneAct::Clip(selected.clone(), true)),
            );
            add(
                "Selection",
                "Delete",
                "Del",
                Run::Pane(PaneAct::Delete(selected.clone())),
            );
            add(
                "Selection",
                "Copy path",
                "",
                Run::Pane(PaneAct::CopyPaths(selected.clone())),
            );
        }
        if let Some(name) = first {
            add(
                "Selection",
                "Open or preview",
                "Enter",
                Run::Pane(PaneAct::Activate(name.clone())),
            );
            add(
                "Selection",
                "Edit in ride",
                "Ctrl+E",
                Run::Pane(PaneAct::Ride(name.clone())),
            );
            add(
                "Selection",
                "Open with app",
                "",
                Run::Pane(PaneAct::OpenWith(name.clone())),
            );
            add(
                "Selection",
                "Rename",
                "F2",
                Run::Pane(PaneAct::StartRename(name.clone())),
            );
            add(
                "Selection",
                "Properties and permissions",
                "Alt+Enter",
                Run::Pane(PaneAct::Properties(name)),
            );
        }
        add("Transfers", "Show transfers", "", Run::ShowJobs);
        add(
            "Transfers",
            "Resume all stopped transfers",
            "",
            Run::ResumeAll,
        );
        add("Transfers", "Stop all transfers", "", Run::StopAll);
        add(
            "Transfers",
            "Clear finished transfers",
            "",
            Run::ClearFinished,
        );
        for (c, label) in engine::Conflict::ALL {
            let title = format!("When a file is already there: {label}");
            add("Transfers", &title, "", Run::Conflict(c));
        }
        add(
            "Transfers",
            "Check copies with SHA-256: on or off",
            "",
            Run::Verify,
        );
        add("Connect", "This computer", "", Run::Connect(Place::Local));
        for site in &self.sites {
            add(
                "Connect",
                &site.name,
                "",
                Run::Connect(Place::Server(site.key())),
            );
        }
        for host in &self.hosts {
            add(
                "Connect",
                host,
                "",
                Run::Connect(Place::Server(host.clone())),
            );
        }
        add("Connect", "New site…", "", Run::NewSite);
        for k in &self.known {
            add("Go", &k.name, "", Run::Go(Place::Local, k.path.clone()));
        }
        for d in &self.drives {
            add("Go", &d.title(), "", Run::Go(Place::Local, d.root.clone()));
        }
        lines
    }

    pub(super) fn palette_ui(&mut self, ctx: &Context) {
        let Some(mut p) = self.palette.take() else {
            return;
        };
        let mut lines = self.palette_lines();
        // A path typed in goes there, in the pane in focus.
        let typed = p.query.trim().to_string();
        let place = self.pane(self.focus).and_then(|p| p.place.clone());
        if typed.starts_with('/') || typed.starts_with('~') || typed.get(1..3) == Some(":\\") {
            let target = if typed.starts_with('/') || typed.starts_with('~') {
                place.clone().filter(|p| *p != Place::Local)
            } else {
                Some(Place::Local)
            };
            if let Some(target) = target {
                lines.insert(
                    0,
                    Line {
                        group: "Go",
                        title: format!("Go to {typed}"),
                        keys: "Enter",
                        run: Run::Go(target, typed.clone()),
                    },
                );
            }
        }
        let mut shown: Vec<(i32, usize)> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| {
                if p.query.trim().is_empty() || l.title.starts_with("Go to ") {
                    return Some((0, i));
                }
                // "connect vps" finds by the group too, once there is a space in it.
                let grouped =
                    p.query.trim().contains(' ').then(|| {
                        score(&format!("{} {}", l.group, l.title), &p.query).map(|s| s - 4)
                    });
                let s = score(&l.title, &p.query).max(grouped.flatten());
                s.map(|s| (s, i))
            })
            .collect();
        shown.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        p.selected = p.selected.min(shown.len().saturating_sub(1));
        let (down, up, enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Escape),
            )
        });
        if down && p.selected + 1 < shown.len() {
            p.selected += 1;
        }
        if up {
            p.selected = p.selected.saturating_sub(1);
        }
        let mut chosen = enter.then(|| shown.get(p.selected).map(|s| s.1)).flatten();
        let screen = ctx.content_rect();
        let width = 600.0_f32.min(screen.width() - 40.0);
        let mut close = escape;
        let area = egui::Area::new(egui::Id::new("palette"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(screen.center().x - width / 2.0, screen.top() + 60.0))
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(theme::PANEL_BG())
                    .stroke(Stroke::new(1.0, theme::LINE()))
                    .corner_radius(14)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 14],
                        blur: 48,
                        spread: 0,
                        color: Color32::from_black_alpha(160),
                    })
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        Frame::NONE
                            .fill(theme::tint(6))
                            .corner_radius(9)
                            .inner_margin(Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(icons::SEARCH)
                                            .size(15.0)
                                            .color(theme::COMMENT()),
                                    );
                                    let edit = ui.add(
                                        TextEdit::singleline(&mut p.query)
                                            .frame(egui::Frame::NONE)
                                            .font(egui::FontId::proportional(15.0))
                                            .hint_text(
                                                "Type a command, a server, a place or a path",
                                            )
                                            .desired_width(f32::INFINITY),
                                    );
                                    if std::mem::take(&mut p.focus) || !edit.has_focus() {
                                        edit.request_focus();
                                    }
                                    if edit.changed() {
                                        p.selected = 0;
                                    }
                                });
                            });
                        ui.add_space(6.0);
                        if shown.is_empty() {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new("Nothing matches")
                                    .size(12.5)
                                    .color(theme::COMMENT()),
                            );
                            ui.add_space(10.0);
                            return;
                        }
                        let row_h = 32.0;
                        ScrollArea::vertical()
                            .max_height(row_h * 11.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (n, (_, i)) in shown.iter().enumerate() {
                                    let line = &lines[*i];
                                    let on = n == p.selected;
                                    let (rect, resp) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), row_h),
                                        Sense::click(),
                                    );
                                    if on {
                                        ui.painter().rect_filled(
                                            rect,
                                            8,
                                            theme::ACCENT().gamma_multiply(0.16),
                                        );
                                        if down || up {
                                            ui.scroll_to_rect(rect, None);
                                        }
                                    } else if resp.hovered() {
                                        ui.painter().rect_filled(rect, 8, theme::tint(8));
                                    }
                                    let y = rect.center().y;
                                    ui.painter().text(
                                        pos2(rect.left() + 12.0, y),
                                        Align2::LEFT_CENTER,
                                        line.group,
                                        FontId::proportional(11.5),
                                        theme::COMMENT(),
                                    );
                                    ui.painter().text(
                                        pos2(rect.left() + 92.0, y),
                                        Align2::LEFT_CENTER,
                                        &line.title,
                                        FontId::proportional(13.5),
                                        if on { theme::STRONG() } else { theme::TEXT() },
                                    );
                                    if !line.keys.is_empty() {
                                        let galley = ui.painter().layout_no_wrap(
                                            line.keys.to_string(),
                                            FontId::monospace(11.5),
                                            theme::COMMENT(),
                                        );
                                        let key_rect = Rect::from_min_size(
                                            pos2(
                                                rect.right() - galley.size().x - 22.0,
                                                y - galley.size().y / 2.0 - 3.0,
                                            ),
                                            galley.size() + vec2(12.0, 6.0),
                                        );
                                        ui.painter().rect_filled(key_rect, 5, theme::tint(10));
                                        ui.painter().galley(
                                            key_rect.min + vec2(6.0, 3.0),
                                            galley,
                                            theme::COMMENT(),
                                        );
                                    }
                                    if resp.clicked() {
                                        chosen = Some(*i);
                                    }
                                    if resp.hovered()
                                        && ui.input(|i| i.pointer.delta() != Vec2::ZERO)
                                    {
                                        p.selected = n;
                                    }
                                }
                            });
                    });
            });
        // A click outside closes it.
        if ctx.input(|i| i.pointer.any_pressed())
            && !area
                .response
                .rect
                .contains(ctx.input(|i| i.pointer.interact_pos()).unwrap_or_default())
        {
            close = true;
        }
        if let Some(i) = chosen {
            let run = lines[i].run.clone();
            self.run_command(ctx, run);
            return;
        }
        if !close {
            self.palette = Some(p);
        }
    }

    fn run_command(&mut self, ctx: &Context, run: Run) {
        let id = self.focus;
        match run {
            Run::Pane(act) => self.apply(ctx, id, act),
            Run::Connect(place) => self.set_place(ctx, id, place, None),
            Run::Go(place, path) => {
                if self.pane(id).and_then(|p| p.place.clone()) == Some(place.clone()) {
                    self.go(id, path);
                } else {
                    self.set_place(ctx, id, place, Some(path));
                }
            }
            Run::ShowJobs => self.show_jobs = true,
            Run::ResumeAll => {
                self.queue.resume_all();
                self.show_jobs = true;
            }
            Run::StopAll => self.queue.stop_all(),
            Run::ClearFinished => self.queue.clear_finished(),
            Run::Conflict(c) => {
                self.queue.options.lock().unwrap().conflict = c;
                self.note = format!(
                    "Files already there: {}",
                    engine::Conflict::ALL
                        .iter()
                        .find(|x| x.0 == c)
                        .map_or("", |x| x.1)
                );
            }
            Run::Verify => {
                let mut o = self.queue.options.lock().unwrap();
                o.verify = !o.verify;
                self.note = if o.verify {
                    "Copies are checked with SHA-256"
                } else {
                    "Copies aren't checked"
                }
                .into();
            }
            Run::NewSite => {
                self.auth = Some(Auth::Site {
                    old: None,
                    site: Site {
                        port: 22,
                        ..Site::default()
                    },
                    port: "22".into(),
                    password: String::new(),
                    focus: true,
                })
            }
            Run::SelectAll => {
                let hidden = self.settings.show_hidden;
                if let Some(p) = self.pane_mut(id) {
                    p.selected = p.visible(hidden).iter().map(|i| i.name.clone()).collect();
                }
            }
            Run::CopyToOther => {
                let names = self
                    .pane(id)
                    .map(|p| p.selected_names())
                    .unwrap_or_default();
                if let Some(to) = self.other_pane(id) {
                    self.copy(id, &names, to, None);
                }
            }
            Run::FocusSearch(contents) => {
                if let Some(p) = self.pane_mut(id) {
                    p.focus_search = true;
                    p.search_contents = contents;
                }
            }
            Run::NextPane => {
                let ids = self.layout.panes();
                if let Some(at) = ids.iter().position(|p| *p == id) {
                    self.set_focus(ids[(at + 1) % ids.len()]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_in_order_match_and_word_starts_win() {
        assert!(score("New folder", "nf").is_some());
        assert!(score("New folder", "fn").is_none());
        assert!(score("Compare with the other pane", "cmp").is_some());
        assert!(score("Split right", "sr") > score("Show transfers", "sr"));
        assert!(score("Refresh", "ref") > score("Clear finished transfers", "ref"));
    }
}
