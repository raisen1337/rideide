//! How a tab is split into panes: a tree whose leaves are panes and whose inner nodes
//! divide their space in two, side by side or one above the other.

use eframe::egui::{pos2, vec2, Rect};

pub enum Node {
    Pane(u64),
    Split {
        /// Side by side (a line from top to bottom between them), or one above the other.
        across: bool,
        /// The first part's share of the space.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// A line between two parts of a split, which can be dragged.
pub struct Divider {
    /// Which split it belongs to: the turns (false = first, true = second) from the root.
    pub path: Vec<bool>,
    pub across: bool,
    /// The whole space of that split, to turn a drag into a new ratio.
    pub area: Rect,
    pub line: Rect,
}

/// Where on a pane a dragged tab lands: beside it on one side, or in among its tabs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Zone {
    Into,
    Left,
    Right,
    Above,
    Below,
}

/// The zone of `rect` under `pointer`, and the part of the pane the tab would take.
pub fn zone_at(rect: Rect, pointer: eframe::egui::Pos2) -> (Zone, Rect) {
    let (w, h) = (rect.width() * 0.28, rect.height() * 0.28);
    let half_w = vec2(rect.width() / 2.0, rect.height());
    let half_h = vec2(rect.width(), rect.height() / 2.0);
    if pointer.x < rect.left() + w {
        (Zone::Left, Rect::from_min_size(rect.min, half_w))
    } else if pointer.x > rect.right() - w {
        (Zone::Right, Rect::from_min_size(rect.center_top(), half_w))
    } else if pointer.y < rect.top() + h {
        (Zone::Above, Rect::from_min_size(rect.min, half_h))
    } else if pointer.y > rect.bottom() - h {
        (Zone::Below, Rect::from_min_size(rect.left_center(), half_h))
    } else {
        (Zone::Into, rect)
    }
}

/// Directions focus moves in with Alt and an arrow key.
#[derive(Clone, Copy)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Node {
    /// Splits the pane `target` in two, the new pane `id` after it, or before it (to the
    /// left or above) with `before`.
    pub fn split(&mut self, target: u64, id: u64, across: bool, before: bool) -> bool {
        match self {
            Node::Pane(pane) if *pane == target => {
                let (old, new) = (Box::new(Node::Pane(target)), Box::new(Node::Pane(id)));
                let (first, second) = if before { (new, old) } else { (old, new) };
                *self = Node::Split {
                    across,
                    ratio: 0.5,
                    first,
                    second,
                };
                true
            }
            Node::Pane(_) => false,
            Node::Split { first, second, .. } => {
                first.split(target, id, across, before) || second.split(target, id, across, before)
            }
        }
    }

    /// Takes the pane out; its sibling takes the space. False if it is the only pane.
    pub fn remove(&mut self, target: u64) -> bool {
        let Node::Split { first, second, .. } = self else {
            return false;
        };
        let keep = if matches!(**first, Node::Pane(p) if p == target) {
            Some(std::mem::replace(&mut **second, Node::Pane(0)))
        } else if matches!(**second, Node::Pane(p) if p == target) {
            Some(std::mem::replace(&mut **first, Node::Pane(0)))
        } else {
            None
        };
        match keep {
            Some(node) => {
                *self = node;
                true
            }
            None => first.remove(target) || second.remove(target),
        }
    }

    pub fn panes(&self) -> Vec<u64> {
        match self {
            Node::Pane(id) => vec![*id],
            Node::Split { first, second, .. } => {
                let mut ids = first.panes();
                ids.extend(second.panes());
                ids
            }
        }
    }

    /// Where each pane goes in `area`, and the dividers between them.
    pub fn layout(&self, area: Rect, panes: &mut Vec<(u64, Rect)>, dividers: &mut Vec<Divider>) {
        self.layout_at(area, &mut Vec::new(), panes, dividers);
    }

