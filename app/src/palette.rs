//! The command palette (Ctrl+Shift+P) and the file finder (Ctrl+P).

use super::*;

#[derive(Clone)]
enum Command {
    GoToFile,
    OpenFile(String),
    Save,
    CloseTab,
    Find,
    SearchFolder,
    InlineEdit,
    ToggleFiles,
    ToggleTerminal,
    ToggleChat,
    NewTerminal,
    ViewCode,
    ViewAgents,
    ShowChanges,
    CommitMessage,
    NewChat,
    NewAgent,
    StopAgents,
    Themes,
    Settings,
    Theme(String),
    /// A Marketplace extension: its id, and whether it is a file icon theme.
    InstallExtension(String, bool),
    ToggleGlass,
    Disconnect,
    FormatDocument,
    ToggleFormatOnSave,
    FoldAll,
    UnfoldAll,
    ToggleMinimap,
    ToggleBreadcrumbs,
    /// Lists the open file's symbols (the palette with `@`).
    GoToSymbol,
    GoToLine(usize),
    /// Picks a second file to compare the open one with.
    CompareWith,
    Compare(String),
}

/// One row of the palette.
struct Entry {
    icon: char,
    icon_color: Color32,
    /// "Group: what it does"; the group is shown dimmed.
    label: String,
    keys: &'static str,
    /// A dim note at the right, for Marketplace results.
    detail: String,
    command: Command,
}

/// Results from the Marketplace, or why there are none.
type MarketResult = Result<Vec<themes::MarketHit>, String>;

#[derive(Default)]
pub(super) struct Palette {
    open: bool,
    /// Finding a file by name, rather than picking a command.
    files_mode: bool,
    query: String,
    selected: usize,
    focus: bool,
    /// Files found for `asked`.
    files: Vec<String>,
    asked: Option<String>,
    /// The Marketplace search: the text the results are for, the results, and the search
    /// under way with its text.
    market_for: String,
    market: Vec<themes::MarketHit>,
    market_error: String,
    market_rx: Option<(String, Receiver<MarketResult>)>,
    /// When the text last changed; the Marketplace is asked once typing pauses.
    typed_at: f64,
    /// The highlighted row and since when; a Marketplace row is previewed once it rests.
    resting: (usize, f64),
    /// A row to highlight once the list is that long (RIDE_PALETTE_ROW).
    want_row: Option<usize>,
    /// Picking the file to compare this one with, rather than one to open.
    compare: Option<String>,
}

impl App {
    pub(super) fn open_palette(&mut self, files_mode: bool) {
        self.palette = Palette {
            open: true,
            files_mode,
            focus: true,
            ..Palette::default()
        };
    }

    /// Opens the command palette with some text already typed (`@` for symbols).
    pub(super) fn open_palette_with(&mut self, text: &str) {
        self.open_palette(false);
        self.palette.query = text.to_string();
    }

    /// RIDE_PALETTE=TEXT opens the command palette with that text typed, and
    /// RIDE_PALETTE_ROW=N highlights row N, to look at the palette without typing.
    pub(super) fn palette_from_env(&mut self) {
        let Ok(text) = std::env::var("RIDE_PALETTE") else {
            return;
        };
        self.open_palette(false);
        self.palette.query = text;
        self.palette.typed_at = f64::NEG_INFINITY;
        self.palette.want_row = std::env::var("RIDE_PALETTE_ROW")
            .ok()
            .and_then(|row| row.parse().ok());
    }

    pub(super) fn palette_files_found(&mut self, query: &str, paths: Vec<String>) {
        if self.palette.asked.as_deref() == Some(query) {
            self.palette.files = paths;
        }
    }

