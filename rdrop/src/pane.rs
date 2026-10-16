//! Drawing a pane: its header, the list of what is in it (as details or as big icons),
//! "This PC" with the drives and the user's folders, names edited in place, and the line at
//! the bottom with how many items there are and how many are picked.

use super::*;

#[derive(Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub(super) enum View {
    #[default]
    Details,
    Icons,
}

const TILE: Vec2 = Vec2::new(112.0, 104.0);

impl App {
    pub(super) fn pane_ui(
        &mut self,
        ui: &mut Ui,
        id: u64,
        rect: Rect,
        acts: &mut Vec<(u64, PaneAct)>,
    ) {
        let focused = self.focus == id;
        let several = self.panes.len() > 1;
        let hosts: Vec<String> = self
            .sites
            .iter()
            .map(Site::key)
            .chain(self.hosts.iter().cloned())
            .collect();
        if ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_pressed()) {
            self.set_focus(id);
        }
        let show_hidden = self.settings.show_hidden;
        let Some(index) = self.panes.iter().position(|p| p.id == id) else {
            return;
        };
        let pane = &mut self.panes[index];
        let border = if focused {
            theme::ACCENT().gamma_multiply(0.6)
        } else {
            theme::LINE()
        };
        ui.painter().rect(
            rect,
            10,
            theme::PANEL_BG(),
            Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let mut act = |a: PaneAct| acts.push((id, a));

        // The header: what this pane shows, a handle to move it by, and what it can do.
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H));
        let handle = ui.interact(header, ui.id().with("handle"), Sense::click_and_drag());
        if handle.drag_started() {
            act(PaneAct::StartPane);
        }
        if handle.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        let mut head = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(header.shrink2(vec2(10.0, 4.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let (icon, title) = match &pane.place {
            Some(Place::Local) => (ui::DESKTOP, "This computer".to_string()),
            Some(place @ Place::Server(_)) => (icons::REMOTE, place.label().to_string()),
            None => (icons::ADD, "Pick a place".to_string()),
        };
        let dot = match &pane.place {
            Some(Place::Server(_)) if pane.connected => GOOD,
            _ => theme::COMMENT(),
        };
        head.label(RichText::new(icon).color(dot));
        let title: String = if title.chars().count() > 22 {
            title.chars().take(20).chain(['…']).collect()
        } else {
            title
        };
        head.menu_button(
            RichText::new(format!("{title}  {}", icons::CHEVRON_DOWN))
                .size(13.0)
                .strong(),
            |ui| {
                if ui
                    .button(format!("{}  This computer", icons::FOLDER_ROOT))
                    .clicked()
                {
                    act(PaneAct::Pick(Place::Local));
                }
                ui.separator();
                for host in &hosts {
                    if ui
                        .button(format!(
                            "{}  {}",
                            icons::REMOTE,
                            host.strip_prefix("site:").unwrap_or(host)
                        ))
                        .clicked()
                    {
                        act(PaneAct::Pick(Place::Server(host.clone())));
                    }
                }
            },
        );
        head.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if several
                && ui
                    .small_button(icons::CLOSE.to_string())
                    .on_hover_text("Close this pane")
                    .clicked()
            {
                act(PaneAct::Close);
            }
            let usable = pane.ready();
            ui.menu_button("⋯", |ui| {
                ui.add_enabled_ui(usable, |ui| {
                    if ui
                        .add(egui::Button::new("New folder").shortcut_text("Ctrl+Shift+N"))
                        .clicked()
                    {
                        act(PaneAct::NewFolder);
                    }
                    if ui
                        .add(egui::Button::new("Paste").shortcut_text("Ctrl+V"))
                        .clicked()
                    {
                        act(PaneAct::Paste);
                    }
                    if ui
                        .add(egui::Button::new("Refresh").shortcut_text("Ctrl+R"))
                        .clicked()
                    {
                        act(PaneAct::Open(pane.path.clone()));
                    }
                });
                ui.separator();
                let view = pane.view;
                if ui
                    .add(egui::Button::selectable(view == View::Details, "Details"))
                    .clicked()
                {
                    act(PaneAct::View(View::Details));
                }
                if ui
                    .add(egui::Button::selectable(view == View::Icons, "Large icons"))
                    .clicked()
                {
                    act(PaneAct::View(View::Icons));
                }
                if ui
                    .add(
                        egui::Button::selectable(show_hidden, "Hidden items")
                            .shortcut_text("Ctrl+H"),
                    )
                    .clicked()
                {
                    act(PaneAct::ToggleHidden);
                }
                ui.separator();
                if ui
                    .add_enabled(
                        usable && several,
                        egui::Button::new("Compare with the other pane"),
                    )
                    .clicked()
                {
                    act(PaneAct::Compare);
                }
                if pane.is_local()
                    && ui
                        .add_enabled(usable && several && !pane.path.is_empty(), egui::Button::new("Watch and upload changes"))
                        .on_hover_text("What is saved in this folder goes up to the server folder in the other pane")
                        .clicked()
                {
                    act(PaneAct::Watch);
                }
                ui.separator();
                if ui.button("Split right").clicked() {
                    act(PaneAct::Split(true));
                }
                if ui.button("Split down").clicked() {
                    act(PaneAct::Split(false));
                }
            });
            if rect.width() >= 420.0 {
                ui.add_space(4.0);
                let view_icon = if pane.view == View::Details {
                    "▦"
                } else {
                    "☰"
                };
                if ui
                    .small_button(view_icon)
                    .on_hover_text("Details or large icons")
                    .clicked()
                {
                    act(PaneAct::View(if pane.view == View::Details {
                        View::Icons
                    } else {
                        View::Details
                    }));
                }
                ui.add_enabled_ui(usable, |ui| {
                    if ui
                        .small_button("⟳")
                        .on_hover_text("Refresh (Ctrl+R)")
                        .clicked()
                    {
                        act(PaneAct::Open(pane.path.clone()));
                    }
                    if ui
                        .small_button("↑")
                        .on_hover_text("Up (Backspace)")
                        .clicked()
                    {
                        if let Some(up) = pane.up() {
                            act(PaneAct::Open(up));
                        }
                    }
                    if ui
                        .add_enabled(!pane.back.is_empty(), egui::Button::new("←").small())
                        .on_hover_text("Back (Alt+Left)")
                        .clicked()
                    {
                        act(PaneAct::Back);
                    }
                });
            }
        });

        let body = Rect::from_min_max(rect.min + vec2(8.0, HEADER_H), rect.max - vec2(8.0, 30.0));
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(body)
                .layout(Layout::top_down(Align::Min)),
        );
        let ui = &mut ui;

        if !pane.ready() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| match &pane.place {
                Some(Place::Server(host)) => {
                    if let Some(e) = &pane.error {
                        ui.label(RichText::new(ui::WARNING).size(28.0).color(ui::RED));
                        ui.add_space(6.0);
                        ui.add(egui::Label::new(RichText::new(e).size(12.5).color(theme::TEXT())).wrap());
                        ui.add_space(10.0);
                        if ui::button(ui, "Try again", ui::Kind::Primary(theme::ACCENT()), true) {
                            act(PaneAct::Pick(Place::Server(host.clone())));
                        }
                    } else {
                        ui.spinner();
                        let label = Place::Server(host.clone());
                        ui.label(RichText::new(format!("Connecting to {}…", label.label())).size(13.0).color(theme::COMMENT()));
                    }
                }
                _ => {
                    ui.label(RichText::new("Open a server here: pick one above, click one on the left, or drag one onto this pane.").size(13.0).color(theme::COMMENT()));
                    ui.add_space(10.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(format!("{}  This computer", icons::FOLDER_ROOT)).clicked() {
                            act(PaneAct::Pick(Place::Local));
                        }
                        for host in hosts.iter().take(8) {
                            if ui.button(format!("{}  {}", icons::REMOTE, host.strip_prefix("site:").unwrap_or(host))).clicked() {
                                act(PaneAct::Pick(Place::Server(host.clone())));
                            }
                        }
                    });
                }
            });
            return;
        }

        // Where it is, a path to type into, and a box to search under it, as in Explorer.
        let can_search = !(pane.is_local() && pane.path.is_empty());
        ui.horizontal(|ui| {
            let search_w = if can_search {
                (ui.available_width() * 0.36).clamp(150.0, 260.0)
            } else {
                0.0
            };
            let edit = ui.add(
                TextEdit::singleline(&mut pane.path_edit)
                    .desired_width(ui.available_width() - search_w - 8.0)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("This PC"),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                act(PaneAct::Open(pane.path_edit.clone()));
            }
            if can_search {
                let hint = if pane.search_contents {
                    "Search in files"
                } else {
                    "Search"
                };
                let field = TextEdit::singleline(&mut pane.search_edit)
                    .desired_width(search_w - 34.0)
                    .hint_text(format!("{}  {hint}", icons::SEARCH));
                let resp = ui
                    .add(field)
                    .on_hover_text("Ctrl+F · Enter to search · Esc to go back to the folder");
                if std::mem::take(&mut pane.focus_search) {
                    resp.request_focus();
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    act(PaneAct::Search(
                        pane.search_edit.clone(),
                        pane.search_contents,
                    ));
                }
                if (resp.has_focus() || resp.lost_focus())
                    && ui.input(|i| i.key_pressed(Key::Escape))
                {
                    act(PaneAct::EndSearch);
                }
                let toggle = egui::Button::selectable(
                    pane.search_contents,
                    RichText::new("Aa").size(11.5).monospace(),
                )
                .min_size(vec2(26.0, 22.0));
                if ui
                    .add(toggle)
                    .on_hover_text(if pane.search_contents {
                        "Looking inside files: click to search names"
                    } else {
                        "Search names: click to look inside files"
                    })
                    .clicked()
                {
                    pane.search_contents = !pane.search_contents;
                    if pane.search.is_some() && !pane.search_edit.trim().is_empty() {
                        act(PaneAct::Search(
                            pane.search_edit.clone(),
                            pane.search_contents,
                        ));
                    }
                }
            }
        });
        ui.add_space(4.0);
        // While searching: what, where, how many, and the way back.
        if let Some(s) = &pane.search {
            let fill = theme::ACCENT().gamma_multiply(0.10);
            Frame::NONE
                .fill(fill)
                .corner_radius(7)
                .inner_margin(Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if s.running {
                            ui.add(egui::Spinner::new().size(12.0));
                        } else {
                            ui.label(RichText::new(icons::SEARCH).color(theme::ACCENT()));
                        }
                        let what = if s.contents { "inside files" } else { "names" };
                        let mut text = format!(
                            "{} for \u{201c}{}\u{201d} in {what}",
                            if s.found == 1 {
                                "1 result".to_string()
                            } else {
                                format!("{} results", s.found)
                            },
                            s.query
                        );
                        if s.found >= search::LIMIT {
                            text.push_str(&format!("  ·  stopped at {}", search::LIMIT));
                        } else if let Some(took) = s.took {
                            text.push_str(&format!("  ·  {:.1} s", took.as_secs_f32()));
                        } else {
                            text.push_str("  ·  searching…");
                        }
                        ui.add(
                            egui::Label::new(RichText::new(text).size(12.0).color(theme::TEXT()))
                                .truncate(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .small_button(icons::CLOSE.to_string())
                                .on_hover_text("Back to the folder (Esc)")
                                .clicked()
                            {
                                act(PaneAct::EndSearch);
                            }
                            if s.running && ui.small_button("Stop").clicked() {
                                s.cancel();
                            }
                        });
                    });
                    if let Some(e) = &s.error {
                        ui.label(RichText::new(e).size(12.0).color(BAD));
                    }
                });
            ui.add_space(4.0);
        }
        if let Some(e) = &pane.error {
            ui.label(RichText::new(e).size(12.5).color(BAD));
        }

        // This PC: the drives and the user's folders.
        if pane.is_local() && pane.path.is_empty() {
            let (drives, known) = (self.drives.clone(), self.known.clone());
            this_pc(ui, &drives, &known, &mut act);
            status_line(ui, rect, &format!("{} drives", drives.len()));
            return;
        }

        let items = pane.visible(show_hidden);
        let renaming = self.renaming.as_mut().filter(|r| r.pane == id);
        let offset = self.offset;
        let dragging_rows = matches!(self.drag, Some(Drag::Rows { .. }));
        let mut ev = Events::default();
        let remote = !pane.is_local();
        let scroll_to = pane.scroll_to.take();
        match pane.view {
            View::Details => details(
                ui,
                pane,
                &items,
                renaming,
                offset,
                remote,
                focused,
                dragging_rows,
                scroll_to,
                &mut ev,
            ),
            View::Icons => tiles(
                ui,
                pane,
                &items,
                renaming,
                focused,
                dragging_rows,
                scroll_to,
                &mut ev,
                &mut self.thumbs,
            ),
        }

        // Selecting, as in Explorer: a click picks one, Ctrl adds, Shift takes a range; a
        // click on the one already picked, left alone a moment, edits its name.
        let now = ui.input(|i| i.time);
        if let Some((index, mods)) = ev.clicked {
            let names: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
            let alone = pane.selected.len() == 1 && pane.selected.contains(&names[index]);
            if mods.shift {
                let from = pane
                    .anchor
                    .unwrap_or(index)
                    .min(names.len().saturating_sub(1));
                let (a, b) = (from.min(index), from.max(index));
                pane.selected = names[a..=b].iter().cloned().collect();
            } else if mods.command {
                if !pane.selected.remove(&names[index]) {
                    pane.selected.insert(names[index].clone());
                }
                pane.anchor = Some(index);
            } else {
                pane.selected = [names[index].clone()].into();
                pane.anchor = Some(index);
                if alone && focused {
                    self.slow_click = Some((id, names[index].clone(), now));
                }
            }
        }
        if let Some(index) = ev.opened {
            self.slow_click = None;
            act(PaneAct::Activate(items[index].name.clone()));
        }
        if let Some((pane_id, name, at)) = &self.slow_click {
            if *pane_id == id && now - at > 0.55 {
                act(PaneAct::StartRename(name.clone()));
                self.slow_click = None;
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        let Some(index) = self.panes.iter().position(|p| p.id == id) else {
            return;
        };
        let pane = &mut self.panes[index];
        if let Some(index) = ev.drag_from.filter(|_| !dragging_rows) {
            let name = items[index].name.clone();
            if !pane.selected.contains(&name) {
                pane.selected = [name].into();
            }
            act(PaneAct::StartRows(pane.selected_names()));
        }
        if let Some((index, what)) = ev.menu {
            let name = items[index].name.clone();
            if !pane.selected.contains(&name) {
                pane.selected = [name.clone()].into();
            }
            let names = pane.selected_names();
            act(match what {
                Menu::Open => PaneAct::Activate(name),
                Menu::OpenWith => PaneAct::OpenWith(name),
                Menu::Cut => PaneAct::Clip(names, true),
                Menu::Copy => PaneAct::Clip(names, false),
                Menu::CopyTo => PaneAct::CopyTo(names),
                Menu::Rename => PaneAct::StartRename(name),
                Menu::Delete => PaneAct::Delete(names),
                Menu::DeleteForGood => PaneAct::DeleteForGood(names),
                Menu::CopyPath => PaneAct::CopyPaths(names),
                Menu::Reveal => PaneAct::Reveal(pane.child(&name)),
                Menu::Properties => PaneAct::Properties(name),
                Menu::Ride => PaneAct::Ride(name),
            });
        }
        if ev.empty_clicked {
            pane.selected.clear();
        }
        if let Some(what) = ev.empty_menu {
            act(what);
        }
        if ev.rename_done.is_some() {
            act(PaneAct::FinishRename(ev.rename_done.unwrap()));
        }

        // The line at the bottom: how many, and how many picked.
        let picked: Vec<&Item> = items
            .iter()
            .filter(|i| pane.selected.contains(&i.name))
            .collect();
        let mut text = format!("{} items", items.len());
        if !picked.is_empty() {
            let bytes: u64 = picked.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();
            text.push_str(&format!("  ·  {} selected", picked.len()));
            if bytes > 0 {
                text.push_str(&format!("  ·  {}", size_text(bytes)));
            }
        }
        if pane.loading {
            text.push_str("  ·  loading…");
        }
        status_line(ui, rect, &text);
    }
}

