//! The editor area split into panes: columns side by side, each a stack of panes, and
//! every pane with its own tabs. A tab dragged to the edge of a pane opens a new pane there.

use super::*;
use egui::Rect;

struct Column {
    panes: Vec<u32>,
    /// Its share of the width, relative to the other columns.
    weight: f32,
}

pub(super) struct Panes {
    columns: Vec<Column>,
    next_id: u32,
    /// The tab each pane shows, by the tab's uid.
    pub(super) shown: HashMap<u32, u64>,
    /// The tab being dragged by its title, and whether it was just let go.
    pub(super) drag: Option<u64>,
    pub(super) dropped: bool,
}

impl Default for Panes {
    fn default() -> Self {
        Self {
            columns: vec![Column {
                panes: vec![0],
                weight: 1.0,
            }],
            next_id: 1,
            shown: HashMap::new(),
            drag: None,
            dropped: false,
        }
    }
}

/// Where in a pane a dragged tab would land.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Zone {
    Into,
    Left,
    Right,
    Above,
    Below,
}

/// The zone under the pointer, and the part of the pane the tab would take up.
fn zone_at(rect: Rect, pointer: Pos2) -> (Zone, Rect) {
    let (w, h) = (rect.width() * 0.25, rect.height() * 0.25);
    let half = vec2(rect.width() / 2.0, rect.height());
    let flat = vec2(rect.width(), rect.height() / 2.0);
    if pointer.x < rect.left() + w {
        (Zone::Left, Rect::from_min_size(rect.min, half))
    } else if pointer.x > rect.right() - w {
        (Zone::Right, Rect::from_min_size(rect.center_top(), half))
    } else if pointer.y > rect.bottom() - h {
        (Zone::Below, Rect::from_min_size(rect.left_center(), flat))
    // The strip of tab titles at the top means "among these tabs", not "above them".
    } else if pointer.y < rect.top() + h && pointer.y > rect.top() + 36.0 {
        (Zone::Above, Rect::from_min_size(rect.min, flat))
    } else {
        (Zone::Into, rect)
    }
}

impl Panes {
    fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.columns.iter().flat_map(|c| c.panes.iter().copied())
    }

    fn first(&self) -> u32 {
        self.ids().next().unwrap_or(0)
    }

    fn count(&self) -> usize {
        self.ids().count()
    }

    /// Opens a new pane beside `target` and returns its id.
    fn split(&mut self, target: u32, zone: Zone) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let Some(col) = self.columns.iter().position(|c| c.panes.contains(&target)) else {
            return target;
        };
        let row = self.columns[col]
            .panes
            .iter()
            .position(|p| *p == target)
            .unwrap_or(0);
        match zone {
            Zone::Left | Zone::Right => {
                // The new column takes half of the one it was split from.
                let weight = self.columns[col].weight / 2.0;
                self.columns[col].weight = weight;
                let at = col + usize::from(zone == Zone::Right);
                let column = Column {
                    panes: vec![id],
                    weight,
                };
                self.columns.insert(at, column);
            }
            _ => {
                let at = row + usize::from(zone == Zone::Below);
                self.columns[col].panes.insert(at, id);
            }
        }
        id
    }

    /// Closes the panes that have no tab left. One pane always stays.
    fn prune(&mut self, used: &HashSet<u32>) {
        let keep = self
            .ids()
            .find(|id| used.contains(id))
            .unwrap_or(self.first());
        for column in &mut self.columns {
            column.panes.retain(|id| used.contains(id) || *id == keep);
        }
        self.columns.retain(|c| !c.panes.is_empty());
        self.shown.retain(|id, _| used.contains(id));
    }

    /// Where each pane goes in `area`.
    fn layout(&self, area: Rect) -> Vec<(u32, Rect)> {
        let total: f32 = self.columns.iter().map(|c| c.weight).sum();
        let mut rects = Vec::new();
        let mut x = area.left();
        for column in &self.columns {
            let w = area.width() * column.weight / total;
            let h = area.height() / column.panes.len() as f32;
            for (row, pane) in column.panes.iter().enumerate() {
                let min = Pos2::new(x, area.top() + row as f32 * h);
                rects.push((*pane, Rect::from_min_size(min, vec2(w, h))));
            }
            x += w;
        }
        rects
    }
}

