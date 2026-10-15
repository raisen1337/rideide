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

/// Where spaces and tabs are shown as faint marks.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Whitespace {
    None,
    Selection,
    All,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CursorStyle {
    Line,
    Block,
    Underline,
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

/// A language server: what it is called, the file extensions it is for (space-separated),
/// the command that starts it on the host, and the one that installs it there.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LangServer {
    pub name: String,
    pub exts: String,
    pub command: String,
    pub install: String,
}

/// Installs the Lua language server from its GitHub releases into ~/.local, for the
/// processor of the host.
const LUA_INSTALL: &str = r#"a=$(uname -m | sed 's/x86_64/x64/;s/aarch64/arm64/') && mkdir -p ~/.local/share/lua-language-server ~/.local/bin && curl -fsSL "$(curl -fsSL https://api.github.com/repos/LuaLS/lua-language-server/releases/latest | grep -o "https://[^\"]*linux-$a.tar.gz" | head -n 1)" | tar xz -C ~/.local/share/lua-language-server && printf '#!/bin/sh\nexec "$HOME/.local/share/lua-language-server/bin/lua-language-server" "$@"\n' > ~/.local/bin/lua-language-server && chmod +x ~/.local/bin/lua-language-server"#;

pub fn default_servers() -> Vec<LangServer> {
    [
        (
            "TypeScript",
            "ts tsx js jsx mjs cjs mts cts",
            // TypeScript 7 has its language server built in.
            "tsc --lsp --stdio",
            "npm install -g typescript",
        ),
        (
            "Python",
            "py pyi",
            "pyright-langserver --stdio",
            "npm install -g pyright",
        ),
        ("Lua", "lua", "lua-language-server", LUA_INSTALL),
        (
            "Rust",
            "rs",
            "rust-analyzer",
            "rustup component add rust-analyzer",
        ),
        (
            "Go",
            "go",
            "gopls",
            "go install golang.org/x/tools/gopls@latest",
        ),
        (
            "C and C++",
            "c h cpp hpp cc cxx hh",
            "clangd",
            "sudo apt-get install -y clangd || apt-get install -y clangd",
        ),
        (
            "PHP",
            "php",
            "intelephense --stdio",
            "npm install -g intelephense",
        ),
        (
            "JSON",
            "json jsonc",
            "vscode-json-language-server --stdio",
            "npm install -g vscode-langservers-extracted",
        ),
        (
            "CSS",
            "css scss less",
            "vscode-css-language-server --stdio",
            "npm install -g vscode-langservers-extracted",
        ),
        (
            "HTML",
            "html htm",
            "vscode-html-language-server --stdio",
            "npm install -g vscode-langservers-extracted",
        ),
        (
            "Bash",
            "sh bash",
            "bash-language-server start",
            "npm install -g bash-language-server",
        ),
        (
            "YAML",
            "yaml yml",
            "yaml-language-server --stdio",
            "npm install -g yaml-language-server",
        ),
    ]
    .into_iter()
    .map(|(name, exts, command, install)| LangServer {
        name: name.into(),
        exts: exts.into(),
        command: command.into(),
        install: install.into(),
    })
    .collect()
}

/// The language server for a file, if one is set for its kind.
pub fn server_for(path: &str) -> Option<LangServer> {
    let ext = path.rsplit_once('.')?.1.to_lowercase();
    with(|s| {
        if !s.language_servers_on {
            return None;
        }
        s.language_servers
            .iter()
            .find(|l| {
                l.exts
                    .split_whitespace()
                    .any(|e| e.trim_start_matches('.') == ext)
            })
            .filter(|l| !l.command.trim().is_empty() && !l.name.trim().is_empty())
            .cloned()
    })
}

/// An MCP server Claude is given, reached over HTTP. One on this computer is reached from
/// the host through a tunnel ride opens.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct McpServer {
    pub name: String,
    pub url: String,
    pub on: bool,
}

