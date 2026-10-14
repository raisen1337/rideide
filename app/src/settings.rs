//! Everything the user can adjust, kept in one place: the values, where they are stored,
//! and the page that edits them.

use super::*;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Indent {
    /// Follow what the file already uses.
    Auto,
    Spaces,
    Tabs,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CatchUp {
    Off,
    Daily,
    EveryConnect,
}

/// A formatter: the file extensions it is for (space-separated) and its command, in
/// which `{file}` stands for the file's path.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Formatter {
    pub exts: String,
    pub command: String,
}

pub fn default_formatters() -> Vec<Formatter> {
    [
        (
            "js jsx ts tsx mjs cjs json css scss less html vue md yaml yml",
            // Through npx, so it works wherever Node is, and uses a project's own copy first.
            "npx --yes prettier --stdin-filepath {file}",
        ),
        ("lua", "npx --yes @johnnymorganz/stylua-bin -"),
        ("py", "black -q -"),
        ("rs", "rustfmt --edition 2021 --emit stdout"),
        ("go", "gofmt"),
        ("sh bash", "shfmt"),
        (
            "c h cpp hpp cc cs java",
            "clang-format --assume-filename={file}",
        ),
    ]
    .into_iter()
    .map(|(exts, command)| Formatter {
        exts: exts.into(),
        command: command.into(),
    })
    .collect()
}

/// The formatter command for a file, with its path filled in.
pub fn formatter_for(path: &str) -> Option<String> {
    let ext = path.rsplit_once('.')?.1.to_lowercase();
    with(|s| {
        s.formatters
            .iter()
            .find(|f| {
                f.exts
                    .split_whitespace()
                    .any(|e| e.trim_start_matches('.') == ext)
            })
            .filter(|f| !f.command.trim().is_empty())
            .map(|f| f.command.replace("{file}", &crate::tools::quote(path)))
    })
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Appearance
    /// Scale of the whole interface.
    pub zoom: f32,
    pub code_size: f32,
    pub term_size: f32,
    /// Extra space between lines of code, in points.
    pub line_gap: f32,
    pub splash: bool,
    // Editor
    pub tab_width: usize,
    pub indent: Indent,
    pub auto_indent: bool,
    pub line_numbers: bool,
    pub highlight_line: bool,
    pub cursor_blink: bool,
    pub trim_on_save: bool,
    pub newline_on_save: bool,
    /// Seconds without typing after which an edited file is saved; 0 turns it off.
    pub autosave: f32,
    pub minimap: bool,
    /// Who last changed the cursor's line, and when, at its end.
    pub blame: bool,
    /// The file's path and the function the cursor is in, above the code.
    pub breadcrumbs: bool,
    pub format_on_save: bool,
    /// Which program formats which files; run on the host, text in on stdin, out on stdout.
    pub formatters: Vec<Formatter>,
    // Files
    pub confirm_delete: bool,
    /// Files above this size, in megabytes, aren't opened.
    pub max_open_mb: u32,
    // Terminal
    pub scrollback: usize,
    // Claude
    /// The permission mode new chats start in: "default", "acceptEdits" or "auto".
    pub chat_mode: String,
    pub edit_model: String,
    pub search_model: String,
    pub catchup_model: String,
    pub catchup: CatchUp,
    pub team_budget: u32,
    pub notifications: bool,
    // Connection
    pub reconnect: bool,
    /// Set Claude Code on a host up to show previews through ride.
    pub claude_previews: bool,
    // Updates
    /// Check for a new version once a day, download it, and install it when ride closes.
    pub auto_update: bool,
    /// Edit files together with others on the same host.
    pub live_editing: bool,
    /// The name others see in live editing when there's no GitHub sign-in.
    pub nickname: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            code_size: 14.0,
            term_size: 14.0,
            line_gap: 4.0,
            splash: true,
            tab_width: 4,
            indent: Indent::Auto,
            auto_indent: true,
            line_numbers: true,
            highlight_line: true,
            cursor_blink: true,
            trim_on_save: false,
            newline_on_save: false,
            autosave: 0.0,
            minimap: true,
            blame: true,
            breadcrumbs: true,
            format_on_save: false,
            formatters: default_formatters(),
            confirm_delete: true,
            max_open_mb: 64,
            scrollback: 10_000,
            chat_mode: "default".into(),
            edit_model: "sonnet".into(),
            search_model: "sonnet".into(),
            catchup_model: "sonnet".into(),
            catchup: CatchUp::Daily,
            team_budget: 40,
            notifications: true,
            reconnect: true,
            claude_previews: true,
            auto_update: true,
            live_editing: true,
            nickname: String::new(),
        }
    }
}

