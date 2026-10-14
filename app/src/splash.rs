//! The start screen: a small square in the middle of the display in which the logo
//! draws itself, before the window grows into the editor.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Shape, Stroke, Ui};

/// Side of the start window, in points.
pub const SIZE: f32 = 300.0;
/// How long the start screen stays, in seconds, the fade-out at its end included.
pub const DURATION: f32 = 1.75;
const FADE: f32 = 0.22;

const TILE_TOP: Color32 = Color32::from_rgb(0x2b, 0x2c, 0x33);
const TILE_BOTTOM: Color32 = Color32::from_rgb(0x13, 0x13, 0x16);
const INK: Color32 = Color32::from_rgb(0xf3, 0xf4, 0xf6);
const CARET: Color32 = Color32::from_rgb(0x8f, 0xb4, 0xfa);
const DIM: Color32 = Color32::from_rgb(0x8a, 0x8c, 0x94);

/// 0 → 1 over `[from, to]`, eased out, and clamped outside it.
fn ease(t: f32, from: f32, to: f32) -> f32 {
    let x = ((t - from) / (to - from)).clamp(0.0, 1.0);
    1.0 - (1.0 - x).powi(3)
}

/// The first `part` (0..1) of a polyline, by length.
fn partial(points: &[Pos2], part: f32) -> Vec<Pos2> {
    let total: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mut budget = total * part.clamp(0.0, 1.0);
    let mut out = vec![points[0]];
    for pair in points.windows(2) {
        let length = pair[0].distance(pair[1]);
        if length >= budget {
            out.push(pair[0] + (pair[1] - pair[0]) * (budget / length.max(0.001)));
            break;
        }
        budget -= length;
        out.push(pair[1]);
    }
    out
}

/// Draws the start screen `t` seconds after it appeared, filling the whole `ui`.
pub fn show(ui: &mut Ui, t: f32) {
    let area = ui.max_rect();
    let fade = 1.0 - ease(t, DURATION - FADE, DURATION);
    let tint = |color: Color32, alpha: f32| color.gamma_multiply(alpha * fade);
    let painter = ui.painter();

    // The tile: lighter at the top, like the icon.
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(area.left_top(), TILE_TOP);
    mesh.colored_vertex(area.right_top(), TILE_TOP);
    mesh.colored_vertex(area.right_bottom(), TILE_BOTTOM);
    mesh.colored_vertex(area.left_bottom(), TILE_BOTTOM);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));

    // The logo is laid out on the icon's own 256-unit grid, then scaled into place.
    let scale = 150.0 / 256.0;
    let origin = pos2(area.center().x - 128.0 * scale, area.top() + 40.0);
    let at = |x: f32, y: f32| origin + vec2(x, y) * scale;
    let width = 30.0 * scale;
    let stroke = |alpha: f32| Stroke::new(width, tint(INK, alpha));
    // A thick line with round ends, as the icon has.
    let rounded = |points: Vec<Pos2>, alpha: f32| {
        if points.len() < 2 {
            return;
        }
        for end in [points[0], points[points.len() - 1]] {
            painter.circle_filled(end, width / 2.0, tint(INK, alpha));
        }
        painter.add(Shape::line(points, stroke(alpha)));
    };

    // The stem of the r rises first.
    let stem = ease(t, 0.10, 0.50);
    if stem > 0.0 {
        rounded(partial(&[at(86.0, 188.0), at(86.0, 110.0)], stem), 1.0);
    }
    // Then its arm sweeps out of the stem.
    let arm = ease(t, 0.38, 0.85);
    if arm > 0.0 {
        let (p0, p1, p2, p3) = (
            at(86.0, 134.0),
            at(86.0, 104.0),
            at(106.0, 86.0),
            at(138.0, 86.0),
        );
        let mut curve: Vec<Pos2> = (0..=24)
            .map(|n| {
                let u = n as f32 / 24.0;
                let v = 1.0 - u;
                let x = v * v * v * p0.x
                    + 3.0 * v * v * u * p1.x
                    + 3.0 * v * u * u * p2.x
                    + u * u * u * p3.x;
                let y = v * v * v * p0.y
                    + 3.0 * v * v * u * p1.y
                    + 3.0 * v * u * u * p2.y
                    + u * u * u * p3.y;
                pos2(x, y)
            })
            .collect();
        curve.push(at(148.0, 86.0));
        rounded(partial(&curve, arm), 1.0);
    }
    // The cursor pops in, a little too big at first, then blinks like a terminal's.
    let pop = ease(t, 0.78, 1.02);
    if pop > 0.0 {
        let overshoot = 1.0 + 0.28 * (1.0 - pop) * (pop * std::f32::consts::PI).sin();
        let blink = if t > 1.1 && ((t - 1.1) * 2.6).fract() > 0.55 {
            0.35
        } else {
            1.0
        };
        let center = at(164.0, 169.0);
        let size = vec2(36.0, 46.0) * scale * pop * overshoot;
        painter.rect_filled(
            Rect::from_center_size(center, size),
            10.0 * scale,
            tint(CARET, pop * blink),
        );
    }

    // The name settles in under the logo.
    let name = ease(t, 0.75, 1.15);
    if name > 0.0 {
        let y = area.top() + 206.0 + 8.0 * (1.0 - name);
        painter.text(
            pos2(area.center().x, y),
            Align2::CENTER_CENTER,
            "ride",
            FontId::proportional(26.0),
            tint(INK, name),
        );
        painter.text(
            pos2(area.center().x, y + 30.0),
            Align2::CENTER_CENTER,
            "starting",
            FontId::proportional(12.5),
            tint(DIM, name),
        );
    }

    // A thin line along the bottom fills as the start screen runs its course.
    let progress = ease(t, 0.0, DURATION - FADE);
    let track = Rect::from_min_size(
        pos2(area.left(), area.bottom() - 3.0),
        vec2(area.width(), 3.0),
    );
    painter.rect_filled(track, 0.0, tint(Color32::WHITE, 0.06));
    painter.rect_filled(
        Rect::from_min_size(track.min, vec2(track.width() * progress, 3.0)),
        0.0,
        tint(CARET, 0.9),
    );
}