    fn layout_at(
        &self,
        area: Rect,
        path: &mut Vec<bool>,
        panes: &mut Vec<(u64, Rect)>,
        dividers: &mut Vec<Divider>,
    ) {
        match self {
            Node::Pane(id) => panes.push((*id, area)),
            Node::Split {
                across,
                ratio,
                first,
                second,
            } => {
                let (a, b, line) = if *across {
                    let x = area.left() + area.width() * ratio;
                    (
                        Rect::from_min_max(area.min, pos2(x, area.max.y)),
                        Rect::from_min_max(pos2(x, area.min.y), area.max),
                        Rect::from_center_size(pos2(x, area.center().y), vec2(7.0, area.height())),
                    )
                } else {
                    let y = area.top() + area.height() * ratio;
                    (
                        Rect::from_min_max(area.min, pos2(area.max.x, y)),
                        Rect::from_min_max(pos2(area.min.x, y), area.max),
                        Rect::from_center_size(pos2(area.center().x, y), vec2(area.width(), 7.0)),
                    )
                };
                dividers.push(Divider {
                    path: path.clone(),
                    across: *across,
                    area,
                    line,
                });
                path.push(false);
                first.layout_at(a, path, panes, dividers);
                path.pop();
                path.push(true);
                second.layout_at(b, path, panes, dividers);
                path.pop();
            }
        }
    }

    /// Moves the divider of the split at `path` by `delta` points.
    pub fn drag(&mut self, path: &[bool], delta: f32, size: f32) {
        let mut node = self;
        for &second in path {
            let Node::Split {
                first,
                second: other,
                ..
            } = node
            else {
                return;
            };
            node = if second { other } else { first };
        }
        if let Node::Split { ratio, .. } = node {
            if size > 0.0 {
                *ratio = (*ratio + delta / size).clamp(0.1, 0.9);
            }
        }
    }
}

/// The pane next to `from` in the direction given: of those whose edge touches it on that
/// side and that overlap it, the one closest to its middle.
pub fn neighbour(rects: &[(u64, Rect)], from: u64, dir: Dir) -> Option<u64> {
    let (_, here) = rects.iter().find(|(id, _)| *id == from)?;
    const NEAR: f32 = 3.0;
    rects
        .iter()
        .filter(|(id, _)| *id != from)
        .filter(|(_, r)| match dir {
            Dir::Left => (r.right() - here.left()).abs() < NEAR,
            Dir::Right => (r.left() - here.right()).abs() < NEAR,
            Dir::Up => (r.bottom() - here.top()).abs() < NEAR,
            Dir::Down => (r.top() - here.bottom()).abs() < NEAR,
        })
        .filter(|(_, r)| match dir {
            Dir::Left | Dir::Right => r.top() < here.bottom() && r.bottom() > here.top(),
            Dir::Up | Dir::Down => r.left() < here.right() && r.right() > here.left(),
        })
        .min_by(|(_, a), (_, b)| {
            let gap = |r: &Rect| (r.center() - here.center()).length();
            gap(a).total_cmp(&gap(b))
        })
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0))
    }

    #[test]
    fn split_and_remove() {
        let mut root = Node::Pane(1);
        assert!(root.split(1, 2, true, false));
        assert!(root.split(2, 3, false, false));
        assert_eq!(root.panes(), [1, 2, 3]);
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        root.layout(area(), &mut panes, &mut dividers);
        assert_eq!(
            panes[0].1,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(50.0, 100.0))
        );
        assert_eq!(
            panes[2].1,
            Rect::from_min_max(pos2(50.0, 50.0), pos2(100.0, 100.0))
        );
        assert_eq!(dividers.len(), 2);
        assert_eq!(neighbour(&panes, 1, Dir::Right), Some(2));
        assert_eq!(neighbour(&panes, 3, Dir::Up), Some(2));
        assert_eq!(neighbour(&panes, 3, Dir::Left), Some(1));
        assert_eq!(neighbour(&panes, 1, Dir::Left), None);
        assert!(root.remove(2));
        assert_eq!(root.panes(), [1, 3]);
        assert!(root.remove(1));
        assert_eq!(root.panes(), [3]);
        assert!(!root.remove(3));
    }

    #[test]
    fn split_before_puts_the_new_pane_first() {
        let mut root = Node::Pane(1);
        root.split(1, 2, false, true);
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        root.layout(area(), &mut panes, &mut dividers);
        assert_eq!(
            panes[0],
            (2, Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 50.0)))
        );
        assert_eq!(root.panes(), [2, 1]);
    }

    #[test]
    fn dividers_move_within_bounds() {
        let mut root = Node::Pane(1);
        root.split(1, 2, true, false);
        root.drag(&[], 30.0, 100.0);
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        root.layout(area(), &mut panes, &mut dividers);
        assert_eq!(panes[0].1.width(), 80.0);
        root.drag(&[], 500.0, 100.0);
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        root.layout(area(), &mut panes, &mut dividers);
        assert_eq!(panes[0].1.width(), 90.0);
    }
}