thread_local! {
    // Read all over the interface, which runs on one thread.
    static CURRENT: RefCell<Settings> = RefCell::new(Settings::default());
}

/// Reads something from the current settings.
pub fn with<T>(read: impl FnOnce(&Settings) -> T) -> T {
    CURRENT.with(|current| read(&current.borrow()))
}

pub fn get() -> Settings {
    with(Settings::clone)
}

fn set(settings: Settings) {
    CURRENT.with(|current| *current.borrow_mut() = settings);
}

/// Changes a setting from elsewhere than the settings page (the command palette), and
/// stores it.
pub fn change(edit: impl FnOnce(&mut Settings)) {
    let mut settings = get();
    edit(&mut settings);
    let settings = sane(settings);
    let _ = save(&settings);
    set(settings);
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("ride").join("settings.json"))
}

/// Loads the stored settings; anything missing or unreadable keeps its default.
pub fn load() {
    let stored = path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str::<Settings>(&text).ok());
    if let Some(settings) = stored {
        set(sane(settings));
    }
}

/// Keeps hand-edited or outdated values inside what the interface can work with.
fn sane(mut s: Settings) -> Settings {
    s.zoom = s.zoom.clamp(0.7, 2.0);
    s.code_size = s.code_size.clamp(8.0, 32.0);
    s.term_size = s.term_size.clamp(8.0, 32.0);
    s.line_gap = s.line_gap.clamp(0.0, 16.0);
    s.tab_width = s.tab_width.clamp(1, 8);
    s.autosave = s.autosave.clamp(0.0, 60.0);
    s.max_open_mb = s.max_open_mb.clamp(1, 200);
    s.scrollback = s.scrollback.clamp(100, 200_000);
    s.team_budget = s.team_budget.clamp(1, 500);
    s
}

fn save(settings: &Settings) -> std::io::Result<()> {
    let Some(path) = path() else { return Ok(()) };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?,
    )
}

pub fn chat_mode() -> chat::Mode {
    with(|s| match s.chat_mode.as_str() {
        "acceptEdits" => chat::Mode::Edit,
        "auto" => chat::Mode::Auto,
        _ => chat::Mode::Ask,
    })
}

const SECTIONS: [&str; 8] = [
    "Appearance",
    "Editor",
    "Files",
    "Terminal",
    "Claude",
    "Connection",
    "Updates",
    "Account",
];
const MODELS: [(&str, &str); 4] = [
    ("haiku", "Haiku"),
    ("sonnet", "Sonnet"),
    ("opus", "Opus"),
    ("fable", "Fable"),
];

/// One row of the page: what the setting is, a line on what it does, and its control.
fn row(ui: &mut Ui, title: &str, help: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - 300.0).max(160.0));
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(title).color(theme::STRONG()));
            ui.label(RichText::new(help).small().color(theme::COMMENT()));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| control(ui));
    });
    ui.add_space(10.0);
}

fn toggle(ui: &mut Ui, value: &mut bool) {
    ui.checkbox(value, "");
}

fn pick<T: PartialEq + Copy>(ui: &mut Ui, salt: &str, value: &mut T, options: &[(T, &str)]) {
    let shown = options
        .iter()
        .find(|(option, _)| option == value)
        .map_or("", |(_, label)| label);
    egui::ComboBox::from_id_salt(salt)
        .selected_text(shown)
        .width(170.0)
        .show_ui(ui, |ui| {
            for (option, label) in options {
                ui.selectable_value(value, *option, *label);
            }
        });
}

fn pick_text(ui: &mut Ui, salt: &str, value: &mut String, options: &[(&str, &str)]) {
    let shown = options
        .iter()
        .find(|(option, _)| option == value)
        .map_or(value.as_str(), |(_, label)| label)
        .to_string();
    egui::ComboBox::from_id_salt(salt)
        .selected_text(shown)
        .width(170.0)
        .show_ui(ui, |ui| {
            for (option, label) in options {
                if ui.selectable_label(value == option, *label).clicked() {
                    *value = option.to_string();
                }
            }
        });
}