/// What a click, a key or a menu in the list asked for.
#[derive(Default)]
struct Events {
    clicked: Option<(usize, Modifiers)>,
    opened: Option<usize>,
    drag_from: Option<usize>,
    menu: Option<(usize, Menu)>,
    empty_clicked: bool,
    empty_menu: Option<PaneAct>,
    /// The name being edited is done: kept (true) or dropped.
    rename_done: Option<bool>,
}

#[derive(Clone, Copy)]
enum Menu {
    Open,
    OpenWith,
    Cut,
    Copy,
    CopyTo,
    Rename,
    Delete,
    DeleteForGood,
    CopyPath,
    Reveal,
    Properties,
    Ride,
}

fn status_line(ui: &mut Ui, rect: Rect, text: &str) {
    let at = egui::pos2(rect.left() + 14.0, rect.bottom() - 15.0);
    ui.painter().text(
        at,
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(11.5),
        theme::COMMENT(),
    );
}

/// The menu of an item: the same in both views.
fn item_menu(ui: &mut Ui, index: usize, local: bool, dir: bool, menu: &mut Option<(usize, Menu)>) {
    let mut item = |ui: &mut Ui, text: &str, keys: &str, what: Menu| {
        if ui
            .add(egui::Button::new(text).shortcut_text(keys))
            .clicked()
        {
            *menu = Some((index, what));
        }
    };
    item(
        ui,
        if dir { "Open" } else { "Preview" },
        "Enter",
        Menu::Open,
    );
    if !dir {
        item(ui, "Open with app", "", Menu::OpenWith);
    }
    item(
        ui,
        if dir { "Open in ride" } else { "Edit in ride" },
        "Ctrl+E",
        Menu::Ride,
    );
    ui.separator();
    item(ui, "Cut", "Ctrl+X", Menu::Cut);
    item(ui, "Copy", "Ctrl+C", Menu::Copy);
    item(ui, "Copy to the other pane", "F5", Menu::CopyTo);
    ui.separator();
    item(ui, "Rename", "F2", Menu::Rename);
    if local {
        item(ui, "Delete", "Del", Menu::Delete);
        item(ui, "Delete for good…", "Shift+Del", Menu::DeleteForGood);
    } else {
        item(ui, "Delete…", "Del", Menu::Delete);
    }
    ui.separator();
    item(ui, "Copy path", "", Menu::CopyPath);
    if local {
        item(ui, "Show in Explorer", "", Menu::Reveal);
    }
    item(ui, "Properties", "Alt+Enter", Menu::Properties);
}

