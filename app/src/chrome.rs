//! The window's frame, in the manner of VS Code: a slim title bar with the project in the
//! middle, the activity bar of icons down the left, the panel at the bottom with the
//! terminal and the problems, and the connection at the left of the status bar.

use super::*;
use eframe::egui::Rect;

/// What the panel at the bottom shows.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum PanelTab {
    Terminal,
    Problems,
}

pub(super) const ACTIVITY_W: f32 = 46.0;
/// Codicons for the layout toggles.
const LAYOUT_SIDEBAR: char = '\u{ebf3}';
const LAYOUT_PANEL: char = '\u{ebf5}';
const LAYOUT_SECONDARY: char = '\u{ebf4}';
pub(super) const WARNING: char = '\u{ea6c}';

/// An icon of the activity bar: brighter when its view is shown, with a bar at its left.
fn activity_button(ui: &mut Ui, icon: char, active: bool, hint: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ACTIVITY_W, 42.0), Sense::click());
    let painter = ui.painter();
    if active {
        painter.rect_filled(
            Rect::from_min_size(
                rect.left_top() + vec2(0.0, 6.0),
                vec2(2.0, rect.height() - 12.0),
            ),
            1.0,
            theme::ACCENT(),
        );
    }
    let color = if active || resp.hovered() {
        theme::STRONG()
    } else {
        theme::COMMENT()
    };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(20.0),
        color,
    );
    resp.on_hover_text(hint).clicked()
}

/// A layout toggle of the title bar.
fn layout_toggle(ui: &mut Ui, on: bool, icon: char, hint: &str) -> bool {
    let color = if on { theme::TEXT() } else { theme::COMMENT() };
    let button = egui::Button::new(RichText::new(icon.to_string()).size(15.0).color(color))
        .frame(false)
        .min_size(vec2(28.0, 24.0));
    ui.add(button).on_hover_text(hint).clicked()
}