impl App {
    /// Applies what the settings ask of the window itself. Called every frame; cheap when nothing changed.
    pub(super) fn apply_settings(&mut self, ctx: &Context) {
        let wanted = with(|s| s.zoom);
        let shown = ctx.zoom_factor();
        if (wanted - self.applied_zoom).abs() > 0.001 {
            // Changed in Settings (or the first frame): put on the window.
            ctx.set_zoom_factor(wanted);
            self.applied_zoom = wanted;
        } else if (shown - self.applied_zoom).abs() > 0.001 {
            // Ctrl+ / Ctrl- / Ctrl+0 changed it: kept, and remembered for next time.
            let zoom = shown.clamp(0.7, 2.0);
            if (zoom - shown).abs() > 0.001 {
                ctx.set_zoom_factor(zoom);
            }
            change(|s| s.zoom = zoom);
            self.applied_zoom = zoom;
        }
    }

    /// Applies the on-save clean-ups the settings ask for, as an edit that can be undone.
    pub(super) fn tidy_before_save(&mut self, index: usize) {
        let (trim, newline) = with(|s| (s.trim_on_save, s.newline_on_save));
        let Some(tab) = self.tabs.get_mut(index).filter(|t| t.preview.is_none()) else {
            return;
        };
        if !trim && !newline {
            return;
        }
        let text = tab.buf.text();
        let mut tidy = text.clone();
        if trim {
            tidy = tidy
                .split('\n')
                .map(|line| line.trim_end_matches([' ', '\t']))
                .collect::<Vec<_>>()
                .join("\n");
        }
        if newline && !tidy.is_empty() && !tidy.ends_with('\n') {
            tidy.push('\n');
        }
        if tidy != text {
            let cursor = tab.buf.cursor;
            tab.buf.select_all();
            tab.buf.insert(&tidy);
            tab.buf.set_cursor(cursor, false);
        }
    }