    /// Every command that makes sense right now.
    fn commands(&self) -> Vec<Entry> {
        let mut list: Vec<Entry> = Vec::new();
        let mut add = |icon: char, label: &str, keys: &'static str, command: Command| {
            let icon_color = if label.starts_with("Claude") {
                theme::CLAUDE()
            } else {
                theme::COMMENT()
            };
            list.push(Entry {
                icon,
                icon_color,
                label: label.to_string(),
                keys,
                detail: String::new(),
                command,
            });
        };
        let online = self.online();
        if online {
            add(icons::SEARCH, "Go to file…", "Ctrl+P", Command::GoToFile);
        }
        if self.docked_active().is_some() {
            add(icons::SAVE, "File: Save", "Ctrl+S", Command::Save);
            add(icons::CLOSE, "File: Close tab", "Ctrl+W", Command::CloseTab);
            add(
                icons::SEARCH,
                "Editor: Find in file",
                "Ctrl+F",
                Command::Find,
            );
            add(
                icons::SPARKLE,
                "Claude: Edit selection",
                "Ctrl+K",
                Command::InlineEdit,
            );
            add(
                icons::EDIT,
                "Editor: Format document",
                "Shift+Alt+F",
                Command::FormatDocument,
            );
            add(
                icons::METHOD,
                "Go to symbol in file…",
                "Ctrl+Shift+O",
                Command::GoToSymbol,
            );
            add(
                icons::CHEVRON_RIGHT,
                "Editor: Fold all",
                "",
                Command::FoldAll,
            );
            add(
                icons::CHEVRON_DOWN,
                "Editor: Unfold all",
                "",
                Command::UnfoldAll,
            );
            if online {
                add(
                    icons::COMPARE,
                    "File: Compare with another file…",
                    "",
                    Command::CompareWith,
                );
            }
        }
        let on_off = |on: bool| if on { "off" } else { "on" };
        let (format, minimap, crumbs) =
            settings::with(|s| (s.format_on_save, s.minimap, s.breadcrumbs));
        add(
            icons::EDIT,
            &format!("Editor: Turn format on save {}", on_off(format)),
            "",
            Command::ToggleFormatOnSave,
        );
        add(
            icons::EYE,
            &format!("View: Turn the minimap {}", on_off(minimap)),
            "",
            Command::ToggleMinimap,
        );
        add(
            icons::EYE,
            &format!("View: Turn breadcrumbs {}", on_off(crumbs)),
            "",
            Command::ToggleBreadcrumbs,
        );
        if online {
            add(
                icons::SEARCH,
                "Search: Search in folder",
                "Ctrl+Shift+F",
                Command::SearchFolder,
            );
            add(icons::CODE, "View: Code", "", Command::ViewCode);
            add(icons::AGENTS, "View: Agents", "", Command::ViewAgents);
            add(
                icons::FILES,
                "View: Toggle files panel",
                "",
                Command::ToggleFiles,
            );
            add(
                icons::TERMINAL,
                "View: Toggle terminal",
                "Ctrl+`",
                Command::ToggleTerminal,
            );
            add(
                icons::SPARKLE,
                "View: Toggle Claude chat",
                "",
                Command::ToggleChat,
            );
            add(
                icons::DIFF,
                "View: Show what Claude changed",
                "",
                Command::ShowChanges,
            );
            add(
                icons::TERMINAL,
                "Terminal: New terminal",
                "",
                Command::NewTerminal,
            );
            add(icons::ADD, "Claude: New chat", "", Command::NewChat);
            add(
                icons::COMMIT,
                "Claude: Write a commit message",
                "",
                Command::CommitMessage,
            );
            add(icons::AGENTS, "Agents: New agent", "", Command::NewAgent);
            add(
                icons::STOP,
                "Agents: Stop all agents",
                "",
                Command::StopAgents,
            );
            add(
                icons::DISCONNECT,
                "Connection: Disconnect",
                "",
                Command::Disconnect,
            );
        }
        add(
            icons::PALETTE,
            "Appearance: Toggle glass window",
            "",
            Command::ToggleGlass,
        );
        add(
            icons::GEAR,
            "Settings: Open settings",
            "Ctrl+,",
            Command::Settings,
        );
        add(
            icons::PALETTE,
            "Themes: Browse the Marketplace",
            "",
            Command::Themes,
        );
        for name in self.theme_names() {
            add(
                icons::PALETTE,
                &format!("Theme: {name}"),
                "",
                Command::Theme(name),
            );
        }
        list
    }

    fn run_command(&mut self, ctx: &Context, command: Command) {
        match command {
            Command::GoToFile => return self.open_palette(true),
            Command::OpenFile(path) => {
                self.view = View::Code;
                self.open_file(path, None);
            }
            Command::Save => {
                if let Some(index) = self.docked_active() {
                    self.save_tab(index);
                }
            }
            Command::CloseTab => {
                if let Some(index) = self.docked_active() {
                    self.close_tab(index, false);
                }
            }
            Command::Find => {
                self.find = Some(self.find.take().unwrap_or_default());
                self.focus_find = true;
            }
            Command::SearchFolder => {
                if self.show_search {
                    self.focus_search = true;
                } else {
                    self.toggle_search();
                }
            }
            Command::InlineEdit => {
                if let Some(index) = self.docked_active() {
                    self.editor_action(index, editor::Action::InlineEdit);
                }
            }
            Command::ToggleFiles => self.show_side = !self.show_side,
            Command::ToggleTerminal => self.show_term = !self.show_term,
            Command::ToggleChat => self.show_chat = !self.show_chat,
            Command::NewTerminal => {
                self.view = View::Code;
                self.new_term();
            }
            Command::ViewCode => self.view = View::Code,
            Command::ViewAgents => self.view = View::Agents,
            Command::ShowChanges => {
                self.view = View::Code;
                self.side = Side::Changes;
                self.show_side = true;
            }
            Command::CommitMessage => self.commit_message(),
            Command::NewChat => {
                self.chat_stop();
                self.chat.reset();
                self.show_chat = true;
            }
            Command::NewAgent => {
                self.view = View::Agents;
                let mode = self.agents.first().map_or(chat::Mode::Ask, |a| a.chat.mode);
                self.active_agent = self.new_agent("agent", "", mode);
            }
            Command::StopAgents => {
                for index in 0..self.agents.len() {
                    self.stop_agent(index);
                }
            }
            Command::Themes => self.view = View::Themes,
            Command::Settings => self.view = View::Settings,
            Command::Theme(name) => self.select_theme(ctx, &name),
            Command::InstallExtension(id, _) => {
                self.install_market(ctx, &id);
                self.status = format!("Installing {id}…");
            }
            Command::ToggleGlass => self.themes.glass = !self.themes.glass,
            Command::Disconnect => self.disconnect(),
            Command::FormatDocument => {
                if let Some(index) = self.docked_active() {
                    self.format_document(index);
                }
            }
            Command::ToggleFormatOnSave => {
                settings::change(|s| s.format_on_save = !s.format_on_save);
                let on = settings::with(|s| s.format_on_save);
                self.status = format!("Format on save is {}", if on { "on" } else { "off" });
            }
            Command::ToggleMinimap => settings::change(|s| s.minimap = !s.minimap),
            Command::ToggleBreadcrumbs => settings::change(|s| s.breadcrumbs = !s.breadcrumbs),
            Command::FoldAll | Command::UnfoldAll => {
                if let Some(index) = self.docked_active() {
                    let view = &mut self.tabs[index].view;
                    if matches!(command, Command::FoldAll) {
                        view.fold_all();
                    } else {
                        view.unfold_all();
                    }
                }
            }
            Command::GoToSymbol => return self.open_palette_with("@"),
            Command::GoToLine(line) => {
                if let Some(index) = self.docked_active() {
                    nav::jump(&mut self.tabs[index], line);
                }
            }
            Command::CompareWith => {
                if let Some(index) = self.docked_active() {
                    let left = self.tabs[index].path.clone();
                    self.open_palette(true);
                    self.palette.compare = Some(left);
                }
                return;
            }
            Command::Compare(right) => {
                let left = self.palette.compare.take().unwrap_or_default();
                // The open text, unsaved edits included, is what is compared.
                let left_text = self
                    .tabs
                    .iter()
                    .find(|t| t.path == left)
                    .map(|t| t.buf.text())
                    .unwrap_or_default();
                self.request_then(
                    Request::ReadFile {
                        path: right.clone(),
                    },
                    Pending::Compare {
                        left,
                        left_text,
                        right,
                    },
                );
            }
        }
        self.palette.open = false;
        // A preview ends with the palette; a chosen theme is already the current one.
        self.end_icon_preview();
        self.preview_theme(ctx, None);
    }

    /// With `@` typed first, the open file's functions and types, narrowed by what
    /// follows; otherwise `None`.
    fn symbol_entries(&self) -> Option<Vec<Entry>> {
        let wanted = self.palette.query.strip_prefix('@')?;
        if self.palette.files_mode {
            return None;
        }
        let words: Vec<String> = wanted
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let Some(index) = self.docked_active() else {
            return Some(Vec::new());
        };
        let entries = self.tabs[index]
            .view
            .symbols
            .iter()
            .filter(|s| {
                let name = s.name.to_lowercase();
                words.iter().all(|w| name.contains(w))
            })
            .map(|s| {
                let (icon, icon_color) = nav::symbol_icon(s.kind);
                Entry {
                    icon,
                    icon_color,
                    label: s.name.clone(),
                    keys: "",
                    detail: format!("line {}", s.line + 1),
                    command: Command::GoToLine(s.line),
                }
            })
            .collect();
        Some(entries)
    }

    /// Asks the Marketplace about the palette's text once typing has paused, and takes in
    /// the answer. Two letters at least: anything shorter matches half the Marketplace.
    fn palette_market(&mut self, ctx: &Context) {
        if let Some((query, rx)) = &self.palette.market_rx {
            if let Ok(result) = rx.try_recv() {
                self.palette.market_for = query.clone();
                self.palette.market_rx = None;
                match result {
                    Ok(hits) => {
                        self.palette.market = hits;
                        self.palette.market_error.clear();
                    }
                    Err(error) => {
                        self.palette.market.clear();
                        self.palette.market_error = error;
                    }
                }
            }
        }
        let wanted = self.palette.query.trim().to_string();
        let asked = match &self.palette.market_rx {
            Some((query, _)) => query,
            None => &self.palette.market_for,
        };
        if wanted.chars().count() < 2 || *asked == wanted {
            return;
        }
        let wait = 0.35 - (ctx.input(|i| i.time) - self.palette.typed_at);
        if wait > 0.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
            return;
        }
        let (tx, rx) = channel();
        let (query, ctx) = (wanted.clone(), ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(themes::palette_search(&query));
            ctx.request_repaint();
        });
        self.palette.market_rx = Some((wanted, rx));
    }

    /// The Marketplace results for the text as it is now, as palette rows.
    fn market_entries(&self) -> Vec<Entry> {
        let wanted = self.palette.query.trim();
        if wanted.chars().count() < 2 || self.palette.market_for != wanted {
            return Vec::new();
        }
        // Asking for icons puts the icon themes first.
        let icons_first = wanted.to_lowercase().contains("icon");
        let mut hits: Vec<&themes::MarketHit> = self.palette.market.iter().collect();
        hits.sort_by_key(|hit| hit.icons != icons_first);
        hits.into_iter()
            .map(|hit| {
                let (installed, installing) = self.extension_state(&hit.id);
                let kind = if hit.icons {
                    "file icons"
                } else {
                    "colour theme"
                };
                let detail = if installing {
                    "Installing…".to_string()
                } else if installed {
                    "Installed".to_string()
                } else {
                    format!(
                        "{} · {} · {}",
                        hit.publisher,
                        kind,
                        themes::short_count(hit.installs)
                    )
                };
                Entry {
                    icon: if hit.icons {
                        icons::FILES
                    } else {
                        icons::PALETTE
                    },
                    icon_color: theme::COMMENT(),
                    label: format!("Marketplace: {}", hit.name),
                    keys: "",
                    detail,
                    command: Command::InstallExtension(hit.id.clone(), hit.icons),
                }
            })
            .collect()
    }

    pub(super) fn palette_window(&mut self, ctx: &Context) {
        if !self.palette.open {
            return;
        }
        // File names are looked up on the host as the query changes.
        if self.palette.files_mode
            && self.palette.asked.as_deref() != Some(self.palette.query.as_str())
        {
            let query = self.palette.query.clone();
            self.palette.asked = Some(query.clone());
            let request = Request::FindFiles {
                root: self.root.clone(),
                query: query.clone(),
            };
            self.request_then(request, Pending::PaletteFiles { query });
        }

        let entries: Vec<Entry> = if let Some(found) = self.symbol_entries() {
            found
        } else if self.palette.files_mode {
            let root = self.root.clone();
            self.palette
                .files
                .iter()
                .map(|file| {
                    let (icon, icon_color) = icons::for_entry(file_name(file), false, false);
                    Entry {
                        icon,
                        icon_color,
                        label: file.clone(),
                        keys: "",
                        detail: String::new(),
                        command: match self.palette.compare {
                            Some(_) => Command::Compare(join(&root, file)),
                            None => Command::OpenFile(join(&root, file)),
                        },
                    }
                })
                .collect()
        } else {
            self.palette_market(ctx);
            let words: Vec<String> = self
                .palette
                .query
                .to_lowercase()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let mut entries: Vec<Entry> = self
                .commands()
                .into_iter()
                .filter(|entry| {
                    let label = entry.label.to_lowercase();
                    words.iter().all(|w| label.contains(w))
                })
                .collect();
            entries.extend(self.market_entries());
            entries
        };
        let mut entries = entries;

        // The arrow keys move the highlight, so they are taken before the text box sees them.
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if let Some(row) = self.palette.want_row.filter(|row| *row < entries.len()) {
            self.palette.selected = row;
            self.palette.want_row = None;
        }
        if down {
            self.palette.selected += 1;
        }
        if up {
            self.palette.selected = self.palette.selected.saturating_sub(1);
        }
        self.palette.selected = self.palette.selected.min(entries.len().saturating_sub(1));

        // A theme under the highlight is shown right away, as a preview; moving off it,
        // or closing the palette without choosing, puts the chosen theme back. A
        // Marketplace one is fetched once the highlight rests on it: its colours then
        // show on the whole window, or its file icons in the tree.
        let now = ctx.input(|i| i.time);
        let selected = self.palette.selected;
        if self.palette.resting.0 != selected {
            self.palette.resting = (selected, now);
        }
        let highlighted = entries.get(selected).map(|entry| entry.command.clone());
        let (mut market_colours, mut market_icons) = (false, false);
        if let Some(Command::InstallExtension(id, icons)) = &highlighted {
            let rest = 0.3 - (now - self.palette.resting.1);
            if rest <= 0.0 {
                let note = self.market_preview(ctx, id, *icons);
                market_colours = !icons && note == "Previewing";
                market_icons = *icons && note.starts_with("Previewing");
                entries[selected].detail = note;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(rest));
            }
        }
        if !market_icons {
            self.end_icon_preview();
        }
        let previewed = match &highlighted {
            Some(Command::Theme(name)) => Some(name.clone()),
            _ => None,
        };
        if !market_colours {
            self.preview_theme(ctx, previewed.as_deref());
        }
        let previewing = previewed.is_some() || market_colours || market_icons;

        // Everything behind the palette dims, so it reads as the one thing to deal with.
        let backdrop = egui::LayerId::new(egui::Order::Middle, egui::Id::new("palette_backdrop"));
        ctx.layer_painter(backdrop).rect_filled(
            ctx.content_rect(),
            0.0,
            Color32::from_black_alpha(if previewing { 24 } else { 110 }),
        );

        let mut run = None;
        let area = egui::Area::new(egui::Id::new("command_palette"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, [0.0, 84.0])
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(ui.visuals().window_fill)
                    .stroke((1.0, theme::tint(34)))
                    .corner_radius(16)
                    .shadow(egui::Shadow {
                        offset: [0, 14],
                        blur: 40,
                        spread: 0,
                        color: Color32::from_black_alpha(130),
                    })
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(620.0_f32.min(ui.ctx().content_rect().width() - 44.0));
                        self.palette_contents(ui, &entries, up || down, &mut run);
                    });
            })
            .response;

        if enter {
            run = entries
                .get(self.palette.selected)
                .map(|entry| entry.command.clone())
                .or(run);
        }
        let clicked_away = ctx.input(|i| i.pointer.primary_clicked())
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| !area.rect.contains(p));
        if let Some(command) = run {
            self.run_command(ctx, command);
        } else if escape || clicked_away {
            self.palette.open = false;
            self.end_icon_preview();
            self.preview_theme(ctx, None);
        }
    }

    fn palette_contents(
        &mut self,
        ui: &mut Ui,
        entries: &[Entry],
        moved: bool,
        run: &mut Option<Command>,
    ) {
        // The search field.
        Frame::NONE
            .fill(theme::tint(9))
            .corner_radius(11)
            .inner_margin(Margin::symmetric(13, 9))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icons::SEARCH)
                            .size(16.0)
                            .color(theme::COMMENT()),
                    );
                    let hint = if self.palette.compare.is_some() {
                        "Compare with: type part of a file's name"
                    } else if self.palette.files_mode {
                        "Go to file: type part of its name"
                    } else {
                        "Type a command"
                    };
                    let edit = ui.add(
                        TextEdit::singleline(&mut self.palette.query)
                            .desired_width(f32::INFINITY)
                            .frame(Frame::NONE)
                            .font(FontId::proportional(16.0))
                            .hint_text(hint),
                    );
                    if std::mem::take(&mut self.palette.focus) {
                        edit.request_focus();
                    }
                    if edit.changed() {
                        self.palette.selected = 0;
                        self.palette.typed_at = ui.input(|i| i.time);
                    }
                });
            });
        ui.add_space(6.0);

        let pointer_moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
        ScrollArea::vertical().max_height(430.0).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (index, entry) in entries.iter().enumerate() {
                let selected = index == self.palette.selected;
                let row = Frame::NONE
                    .fill(if selected {
                        theme::tint(20)
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(9)
                    .inner_margin(Margin::symmetric(11, 7))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                // Shortcuts as keycaps, last key first in this right-to-left run.
                                for key in entry.keys.split('+').filter(|k| !k.is_empty()).rev() {
                                    Frame::NONE
                                        .fill(theme::tint(16))
                                        .stroke((1.0, theme::tint(26)))
                                        .corner_radius(5)
                                        .inner_margin(Margin::symmetric(6, 1))
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(key)
                                                        .small()
                                                        .color(theme::MUTED()),
                                                )
                                                .selectable(false),
                                            );
                                        });
                                }
                                if !entry.detail.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&entry.detail)
                                                .small()
                                                .color(theme::COMMENT()),
                                        )
                                        .selectable(false),
                                    );
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    let plain = |ui: &mut Ui, text: RichText| {
                                        ui.add(egui::Label::new(text).selectable(false))
                                    };
                                    plain(
                                        ui,
                                        RichText::new(format!("{}   ", entry.icon))
                                            .size(15.0)
                                            .color(entry.icon_color),
                                    );
                                    match entry.label.split_once(": ") {
                                        Some((group, name)) => {
                                            plain(
                                                ui,
                                                RichText::new(format!("{group}  "))
                                                    .color(theme::COMMENT()),
                                            );
                                            plain(ui, RichText::new(name).color(theme::STRONG()));
                                        }
                                        None => {
                                            plain(
                                                ui,
                                                RichText::new(&entry.label).color(theme::STRONG()),
                                            );
                                        }
                                    }
                                });
                            });
                        });
                    })
                    .response;
                if selected && moved {
                    row.scroll_to_me(None);
                }
                if row.contains_pointer() {
                    // The highlight follows the mouse only when the mouse actually moves.
                    if pointer_moved {
                        self.palette.selected = index;
                    }
                    if ui.input(|i| i.pointer.primary_clicked()) {
                        *run = Some(entry.command.clone());
                    }
                }
            }
            if entries.is_empty() {
                let text = if self.palette.files_mode {
                    "No files match"
                } else {
                    "No commands match"
                };
                ui.add_space(6.0);
                ui.label(RichText::new(format!("   {text}")).color(theme::COMMENT()));
                ui.add_space(6.0);
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(
                RichText::new("↑ ↓  to move      Enter  to run      Esc  to close")
                    .small()
                    .color(theme::COMMENT()),
            );
            if !self.palette.files_mode {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(6.0);
                    if self.palette.market_rx.is_some() {
                        ui.label(
                            RichText::new("Searching the Marketplace…")
                                .small()
                                .color(theme::COMMENT()),
                        );
                        ui.spinner();
                    } else if !self.palette.market_error.is_empty() {
                        let error = RichText::new("Marketplace unreachable")
                            .small()
                            .color(theme::COMMENT());
                        ui.label(error).on_hover_text(&self.palette.market_error);
                    }
                });
            }
        });
    }
}
