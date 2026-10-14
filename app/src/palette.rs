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
    ToggleGlass,
    Disconnect,
}

/// One row of the palette.
struct Entry {
    icon: char,
    icon_color: Color32,
    /// "Group: what it does"; the group is shown dimmed.
    label: String,
    keys: &'static str,
    command: Command,
}

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
        }
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
            Command::ToggleGlass => self.themes.glass = !self.themes.glass,
            Command::Disconnect => self.disconnect(),
        }
        self.palette.open = false;
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

        let entries: Vec<Entry> = if self.palette.files_mode {
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
                        command: Command::OpenFile(join(&root, file)),
                    }
                })
                .collect()
        } else {
            let words: Vec<String> = self
                .palette
                .query
                .to_lowercase()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            self.commands()
                .into_iter()
                .filter(|entry| {
                    let label = entry.label.to_lowercase();
                    words.iter().all(|w| label.contains(w))
                })
                .collect()
        };

        // The arrow keys move the highlight, so they are taken before the text box sees them.
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if down {
            self.palette.selected += 1;
        }
        if up {
            self.palette.selected = self.palette.selected.saturating_sub(1);
        }
        self.palette.selected = self.palette.selected.min(entries.len().saturating_sub(1));

        // A theme under the highlight is shown right away, as a preview; moving off it,
        // or closing the palette without choosing, puts the chosen theme back.
        let previewed = match entries
            .get(self.palette.selected)
            .map(|entry| &entry.command)
        {
            Some(Command::Theme(name)) => Some(name.clone()),
            _ => None,
        };
        self.preview_theme(ctx, previewed.as_deref());

        // Everything behind the palette dims, so it reads as the one thing to deal with.
        let backdrop = egui::LayerId::new(egui::Order::Middle, egui::Id::new("palette_backdrop"));
        ctx.layer_painter(backdrop).rect_filled(
            ctx.content_rect(),
            0.0,
            Color32::from_black_alpha(if previewed.is_some() { 24 } else { 110 }),
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
                    let hint = if self.palette.files_mode {
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
        });
    }
}
