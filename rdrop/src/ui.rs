//! rdrop's dialogs and the pieces they are made of, drawn the same way everywhere: the
//! window dims behind, the dialog has a header with an icon, a title and a line saying what
//! it is for, its fields with their labels above them, and a footer with the buttons and
//! the keys that work.

use crate::theme;
use eframe::egui::{
    self, vec2, Align, Align2, Color32, Context, CornerRadius, FontId, Frame, Key, Layout, Margin,
    RichText, Sense, Stroke, TextEdit, Ui,
};

/// What a dialog's buttons, or Enter and Escape, asked for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Answer {
    Waiting,
    /// The main button, or Enter.
    Confirm,
    /// The second button, when there is one.
    Other,
    Cancel,
}

/// How the main button looks: the accent, or red for what can't be undone.
#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Normal,
    Danger,
}

pub const RED: Color32 = Color32::from_rgb(0xf0, 0x6a, 0x78);

pub struct Dialog<'a> {
    pub id: &'a str,
    pub icon: char,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub confirm: &'a str,
    /// A second button beside the main one, such as "Save and connect".
    pub other: Option<&'a str>,
    pub tone: Tone,
    /// Whether the main button may be used.
    pub can: bool,
    pub width: f32,
}

impl Dialog<'_> {
    /// Shows the dialog; `body` draws what is in it and returns true when it confirms
    /// (Enter in a field).
    pub fn show(self, ctx: &Context, body: impl FnOnce(&mut Ui) -> bool) -> Answer {
        let mut answer = Answer::Waiting;
        let accent = match self.tone {
            Tone::Normal => theme::ACCENT(),
            Tone::Danger => RED,
        };
        let frame = Frame::NONE
            .fill(theme::PANEL_BG())
            .stroke(Stroke::new(1.0, theme::LINE()))
            .corner_radius(16)
            .shadow(egui::epaint::Shadow {
                offset: [0, 18],
                blur: 60,
                spread: 0,
                color: Color32::from_black_alpha(170),
            });
        let shown = egui::Modal::new(egui::Id::new(("rdrop-dialog", self.id)))
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(150))
            .show(ctx, |ui| {
                ui.set_width(self.width);
                ui.spacing_mut().item_spacing.y = 0.0;
                // Header: the icon in a soft disc, the title and what the dialog is for.
                Frame::NONE
                    .inner_margin(Margin {
                        left: 24,
                        right: 24,
                        top: 22,
                        bottom: 6,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (disc, _) =
                                ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                            ui.painter().circle_filled(
                                disc.center(),
                                20.0,
                                accent.gamma_multiply(0.16),
                            );
                            ui.painter().text(
                                disc.center(),
                                Align2::CENTER_CENTER,
                                self.icon,
                                FontId::proportional(18.0),
                                accent,
                            );
                            ui.add_space(12.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 3.0;
                                ui.add_space(1.0);
                                ui.label(
                                    RichText::new(self.title)
                                        .size(17.0)
                                        .strong()
                                        .color(theme::STRONG()),
                                );
                                if !self.subtitle.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(self.subtitle)
                                                .size(12.5)
                                                .color(theme::MUTED()),
                                        )
                                        .wrap(),
                                    );
                                }
                            });
                        });
                    });
                // Body.
                Frame::NONE
                    .inner_margin(Margin {
                        left: 24,
                        right: 24,
                        top: 14,
                        bottom: 20,
                    })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 10.0;
                        if body(ui) && self.can {
                            answer = Answer::Confirm;
                        }
                    });
                // Footer: the keys that work, and the buttons.
                let footer = ui.available_rect_before_wrap();
                ui.painter().hline(
                    footer.x_range(),
                    footer.top(),
                    Stroke::new(1.0, theme::LINE()),
                );
                Frame::NONE
                    .fill(theme::tint(4))
                    .corner_radius(CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 16,
                        se: 16,
                    })
                    .inner_margin(Margin {
                        left: 24,
                        right: 18,
                        top: 14,
                        bottom: 14,
                    })
                    .show(ui, |ui| {
                        ui.set_width(self.width - 42.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Enter  ·  Esc")
                                    .size(11.5)
                                    .color(theme::COMMENT()),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                if button(ui, self.confirm, Kind::Primary(accent), self.can) {
                                    answer = Answer::Confirm;
                                }
                                if let Some(other) = self.other {
                                    if button(ui, other, Kind::Secondary, self.can) {
                                        answer = Answer::Other;
                                    }
                                }
                                if button(ui, "Cancel", Kind::Quiet, true) {
                                    answer = Answer::Cancel;
                                }
                            });
                        });
                    });
            });
        if answer == Answer::Waiting {
            if shown.should_close() || ctx.input(|i| i.key_pressed(Key::Escape)) {
                answer = Answer::Cancel;
            }
        }
        answer
    }
}

pub enum Kind {
    Primary(Color32),
    Secondary,
    Quiet,
}

pub fn button(ui: &mut Ui, text: &str, kind: Kind, enabled: bool) -> bool {
    let (fill, color, stroke) = match kind {
        Kind::Primary(c) => (
            if enabled { c } else { c.gamma_multiply(0.35) },
            Color32::from_rgb(0x0b, 0x0b, 0x0f),
            Stroke::NONE,
        ),
        Kind::Secondary => (
            theme::ACCENT().gamma_multiply(0.14),
            theme::ACCENT(),
            Stroke::new(1.0, theme::ACCENT().gamma_multiply(0.5)),
        ),
        Kind::Quiet => (theme::tint(10), theme::TEXT(), Stroke::NONE),
    };
    let strong = matches!(kind, Kind::Primary(_));
    let mut label = RichText::new(text).size(13.0).color(color);
    if strong {
        label = label.strong();
    }
    let b = egui::Button::new(label)
        .fill(fill)
        .stroke(stroke)
        .corner_radius(9)
        .min_size(vec2(if strong { 104.0 } else { 84.0 }, 34.0));
    ui.add_enabled(enabled, b).clicked()
}