fn empty_menu(ui: &mut Ui, out: &mut Option<PaneAct>) {
    if ui
        .add(egui::Button::new("New folder").shortcut_text("Ctrl+Shift+N"))
        .clicked()
    {
        *out = Some(PaneAct::NewFolder);
    }
    if ui
        .add(egui::Button::new("Paste").shortcut_text("Ctrl+V"))
        .clicked()
    {
        *out = Some(PaneAct::Paste);
    }
    if ui
        .add(egui::Button::new("Refresh").shortcut_text("Ctrl+R"))
        .clicked()
    {
        *out = Some(PaneAct::Refresh);
    }
}

/// The name of an item, or the box to edit it in.
fn name_or_edit(
    ui: &mut Ui,
    rect: Rect,
    item: &Item,
    renaming: &mut Option<&mut explorer::Renaming>,
    font: f32,
    color: Color32,
    centered: bool,
    done: &mut Option<bool>,
) {
    if let Some(r) = renaming.as_mut().filter(|r| r.name == item.name) {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let edit = child.add(
            TextEdit::singleline(&mut r.text)
                .font(FontId::proportional(font))
                .desired_width(rect.width())
                .margin(Margin::symmetric(4, 2)),
        );
        if std::mem::take(&mut r.focus) {
            edit.request_focus();
            // The name is picked without its extension, as Explorer does.
            let stem = r
                .text
                .rfind('.')
                .filter(|at| *at > 0 && !item.is_dir)
                .unwrap_or(r.text.chars().count());
            let range = egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(stem),
            );
            if let Some(mut state) = egui::TextEdit::load_state(child.ctx(), edit.id) {
                state.cursor.set_char_range(Some(range));
                state.store(child.ctx(), edit.id);
            }
        }
        if child.input(|i| i.key_pressed(Key::Escape)) {
            *done = Some(false);
        } else if edit.lost_focus() {
            *done = Some(true);
        }
        return;
    }
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    if centered {
        let galley = painter.layout(
            item.name.clone(),
            FontId::proportional(font),
            color,
            rect.width(),
        );
        let at = egui::pos2(rect.center().x - galley.size().x / 2.0, rect.top());
        painter.galley(at, galley, color);
    } else {
        painter.text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            &item.name,
            FontId::proportional(font),
            color,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn details(
    ui: &mut Ui,
    pane: &mut Pane,
    items: &[Item],
    mut renaming: Option<&mut explorer::Renaming>,
    offset: i64,
    remote: bool,
    focused: bool,
    dragging: bool,
    scroll_to: Option<String>,
    ev: &mut Events,
) {
    // Columns that fit the pane: permissions on a server when there is room.
    let full = ui.available_width();
    let size_w = if full > 330.0 { 84.0 } else { 0.0 };
    let time_w = if full > 480.0 { 140.0 } else { 0.0 };
    let perm_w = if remote && full > 640.0 { 104.0 } else { 0.0 };
    let widths = [full - size_w - time_w - perm_w, perm_w, size_w, time_w];
    let mut resort = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, (col, title)) in [
            (Some(Col::Name), "Name"),
            (None, "Permissions"),
            (Some(Col::Size), "Size"),
            (Some(Col::Modified), "Modified"),
        ]
        .into_iter()
        .enumerate()
        {
            if widths[i] <= 0.0 {
                continue;
            }
            let (r, resp) = ui.allocate_exact_size(vec2(widths[i], 20.0), Sense::click());
            let arrow = match col {
                Some(c) if pane.sort.0 == c => {
                    if pane.sort.1 {
                        " ▲"
                    } else {
                        " ▼"
                    }
                }
                _ => "",
            };
            let (align, at) = if i == 0 {
                (Align2::LEFT_CENTER, r.left_center() + vec2(28.0, 0.0))
            } else {
                (Align2::RIGHT_CENTER, r.right_center() - vec2(10.0, 0.0))
            };
            ui.painter().text(
                at,
                align,
                format!("{title}{arrow}"),
                FontId::proportional(11.5),
                theme::COMMENT(),
            );
            if resp.clicked() {
                resort = col;
            }
        }
    });
    if let Some(col) = resort {
        pane.sort = (
            col,
            if pane.sort.0 == col {
                !pane.sort.1
            } else {
                true
            },
        );
        pane.sort_items();
    }
    let line = ui.cursor().top();
    ui.painter()
        .hline(ui.max_rect().x_range(), line, (1.0, theme::LINE()));
    if pane.loading && pane.items.is_empty() {
        ui.add_space(16.0);
        ui.vertical_centered(|ui| ui.spinner());
    }
    let mut area = ScrollArea::vertical().id_salt("rows").auto_shrink(false);
    if let Some(name) = &scroll_to {
        if let Some(at) = items.iter().position(|i| &i.name == name) {
            let view = ui.available_height();
            area = area.vertical_scroll_offset((at as f32 * ROW_H - view / 2.0).max(0.0));
        }
    }
    let list = area.show_rows(ui, ROW_H, items.len(), |ui, range| {
        for index in range {
            let item = &items[index];
            let selected = pane.selected.contains(&item.name);
            let (r, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
            let fill = if selected {
                if focused {
                    theme::ACCENT().gamma_multiply(0.22)
                } else {
                    theme::tint(14)
                }
            } else if resp.hovered() && !dragging {
                theme::tint(7)
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(r, 6.0, fill);
            let (glyph, mut color) = icons::for_entry(&item.name, item.is_dir, false);
            let mut text = theme::TEXT();
            if item.hidden {
                color = color.gamma_multiply(0.5);
                text = theme::COMMENT();
            }
            let painter = ui.painter_at(r);
            painter.text(
                r.left_center() + vec2(8.0, 0.0),
                Align2::LEFT_CENTER,
                glyph,
                FontId::proportional(14.0),
                color,
            );
            if item.link {
                painter.text(
                    r.left_center() + vec2(17.0, 6.0),
                    Align2::LEFT_CENTER,
                    "↪",
                    FontId::proportional(9.0),
                    theme::ACCENT(),
                );
            }
            let name_rect = Rect::from_min_max(
                egui::pos2(r.left() + 26.0, r.top() + 2.0),
                egui::pos2(r.left() + widths[0] - 6.0, r.bottom() - 2.0),
            );
            name_or_edit(
                ui,
                name_rect,
                item,
                &mut renaming,
                13.0,
                text,
                false,
                &mut ev.rename_done,
            );
            let mut x = r.left() + widths[0];
            if widths[1] > 0.0 {
                x += widths[1];
                if let Some(mode) = item.perms {
                    painter.text(
                        egui::pos2(x - 10.0, r.center().y),
                        Align2::RIGHT_CENTER,
                        explorer::mode_text(mode, item.is_dir),
                        FontId::monospace(11.5),
                        theme::MUTED(),
                    );
                }
            }
            if widths[2] > 0.0 {
                x += widths[2];
                if !item.is_dir {
                    painter.text(
                        egui::pos2(x - 10.0, r.center().y),
                        Align2::RIGHT_CENTER,
                        size_text(item.size),
                        FontId::proportional(12.0),
                        theme::MUTED(),
                    );
                }
            }
            if widths[3] > 0.0 {
                painter.text(
                    egui::pos2(r.right() - 10.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    time_text(item.mtime, offset),
                    FontId::proportional(12.0),
                    theme::MUTED(),
                );
            }
            if resp.clicked() {
                ev.clicked = Some((index, ui.input(|i| i.modifiers)));
            }
            if resp.double_clicked() {
                ev.opened = Some(index);
            }
            if resp.drag_started() {
                ev.drag_from = Some(index);
            }
            let local = pane.is_local();
            resp.context_menu(|ui| item_menu(ui, index, local, item.is_dir, &mut ev.menu));
        }
    });
    empty_space(ui, list.inner_rect, list.content_size.y, ev);
}

/// The part of the list below the last row: a click there picks nothing, a right click
/// offers a new folder and paste.
fn empty_space(ui: &mut Ui, inner: Rect, content: f32, ev: &mut Events) {
    let rest = Rect::from_min_max(
        egui::pos2(inner.left(), inner.top() + content.min(inner.height())),
        inner.max,
    );
    if rest.height() > 4.0 {
        let resp = ui.interact(rest, ui.id().with("rest"), Sense::click());
        if resp.clicked() {
            ev.empty_clicked = true;
        }
        resp.context_menu(|ui| empty_menu(ui, &mut ev.empty_menu));
    }
}

#[allow(clippy::too_many_arguments)]
fn tiles(
    ui: &mut Ui,
    pane: &mut Pane,
    items: &[Item],
    mut renaming: Option<&mut explorer::Renaming>,
    focused: bool,
    dragging: bool,
    scroll_to: Option<String>,
    ev: &mut Events,
    thumbs: &mut Thumbs,
) {
    let per_row = ((ui.available_width() / TILE.x).floor() as usize).max(1);
    let rows = items.len().div_ceil(per_row);
    let mut area = ScrollArea::vertical().id_salt("tiles").auto_shrink(false);
    if let Some(name) = &scroll_to {
        if let Some(at) = items.iter().position(|i| &i.name == name) {
            area = area.vertical_scroll_offset(((at / per_row) as f32 * TILE.y - 40.0).max(0.0));
        }
    }
    let local = pane.is_local();
    let list = area.show_rows(ui, TILE.y, rows, |ui, range| {
        for row in range {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for index in row * per_row..((row + 1) * per_row).min(items.len()) {
                    let item = &items[index];
                    let selected = pane.selected.contains(&item.name);
                    let (r, resp) = ui.allocate_exact_size(TILE, Sense::click_and_drag());
                    let fill = if selected {
                        if focused {
                            theme::ACCENT().gamma_multiply(0.22)
                        } else {
                            theme::tint(14)
                        }
                    } else if resp.hovered() && !dragging {
                        theme::tint(7)
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(r.shrink(3.0), 10.0, fill);
                    let icon_rect = Rect::from_center_size(
                        egui::pos2(r.center().x, r.top() + 36.0),
                        vec2(56.0, 56.0),
                    );
                    let path = pane.child(&item.name);
                    match local
                        .then(|| thumbs.get(ui.ctx(), &path, &item.name))
                        .flatten()
                    {
                        Some((texture, size)) => {
                            let scale = (56.0 / size.x).min(56.0 / size.y);
                            let shown = Rect::from_center_size(icon_rect.center(), size * scale);
                            ui.painter().image(
                                texture,
                                shown,
                                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        None => {
                            let (glyph, mut color) =
                                icons::for_entry(&item.name, item.is_dir, false);
                            if item.hidden {
                                color = color.gamma_multiply(0.5);
                            }
                            ui.painter().text(
                                icon_rect.center(),
                                Align2::CENTER_CENTER,
                                glyph,
                                FontId::proportional(40.0),
                                color,
                            );
                        }
                    }
                    let name_rect = Rect::from_min_max(
                        egui::pos2(r.left() + 6.0, r.top() + 68.0),
                        egui::pos2(r.right() - 6.0, r.bottom() - 4.0),
                    );
                    let color = if item.hidden {
                        theme::COMMENT()
                    } else {
                        theme::TEXT()
                    };
                    name_or_edit(
                        ui,
                        name_rect,
                        item,
                        &mut renaming,
                        12.0,
                        color,
                        true,
                        &mut ev.rename_done,
                    );
                    if resp.clicked() {
                        ev.clicked = Some((index, ui.input(|i| i.modifiers)));
                    }
                    if resp.double_clicked() {
                        ev.opened = Some(index);
                    }
                    if resp.drag_started() {
                        ev.drag_from = Some(index);
                    }
                    resp.on_hover_text(&item.name)
                        .context_menu(|ui| item_menu(ui, index, local, item.is_dir, &mut ev.menu));
                }
            });
        }
    });
    empty_space(ui, list.inner_rect, list.content_size.y, ev);
}

/// This PC: the user's folders, then every drive with how full it is.
fn this_pc(
    ui: &mut Ui,
    drives: &[winshell::Drive],
    known: &[KnownFolder],
    act: &mut impl FnMut(PaneAct),
) {
    ScrollArea::vertical()
        .id_salt("pc")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.add_space(6.0);
            ui.label(
                RichText::new("FOLDERS")
                    .size(11.5)
                    .strong()
                    .color(theme::COMMENT()),
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for k in known {
                    let (r, resp) = ui.allocate_exact_size(vec2(150.0, 44.0), Sense::click());
                    let fill = if resp.hovered() {
                        theme::tint(10)
                    } else {
                        theme::tint(5)
                    };
                    ui.painter().rect_filled(r, 10.0, fill);
                    ui.painter().text(
                        r.left_center() + vec2(14.0, 0.0),
                        Align2::LEFT_CENTER,
                        k.icon,
                        FontId::proportional(16.0),
                        theme::ACCENT(),
                    );
                    ui.painter().text(
                        r.left_center() + vec2(40.0, 0.0),
                        Align2::LEFT_CENTER,
                        &k.name,
                        FontId::proportional(13.0),
                        theme::TEXT(),
                    );
                    if resp
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        act(PaneAct::Open(k.path.clone()));
                    }
                }
            });
            ui.add_space(14.0);
            ui.label(
                RichText::new("DEVICES AND DRIVES")
                    .size(11.5)
                    .strong()
                    .color(theme::COMMENT()),
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for d in drives {
                    let (r, resp) = ui.allocate_exact_size(vec2(260.0, 70.0), Sense::click());
                    let fill = if resp.hovered() {
                        theme::tint(10)
                    } else {
                        theme::tint(5)
                    };
                    ui.painter().rect_filled(r, 12.0, fill);
                    let glyph = drive_icon(d);
                    ui.painter().text(
                        r.left_center() + vec2(18.0, 0.0),
                        Align2::LEFT_CENTER,
                        glyph,
                        FontId::proportional(22.0),
                        theme::ACCENT(),
                    );
                    ui.painter().text(
                        egui::pos2(r.left() + 52.0, r.top() + 16.0),
                        Align2::LEFT_CENTER,
                        d.title(),
                        FontId::proportional(13.5),
                        theme::TEXT(),
                    );
                    if d.total > 0 {
                        let used = 1.0 - d.free as f32 / d.total as f32;
                        let bar = Rect::from_min_size(
                            egui::pos2(r.left() + 52.0, r.top() + 32.0),
                            vec2(r.width() - 70.0, 7.0),
                        );
                        ui.painter().rect_filled(bar, 4.0, theme::tint(18));
                        let color = if used > 0.9 { ui::RED } else { theme::ACCENT() };
                        ui.painter().rect_filled(
                            Rect::from_min_size(bar.min, vec2(bar.width() * used, bar.height())),
                            4.0,
                            color,
                        );
                        let text = format!("{} free of {}", size_text(d.free), size_text(d.total));
                        ui.painter().text(
                            egui::pos2(r.left() + 52.0, r.top() + 52.0),
                            Align2::LEFT_CENTER,
                            text,
                            FontId::proportional(11.5),
                            theme::COMMENT(),
                        );
                    } else {
                        ui.painter().text(
                            egui::pos2(r.left() + 52.0, r.top() + 40.0),
                            Align2::LEFT_CENTER,
                            "No disk",
                            FontId::proportional(11.5),
                            theme::COMMENT(),
                        );
                    }
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                    if resp.double_clicked() || resp.clicked() {
                        act(PaneAct::Open(d.root.clone()));
                    }
                }
            });
        });
}