impl McpServer {
    /// The Tribib design editor, where its desktop app listens.
    pub fn tribib() -> Self {
        Self {
            name: "tribib".into(),
            url: "http://127.0.0.1:3848/mcp".into(),
            on: true,
        }
    }
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
    /// Types and parameter names the language server infers, shown faintly in the code.
    pub inlay_hints: bool,
    /// Colours from the language server, which knows a parameter from a field.
    pub semantic_highlighting: bool,
    /// The first lines of the blocks the top of the view is in stay at the top.
    pub sticky_scroll: bool,
    pub sticky_lines: usize,
    /// Faint vertical lines at each indentation step.
    pub indent_guides: bool,
    /// The bracket matching the one at the cursor is marked.
    pub bracket_match: bool,
    /// Brackets and quotes get their closing partner as they are typed.
    pub auto_close: bool,
    pub whitespace: Whitespace,
    /// Columns to draw a faint vertical line at, such as "80, 120".
    pub rulers: String,
    pub cursor_style: CursorStyle,
    /// The last line can be scrolled up to the top of the view.
    pub scroll_past_end: bool,
    /// Keyboard shortcuts changed from their defaults: command id to keys, "" for none.
    pub keybindings: std::collections::BTreeMap<String, String>,
    /// Which program formats which files; run on the host, text in on stdin, out on stdout.
    pub formatters: Vec<Formatter>,
    /// Completion, problems, go to definition and the rest, from language servers.
    pub language_servers_on: bool,
    pub language_servers: Vec<LangServer>,
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
    /// MCP servers every Claude ride starts is given.
    pub mcp_servers: Vec<McpServer>,
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
            inlay_hints: true,
            semantic_highlighting: true,
            sticky_scroll: true,
            sticky_lines: 5,
            indent_guides: true,
            bracket_match: true,
            auto_close: true,
            whitespace: Whitespace::Selection,
            rulers: String::new(),
            cursor_style: CursorStyle::Line,
            scroll_past_end: true,
            keybindings: Default::default(),
            formatters: default_formatters(),
            language_servers_on: true,
            language_servers: default_servers(),
            confirm_delete: true,
            max_open_mb: 64,
            scrollback: 10_000,
            chat_mode: "default".into(),
            edit_model: "sonnet".into(),
            search_model: "sonnet".into(),
            catchup_model: "sonnet".into(),
            catchup: CatchUp::Daily,
            team_budget: 40,
            mcp_servers: Vec::new(),
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
    s.sticky_lines = s.sticky_lines.clamp(1, 10);
    s
}

/// The columns rulers are drawn at.
pub fn rulers() -> Vec<usize> {
    with(|s| {
        s.rulers
            .split([',', ' '])
            .filter_map(|c| c.trim().parse().ok())
            .filter(|&c| c > 0 && c < 1000)
            .collect()
    })
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
    with(|s| chat::Mode::from_key(&s.chat_mode))
}

const SECTIONS: [&str; 9] = [
    "Appearance",
    "Editor",
    "Files",
    "Terminal",
    "Claude",
    "Connection",
    "Updates",
    "Account",
    "Keyboard shortcuts",
];
/// The section of the keyboard shortcuts.
pub const SHORTCUTS: usize = 8;
/// The order the sections are listed in.
const ORDER: [usize; 9] = [0, 1, SHORTCUTS, 2, 3, 4, 5, 6, 7];