/// A field with its label above it: a quiet box whose border lights up while it is typed in.
/// Returns its response; Enter in it is `lost_focus() && enter`.
pub fn field(
    ui: &mut Ui,
    id: &str,
    label: &str,
    value: &mut String,
    hint: &str,
    focus: bool,
) -> egui::Response {
    field_with(ui, id, label, value, hint, focus, false, None)
}

/// A password field, with an eye to show what was typed.
pub fn password(
    ui: &mut Ui,
    id: &str,
    label: &str,
    value: &mut String,
    hint: &str,
    focus: bool,
) -> egui::Response {
    let shown_id = ui.make_persistent_id(("shown", id));
    let shown = ui.data(|d| d.get_temp::<bool>(shown_id).unwrap_or(false));
    let mut toggle = false;
    let resp = field_with(ui, id, label, value, hint, focus, !shown, Some(&mut toggle));
    if toggle {
        ui.data_mut(|d| d.insert_temp(shown_id, !shown));
    }
    resp
}

#[allow(clippy::too_many_arguments)]
fn field_with(
    ui: &mut Ui,
    id: &str,
    label: &str,
    value: &mut String,
    hint: &str,
    focus: bool,
    secret: bool,
    eye: Option<&mut bool>,
) -> egui::Response {
    let id = ui.make_persistent_id(("field", id));
    if !label.is_empty() {
        ui.label(
            RichText::new(label)
                .size(12.0)
                .strong()
                .color(theme::MUTED()),
        );
        ui.add_space(-4.0);
    }
    let focused = ui.memory(|m| m.has_focus(id));
    let stroke = if focused {
        theme::ACCENT()
    } else {
        theme::LINE()
    };
    let fill = if focused {
        theme::ACCENT().gamma_multiply(0.06)
    } else {
        theme::tint(6)
    };
    let has_eye = eye.is_some();
    let mut clicked_eye = false;
    let resp = Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(if focused { 1.5 } else { 1.0 }, stroke))
        .corner_radius(9)
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let width = ui.available_width() - if has_eye { 30.0 } else { 0.0 };
                let edit = ui.add(
                    TextEdit::singleline(value)
                        .id(id)
                        .frame(Frame::NONE)
                        .password(secret)
                        .desired_width(width)
                        .font(FontId::proportional(14.0))
                        .hint_text(RichText::new(hint).color(theme::COMMENT())),
                );
                if has_eye {
                    let icon = if secret { "◉" } else { "○" };
                    let b =
                        egui::Button::new(RichText::new(icon).size(13.0).color(theme::COMMENT()))
                            .frame(false);
                    if ui
                        .add(b)
                        .on_hover_text(if secret { "Show" } else { "Hide" })
                        .clicked()
                    {
                        clicked_eye = true;
                    }
                }
                edit
            })
            .inner
        })
        .inner;
    if let Some(eye) = eye {
        *eye = clicked_eye;
    }
    if focus {
        resp.request_focus();
    }
    resp
}

/// An on/off switch with its label, for settings such as "Remember password".
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str, detail: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let (rect, resp) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
        let t = ui.ctx().animate_bool(resp.id, *on);
        let track = if *on {
            theme::ACCENT()
        } else {
            theme::tint(28)
        };
        ui.painter().rect_filled(
            rect,
            11.0,
            track.gamma_multiply(if *on { 0.9 } else { 1.0 }),
        );
        let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
        ui.painter()
            .circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
        let text = ui.add(
            egui::Label::new(RichText::new(label).size(13.0).color(theme::TEXT()))
                .sense(Sense::click()),
        );
        if resp.clicked() || text.clicked() {
            *on = !*on;
            changed = true;
        }
        if !detail.is_empty() {
            ui.label(RichText::new(detail).size(11.5).color(theme::COMMENT()));
        }
    });
    changed
}

/// A quiet box for a note, a warning or a list, inside a dialog.
pub fn callout(ui: &mut Ui, color: Color32, body: impl FnOnce(&mut Ui)) {
    Frame::NONE
        .fill(color.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.35)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui)
        });
}

/// Two fields side by side, the first wide and the second narrow, as host and port.
pub fn pair(
    ui: &mut Ui,
    wide: impl FnOnce(&mut Ui),
    narrow: impl FnOnce(&mut Ui),
    narrow_width: f32,
) {
    ui.horizontal_top(|ui| {
        let total = ui.available_width();
        ui.allocate_ui_with_layout(
            vec2(total - narrow_width - 12.0, 0.0),
            Layout::top_down(Align::Min),
            wide,
        );
        ui.add_space(12.0);
        ui.allocate_ui_with_layout(
            vec2(narrow_width, 0.0),
            Layout::top_down(Align::Min),
            narrow,
        );
    });
}

/// Codicons the shared icon list doesn't have.
pub const LOCK: char = '\u{ea75}';
pub const WARNING: char = '\u{ea6c}';
pub const FOLDER: char = '\u{ea83}';
pub const DESKTOP: char = '\u{ea7a}';
pub const BOOK: char = '\u{eaa4}';
/// Material Design glyphs from the bundled Nerd Font: a hard disk and a USB stick.
pub const DRIVE: char = '\u{f02ca}';
pub const USB: char = '\u{f0553}';