/// A drive's glyph: a disk, a USB stick, or a network place.
pub(super) fn drive_icon(d: &winshell::Drive) -> char {
    match d.kind {
        winshell::DriveKind::Network => icons::REMOTE,
        winshell::DriveKind::Removable => ui::USB,
        _ => ui::DRIVE,
    }
}

/// One of the user's folders, as This PC and the list on the left show it.
#[derive(Clone)]
pub(super) struct KnownFolder {
    pub name: String,
    pub path: String,
    pub icon: char,
}

/// Small pictures of images here, made on a thread and kept.
#[derive(Default)]
pub(super) struct Thumbs {
    done: HashMap<String, Option<(egui::TextureHandle, Vec2)>>,
    asked: HashSet<String>,
    rx: Option<std::sync::mpsc::Receiver<(String, Option<egui::ColorImage>)>>,
    tx: Option<std::sync::mpsc::Sender<(String, Option<egui::ColorImage>)>>,
}

impl Thumbs {
    fn get(&mut self, ctx: &Context, path: &str, name: &str) -> Option<(egui::TextureId, Vec2)> {
        if !matches!(preview::kind_of(name), preview::Kind::Image) {
            return None;
        }
        if self.tx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.tx = Some(tx);
            self.rx = Some(rx);
        }
        while let Some(Ok((p, img))) = self.rx.as_ref().map(|rx| rx.try_recv()) {
            let entry = img.map(|img| {
                let size = vec2(img.size[0] as f32, img.size[1] as f32);
                (
                    ctx.load_texture(format!("thumb:{p}"), img, egui::TextureOptions::LINEAR),
                    size,
                )
            });
            self.done.insert(p, entry);
        }
        if let Some(found) = self.done.get(path) {
            return found.as_ref().map(|(t, s)| (t.id(), *s));
        }
        if self.asked.insert(path.to_string()) {
            let (tx, path, repaint) = (self.tx.clone().unwrap(), path.to_string(), ctx.clone());
            std::thread::spawn(move || {
                let img = std::fs::metadata(&path)
                    .ok()
                    .filter(|m| m.len() < 40 * 1024 * 1024)
                    .and_then(|_| image::open(&path).ok())
                    .map(|img| {
                        let t = img.thumbnail(128, 128).to_rgba8();
                        egui::ColorImage::from_rgba_unmultiplied(
                            [t.width() as usize, t.height() as usize],
                            t.as_raw(),
                        )
                    });
                let _ = tx.send((path, img));
                repaint.request_repaint();
            });
        }
        None
    }
}