impl App {
    /// The title bar: the name of what is open in the middle (a click goes to a file, as in
    /// VS Code), the layout toggles and the window's buttons at the right.
    pub(super) fn title_bar(&mut self, ui: &mut Ui) {
        title_drag(ui);
        let full = ui.max_rect();
        let height = 26.0;
        let right_w = 3.0 * 46.0 + 3.0 * 30.0 + 16.0;
        // The middle box stays centred while there's room, and moves left of the buttons
        // when there isn't.
        let width = (full.width() * 0.42).clamp(200.0, 560.0);
        let mut center = Rect::from_center_size(full.center(), vec2(width, height));
        let limit = full.right() - right_w - 8.0;
        if center.right() > limit {
            center = center.translate(vec2(limit - center.right(), 0.0));
        }
        let left_edge = full.left() + 60.0;
        if center.left() < left_edge {
            center.min.x = left_edge;
        }

        ui.horizontal(|ui| {
            ui.set_min_height(height);
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(RichText::new("ride").strong().color(theme::COMMENT()))
                    .selectable(false),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                window_buttons(ui);
                ui.add_space(10.0);
                ui.spacing_mut().item_spacing.x = 2.0;
                let online = self.online();
                if layout_toggle(
                    ui,
                    self.show_chat && online,
                    LAYOUT_SECONDARY,
                    "Claude chat",
                ) {
                    self.show_chat = !self.show_chat;
                }
                if layout_toggle(
                    ui,
                    self.show_term && online,
                    LAYOUT_PANEL,
                    "Panel: terminal and problems (Ctrl+`)",
                ) {
                    self.show_term = !self.show_term;
                }
                if layout_toggle(ui, self.show_side && online, LAYOUT_SIDEBAR, "Side bar") {
                    self.show_side = !self.show_side;
                }
            });
        });

        if center.width() < 120.0 {
            return;
        }
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(center)
                .layout(Layout::left_to_right(Align::Center)),
        );
        self.command_center(&mut ui, center);
    }

    /// The box in the middle of the title bar. Connected: the folder and the connection,
    /// and a click opens the file finder. Not connected: where to connect to.
    fn command_center(&mut self, ui: &mut Ui, rect: Rect) {
        match &self.link {
            Link::Online(_) => {
                let resp = ui.interact(rect, ui.id().with("command_center"), Sense::click());
                let fill = if resp.hovered() {
                    theme::tint(16)
                } else {
                    theme::tint(9)
                };
                ui.painter().rect(
                    rect,
                    6,
                    fill,
                    (1.0, theme::LINE()),
                    egui::StrokeKind::Inside,
                );
                let host = share::label(&self.host).unwrap_or_else(|| self.host.trim().to_string());
                let folder = file_name(&self.root).to_string();
                let text = pieces(&[
                    (&format!("{}  ", icons::SEARCH), theme::COMMENT()),
                    (&folder, theme::TEXT()),
                    (&format!("  —  {host}"), theme::COMMENT()),
                ]);
                let galley = egui::WidgetText::from(text).into_galley(
                    ui,
                    Some(TextWrapMode::Truncate),
                    rect.width() - 20.0,
                    egui::TextStyle::Body,
                );
                let at = rect.center() - galley.size() / 2.0;
                ui.painter().galley(at, galley, theme::TEXT());
                if resp
                    .on_hover_text("Go to a file (Ctrl+P) · commands with Ctrl+Shift+P")
                    .clicked()
                {
                    self.open_palette(true);
                }
            }
            Link::Connecting { status, .. } => {
                ui.spinner();
                let shown = share::label(&self.host).unwrap_or_else(|| self.host.clone());
                let text = format!("{shown}: {}", status.lock().unwrap());
                ui.add(egui::Label::new(RichText::new(text).color(theme::COMMENT())).truncate());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Cancel").clicked() {
                        self.disconnect();
                    }
                });
            }
            Link::Offline => {
                ui.painter().rect(
                    rect,
                    6,
                    theme::tint(9),
                    (1.0, theme::LINE()),
                    egui::StrokeKind::Inside,
                );
                ui.add_space(8.0);
                ui.label(RichText::new(icons::REMOTE).color(theme::COMMENT()));
                let mut connect = false;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(2.0);
                    let button = egui::Button::new(
                        RichText::new(format!("{}  Connect", icons::PLUG))
                            .small()
                            .color(Color32::WHITE),
                    )
                    .fill(theme::ACCENT().gamma_multiply(0.6))
                    .corner_radius(4);
                    connect |= ui.add(button).clicked();
                    if !self.hosts.is_empty() {
                        ui.menu_button(
                            RichText::new(icons::CHEVRON_DOWN).color(theme::COMMENT()),
                            |ui| {
                                for host in &self.hosts {
                                    if menu_item(ui, icons::REMOTE, host) {
                                        self.host = host.clone();
                                    }
                                }
                            },
                        );
                    }
                    let edit = ui.add(
                        TextEdit::singleline(&mut self.host)
                            .desired_width(f32::INFINITY)
                            .frame(Frame::NONE)
                            .hint_text("Connect to a host, user@host or a session ID"),
                    );
                    connect |= edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                });
                if connect {
                    self.connect(ui.ctx());
                }
            }
        }
    }

    /// The column of icons at the far left: the side bar's views at the top, the other
    /// views and the account at the bottom.
    pub(super) fn activity_bar(&mut self, ui: &mut Ui) {
        let online = self.online();
        let items = [
            (Side::Files, icons::FILES, "Explorer"),
            (Side::Search, icons::SEARCH, "Search (Ctrl+Shift+F)"),
            (Side::Git, icons::SOURCE_CONTROL, "Source control"),
            (Side::Changes, icons::DIFF, "What Claude changed"),
            (Side::Outline, icons::METHOD, "Outline (Ctrl+Shift+O)"),
            (Side::Database, icons::DATABASE, "Databases"),
            (Side::Server, icons::PULSE, "Server monitor and ports"),
        ];
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add_space(4.0);
        for (side, icon, hint) in items {
            let active = online && self.view == View::Code && self.show_side && self.side == side;
            if activity_button(ui, icon, active, hint) {
                if active {
                    self.show_side = false;
                } else {
                    self.view = View::Code;
                    self.show_side = true;
                    match side {
                        Side::Git => self.git_refresh(),
                        Side::Search => self.focus_search = true,
                        _ => {}
                    }
                    self.side = side;
                }
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(4.0);
            if activity_button(
                ui,
                icons::GEAR,
                self.view == View::Settings,
                "Settings (Ctrl+,)",
            ) {
                self.view = if self.view == View::Settings {
                    View::Code
                } else {
                    View::Settings
                };
            }
            let account = match share::me(&self.github) {
                Some(me) if !me.avatar.is_empty() => {
                    format!("Signed in to GitHub as @{}", me.login)
                }
                Some(me) => format!("{} — your nickname in live editing", me.name),
                None => "Account: sign in with GitHub".to_string(),
            };
            if activity_button(ui, icons::SIGN_IN, false, &account) {
                self.view = View::Settings;
                self.settings_section = 7;
            }
            if activity_button(
                ui,
                icons::PALETTE,
                self.view == View::Themes,
                "Themes and icons",
            ) {
                self.view = if self.view == View::Themes {
                    View::Code
                } else {
                    View::Themes
                };
            }
            if online
                && activity_button(
                    ui,
                    icons::AGENTS,
                    self.view == View::Agents,
                    "Agents: a team of Claude sessions",
                )
            {
                self.view = if self.view == View::Agents {
                    View::Code
                } else {
                    View::Agents
                };
            }
        });
    }

    /// The side bar's title, as VS Code names its views.
    pub(super) fn side_title(&mut self, ui: &mut Ui) {
        let title = match self.side {
            Side::Files => "EXPLORER",
            Side::Search => "SEARCH",
            Side::Git => "SOURCE CONTROL",
            Side::Changes => "CLAUDE'S CHANGES",
            Side::Outline => "OUTLINE",
            Side::Database => "DATABASES",
            Side::Server => "SERVER",
        };
        ui.horizontal(|ui| {
            ui.set_min_height(26.0);
            ui.add_space(4.0);
            ui.label(
                RichText::new(title)
                    .small()
                    .strong()
                    .color(theme::COMMENT()),
            );
            if self.side == Side::Files {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, icons::FOLDER_ROOT, "Open another folder") {
                        self.root_edit = !self.root_edit;
                        self.root_input = self.root.clone();
                    }
                });
            }
        });
        if self.side == Side::Files && self.root_edit {
            ui.horizontal(|ui| {
                let edit = ui.add(
                    TextEdit::singleline(&mut self.root_input)
                        .desired_width(f32::INFINITY)
                        .hint_text("Folder on the host"),
                );
                if self.root_focus {
                    self.root_focus = false;
                    edit.request_focus();
                }
                if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    self.root_edit = false;
                    self.open_root(self.root_input.trim().to_string());
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.root_edit = false;
                }
            });
            ui.add_space(4.0);
        }
    }

    /// The tabs at the top of the bottom panel: TERMINAL and PROBLEMS.
    pub(super) fn panel_tabs(&mut self, ui: &mut Ui) {
        let (errors, warnings) = self.problem_counts();
        let tabs = [
            (PanelTab::Terminal, "TERMINAL".to_string()),
            (
                PanelTab::Problems,
                if errors + warnings > 0 {
                    format!("PROBLEMS  {}", errors + warnings)
                } else {
                    "PROBLEMS".to_string()
                },
            ),
        ];
        for (tab, label) in tabs {
            // A guest has no terminal on the host.
            if tab == PanelTab::Terminal && self.guest() {
                continue;
            }
            let active = self.panel_tab == tab;
            let color = if active {
                theme::STRONG()
            } else {
                theme::COMMENT()
            };
            let resp = ui.add(
                egui::Label::new(RichText::new(label).small().strong().color(color))
                    .sense(Sense::click())
                    .selectable(false),
            );
            if active {
                let r = resp.rect;
                ui.painter().line_segment(
                    [
                        r.left_bottom() + vec2(0.0, 4.0),
                        r.right_bottom() + vec2(0.0, 4.0),
                    ],
                    (1.5, theme::ACCENT()),
                );
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.panel_tab = tab;
            }
            ui.add_space(14.0);
        }
    }

    /// The connection at the left of the status bar, as VS Code shows a remote: a click
    /// lists the connections and what can be done with them.
    pub(super) fn remote_chip(&mut self, ui: &mut Ui) {
        if !self.online() {
            let text = RichText::new(format!("{}  Not connected", icons::REMOTE))
                .small()
                .color(theme::COMMENT());
            ui.label(text);
            return;
        }
        let more = match self.sessions.len() {
            0 => String::new(),
            n => format!("  +{n}"),
        };
        let shown = share::label(&self.host).unwrap_or_else(|| self.host.trim().to_string());
        let text = RichText::new(format!("{}  {shown}{more}", icons::REMOTE))
            .small()
            .color(Color32::WHITE);
        let chip = egui::Button::new(text)
            .fill(theme::ACCENT().gamma_multiply(0.7))
            .corner_radius(3);
        egui::containers::menu::MenuButton::from_button(chip).ui(ui, |ui| self.sessions_menu(ui));
        if !self.git.branch.is_empty() {
            let text = RichText::new(format!("{}  {}", icons::SOURCE_CONTROL, self.git.branch))
                .small()
                .color(theme::COMMENT());
            let label = egui::Label::new(text).sense(Sense::click());
            if ui.add(label).on_hover_text("Source control").clicked() {
                self.view = View::Code;
                self.side = Side::Git;
                self.show_side = true;
                self.git_refresh();
            }
        }
        let (errors, warnings) = self.problem_counts();
        let color = match (errors, warnings) {
            (1.., _) => theme::ANSI()[1],
            (0, 1..) => theme::ANSI()[3],
            _ => theme::COMMENT(),
        };
        let text = RichText::new(format!(
            "{}  {errors}   {WARNING}  {warnings}",
            icons::ERROR
        ))
        .small()
        .color(color);
        let label = egui::Label::new(text).sense(Sense::click());
        if ui.add(label).on_hover_text("Problems").clicked() {
            self.show_problems();
        }
    }

    /// Opens the bottom panel on the problems.
    pub(super) fn show_problems(&mut self) {
        self.view = View::Code;
        self.show_term = true;
        self.panel_tab = PanelTab::Problems;
    }
}
