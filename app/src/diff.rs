//! A line diff, used for inline-edit proposals and the Changes panel.

use crate::theme;
use eframe::egui::{self, text::LayoutJob, Color32, FontId, ScrollArea, TextFormat, Ui};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Op {
    Same,
    Removed,
    Added,
}

pub type Line = (Op, String);

/// Above this many line pairs the table would be too large; the diff degrades to
/// "everything removed, everything added".
const MAX_CELLS: usize = 4_000_000;
/// Unchanged lines kept around each change when a diff is shown.
const CONTEXT: usize = 3;

/// Line-by-line diff of two texts, by longest common subsequence.
pub fn lines(old: &str, new: &str) -> Vec<Line> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();

    // Shared head and tail don't need the table.
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (mid_a, mid_b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);

    let mut out: Vec<Line> = a[..head]
        .iter()
        .map(|l| (Op::Same, l.to_string()))
        .collect();
    if mid_a.len() * mid_b.len() > MAX_CELLS {
        out.extend(mid_a.iter().map(|l| (Op::Removed, l.to_string())));
        out.extend(mid_b.iter().map(|l| (Op::Added, l.to_string())));
    } else {
        // lcs[i][j] = length of the LCS of mid_a[i..] and mid_b[j..].
        let (n, m) = (mid_a.len(), mid_b.len());
        let mut lcs = vec![0u32; (n + 1) * (m + 1)];
        let at = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[at(i, j)] = if mid_a[i] == mid_b[j] {
                    lcs[at(i + 1, j + 1)] + 1
                } else {
                    lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if mid_a[i] == mid_b[j] {
                out.push((Op::Same, mid_a[i].to_string()));
                (i, j) = (i + 1, j + 1);
            } else if lcs[at(i + 1, j)] >= lcs[at(i, j + 1)] {
                out.push((Op::Removed, mid_a[i].to_string()));
                i += 1;
            } else {
                out.push((Op::Added, mid_b[j].to_string()));
                j += 1;
            }
        }
        out.extend(mid_a[i..].iter().map(|l| (Op::Removed, l.to_string())));
        out.extend(mid_b[j..].iter().map(|l| (Op::Added, l.to_string())));
    }
    out.extend(
        a[a.len() - tail..]
            .iter()
            .map(|l| (Op::Same, l.to_string())),
    );
    out
}

/// (added, removed) line counts.
pub fn stats(diff: &[Line]) -> (usize, usize) {
    let count = |op: Op| diff.iter().filter(|(o, _)| *o == op).count();
    (count(Op::Added), count(Op::Removed))
}

/// Draws a diff: red for removed lines, green for added, with long unchanged runs folded.
pub fn show(ui: &mut Ui, diff: &[Line], salt: &str, max_height: f32) {
    let near_change = |i: usize| {
        let from = i.saturating_sub(CONTEXT);
        let to = (i + CONTEXT + 1).min(diff.len());
        diff[from..to].iter().any(|(op, _)| *op != Op::Same)
    };
    let font = FontId::monospace(13.0);
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let mut folded = false;
    for (i, (op, text)) in diff.iter().enumerate() {
        if *op == Op::Same && !near_change(i) {
            if !folded {
                job.append(
                    "    ⋯\n",
                    0.0,
                    TextFormat::simple(font.clone(), theme::LINE_NUMBER()),
                );
                folded = true;
            }
            continue;
        }
        folded = false;
        let (mark, color, background) = match op {
            Op::Same => (' ', theme::COMMENT(), Color32::TRANSPARENT),
            Op::Removed => (
                '-',
                Color32::from_rgb(0xf0, 0x8a, 0x8a),
                Color32::from_rgba_unmultiplied(0xe0, 0x50, 0x50, 36),
            ),
            Op::Added => (
                '+',
                Color32::from_rgb(0x9a, 0xd8, 0x8a),
                Color32::from_rgba_unmultiplied(0x60, 0xc0, 0x60, 34),
            ),
        };
        let format = TextFormat {
            background,
            ..TextFormat::simple(font.clone(), color)
        };
        job.append(
            &format!("{mark} {}\n", text.replace('\t', "    ")),
            0.0,
            format,
        );
    }
    ScrollArea::both()
        .id_salt(salt)
        .max_height(max_height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.add(egui::Label::new(job).extend());
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_changed_line() {
        let diff = lines("a\nb\nc\nd", "a\nB\nc\nd\ne");
        let changed: Vec<&Line> = diff.iter().filter(|(op, _)| *op != Op::Same).collect();
        assert_eq!(
            changed,
            [
                &(Op::Removed, "b".to_string()),
                &(Op::Added, "B".to_string()),
                &(Op::Added, "e".to_string())
            ]
        );
        assert_eq!(stats(&diff), (2, 1));
    }

    #[test]
    fn identical_and_empty_inputs() {
        assert!(lines("x\ny", "x\ny").iter().all(|(op, _)| *op == Op::Same));
        assert_eq!(lines("", "new"), [(Op::Added, "new".to_string())]);
        assert_eq!(lines("old", ""), [(Op::Removed, "old".to_string())]);
    }
}