impl App {
    /// The pane keyboard shortcuts act on, and where newly opened files go.
    pub(super) fn focus_pane(&self) -> u32 {
        self.docked_active()
            .map_or(self.panes.first(), |index| self.tabs[index].pane)
    }

    /// The tab a pane shows.
    pub(super) fn pane_tab(&self, pane: u32) -> Option<usize> {
        let here = |t: &Tab| t.pane == pane && !t.detached;
        let shown = self.panes.shown.get(&pane).copied();
        self.tabs
            .iter()
            .position(|t| here(t) && Some(t.uid) == shown)
            .or_else(|| self.tabs.iter().position(here))
    }

    /// Moves a tab into a new pane beside the one it is in.
    pub(super) fn split_tab(&mut self, index: usize, zone: Zone) {
        let pane = self.panes.split(self.tabs[index].pane, zone);
        self.tabs[index].pane = pane;
        self.panes.shown.insert(pane, self.tabs[index].uid);
        self.active = index;
    }

    pub(super) fn editor_area(&mut self, ui: &mut Ui) {
        // RIDE_SPLIT lays the open tabs out in panes, to look at a split without dragging:
        // the second tab to the right, the third below it.
        if self.panes.count() == 1
            && self.tabs.len() >= 3
            && std::env::var_os("RIDE_SPLIT").is_some()
        {
            self.split_tab(1, Zone::Right);
            self.split_tab(2, Zone::Below);
            self.active = 0;
        }
        let first = self.panes.first();
        let known: HashSet<u32> = self.panes.ids().collect();
        for tab in &mut self.tabs {
            if !known.contains(&tab.pane) {
                tab.pane = first;
            }
        }
        let used: HashSet<u32> = self
            .tabs
            .iter()
            .filter(|t| !t.detached)
            .map(|t| t.pane)
            .collect();
        self.panes.prune(&used);
        if let Some(index) = self.docked_active() {
            let tab = &self.tabs[index];
            self.panes.shown.insert(tab.pane, tab.uid);
        }
        let focus = self.focus_pane();

        let area = ui.available_rect_before_wrap();
        let rects = self.panes.layout(area);
        let pressed = ui.input(|i| {
            i.pointer
                .primary_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        for (pane, rect) in &rects {
            let builder = egui::UiBuilder::new()
                .id_salt(("pane", *pane))
                .max_rect(*rect);
            ui.scope_builder(builder, |ui| {
                ui.set_clip_rect(rect.intersect(ui.clip_rect()));
                self.editor_pane(ui, *pane, *pane == focus);
            });
            if rect.left() > area.left() + 1.0 {
                ui.painter()
                    .vline(rect.left(), rect.y_range(), (1.0, theme::LINE()));
            }
            if rect.top() > area.top() + 1.0 {
                ui.painter()
                    .hline(rect.x_range(), rect.top(), (1.0, theme::LINE()));
            }
            // A click anywhere in a pane makes it the one that shortcuts act on.
            if pressed.is_some_and(|p| rect.contains(p)) && *pane != focus {
                if let Some(index) = self.pane_tab(*pane) {
                    self.active = index;
                }
            }
        }
        ui.advance_cursor_after_rect(area);

        // The line between two columns can be dragged to resize them.
        let total: f32 = self.panes.columns.iter().map(|c| c.weight).sum();
        let mut x = area.left();
        for col in 1..self.panes.columns.len() {
            x += area.width() * self.panes.columns[col - 1].weight / total;
            let handle =
                Rect::from_center_size(Pos2::new(x, area.center().y), vec2(7.0, area.height()));
            let resp = ui.interact(handle, ui.id().with(("pane_resize", col)), Sense::drag());
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.dragged() {
                let delta = resp.drag_delta().x / area.width() * total;
                let least = total * 0.12;
                let (left, right) = (
                    self.panes.columns[col - 1].weight,
                    self.panes.columns[col].weight,
                );
                let delta = delta.clamp(least - left, right - least);
                if least - left <= right - least {
                    self.panes.columns[col - 1].weight = left + delta;
                    self.panes.columns[col].weight = right - delta;
                }
            }
        }

        // A tab dragged over a pane shows where it would land, and lands there when let go.
        let pointer = ui.input(|i| i.pointer.interact_pos());
        if let (Some(uid), Some(pointer)) = (self.panes.drag, pointer) {
            if let Some((pane, rect)) = rects.iter().find(|(_, rect)| rect.contains(pointer)) {
                let (zone, shape) = zone_at(*rect, pointer);
                let from = self.tabs.iter().position(|t| t.uid == uid);
                let moves =
                    from.is_some_and(|index| zone != Zone::Into || self.tabs[index].pane != *pane);
                if moves {
                    let layer =
                        egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pane_drop"));
                    ui.ctx().layer_painter(layer).rect(
                        shape.shrink(4.0),
                        8,
                        theme::ACCENT().gamma_multiply(0.18),
                        (1.5, theme::ACCENT()),
                        egui::StrokeKind::Inside,
                    );
                }
                if let (true, true, Some(index)) = (self.panes.dropped, moves, from) {
                    if zone == Zone::Into {
                        self.tabs[index].pane = *pane;
                        self.panes.shown.insert(*pane, uid);
                        self.active = index;
                    } else {
                        // Splitting is relative to the pane under the pointer, not the tab's own.
                        let target = self.panes.split(*pane, zone);
                        self.tabs[index].pane = target;
                        self.panes.shown.insert(target, uid);
                        self.active = index;
                    }
                }
            }
        }
        if !ui.input(|i| i.pointer.any_down()) {
            self.panes.drag = None;
        }
        self.panes.dropped = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panes_split_and_close() {
        let mut panes = Panes::default();
        let right = panes.split(0, Zone::Right);
        let below = panes.split(right, Zone::Below);
        let left = panes.split(0, Zone::Left);
        let order: Vec<Vec<u32>> = panes.columns.iter().map(|c| c.panes.clone()).collect();
        assert_eq!(order, [vec![left], vec![0], vec![right, below]]);

        let area = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let rects = panes.layout(area);
        // Splitting halves the column split from: 0 was halved twice, `right` once.
        let width = |id: u32| rects.iter().find(|(p, _)| *p == id).unwrap().1.width();
        assert_eq!((width(left), width(0), width(right)), (200.0, 200.0, 400.0));
        assert_eq!(
            rects.iter().find(|(p, _)| *p == below).unwrap().1.top(),
            300.0
        );

        // Panes without tabs go away; the columns left share the width.
        panes.prune(&HashSet::from([0, below]));
        let order: Vec<Vec<u32>> = panes.columns.iter().map(|c| c.panes.clone()).collect();
        assert_eq!(order, [vec![0], vec![below]]);
        panes.prune(&HashSet::new());
        assert_eq!(panes.count(), 1, "one pane always stays");
    }

    #[test]
    fn the_drop_zone_follows_the_pointer() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0));
        let zone = |x: f32, y: f32| zone_at(rect, Pos2::new(x, y)).0;
        assert_eq!(zone(20.0, 200.0), Zone::Left);
        assert_eq!(zone(390.0, 200.0), Zone::Right);
        assert_eq!(zone(200.0, 390.0), Zone::Below);
        assert_eq!(zone(200.0, 60.0), Zone::Above);
        assert_eq!(zone(200.0, 10.0), Zone::Into, "the tab titles");
        assert_eq!(zone(200.0, 200.0), Zone::Into);
    }
}