thread_local! {
    /// What the settings are searched for, the section whose title is still to be shown
    /// above the first row that matches, and whether any row did.
    static SEARCH: RefCell<(String, Option<&'static str>, bool)> =
        RefCell::new((String::new(), None, false));
}

/// Whether the search matches any of `texts`: every word of it is in one of them.
fn found(texts: &[&str]) -> bool {
    SEARCH.with(|search| {
        let query = search.borrow().0.to_lowercase();
        let all = texts.join(" ").to_lowercase();
        query.split_whitespace().all(|word| all.contains(word))
    })
}

fn searching() -> bool {
    SEARCH.with(|search| !search.borrow().0.trim().is_empty())
}

/// Shows the title of the section a matching row is in, before its first one.
fn found_in_section(ui: &mut Ui) {
    let title = SEARCH.with(|search| {
        let mut search = search.borrow_mut();
        search.2 = true;
        search.1.take()
    });
    if let Some(title) = title {
        ui.add_space(6.0);
        ui.label(RichText::new(title).size(18.0).color(theme::STRONG()));
        ui.add_space(10.0);
    }
}

/// A part of the page other than a row (a list, a heading): shown when the search
/// matches what it is about.
fn part(ui: &mut Ui, about: &str) -> bool {
    if !found(&[about]) {
        return false;
    }
    found_in_section(ui);
    true
}

/// A heading over a group of rows.
fn heading(ui: &mut Ui, text: &str) {
    if searching() {
        return;
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .small()
            .strong()
            .color(theme::COMMENT()),
    );
    ui.add_space(8.0);
}
const MODELS: [(&str, &str); 4] = [
    ("haiku", "Haiku"),
    ("sonnet", "Sonnet"),
    ("opus", "Opus"),
    ("fable", "Fable"),
];

/// One row of the page: what the setting is, a line on what it does, and its control.
fn row(ui: &mut Ui, title: &str, help: &str, control: impl FnOnce(&mut Ui)) {
    if !found(&[title, help]) {
        return;
    }
    found_in_section(ui);
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
                    ui.add_space(6.0);
                    self.settings_search_box(ui);
                    ui.add_space(8.0);
                    for index in ORDER {
                        let name = &SECTIONS[index];
                        let selected = self.settings_section == index && !searching();
                        let button = egui::Button::selectable(selected, *name)
                            .min_size(vec2(ui.available_width(), 0.0));
                        if ui.add(button).clicked() {
                            self.settings_section = index;
                            SEARCH.with(|search| search.borrow_mut().0.clear());
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
                    ui.set_max_width(if self.settings_section == SHORTCUTS {
                        900.0
                    } else {
                        760.0
                    });
                    if width < 620.0 {
                        self.settings_search_box(ui);
                        ui.add_space(10.0);
                    }
                    if searching() {
                        // Every section, each titled above its first row that matches.
                        SEARCH.with(|search| search.borrow_mut().2 = false);
                        for section in ORDER {
                            SEARCH.with(|search| search.borrow_mut().1 = Some(SECTIONS[section]));
                            self.settings_section_rows(ui, section, &mut s);
                        }
                        SEARCH.with(|search| search.borrow_mut().1 = None);
                        if !SEARCH.with(|search| search.borrow().2) {
                            ui.label(
                                RichText::new("No setting matches that.").color(theme::COMMENT()),
                            );
                        }
                        return;
                    }
                    // On a narrow window all sections follow each other, as there is no list to pick from.
                    let shown: Vec<usize> = if width >= 620.0 {
                        vec![self.settings_section]
                    } else {
                        ORDER.to_vec()
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

    fn settings_search_box(&mut self, ui: &mut Ui) {
        // RIDE_SETTINGS_SEARCH=TEXT searches the settings, to look at the result.
        static ASKED: std::sync::Once = std::sync::Once::new();
        ASKED.call_once(|| {
            if let Ok(text) = std::env::var("RIDE_SETTINGS_SEARCH") {
                SEARCH.with(|search| search.borrow_mut().0 = text);
            }
        });
        let mut text = SEARCH.with(|search| search.borrow().0.clone());
        let edit = chrome::field(ui, "settings_search", &mut text, "Search settings", 1);
        if edit.changed() {
            SEARCH.with(|search| search.borrow_mut().0 = text);
        }
    }

    fn language_server_rows(&mut self, ui: &mut Ui, s: &mut Settings) {
        ui.add_space(14.0);
        row(
                    ui,
                    "Language servers",
                    "Completion, problems, hover, go to definition (F12), references (Shift+F12), rename (F2) and quick fixes (Ctrl+.), from the same programs VS Code uses. They run on the host; ride offers to install one the first time a file needs it.",
                    |ui| toggle(ui, &mut s.language_servers_on),
                );
        if !part(ui, "language servers lsp command install extensions") {
            return;
        }
        ui.add_space(4.0);
        ui.label(
                    RichText::new(
                        "Name, file extensions, the command that starts it on the host (speaking LSP on stdin and stdout), and the one that installs it.",
                    )
                    .small()
                    .color(theme::COMMENT()),
                );
        ui.add_space(6.0);
        let mut remove = None;
        for (index, server) in s.language_servers.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut server.name)
                        .desired_width(90.0)
                        .hint_text("name"),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut server.exts)
                        .desired_width(120.0)
                        .hint_text("extensions")
                        .font(egui::TextStyle::Monospace),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut server.command)
                        .desired_width((ui.available_width() - 40.0).max(120.0))
                        .hint_text("command")
                        .font(egui::TextStyle::Monospace),
                );
                if icon_button(ui, icons::CLOSE, "Remove") {
                    remove = Some(index);
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(98.0);
                ui.label(RichText::new("install").small().color(theme::COMMENT()));
                ui.add(
                    egui::TextEdit::singleline(&mut server.install)
                        .desired_width((ui.available_width() - 40.0).max(120.0))
                        .hint_text("command that installs it")
                        .font(egui::TextStyle::Monospace),
                );
            });
            ui.add_space(4.0);
        }
        if let Some(index) = remove {
            s.language_servers.remove(index);
        }
        ui.horizontal(|ui| {
            if ui.button("Add a language server").clicked() {
                s.language_servers.push(LangServer::default());
            }
            if ui.button("Restore the defaults").clicked() {
                s.language_servers = default_servers();
            }
        });
        ui.add_space(10.0);
    }

    /// Every command with a shortcut: its keys, a way to change them, and the commands
    /// that would clash.
    fn shortcut_rows(&mut self, ui: &mut Ui, s: &mut Settings) {
        let recording_id = ui.id().with("recording_shortcut");
        let mut recording: Option<String> = ui.data(|d| d.get_temp(recording_id)).flatten();
        // The keys pressed while one is being recorded become its shortcut; Escape alone
        // gives up.
        if let Some(id) = recording.clone() {
            let pressed = ui.input_mut(|i| {
                let found = i.events.iter().find_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some(keys::Combo::from_event(*key, *modifiers)),
                    _ => None,
                });
                i.events
                    .retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)));
                found
            });
            if let Some(combo) = pressed {
                let plain_escape = combo.key == Key::Escape && !combo.ctrl && !combo.alt;
                if !plain_escape {
                    let default = keys::command(&id).map_or("", |c| c.default);
                    let text = combo.to_string();
                    if text == default {
                        s.keybindings.remove(&id);
                    } else {
                        s.keybindings.insert(id.clone(), text);
                    }
                }
                recording = None;
            }
        }
        if !searching() {
            ui.label(
                RichText::new(
                    "Click a shortcut, then press the keys you want for it. Escape gives up. Shortcuts in the editor work while it has the keyboard; the others anywhere but in a terminal, except those marked always.",
                )
                .small()
                .color(theme::COMMENT()),
            );
            ui.add_space(10.0);
        }
        // The current settings, rather than the stored ones, decide what clashes.
        let bindings = s.keybindings.clone();
        let combo_of = |id: &str| match bindings.get(id) {
            Some(text) => keys::Combo::parse(text),
            None => keys::command(id).and_then(|c| keys::Combo::parse(c.default)),
        };
        for command in keys::COMMANDS {
            let combo = combo_of(command.id);
            let shown = combo.map(|c| c.to_string()).unwrap_or_default();
            if !found(&[
                command.label,
                command.id,
                &shown,
                "keyboard shortcut keybinding",
            ]) {
                continue;
            }
            found_in_section(ui);
            let changed = s.keybindings.contains_key(command.id);
            let clashes: Vec<&str> = keys::COMMANDS
                .iter()
                .filter(|other| other.id != command.id && combo.is_some())
                .filter(|other| combo_of(other.id) == combo)
                .map(|other| other.label)
                .collect();
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 330.0).max(160.0));
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(command.label).color(theme::STRONG()));
                    let mut note = match command.when {
                        keys::When::Editor => "in the editor".to_string(),
                        keys::When::Always => "always".to_string(),
                        keys::When::Window => String::new(),
                    };
                    if !clashes.is_empty() {
                        if !note.is_empty() {
                            note.push_str(" Â· ");
                        }
                        note.push_str(&format!("same keys as {}", clashes.join(", ")));
                    }
                    let color = if clashes.is_empty() {
                        theme::COMMENT()
                    } else {
                        theme::ANSI()[3]
                    };
                    ui.label(
                        RichText::new(format!("{}  {note}", command.id))
                            .small()
                            .color(color),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if changed && icon_button(ui, icons::REFRESH, "Back to the default") {
                        s.keybindings.remove(command.id);
                    }
                    if combo.is_some() && icon_button(ui, icons::CLOSE, "Remove the shortcut") {
                        s.keybindings.insert(command.id.to_string(), String::new());
                    }
                    let waiting = recording.as_deref() == Some(command.id);
                    let text = if waiting {
                        "Press the keysâ€¦".to_string()
                    } else if shown.is_empty() {
                        "Not set".to_string()
                    } else {
                        shown.clone()
                    };
                    let mut chip = RichText::new(text).monospace();
                    if waiting {
                        chip = chip.color(theme::ACCENT());
                    } else if shown.is_empty() {
                        chip = chip.color(theme::COMMENT());
                    } else if changed {
                        chip = chip.color(theme::STRONG());
                    }
                    let button = egui::Button::new(chip)
                        .min_size(vec2(150.0, 0.0))
                        .selected(waiting);
                    if ui.add(button).on_hover_text("Change the keys").clicked() {
                        recording = if waiting {
                            None
                        } else {
                            Some(command.id.to_string())
                        };
                    }
                });
            });
            ui.add_space(6.0);
        }
        if !searching() {
            ui.add_space(8.0);
            if !s.keybindings.is_empty() && ui.button("Reset every shortcut").clicked() {
                s.keybindings.clear();
            }
        }
        keys::set_recording(recording.is_some());
        ui.data_mut(|d| d.insert_temp(recording_id, recording));
    }

    fn settings_section_rows(&mut self, ui: &mut Ui, section: usize, s: &mut Settings) {
        match section {
            SHORTCUTS => self.shortcut_rows(ui, s),
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
                heading(ui, "Typing");
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
                row(
                    ui,
                    "Close brackets and quotes",
                    "Typing ( [ { \" ' or ` adds its partner after the cursor; typing the partner steps over it, and Backspace between the two takes both. An opening one typed over a selection wraps it.",
                    |ui| toggle(ui, &mut s.auto_close),
                );
                heading(ui, "Display");
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
                row(ui, "Cursor style", "How the cursor is drawn.", |ui| {
                    pick(
                        ui,
                        "cursor_style",
                        &mut s.cursor_style,
                        &[
                            (CursorStyle::Line, "Line"),
                            (CursorStyle::Block, "Block"),
                            (CursorStyle::Underline, "Underline"),
                        ],
                    );
                });
                row(
                    ui,
                    "Sticky scroll",
                    "The first lines of the functions and blocks the top of the view is inside stay pinned above the code. Click one to go to it.",
                    |ui| toggle(ui, &mut s.sticky_scroll),
                );
                row(
                    ui,
                    "Sticky scroll lines",
                    "At most this many lines stay pinned.",
                    |ui| {
                        ui.add(egui::Slider::new(&mut s.sticky_lines, 1..=10));
                    },
                );
                row(
                    ui,
                    "Indent guides",
                    "Faint vertical lines at each indentation step; the block the cursor is in is a shade brighter.",
                    |ui| toggle(ui, &mut s.indent_guides),
                );
                row(
                    ui,
                    "Matching brackets",
                    "Marks the bracket that pairs with the one at the cursor.",
                    |ui| toggle(ui, &mut s.bracket_match),
                );
                row(
                    ui,
                    "Show whitespace",
                    "Faint dots for spaces and arrows for tabs.",
                    |ui| {
                        pick(
                            ui,
                            "whitespace",
                            &mut s.whitespace,
                            &[
                                (Whitespace::None, "Never"),
                                (Whitespace::Selection, "In the selection"),
                                (Whitespace::All, "Everywhere"),
                            ],
                        );
                    },
                );
                row(
                    ui,
                    "Rulers",
                    "Columns to draw a faint vertical line at, such as 80, 120. Empty for none.",
                    |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut s.rulers)
                                .desired_width(170.0)
                                .hint_text("80, 120")
                                .font(egui::TextStyle::Monospace),
                        );
                    },
                );
                row(
                    ui,
                    "Scroll past the last line",
                    "The last line can be scrolled up to the top of the view.",
                    |ui| toggle(ui, &mut s.scroll_past_end),
                );
                heading(ui, "Saving");
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
                heading(ui, "Language features");
                row(
                    ui,
                    "Inlay hints",
                    "Types and parameter names the language server works out, shown faintly in the code. They aren't part of the text.",
                    |ui| toggle(ui, &mut s.inlay_hints),
                );
                row(
                    ui,
                    "Semantic highlighting",
                    "Colours from the language server, on top of the usual ones: it knows a parameter from a local, a field, a constant or a type.",
                    |ui| toggle(ui, &mut s.semantic_highlighting),
                );
                row(
                    ui,
                    "Format on save",
                    "Runs the file's formatter before saving. Shift+Alt+F formats without saving.",
                    |ui| toggle(ui, &mut s.format_on_save),
                );
                if !part(ui, "formatters formatter prettier rustfmt black") {
                    return self.language_server_rows(ui, s);
                }
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
                self.language_server_rows(ui, s);
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
                                ("full", "Full access: never asks"),
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
                if !part(ui, "mcp servers tribib tools") {
                    return;
                }
                ui.add_space(14.0);
                ui.label(RichText::new("MCP servers").color(theme::STRONG()));
                ui.label(
                    RichText::new(
                        "Given to every Claude ride starts: the chat, the agents and the Designer. One on this computer (127.0.0.1) is reached from the server through an SSH tunnel ride opens, so Claude there finds it at the same kind of address.",
                    )
                    .small()
                    .color(theme::COMMENT()),
                );
                ui.add_space(6.0);
                let mut remove = None;
                for (index, server) in s.mcp_servers.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        toggle(ui, &mut server.on);
                        ui.add(
                            egui::TextEdit::singleline(&mut server.name)
                                .desired_width(110.0)
                                .hint_text("name"),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut server.url)
                                .desired_width((ui.available_width() - 40.0).max(160.0))
                                .hint_text("http://127.0.0.1:3848/mcp")
                                .font(egui::TextStyle::Monospace),
                        );
                        if icon_button(ui, icons::CLOSE, "Remove") {
                            remove = Some(index);
                        }
                    });
                }
                if let Some(index) = remove {
                    s.mcp_servers.remove(index);
                }
                ui.horizontal(|ui| {
                    if ui.button("Add an MCP server").clicked() {
                        s.mcp_servers.push(McpServer {
                            on: true,
                            ..McpServer::default()
                        });
                    }
                    let has_tribib = s
                        .mcp_servers
                        .iter()
                        .any(|m| m.name.eq_ignore_ascii_case("tribib"));
                    if !has_tribib && ui.button("Add Tribib").clicked() {
                        s.mcp_servers.push(McpServer::tribib());
                    }
                });
            }
            7 => {
                if searching()
                    && !part(
                        ui,
                        "account github sign in nickname live editing together cursor",
                    )
                {
                    return;
                }
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