    /// Saves edited files once the user has stopped typing for the time set.
    pub(super) fn autosave_tick(&mut self, ctx: &Context) {
        let delay = with(|s| s.autosave) as f64;
        if delay <= 0.0 || !self.online() {
            return;
        }
        let now = ctx.input(|i| i.time);
        let mut due = Vec::new();
        let mut waiting = false;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let revision = tab.buf.revision;
            let (seen, changed_at, saved) = &mut tab.edited;
            if *seen != revision {
                (*seen, *changed_at) = (revision, now);
            }
            if tab.buf.is_dirty() && tab.preview.is_none() && *saved != revision {
                if now - *changed_at >= delay {
                    *saved = revision;
                    due.push(index);
                } else {
                    waiting = true;
                }
            }
        }
        for index in due {
            self.save_tab(index);
        }
        // Nothing else would wake the window when the delay runs out.
        if waiting {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// The settings page: sections on the left, the chosen section's rows on the right.
    pub(super) fn settings_view(&mut self, ui: &mut Ui) {
        let before = get();
        let mut s = before.clone();
        let width = ui.available_width();
        if width >= 620.0 {
            Panel::left("settings_sections")
                .resizable(false)
                .exact_size(210.0)
                .show_separator_line(true)
                .frame(theme::card(theme::glass()).inner_margin(Margin::same(14)))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("SETTINGS")
                            .small()
                            .strong()
                            .color(theme::COMMENT()),
                    );
                    ui.add_space(4.0);
                    for (index, name) in SECTIONS.iter().enumerate() {
                        let selected = self.settings_section == index;
                        let button = egui::Button::selectable(selected, *name)
                            .min_size(vec2(ui.available_width(), 0.0));
                        if ui.add(button).clicked() {
                            self.settings_section = index;
                        }
                    }
                    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                        if ui.button("Reset everything to defaults").clicked() {
                            s = Settings::default();
                        }
                    });
                });
        }
        CentralPanel::default_margins()
            .frame(Frame::NONE.fill(theme::surface()).inner_margin(Margin {
                left: 30,
                right: 24,
                top: 22,
                bottom: 0,
            }))
            .show(ui, |ui| {
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.set_max_width(760.0);
                    // On a narrow window all sections follow each other, as there is no list to pick from.
                    let shown: Vec<usize> = if width >= 620.0 {
                        vec![self.settings_section]
                    } else {
                        (0..SECTIONS.len()).collect()
                    };
                    for section in shown {
                        ui.label(
                            RichText::new(SECTIONS[section])
                                .size(24.0)
                                .color(theme::STRONG()),
                        );
                        ui.add_space(14.0);
                        self.settings_section_rows(ui, section, &mut s);
                        ui.add_space(16.0);
                    }
                });
            });

        let s = sane(s);
        if s != before {
            if let Err(error) = save(&s) {
                self.status = format!("Couldn't save the settings: {error}");
            }
            set(s);
        }
    }

    fn settings_section_rows(&mut self, ui: &mut Ui, section: usize, s: &mut Settings) {
        match section {
            0 => {
                row(
                    ui,
                    "Theme",
                    "Colour themes, file icons and the glass window are in the Themes view.",
                    |ui| {
                        if ui.button("Open Themes").clicked() {
                            self.view = View::Themes;
                        }
                    },
                );
                row(
                    ui,
                    "Interface scale",
                    "Makes everything larger or smaller.",
                    |ui| {
                        ui.add(
                            egui::Slider::new(&mut s.zoom, 0.7..=2.0)
                                .step_by(0.05)
                                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                        );
                    },
                );
                row(ui, "Code font size", "Text size in the editor.", |ui| {
                    ui.add(egui::Slider::new(&mut s.code_size, 8.0..=32.0).step_by(0.5));
                });
                row(ui, "Terminal font size", "Text size in terminals.", |ui| {
                    ui.add(egui::Slider::new(&mut s.term_size, 8.0..=32.0).step_by(0.5));
                });
                row(
                    ui,
                    "Line spacing",
                    "Extra space between lines of code.",
                    |ui| {
                        ui.add(egui::Slider::new(&mut s.line_gap, 0.0..=16.0).step_by(1.0));
                    },
                );
                row(
                    ui,
                    "Start screen",
                    "Show the animated logo while ride starts.",
                    |ui| toggle(ui, &mut s.splash),
                );
            }
            1 => {
                row(
                    ui,
                    "Tab width",
                    "How wide a tab is, and how many spaces an indent step has.",
                    |ui| {
                        ui.add(egui::Slider::new(&mut s.tab_width, 1..=8));
                    },
                );
                row(ui, "Indent with", "What the Tab key inserts.", |ui| {
                    pick(
                        ui,
                        "indent",
                        &mut s.indent,
                        &[
                            (Indent::Auto, "What the file uses"),
                            (Indent::Spaces, "Spaces"),
                            (Indent::Tabs, "Tabs"),
                        ],
                    );
                });
                row(
                    ui,
                    "Keep indentation",
                    "A new line starts at the indentation of the one before.",
                    |ui| toggle(ui, &mut s.auto_indent),
                );
                row(ui, "Line numbers", "Show them beside the code.", |ui| {
                    toggle(ui, &mut s.line_numbers)
                });
                row(
                    ui,
                    "Highlight the current line",
                    "A faint band behind the line the cursor is on.",
                    |ui| toggle(ui, &mut s.highlight_line),
                );
                row(
                    ui,
                    "Blinking cursor",
                    "Off keeps the cursor steady.",
                    |ui| toggle(ui, &mut s.cursor_blink),
                );
                row(
                    ui,
                    "Trim trailing spaces on save",
                    "Removes spaces and tabs at the ends of lines.",
                    |ui| toggle(ui, &mut s.trim_on_save),
                );
                row(
                    ui,
                    "End files with a newline",
                    "Adds one on save when it is missing.",
                    |ui| toggle(ui, &mut s.newline_on_save),
                );
                row(
                    ui,
                    "Autosave",
                    "Saves an edited file after you stop typing for this long.",
                    |ui| {
                        pick(
                            ui,
                            "autosave",
                            &mut s.autosave,
                            &[
                                (0.0, "Off"),
                                (1.0, "After 1 second"),
                                (3.0, "After 3 seconds"),
                                (10.0, "After 10 seconds"),
                            ],
                        );
                    },
                );
                row(
                    ui,
                    "Minimap",
                    "The whole file in miniature at the right; click or drag in it to scroll.",
                    |ui| toggle(ui, &mut s.minimap),
                );
                row(
                    ui,
                    "Blame on the current line",
                    "Who last changed the line the cursor is on, when, and the commit's message, faintly at its end.",
                    |ui| toggle(ui, &mut s.blame),
                );
                row(
                    ui,
                    "Breadcrumbs",
                    "The file's folders and the function you are in, above the code. Click one to jump.",
                    |ui| toggle(ui, &mut s.breadcrumbs),
                );
                row(
                    ui,
                    "Format on save",
                    "Runs the file's formatter before saving. Shift+Alt+F formats without saving.",
                    |ui| toggle(ui, &mut s.format_on_save),
                );
                ui.add_space(4.0);
                ui.label(RichText::new("Formatters").color(theme::STRONG()));
                ui.label(
                    RichText::new(
                        "Run on the host: the text goes in on stdin and the formatted text comes out on stdout. {file} is the file's path. A formatter that isn't installed there leaves the file as it is.",
                    )
                    .small()
                    .color(theme::COMMENT()),
                );
                ui.add_space(6.0);
                let mut remove = None;
                for (index, formatter) in s.formatters.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut formatter.exts)
                                .desired_width(170.0)
                                .hint_text("extensions")
                                .font(egui::TextStyle::Monospace),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut formatter.command)
                                .desired_width((ui.available_width() - 40.0).max(120.0))
                                .hint_text("command")
                                .font(egui::TextStyle::Monospace),
                        );
                        if icon_button(ui, icons::CLOSE, "Remove") {
                            remove = Some(index);
                        }
                    });
                }
                if let Some(index) = remove {
                    s.formatters.remove(index);
                }
                ui.horizontal(|ui| {
                    if ui.button("Add a formatter").clicked() {
                        s.formatters.push(Formatter::default());
                    }
                    if ui.button("Restore the defaults").clicked() {
                        s.formatters = default_formatters();
                    }
                });
                ui.add_space(10.0);
            }
            2 => {
                row(
                    ui,
                    "Confirm before deleting",
                    "Ask before a file or folder is removed from the host.",
                    |ui| toggle(ui, &mut s.confirm_delete),
                );
                row(
                    ui,
                    "Largest file to open",
                    "Bigger files are left for downloading; the whole file crosses the connection.",
                    |ui| {
                        ui.add(egui::Slider::new(&mut s.max_open_mb, 1..=200).suffix(" MB"));
                    },
                );
                row(
                    ui,
                    "Download folder",
                    "Where the save dialog starts. It follows the last place you chose.",
                    |ui| {
                        let shown = self
                            .download_dir
                            .as_ref()
                            .map_or("Not set yet".to_string(), |d| d.display().to_string());
                        ui.label(RichText::new(shown).small().color(theme::COMMENT()));
                    },
                );
            }
            3 => {
                row(ui, "Scrollback", "Lines a terminal keeps above the screen. Applies to terminals opened from now on.", |ui| {
                    ui.add(egui::Slider::new(&mut s.scrollback, 100..=200_000).logarithmic(true));
                });
                row(
                    ui,
                    "Position",
                    "Which edge of the editor the terminal sits on. You can also drag its bar.",
                    |ui| {
                        let mut dock = self.term_dock;
                        pick(
                            ui,
                            "dock",
                            &mut dock,
                            &[
                                (TermDock::Bottom, "Bottom"),
                                (TermDock::Top, "Top"),
                                (TermDock::Left, "Left"),
                                (TermDock::Right, "Right"),
                            ],
                        );
                        self.term_dock = dock;
                    },
                );
            }
            4 => {
                row(
                    ui,
                    "New chats start in",
                    "What Claude may do without asking, until you change it in the chat.",
                    |ui| {
                        pick_text(
                            ui,
                            "chat_mode",
                            &mut s.chat_mode,
                            &[
                                ("default", "Ask: asks before changes"),
                                ("acceptEdits", "Edit: asks for commands"),
                                ("auto", "Auto: edits and safe commands"),
                            ],
                        );
                    },
                );
                row(ui, "Inline edit model", "Used for Ctrl+K.", |ui| {
                    pick_text(ui, "edit_model", &mut s.edit_model, &MODELS)
                });
                row(
                    ui,
                    "AI search model",
                    "Used when the AI switch is on in the folder search.",
                    |ui| pick_text(ui, "search_model", &mut s.search_model, &MODELS),
                );
                row(
                    ui,
                    "Catch-up screen",
                    "\"Let's catch up\" after connecting.",
                    |ui| {
                        pick(
                            ui,
                            "catchup",
                            &mut s.catchup,
                            &[
                                (CatchUp::Off, "Never"),
                                (CatchUp::Daily, "Once a day"),
                                (CatchUp::EveryConnect, "Every time I connect"),
                            ],
                        );
                    },
                );
                row(ui, "Catch-up model", "Writes the catch-up summary.", |ui| {
                    pick_text(ui, "catchup_model", &mut s.catchup_model, &MODELS)
                });
                row(
                    ui,
                    "Team message limit",
                    "Messages agents may exchange per request of yours before they pause.",
                    |ui| {
                        ui.add(egui::Slider::new(&mut s.team_budget, 1..=500).logarithmic(true));
                    },
                );
                row(ui, "Notifications", "A Windows notification when Claude or an agent finishes while ride is in the background.", |ui| toggle(ui, &mut s.notifications));
            }
            7 => {
                let mut live = s.live_editing;
                let mut nickname = s.nickname.clone();
                self.account_rows(ui, &mut live, &mut nickname);
                s.live_editing = live;
                s.nickname = nickname;
            }
            6 => {
                row(ui, "Version", "The ride you are running.", |ui| {
                    ui.label(RichText::new(update::VERSION).color(theme::STRONG()));
                });
                row(
                    ui,
                    "Update automatically",
                    "Checks GitHub when ride opens, and every hour after, and restarts into a new version right away; one found while you work installs when you close ride. Tabs and chats come back.",
                    |ui| toggle(ui, &mut s.auto_update),
                );
                let state = match &self.updater.state {
                    update::State::Idle => String::new(),
                    update::State::Checking => "Checking…".into(),
                    update::State::UpToDate => "You have the newest version.".into(),
                    update::State::Downloading(version) => format!("Downloading {version}…"),
                    update::State::Ready { version, .. } => format!("{version} is ready."),
                    update::State::Failed(error) => error.clone(),
                };
                let ready = self.updater.ready().is_some();
                row(ui, "Check now", &state, |ui| {
                    if ready {
                        if ui.button("Restart and update").clicked() && self.updater.install(true) {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    } else if ui.button("Check for updates").clicked() {
                        self.updater.check_now(ui.ctx());
                    }
                });
                row(
                    ui,
                    "Releases",
                    "What changed in each version, and the setup program for another computer.",
                    |ui| {
                        // egui's own links don't reach the browser in this build.
                        if ui
                            .link("GitHub")
                            .on_hover_text(update::RELEASES_URL)
                            .clicked()
                        {
                            tools::open_in_browser(update::RELEASES_URL);
                        }
                    },
                );
            }
            _ => {
                row(
                    ui,
                    "Reconnect on start",
                    "Start connected to the host and folder you were on.",
                    |ui| toggle(ui, &mut s.reconnect),
                );
                row(
                    ui,
                    "Let Claude show previews",
                    "On a host you connect to, registers ride with Claude Code there (once) and adds a note to its memory, so it can forward a port and open your browser when you ask for a demo.",
                    |ui| toggle(ui, &mut s.claude_previews),
                );
                row(
                    ui,
                    "Hosts",
                    "Taken from your SSH configuration, plus \"This computer\".",
                    |ui| {
                        ui.label(
                            RichText::new("~/.ssh/config")
                                .small()
                                .monospace()
                                .color(theme::COMMENT()),
                        );
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_settings_tolerate_missing_and_wild_values() {
        // An older file with fewer fields: the rest take their defaults.
        let old: Settings =
            serde_json::from_str(r#"{ "code_size": 18.0, "tab_width": 2 }"#).unwrap();
        assert_eq!((old.code_size, old.tab_width), (18.0, 2));
        assert_eq!(old.scrollback, Settings::default().scrollback);
        // Values outside what the interface can use are pulled back in.
        let wild: Settings =
            serde_json::from_str(r#"{ "zoom": 40.0, "tab_width": 0, "code_size": 1.0 }"#).unwrap();
        let fixed = sane(wild);
        assert_eq!(
            (fixed.zoom, fixed.tab_width, fixed.code_size),
            (2.0, 1, 8.0)
        );
        // And a round trip keeps everything.
        let text = serde_json::to_string(&fixed).unwrap();
        assert!(serde_json::from_str::<Settings>(&text).unwrap() == fixed);
    }
}
