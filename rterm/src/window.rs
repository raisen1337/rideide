//! The parts of a window without the system's frame: dragging it by its title bar, the
//! minimize, maximize and close buttons, and resizing it by its edges. Shared by rterm and
//! rkit, which have `icons` and `theme` modules of their own (ride's, included by path).

use crate::icons;
use eframe::egui::{self, vec2, Color32, Context, RichText, Sense, Ui, ViewportCommand};

pub fn bar_button(ui: &mut Ui, icon: char, hint: &str) -> bool {
    let button = egui::Button::new(RichText::new(icon).size(14.0))
        .frame_when_inactive(false)
        .min_size(vec2(28.0, 26.0));
    ui.add(button).on_hover_text(hint).clicked()
}

/// Lets the window be resized by its edges, as it has no system frame showing.
pub fn resize_edges(ctx: &Context) {
    use egui::{CursorIcon as Icon, ResizeDirection as Dir};
    let state = ctx.input(|i| {
        let v = i.viewport();
        v.maximized.unwrap_or(false) || v.fullscreen.unwrap_or(false)
    });
    if state {
        return;
    }
    let rect = ctx.content_rect();
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    const EDGE: f32 = 5.0;
    let (left, right) = (p.x - rect.left() < EDGE, rect.right() - p.x < EDGE);
    let (top, bottom) = (p.y - rect.top() < EDGE, rect.bottom() - p.y < EDGE);
    let (direction, icon) = match (left, right, top, bottom) {
        (true, _, true, _) => (Dir::NorthWest, Icon::ResizeNwSe),
        (_, true, true, _) => (Dir::NorthEast, Icon::ResizeNeSw),
        (true, _, _, true) => (Dir::SouthWest, Icon::ResizeNeSw),
        (_, true, _, true) => (Dir::SouthEast, Icon::ResizeNwSe),
        (true, ..) => (Dir::West, Icon::ResizeHorizontal),
        (_, true, ..) => (Dir::East, Icon::ResizeHorizontal),
        (_, _, true, _) => (Dir::North, Icon::ResizeVertical),
        (_, _, _, true) => (Dir::South, Icon::ResizeVertical),
        _ => return,
    };
    ctx.set_cursor_icon(icon);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}

/// Makes the title bar move the window when dragged and maximize it on a double click.
/// Called before the bar's widgets, so they stay clickable on top.
pub fn title_drag(ui: &Ui) {
    let rect = ui.max_rect().expand2(vec2(12.0, 8.0));
    let resp = ui.interact(rect, ui.id().with("title_drag"), Sense::click_and_drag());
    if resp.double_clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if resp.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

pub fn caption_button(ui: &mut Ui, icon: char, hint: &str, danger: bool) -> bool {
    ui.scope(|ui| {
        if danger {
            let red = Color32::from_rgb(0xc4, 0x2b, 0x1c);
            ui.visuals_mut().widgets.hovered.weak_bg_fill = red;
            ui.visuals_mut().widgets.active.weak_bg_fill = red;
        }
        let button = egui::Button::new(RichText::new(icon).size(13.0))
            .frame_when_inactive(false)
            .min_size(vec2(40.0, 28.0));
        ui.add(button).on_hover_text(hint).clicked()
    })
    .inner
}

pub fn window_buttons(ui: &mut Ui) {
    let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
    ui.spacing_mut().item_spacing.x = 0.0;
    if caption_button(ui, icons::WIN_CLOSE, "Close", true) {
        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
    }
    let (icon, hint) = if maximized {
        (icons::WIN_RESTORE, "Restore")
    } else {
        (icons::WIN_MAXIMIZE, "Maximize")
    };
    if caption_button(ui, icon, hint, false) {
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    if caption_button(ui, icons::WIN_MINIMIZE, "Minimize", false) {
        ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}
