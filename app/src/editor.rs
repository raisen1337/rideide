//! The code editor widget: draws a `Buffer` and turns input events into edits.

use crate::buffer::{Buffer, Pos};
use crate::highlight::{lex, Kind, Lang, State};
use crate::outline;
use crate::theme;
use eframe::egui::{
    self, pos2, text::LayoutJob, vec2, Color32, CursorIcon, Event, EventFilter, FontId, Key, Rect,
    ScrollArea, Sense, TextFormat, Ui,
};
use std::collections::BTreeSet;

/// Lines longer than this are drawn without colours, and only their visible part.
const LONG_LINE: usize = 2000;
/// Files longer than this get no outline, folding or coloured minimap: reading them on
/// every edit would be felt.
const BIG_FILE: usize = 50_000;
const MINIMAP_W: f32 = 96.0;
/// Height of a line in the minimap, and width of a character, in points.
const MINI_ROW: f32 = 2.0;
const MINI_COL: f32 = 1.1;

/// Something the user asked for in the editor that the app has to carry out.
#[derive(Clone, Copy)]
pub enum Action {
    /// Rewrite the selection with Claude (Ctrl+K).
    InlineEdit,
    Explain,
    AddToChat,
    /// Run a query tab's SQL.
    RunQuery,
    /// Go to where what is at this place is defined (F12, Ctrl+click).
    Definition(Pos),
    /// Find every use of what is at the cursor (Shift+F12).
    References,
    /// Rename what is at the cursor everywhere (F2).
    Rename,
    /// Quick fixes for the cursor's line (Ctrl+.).
    CodeActions,
    /// Show completions now (Ctrl+Space).
    Complete,
    /// Take the completion with this index.
    Accept(usize),
    /// Run the quick fix with this index.
    RunAction(usize),
    /// A button of the note about the file's language server.
    Server(crate::intel::NoteAction),
}

#[derive(Default)]
pub struct EditorState {
    /// Scroll so the cursor is visible on the next frame.
    pub reveal_cursor: bool,
    pub request_focus: bool,
    /// Syntax errors found in the file: 0-based line and message.
    pub diags: Vec<(usize, String)>,
    /// Lines that differ from the last commit: first line, count, and 0 added, 1 changed,
    /// 2 removed below.
    pub marks: Vec<(usize, usize, u8)>,
    /// The functions, classes and types declared in the file.
    pub symbols: Vec<outline::Symbol>,
    /// Per line, who changed it last and when (from `git blame`), for the text as it was
    /// at `blame_revision`.
    pub blame: Vec<String>,
    pub blame_revision: Option<u64>,
    /// A `.env` file shown as plain text rather than as its table; values shown in the
    /// table, all of them or by row.
    pub env_as_text: bool,
    pub env_show_all: bool,
    pub env_revealed: BTreeSet<usize>,
    /// Others editing this file live: who, and where.
    pub peers: Vec<crate::collab::Mark>,
    /// What the language server has to show: completions, hover, problems.
    pub intel: crate::intel::Intel,
    /// First lines of the folded blocks.
    pub folded: BTreeSet<usize>,
    /// For each line, the last line of the block starting there.
    fold_ends: Vec<Option<usize>>,
    /// The lines on screen, in order: every line not inside a folded block.
    rows: Vec<usize>,
    rows_key: Option<(u64, usize)>,
    /// Bumped when `folded` changes, so `rows` is rebuilt.
    folds_changed: usize,
    line_count: usize,
    /// Lexer state at the start of each line, filled lazily down to the last visible one.
    states: Vec<State>,
    max_cols: usize,
    cached_revision: Option<u64>,
    /// Where the view is (top and height, in points), for the minimap.
    view_top: f32,
    view_h: f32,
    scroll_to: Option<f32>,
    /// The bracket at the cursor and the one it pairs with, for the revision and cursor
    /// they were found for.
    brackets: Option<(u64, Pos, Option<(Pos, Pos)>)>,
}

impl EditorState {
    fn fold_end(&self, line: usize) -> Option<usize> {
        self.fold_ends.get(line).copied().flatten()
    }

    /// Whether a block starts on `line`.
    pub fn foldable(&self, line: usize) -> bool {
        self.fold_end(line).is_some()
    }

    pub fn toggle_fold(&mut self, line: usize) {
        if !self.folded.remove(&line) && self.foldable(line) {
            self.folded.insert(line);
        }
        self.folds_changed += 1;
    }

    /// Folds the innermost block the line is in, or that starts on it.
    pub fn fold_at(&mut self, line: usize) {
        let start = (0..=line).rev().find(|&s| {
            self.fold_end(s).is_some_and(|end| end >= line) && !self.folded.contains(&s)
        });
        if let Some(start) = start {
            self.folded.insert(start);
            self.folds_changed += 1;
        }
    }

    /// Unfolds the folded block the line is in, or that starts on it.
    pub fn unfold_at(&mut self, line: usize) {
        let start = self
            .folded
            .iter()
            .rev()
            .find(|&&s| s <= line && self.fold_end(s).is_some_and(|end| end >= line))
            .copied();
        if let Some(start) = start {
            self.folded.remove(&start);
            self.folds_changed += 1;
        }
    }

    pub fn fold_all(&mut self) {
        self.folded = (0..self.fold_ends.len())
            .filter(|&l| self.foldable(l))
            .collect();
        self.folds_changed += 1;
    }

    pub fn unfold_all(&mut self) {
        self.folded.clear();
        self.folds_changed += 1;
    }

    /// The folded block hiding `line`, if one does: its first line and its last.
    fn hidden_by(&self, line: usize) -> Option<(usize, usize)> {
        self.folded
            .range(..line)
            .filter_map(|&s| Some((s, self.fold_end(s)?)))
            .find(|&(_, end)| end >= line)
    }

    /// The screen row a line is drawn on; a hidden line is on its fold's row.
    fn row_of(&self, line: usize) -> usize {
        match self.rows.binary_search(&line) {
            Ok(row) => row,
            Err(row) => row.saturating_sub(1),
        }
    }

    fn rebuild(&mut self, buf: &Buffer) {
        let count = buf.line_count();
        if count != self.line_count {
            self.line_count = count;
        }
        if count <= BIG_FILE {
            let lines: Vec<&str> = (0..count).map(|i| buf.line(i)).collect();
            self.fold_ends = outline::fold_ends(&lines);
            self.symbols = outline::symbols(&lines);
        } else {
            self.fold_ends = vec![None; count];
            self.symbols.clear();
        }
        // A block that can't be folded any more (its lines were changed) opens.
        let ends = &self.fold_ends;
        self.folded
            .retain(|&s| ends.get(s).copied().flatten().is_some());
    }

    fn rebuild_rows(&mut self, revision: u64) {
        let key = (revision, self.folds_changed);
        if self.rows_key == Some(key) {
            return;
        }
        self.rows_key = Some(key);
        self.rows.clear();
        let mut line = 0;
        while line < self.line_count {
            self.rows.push(line);
            line = match self
                .folded
                .contains(&line)
                .then(|| self.fold_end(line))
                .flatten()
            {
                Some(end) => end + 1,
                None => line + 1,
            };
        }
        if self.rows.is_empty() {
            self.rows.push(0);
        }
    }

    /// Lines were added or removed at `at`: folds below it move with their text.
    fn shift_folds(&mut self, at: usize, delta: isize) {
        if delta == 0 || self.folded.is_empty() {
            return;
        }
        self.folded = self
            .folded
            .iter()
            .filter_map(|&s| {
                if s <= at {
                    Some(s)
                } else {
                    s.checked_add_signed(delta).filter(|&n| n > at)
                }
            })
            .collect();
        self.folds_changed += 1;
    }
}

fn char_width(c: char, vcol: usize) -> usize {
    if c == '\t' {
        let tab = crate::settings::with(|s| s.tab_width);
        tab - vcol % tab
    } else {
        1
    }
}

/// Screen column of char `col`, with tabs expanded.
fn visual_col(line: &str, col: usize) -> usize {
    line.chars().take(col).fold(0, |v, c| v + char_width(c, v))
}

fn col_at_visual(line: &str, vcol: usize) -> usize {
    let mut v = 0;
    for (i, c) in line.chars().enumerate() {
        let w = char_width(c, v);
        if v + w / 2 >= vcol {
            return i;
        }
        v += w;
    }
    line.chars().count()
}

/// The char drawn at a screen column, if the line reaches that far.
fn col_under(line: &str, vcol: usize) -> Option<usize> {
    let mut v = 0;
    for (i, c) in line.chars().enumerate() {
        let w = char_width(c, v);
        if vcol < v + w {
            return Some(i);
        }
        v += w;
    }
    None
}

/// A wavy line under text, as editors mark problems.
fn squiggle(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    let (step, height) = (3.0, 1.6);
    let mut points = Vec::new();
    let mut x = x0;
    let mut up = true;
    while x < x1 {
        points.push(pos2(x, if up { y - height } else { y }));
        x += step;
        up = !up;
    }
    points.push(pos2(x1, if up { y - height } else { y }));
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.0, color)));
}

fn color(kind: Kind) -> Color32 {
    match kind {
        Kind::Text => theme::TEXT(),
        Kind::Keyword => theme::KEYWORD(),
        Kind::Type => theme::TYPE(),
        Kind::Func => theme::FUNC(),
        Kind::Str => theme::STRING(),
        Kind::Number => theme::NUMBER(),
        Kind::Comment => theme::COMMENT(),
        Kind::Punct => theme::PUNCT(),
    }
}

fn sem_color(kind: crate::lsp::SemKind) -> Color32 {
    use crate::lsp::SemKind;
    match kind {
        SemKind::Type => theme::TYPE(),
        SemKind::Param => theme::PARAM(),
        SemKind::Variable => theme::VARIABLE(),
        SemKind::Property => theme::PROPERTY(),
        SemKind::Constant => theme::CONSTANT(),
        SemKind::Func => theme::FUNC(),
        SemKind::Keyword => theme::KEYWORD(),
    }
}

/// The language server's colours for a line, while the line is as it was when it gave them.
fn line_sem(st: &EditorState, buf: &Buffer, line: usize) -> Vec<(usize, usize, Color32)> {
    match st.intel.sem.get(&line) {
        Some((hash, names)) if *hash == crate::intel::line_hash(buf.line(line)) => names
            .iter()
            .map(|&(from, to, kind)| (from, to, sem_color(kind)))
            .collect(),
        _ => Vec::new(),
    }
}

/// The inlay hints of a line, while it is as it was when they came: where each goes (the
/// char it comes before) and its text.
fn line_hints(st: &EditorState, buf: &Buffer, line: usize) -> Vec<(usize, String)> {
    let hints = &st.intel.hints;
    let from = hints.partition_point(|h| h.at.line < line);
    let to = hints.partition_point(|h| h.at.line <= line);
    if from == to || buf.line(line).len() > LONG_LINE {
        return Vec::new();
    }
    let hash = crate::intel::line_hash(buf.line(line));
    hints[from..to]
        .iter()
        .filter(|h| h.hash == hash)
        .map(|h| (h.at.col, h.label.clone()))
        .collect()
}

/// Columns taken by the hints before char `col`: a hint at the cursor's place is drawn
/// after the cursor.
fn hint_shift(hints: &[(usize, String)], col: usize) -> usize {
    hints
        .iter()
        .filter(|h| h.0 < col)
        .map(|h| h.1.chars().count())
        .sum()
}

/// A screen column on a line with hints, as a column of its text; one inside a hint is
/// the hint's place.
fn without_hints(text: &str, hints: &[(usize, String)], vcol: f32) -> f32 {
    let mut shift = 0.0;
    for (col, label) in hints {
        let start = visual_col(text, *col) as f32 + shift;
        if vcol < start {
            break;
        }
        let width = label.chars().count() as f32;
        if vcol < start + width {
            return visual_col(text, *col) as f32;
        }
        shift += width;
    }
    vcol - shift
}

/// A line laid out in its colours: the syntax's, the language server's for the names it
/// knows, and its inlay hints among the text.
fn layout_line(
    line: &str,
    lang: &Lang,
    state: State,
    font: &FontId,
    sem: &[(usize, usize, Color32)],
    hints: &[(usize, String)],
) -> LayoutJob {
    let mut spans = Vec::new();
    lex(lang, line, state, &mut spans);

    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let hint_format = TextFormat {
        font_id: font.clone(),
        color: theme::COMMENT(),
        background: theme::tint(14),
        ..Default::default()
    };
    let flush = |job: &mut LayoutJob, run: &mut String, colour: Color32| {
        if !run.is_empty() {
            job.append(run, 0.0, TextFormat::simple(font.clone(), colour));
            run.clear();
        }
    };
    let mut run = String::new();
    let mut run_colour = theme::TEXT();
    let mut next_hint = 0;
    let (mut start, mut vcol, mut index) = (0, 0, 0);
    for (end, kind) in spans {
        let base = color(kind);
        // Strings, comments and keywords keep the syntax's colours; names may get the server's.
        let name = matches!(kind, Kind::Text | Kind::Func | Kind::Type);
        for c in line[start..end].chars() {
            while let Some((_, label)) = hints.get(next_hint).filter(|h| h.0 <= index) {
                flush(&mut job, &mut run, run_colour);
                job.append(label, 0.0, hint_format.clone());
                next_hint += 1;
            }
            let colour = if name {
                sem.iter()
                    .find(|s| index >= s.0 && index < s.1)
                    .map_or(base, |s| s.2)
            } else {
                base
            };
            if colour != run_colour {
                flush(&mut job, &mut run, run_colour);
                run_colour = colour;
            }
            let w = char_width(c, vcol);
            if c == '\t' {
                run.extend(std::iter::repeat_n(' ', w));
            } else {
                run.push(c);
            }
            vcol += w;
            index += 1;
        }
        start = end;
    }
    flush(&mut job, &mut run, run_colour);
    for (_, label) in hints.get(next_hint..).unwrap_or(&[]) {
        job.append(label, 0.0, hint_format.clone());
    }
    job
}

/// The bracket next to the cursor (after it, or else before it) and the one it pairs with.
fn matching_brackets(buf: &Buffer, cursor: Pos) -> Option<(Pos, Pos)> {
    let line: Vec<char> = buf.line(cursor.line).chars().collect();
    for col in [Some(cursor.col), cursor.col.checked_sub(1)]
        .into_iter()
        .flatten()
    {
        let Some(&c) = line.get(col) else { continue };
        let (open, close, forward) = match c {
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '{' => ('{', '}', true),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            '}' => ('{', '}', false),
            _ => continue,
        };
        let at = Pos::new(cursor.line, col);
        let mut depth = 0i32;
        // Far enough for any block one would look at; a runaway search would be felt.
        let lines = if forward {
            (cursor.line..buf.line_count().min(cursor.line + 3000)).collect::<Vec<_>>()
        } else {
            (cursor.line.saturating_sub(3000)..=cursor.line)
                .rev()
                .collect()
        };
        for l in lines {
            let chars: Vec<char> = buf.line(l).chars().collect();
            let cols: Vec<usize> = if forward {
                let from = if l == cursor.line { col } else { 0 };
                (from..chars.len()).collect()
            } else {
                let to = if l == cursor.line {
                    col + 1
                } else {
                    chars.len()
                };
                (0..to).rev().collect()
            };
            for i in cols {
                let ch = chars[i];
                if ch == open {
                    depth += if forward { 1 } else { -1 };
                } else if ch == close {
                    depth += if forward { -1 } else { 1 };
                }
                if depth == 0 {
                    return Some((at, Pos::new(l, i)));
                }
            }
        }
        return None;
    }
    None
}

/// The first lines of the blocks the top of the view is inside, outermost first: they stay
/// pinned above the code (sticky scroll).
fn sticky_lines(st: &EditorState, first_row: usize, max: usize) -> Vec<usize> {
    let mut sticky: Vec<usize> = Vec::new();
    while sticky.len() < max {
        // The line just under the pinned ones.
        let Some(&probe) = st.rows.get(first_row + sticky.len()) else {
            break;
        };
        let next = (0..probe)
            .filter(|&s| st.fold_end(s).is_some_and(|end| end >= probe))
            .filter(|s| !sticky.contains(s))
            .find(|&s| st.row_of(s) < first_row + sticky.len());
        match next {
            Some(line) => sticky.push(line),
            None => break,
        }
    }
    sticky
}

/// How far a line is indented, in screen columns; a blank line counts as indented like
/// the lines around it.
fn indent_of(buf: &Buffer, line: usize) -> usize {
    let measure = |l: usize| {
        let text = buf.line(l);
        let blank = text.trim().is_empty();
        let ws = text.chars().take_while(|c| c.is_whitespace()).count();
        (!blank).then(|| visual_col(text, ws))
    };
    if let Some(indent) = measure(line) {
        return indent;
    }
    let before = (line.saturating_sub(100)..line).rev().find_map(measure);
    let after = (line + 1..buf.line_count().min(line + 100)).find_map(measure);
    before.unwrap_or(0).max(after.unwrap_or(0))
}

/// Lexer states down to `line`, from where they were last filled.
fn fill_states(st: &mut EditorState, buf: &Buffer, lang: &Lang, line: usize) {
    if st.states.is_empty() {
        st.states.push(State::Normal);
    }
    let mut scratch = Vec::new();
    while st.states.len() <= line && st.states.len() <= buf.line_count() {
        let i = st.states.len() - 1;
        scratch.clear();
        let next = lex(lang, buf.line(i), st.states[i], &mut scratch);
        st.states.push(next);
    }
}

pub fn show(ui: &mut Ui, buf: &mut Buffer, st: &mut EditorState, lang: &Lang) -> Option<Action> {
    let prefs = crate::settings::get();
    let full = ui.available_rect_before_wrap();
    let minimap = prefs.minimap && full.width() > 420.0;
    let editor_rect = if minimap {
        Rect::from_min_max(full.min, pos2(full.max.x - MINIMAP_W, full.max.y))
    } else {
        full
    };
    let mut action = ui
        .scope_builder(egui::UiBuilder::new().max_rect(editor_rect), |ui| {
            code(ui, buf, st, lang, &prefs)
        })
        .inner;
    if let Some(chosen) = crate::intel::popups(ui, buf, st) {
        action = Some(chosen);
    }
    if minimap {
        let rect = Rect::from_min_max(pos2(editor_rect.max.x, full.min.y), full.max);
        minimap_ui(ui, rect, buf, st, lang, &prefs);
    }
    ui.advance_cursor_after_rect(full);
    action
}

/// The file in miniature beside the editor, with the part on screen marked. Clicking or
/// dragging in it scrolls there.
fn minimap_ui(
    ui: &mut Ui,
    rect: Rect,
    buf: &Buffer,
    st: &mut EditorState,
    lang: &Lang,
    prefs: &crate::settings::Settings,
) {
    let row_h = code_row_h(ui, prefs);
    let total = st.rows.len().max(1);
    let fits = (rect.height() / MINI_ROW).floor().max(1.0) as usize;
    let on_screen = (st.view_h / row_h).ceil() as usize;
    let top_row = (st.view_top / row_h).max(0.0);
    // A file taller than the minimap scrolls in it, in step with the editor.
    let first = if total > fits {
        let room = total.saturating_sub(on_screen).max(1) as f32;
        ((top_row / room).clamp(0.0, 1.0) * (total - fits) as f32) as usize
    } else {
        0
    };
    let last = (first + fits).min(total);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::EDITOR_BG());
    painter.line_segment(
        [rect.left_top(), rect.left_bottom()],
        (1.0, theme::tint(10)),
    );

    let colours = buf.line_count() <= BIG_FILE;
    if colours {
        if let Some(&line) = st.rows.get(last.saturating_sub(1)) {
            fill_states(st, buf, lang, line);
        }
    }
    let x0 = rect.min.x + 6.0;
    let mut spans = Vec::new();
    for (index, &line) in st.rows[first..last].iter().enumerate() {
        let y = rect.min.y + index as f32 * MINI_ROW;
        let text = buf.line(line);
        if text.len() > LONG_LINE {
            continue;
        }
        spans.clear();
        if colours {
            lex(lang, text, st.states[line], &mut spans);
        } else {
            spans.push((text.len(), Kind::Text));
        }
        let (mut start, mut vcol) = (0, 0usize);
        for &(end, kind) in &spans {
            let colour = color(kind).gamma_multiply(0.55);
            let mut run: Option<(usize, usize)> = None;
            for c in text[start..end].chars() {
                let w = char_width(c, vcol);
                if c.is_whitespace() {
                    if let Some((a, b)) = run.take() {
                        let r = Rect::from_min_max(
                            pos2(x0 + a as f32 * MINI_COL, y),
                            pos2(x0 + b as f32 * MINI_COL, y + MINI_ROW - 0.4),
                        );
                        painter.rect_filled(r, 0.0, colour);
                    }
                } else {
                    run = Some(run.map_or((vcol, vcol + w), |(a, _)| (a, vcol + w)));
                }
                vcol += w;
                if vcol as f32 * MINI_COL > MINIMAP_W {
                    break;
                }
            }
            if let Some((a, b)) = run {
                let r = Rect::from_min_max(
                    pos2(x0 + a as f32 * MINI_COL, y),
                    pos2(x0 + b as f32 * MINI_COL, y + MINI_ROW - 0.4),
                );
                painter.rect_filled(r, 0.0, colour);
            }
            start = end;
        }
    }

    // The part of the file the editor shows.
    let slider_top = rect.min.y + (top_row - first as f32) * MINI_ROW;
    let slider = Rect::from_min_size(
        pos2(rect.min.x + 1.0, slider_top),
        vec2(rect.width() - 1.0, (on_screen as f32 * MINI_ROW).max(8.0)),
    );
    let resp = ui.interact(rect, ui.id().with("minimap"), Sense::click_and_drag());
    let shade = if resp.hovered() || resp.dragged() {
        22
    } else {
        12
    };
    painter.rect_filled(slider, 0.0, theme::tint(shade));
    if let Some(p) = resp
        .interact_pointer_pos()
        .filter(|_| resp.is_pointer_button_down_on())
    {
        let row = first as f32 + (p.y - rect.min.y) / MINI_ROW - on_screen as f32 / 2.0;
        st.scroll_to = Some(row.max(0.0) * row_h);
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_for(buf: &Buffer) -> EditorState {
        let mut st = EditorState {
            line_count: buf.line_count(),
            ..Default::default()
        };
        st.rebuild(buf);
        st.rebuild_rows(buf.revision);
        st
    }

    #[test]
    fn folded_blocks_leave_their_lines_off_screen() {
        let buf = Buffer::new("a {\n  b\n  c {\n    d\n  }\n}\ne");
        let mut st = state_for(&buf);
        assert_eq!(st.rows, [0, 1, 2, 3, 4, 5, 6]);
        st.toggle_fold(2);
        st.rebuild_rows(buf.revision);
        // The closing line stays, as the end of the folded block.
        assert_eq!(st.rows, [0, 1, 2, 4, 5, 6]);
        assert_eq!(st.hidden_by(3), Some((2, 3)));
        assert_eq!(st.row_of(5), 4);
        st.fold_at(1);
        st.rebuild_rows(buf.revision);
        assert_eq!(st.rows, [0, 5, 6]);
        st.unfold_all();
        st.rebuild_rows(buf.revision);
        assert_eq!(st.rows.len(), 7);
    }

    #[test]
    fn the_blocks_above_the_view_stay_pinned() {
        let buf = Buffer::new("a {\n  b {\n    c\n    d\n    e\n  }\n}\nf");
        let st = state_for(&buf);
        // Scrolled so line 3 (`d`) is at the top: inside `a {` and `b {`.
        assert_eq!(sticky_lines(&st, 3, 5), [0, 1]);
        assert_eq!(sticky_lines(&st, 3, 1), [0]);
        // At the top nothing is above the view.
        assert!(sticky_lines(&st, 0, 5).is_empty());
        assert!(sticky_lines(&st, 7, 5).is_empty());
    }

    #[test]
    fn brackets_pair_up_across_lines() {
        let buf = Buffer::new("f(a, [b]) {\n  g();\n}");
        assert_eq!(
            matching_brackets(&buf, Pos::new(0, 1)),
            Some((Pos::new(0, 1), Pos::new(0, 8)))
        );
        assert_eq!(
            matching_brackets(&buf, Pos::new(0, 7)),
            Some((Pos::new(0, 7), Pos::new(0, 5)))
        );
        // Just after a closing one, with none after the cursor.
        assert_eq!(
            matching_brackets(&buf, Pos::new(0, 9)),
            Some((Pos::new(0, 8), Pos::new(0, 1)))
        );
        assert_eq!(
            matching_brackets(&buf, Pos::new(2, 0)),
            Some((Pos::new(2, 0), Pos::new(0, 10)))
        );
        assert_eq!(matching_brackets(&buf, Pos::new(1, 2)), None);
    }

    #[test]
    fn clicks_beside_inlay_hints_land_on_the_text() {
        let text = "greet(\"x\")";
        let hints = vec![(6, "name: ".to_string())];
        // Before the hint, inside it, and on the text after it.
        assert_eq!(without_hints(text, &hints, 3.0), 3.0);
        assert_eq!(without_hints(text, &hints, 8.0), 6.0);
        assert_eq!(without_hints(text, &hints, 13.0), 7.0);
        assert_eq!(hint_shift(&hints, 6), 0);
        assert_eq!(hint_shift(&hints, 7), 6);
    }

    #[test]
    fn folds_move_with_lines_added_above() {
        let buf = Buffer::new("x\na {\n  b\n}");
        let mut st = state_for(&buf);
        st.toggle_fold(1);
        st.shift_folds(0, 2);
        assert!(st.folded.contains(&3));
        st.shift_folds(0, -2);
        assert!(st.folded.contains(&1));
    }
}

fn code_row_h(ui: &Ui, prefs: &crate::settings::Settings) -> f32 {
    let font = FontId::monospace(prefs.code_size);
    let glyph_h = ui.ctx().fonts_mut(|f| f.row_height(&font));
    (glyph_h + prefs.line_gap).round()
}

fn code(
    ui: &mut Ui,
    buf: &mut Buffer,
    st: &mut EditorState,
    lang: &Lang,
    prefs: &crate::settings::Settings,
) -> Option<Action> {
    let font = FontId::monospace(prefs.code_size);
    let (cw, glyph_h) = ui
        .ctx()
        .fonts_mut(|f| (f.glyph_width(&font, 'm'), f.row_height(&font)));
    let row_h = (glyph_h + prefs.line_gap).round();
    let pad_y = ((row_h - glyph_h) / 2.0).floor();

    st.intel.typed = None;
    st.intel.keys.clear();
    st.intel.cursor_rect = None;
    if st.cached_revision != Some(buf.revision) {
        st.cached_revision = Some(buf.revision);
        st.states.clear();
        st.max_cols = (0..buf.line_count())
            .map(|i| visual_col(buf.line(i), usize::MAX))
            .max()
            .unwrap_or(0);
        st.line_count = buf.line_count();
        st.rebuild(buf);
    }
    st.rebuild_rows(buf.revision);

    let digits = buf.line_count().to_string().len().max(3);
    // Room after the line numbers for the fold arrows.
    let fold_w = cw * 1.8;
    // Without line numbers a narrow margin is left, so text doesn't touch the edge.
    let gutter_w = if prefs.line_numbers {
        (digits as f32 + 1.5) * cw + fold_w
    } else {
        cw * 0.5 + fold_w
    };
    let mut focused = false;
    let mut action = None;

    let mut area = ScrollArea::both().auto_shrink(false);
    if let Some(y) = st.scroll_to.take() {
        area = area.vertical_scroll_offset(y);
    }
    area.show_viewport(ui, |ui, viewport| {
        st.view_top = viewport.min.y;
        st.view_h = viewport.height();
        // Room below the last line, so it can be scrolled up to the middle of the view.
        let below = if prefs.scroll_past_end {
            viewport.height() - row_h * 3.0
        } else {
            row_h
        };
        let size = vec2(
            gutter_w + (st.max_cols + 8) as f32 * cw,
            st.rows.len() as f32 * row_h + below,
        )
        .max(viewport.size());
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let origin = rect.min;
        let text_x = origin.x + gutter_w;
        let view = Rect::from_min_size(origin + viewport.min.to_vec2(), viewport.size());
        let fold_x = view.min.x + gutter_w - fold_w;

        let first_row = (viewport.min.y / row_h).floor().max(0.0) as usize;
        let last_row = ((viewport.max.y / row_h).ceil() as usize).min(st.rows.len());
        let first_row = first_row.min(last_row);

        if st.request_focus {
            st.request_focus = false;
            resp.request_focus();
        }
        let in_gutter = resp
            .hover_pos()
            .is_some_and(|p| p.x >= view.min.x && p.x < text_x);
        if resp.hovered() && !in_gutter {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
        resp.context_menu(|ui| {
            for (label, id, choice) in [
                (
                    "Go to definition",
                    "editor.definition",
                    Action::Definition(buf.cursor),
                ),
                ("Find references", "editor.references", Action::References),
                ("Rename symbol", "editor.rename", Action::Rename),
                ("Quick fix…", "editor.quickFix", Action::CodeActions),
            ] {
                let label = format!("{label}    {}", crate::keys::shown(id));
                if ui.button(label.trim_end()).clicked() {
                    action = Some(choice);
                    ui.close();
                }
            }
            ui.separator();
            for (label, id, choice) in [
                ("Edit with Claude", "editor.inlineEdit", Action::InlineEdit),
                ("Explain with Claude", "", Action::Explain),
                ("Add selection to chat", "", Action::AddToChat),
            ] {
                let label = format!("{label}    {}", crate::keys::shown(id));
                if ui.button(label.trim_end()).clicked() {
                    action = Some(choice);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(buf.selected_text());
                ui.close();
            }
        });

        let before = (buf.cursor, buf.revision, buf.line_count());
        let hints_then: std::collections::HashMap<usize, Vec<(usize, String)>> = st.rows
            [first_row..last_row]
            .iter()
            .map(|&line| (line, line_hints(st, buf, line)))
            .filter(|(_, hints)| !hints.is_empty())
            .collect();
        let text_col = |buf: &Buffer, line: usize, x: f32| match hints_then.get(&line) {
            Some(hints) => without_hints(buf.line(line), hints, (x - text_x) / cw),
            None => (x - text_x) / cw,
        };

        // ---- mouse ----
        let row_at = |st: &EditorState, p: egui::Pos2| {
            (((p.y - origin.y) / row_h).floor().max(0.0) as usize).min(st.rows.len() - 1)
        };
        let hit = |st: &EditorState, buf: &Buffer, p: egui::Pos2| {
            let line = st.rows[row_at(st, p)].min(buf.line_count() - 1);
            let vcol = text_col(buf, line, p.x).round().max(0.0) as usize;
            Pos::new(line, col_at_visual(buf.line(line), vcol))
        };
        if let Some(p) = resp.interact_pointer_pos() {
            let pos = hit(st, buf, p);
            let (pressed, down, shift, command) = ui.input(|i| {
                (
                    i.pointer.primary_pressed(),
                    i.pointer.primary_down(),
                    i.modifiers.shift,
                    i.modifiers.command,
                )
            });
            // A press on a fold arrow folds or unfolds; the cursor stays where it is.
            let on_arrow = p.x >= fold_x && p.x < text_x && st.foldable(pos.line);
            if pressed && on_arrow {
                st.toggle_fold(pos.line);
            } else if pressed && command && !shift {
                // Ctrl+click goes to the definition.
                resp.request_focus();
                buf.set_cursor(pos, false);
                action = Some(Action::Definition(pos));
            } else if pressed {
                resp.request_focus();
                buf.set_cursor(pos, shift);
            } else if down && resp.dragged() && !on_arrow {
                buf.set_cursor(pos, true);
            }
            if !on_arrow {
                if resp.double_clicked() {
                    buf.select_word_at(pos);
                } else if resp.triple_clicked() {
                    buf.select_line(pos.line);
                }
            }
        }

        // The char under the mouse, for hover: where it is and since when it is there.
        let under = resp
            .hover_pos()
            .filter(|p| p.x >= text_x && !ui.input(|i| i.pointer.any_down()))
            .and_then(|p| {
                let row = row_at(st, p);
                let line = st.rows[row].min(buf.line_count() - 1);
                let vcol = text_col(buf, line, p.x).floor().max(0.0) as usize;
                let col = col_under(buf.line(line), vcol)?;
                let shift = hints_then.get(&line).map_or(0, |h| hint_shift(h, col));
                let x = text_x + (visual_col(buf.line(line), col) + shift) as f32 * cw;
                let y = origin.y + row as f32 * row_h;
                Some((
                    Pos::new(line, col),
                    Rect::from_min_size(pos2(x, y), vec2(cw, row_h)),
                ))
            });
        let now = ui.input(|i| i.time);
        if under.map(|u| u.0) != st.intel.mouse.map(|m| m.0) {
            st.intel.mouse_since = now;
        }
        st.intel.mouse = under;

        // ---- keyboard ----
        let mut vertical = None;
        // An open list (completions, quick fixes) takes the arrows, Enter, Tab and Escape.
        let list_open = st.intel.list_open();
        let popup_open = list_open || st.intel.signature.is_some() || st.intel.hover.is_some();
        if resp.has_focus() {
            focused = true;
            let filter = EventFilter {
                tab: true,
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: popup_open,
            };
            ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, filter));
            let page = ((viewport.height() / row_h) as isize - 1).max(1);
            for event in ui.input(|i| i.events.clone()) {
                if let Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = &event
                {
                    let for_list = matches!(
                        key,
                        Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown | Key::Enter
                    ) || *key == Key::Tab && !modifiers.shift;
                    if list_open && for_list && !modifiers.command
                        || popup_open && *key == Key::Escape
                    {
                        st.intel.keys.push(*key);
                        continue;
                    }
                }
                match event {
                    Event::Text(text) => {
                        buf.type_text(&text);
                        let mut chars = text.chars();
                        st.intel.typed = chars.next().filter(|_| chars.next().is_none());
                    }
                    Event::Paste(text) => {
                        buf.insert(&text);
                        st.intel.completion = None;
                    }
                    Event::Copy if buf.has_selection() => ui.ctx().copy_text(buf.selected_text()),
                    Event::Cut if buf.has_selection() => {
                        ui.ctx().copy_text(buf.selected_text());
                        buf.insert("");
                    }
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        let (sel, cmd) = (modifiers.shift, modifiers.command);
                        // The shortcuts, as set in Settings → Keyboard shortcuts.
                        if let Some(id) = crate::keys::editor_command(key, modifiers) {
                            match id {
                                "editor.inlineEdit" => action = Some(Action::InlineEdit),
                                "editor.definition" => {
                                    action = Some(Action::Definition(buf.cursor))
                                }
                                "editor.references" => action = Some(Action::References),
                                "editor.rename" => action = Some(Action::Rename),
                                "editor.quickFix" => action = Some(Action::CodeActions),
                                "editor.complete" => action = Some(Action::Complete),
                                "editor.fold" => st.fold_at(buf.cursor.line),
                                "editor.unfold" => st.unfold_at(buf.cursor.line),
                                "editor.comment" => {
                                    if let Some(prefix) = lang.line_comment.first() {
                                        buf.toggle_comment(prefix);
                                    }
                                }
                                "editor.duplicateLine" => buf.duplicate_lines(),
                                "editor.moveLineUp" => buf.move_selected_lines(-1),
                                "editor.moveLineDown" => buf.move_selected_lines(1),
                                "editor.deleteLine" => buf.delete_lines(),
                                _ => {}
                            }
                            continue;
                        }
                        match key {
                            Key::ArrowLeft if cmd => buf.move_word_left(sel),
                            Key::ArrowRight if cmd => buf.move_word_right(sel),
                            Key::ArrowLeft => buf.move_left(sel),
                            Key::ArrowRight => buf.move_right(sel),
                            Key::ArrowUp => {
                                buf.move_lines(-1, sel);
                                vertical = Some((-1, sel));
                            }
                            Key::ArrowDown => {
                                buf.move_lines(1, sel);
                                vertical = Some((1, sel));
                            }
                            Key::PageUp => {
                                buf.move_lines(-page, sel);
                                vertical = Some((-1, sel));
                            }
                            Key::PageDown => {
                                buf.move_lines(page, sel);
                                vertical = Some((1, sel));
                            }
                            Key::Home if cmd => buf.move_doc_start(sel),
                            Key::End if cmd => buf.move_doc_end(sel),
                            Key::Home => buf.move_home(sel),
                            Key::End => buf.move_end(sel),
                            Key::Enter => buf.newline(),
                            Key::Backspace => buf.backspace(cmd),
                            Key::Delete => buf.delete(cmd),
                            Key::Tab if sel => buf.outdent(),
                            Key::Tab => buf.tab(),
                            Key::A if cmd => buf.select_all(),
                            Key::Z if cmd && sel => buf.redo(),
                            Key::Z if cmd => buf.undo(),
                            Key::Y if cmd => buf.redo(),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        // Lines added or removed move the folds below them along.
        let added = buf.line_count() as isize - before.2 as isize;
        if added != 0 {
            st.shift_folds(before.0.line.min(buf.cursor.line), added);
        }
        // The cursor never rests inside a folded block: moving up or down steps over it,
        // anything else that lands there (a search, an undo) opens it.
        if let Some((start, end)) = st.hidden_by(buf.cursor.line) {
            match vertical {
                Some((dir, sel)) => {
                    let line = if dir > 0 {
                        (end + 1).min(buf.line_count() - 1)
                    } else {
                        start
                    };
                    let col = buf.cursor.col.min(buf.line(line).chars().count());
                    buf.set_cursor(Pos::new(line, col), sel);
                }
                None => st.unfold_at(buf.cursor.line),
            }
            st.rebuild_rows(buf.revision);
        }
        if (before.0, before.1) != (buf.cursor, buf.revision) {
            st.reveal_cursor = true;
        }
        st.rebuild_rows(buf.revision);

        // An edit above may have changed the text; the cache is rebuilt next frame,
        // so only trust the lexer states when they still match.
        if st.cached_revision != Some(buf.revision) {
            st.states.clear();
        }
        let first_row = first_row.min(st.rows.len());
        let last_row = last_row.min(st.rows.len());
        let visible: Vec<usize> = st.rows[first_row..last_row].to_vec();
        if let Some(&last_line) = visible.last() {
            fill_states(st, buf, lang, last_line.min(buf.line_count()));
        }

        // ---- paint ----
        let painter = ui.painter();
        let hints: std::collections::HashMap<usize, Vec<(usize, String)>> = visible
            .iter()
            .map(|&line| (line, line_hints(st, buf, line)))
            .filter(|(_, hints)| !hints.is_empty())
            .collect();
        let line_y = |st: &EditorState, line: usize| origin.y + st.row_of(line) as f32 * row_h;
        let col_x = |buf: &Buffer, p: Pos| {
            let shift = hints.get(&p.line).map_or(0, |h| hint_shift(h, p.col));
            text_x + (visual_col(buf.line(p.line), p.col) + shift) as f32 * cw
        };

        if !buf.has_selection() && prefs.highlight_line {
            let y = line_y(st, buf.cursor.line);
            painter.rect_filled(
                Rect::from_min_max(pos2(view.min.x, y), pos2(view.max.x, y + row_h)),
                0.0,
                theme::CURRENT_LINE(),
            );
        }

        for column in crate::settings::rulers() {
            let x = (text_x + column as f32 * cw).round() + 0.5;
            painter.line_segment(
                [pos2(x, view.min.y), pos2(x, view.max.y)],
                (1.0, theme::tint(12)),
            );
        }
        if prefs.indent_guides {
            let step = prefs.tab_width.max(1);
            // The block the cursor is in: its guide is a shade brighter.
            let cursor_line = buf.cursor.line;
            let active = (cursor_line.saturating_sub(2000)..cursor_line)
                .rev()
                .find(|&s| st.fold_end(s).is_some_and(|end| end >= cursor_line))
                .map(|s| (s, st.fold_end(s).unwrap_or(s), indent_of(buf, s)));
            for &line in &visible {
                if buf.line(line).len() > LONG_LINE {
                    continue;
                }
                let indent = indent_of(buf, line);
                let y = line_y(st, line);
                let mut column = 0;
                while column < indent {
                    let lit =
                        active.is_some_and(|(s, end, at)| at == column && line > s && line <= end);
                    let x = (text_x + column as f32 * cw).round() + 0.5;
                    let shade = if lit { 44 } else { 16 };
                    painter
                        .line_segment([pos2(x, y), pos2(x, y + row_h)], (1.0, theme::tint(shade)));
                    column += step;
                }
            }
        }

        let (sel_start, sel_end) = buf.selection();
        for &line in &visible {
            let y = line_y(st, line);
            if buf.has_selection() && line >= sel_start.line && line <= sel_end.line {
                let x0 = if line == sel_start.line {
                    col_x(buf, sel_start)
                } else {
                    text_x
                };
                let x1 = if line == sel_end.line {
                    col_x(buf, sel_end)
                } else {
                    // Half a cell past the text stands for the selected line break.
                    col_x(buf, Pos::new(line, usize::MAX)) + cw * 0.5
                };
                painter.rect_filled(
                    Rect::from_min_max(pos2(x0, y), pos2(x1, y + row_h)),
                    0.0,
                    theme::SELECTION(),
                );
            }

            let text = buf.line(line);
            let pos = pos2(text_x, y + pad_y);
            if text.len() > LONG_LINE {
                let skip = ((view.min.x - text_x) / cw).floor().max(0.0) as usize;
                let take = (view.width() / cw).ceil() as usize + 2;
                let shown: String = text
                    .chars()
                    .skip(skip)
                    .take(take)
                    .map(|c| if c == '\t' { ' ' } else { c })
                    .collect();
                let galley = painter.layout_no_wrap(shown, font.clone(), theme::TEXT());
                painter.galley(pos + vec2(skip as f32 * cw, 0.0), galley, theme::TEXT());
            } else if !text.is_empty() || hints.contains_key(&line) {
                let sem = line_sem(st, buf, line);
                let no_hints = Vec::new();
                let line_hints = hints.get(&line).unwrap_or(&no_hints);
                let job = layout_line(text, lang, st.states[line], &font, &sem, line_hints);
                let galley = painter.layout_job(job);
                painter.galley(pos, galley, theme::TEXT());
            }
            // Spaces and tabs, faintly: everywhere, or in the selection.
            let marks = match prefs.whitespace {
                crate::settings::Whitespace::All => true,
                crate::settings::Whitespace::Selection => {
                    buf.has_selection() && line >= sel_start.line && line <= sel_end.line
                }
                crate::settings::Whitespace::None => false,
            };
            if marks && text.len() <= LONG_LINE {
                let mark = theme::COMMENT().gamma_multiply(0.55);
                let mut vcol = 0;
                for (col, c) in text.chars().enumerate() {
                    let w = char_width(c, vcol);
                    vcol += w;
                    let p = Pos::new(line, col);
                    let in_selection = prefs.whitespace == crate::settings::Whitespace::All
                        || p >= sel_start && p < sel_end;
                    if !in_selection || !(c == ' ' || c == '\t') {
                        continue;
                    }
                    let x = col_x(buf, p);
                    let mid = y + row_h / 2.0;
                    if c == ' ' {
                        painter.circle_filled(pos2(x + cw / 2.0, mid), 1.0, mark);
                    } else {
                        let end = x + w as f32 * cw - 3.0;
                        painter.line_segment([pos2(x + 2.0, mid), pos2(end, mid)], (1.0, mark));
                        painter.line_segment(
                            [pos2(end - 3.0, mid - 3.0), pos2(end, mid)],
                            (1.0, mark),
                        );
                        painter.line_segment(
                            [pos2(end - 3.0, mid + 3.0), pos2(end, mid)],
                            (1.0, mark),
                        );
                    }
                }
            }
            // A folded block: a small mark after its first line stands for what is hidden.
            if st.folded.contains(&line) {
                let x = col_x(buf, Pos::new(line, usize::MAX)) + cw;
                let pill = Rect::from_min_size(pos2(x, y + 3.0), vec2(cw * 3.0, row_h - 6.0));
                painter.rect_filled(pill, 4.0, theme::tint(18));
                painter.text(
                    pill.center(),
                    egui::Align2::CENTER_CENTER,
                    "⋯",
                    font.clone(),
                    theme::COMMENT(),
                );
            }
        }

        // Others editing live: their selection faintly, their cursor, and their name on it.
        for mark in &st.peers {
            let Some((at, anchor)) = mark.at else {
                continue;
            };
            let (at, anchor) = (buf.clamp(at), buf.clamp(anchor));
            let (from, to) = if anchor < at {
                (anchor, at)
            } else {
                (at, anchor)
            };
            for &line in visible.iter().filter(|&&l| l >= from.line && l <= to.line) {
                if from == to {
                    break;
                }
                let x0 = if line == from.line {
                    col_x(buf, from)
                } else {
                    text_x
                };
                let x1 = if line == to.line {
                    col_x(buf, to)
                } else {
                    col_x(buf, Pos::new(line, usize::MAX)) + cw * 0.5
                };
                let y = line_y(st, line);
                painter.rect_filled(
                    Rect::from_min_max(pos2(x0, y), pos2(x1, y + row_h)),
                    0.0,
                    mark.color.gamma_multiply(0.22),
                );
            }
            if !visible.contains(&at.line) {
                continue;
            }
            let (x, y) = (col_x(buf, at), line_y(st, at.line));
            painter.rect_filled(
                Rect::from_min_size(pos2(x - 1.0, y), vec2(2.0, row_h)),
                0.0,
                mark.color,
            );
            let label = painter.layout_no_wrap(
                mark.name.clone(),
                FontId::proportional(11.0),
                Color32::from_rgb(0x16, 0x16, 0x18),
            );
            // Above the line, or below it where there is no room above.
            let above = y - label.size().y - 2.0;
            let top = if above < view.min.y { y + row_h } else { above };
            let tag = Rect::from_min_size(pos2(x - 1.0, top), label.size() + vec2(8.0, 2.0));
            painter.rect_filled(tag, 3.0, mark.color);
            painter.galley(tag.min + vec2(4.0, 1.0), label, Color32::BLACK);
        }

        if prefs.bracket_match {
            let key = (buf.revision, buf.cursor);
            if st.brackets.as_ref().map(|b| (b.0, b.1)) != Some(key) {
                st.brackets = Some((key.0, key.1, matching_brackets(buf, buf.cursor)));
            }
            if let Some((_, _, Some((a, b)))) = st.brackets {
                for p in [a, b].into_iter().filter(|p| visible.contains(&p.line)) {
                    let x = col_x(buf, p);
                    let rect = Rect::from_min_size(pos2(x, line_y(st, p.line)), vec2(cw, row_h));
                    painter.rect_filled(rect, 2.0, theme::tint(12));
                    painter.rect_stroke(
                        rect,
                        2.0,
                        (1.0, theme::tint(70)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }

        let cursor_x = col_x(buf, buf.cursor);
        let cursor_rect = Rect::from_min_size(
            pos2(cursor_x - 1.0, line_y(st, buf.cursor.line)),
            vec2(2.0, row_h),
        );
        // Where popups at the cursor go; none while the cursor is scrolled out of view.
        // (RIDE_INTEL_COMPLETE shows them in a test window, which never has the keyboard.)
        let looking = std::env::var_os("RIDE_INTEL_COMPLETE").is_some();
        if (focused || looking) && view.intersects(cursor_rect) {
            st.intel.cursor_rect = Some(cursor_rect);
        }
        if focused {
            let blink_on =
                !prefs.cursor_blink || ui.input(|i| i.time) % 1.0 < 0.5 || st.reveal_cursor;
            if blink_on {
                let y = cursor_rect.min.y;
                match prefs.cursor_style {
                    crate::settings::CursorStyle::Line => {
                        painter.rect_filled(cursor_rect, 0.0, theme::CURSOR());
                    }
                    crate::settings::CursorStyle::Underline => {
                        painter.rect_filled(
                            Rect::from_min_size(pos2(cursor_x, y + row_h - 2.0), vec2(cw, 2.0)),
                            0.0,
                            theme::CURSOR(),
                        );
                    }
                    crate::settings::CursorStyle::Block => {
                        let block = Rect::from_min_size(pos2(cursor_x, y), vec2(cw, row_h));
                        painter.rect_filled(block, 0.0, theme::CURSOR());
                        // The char under it, in the background's colour.
                        let under = buf.line(buf.cursor.line).chars().nth(buf.cursor.col);
                        if let Some(c) = under.filter(|c| !c.is_whitespace()) {
                            painter.text(
                                pos2(cursor_x, y + pad_y),
                                egui::Align2::LEFT_TOP,
                                c,
                                font.clone(),
                                theme::EDITOR_BG(),
                            );
                        }
                    }
                }
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }

        let error = theme::ANSI()[1];
        for (line, message) in st.diags.iter().filter(|(l, _)| visible.contains(l)) {
            let indent = buf
                .line(*line)
                .chars()
                .take_while(|c| c.is_whitespace())
                .count();
            let x0 = col_x(buf, Pos::new(*line, indent));
            let x1 = col_x(buf, Pos::new(*line, usize::MAX)).max(x0 + cw);
            let y = line_y(st, *line) + row_h - 1.5;
            painter.line_segment([pos2(x0, y), pos2(x1, y)], (1.5, error));
            painter.text(
                pos2(x1 + cw * 3.0, line_y(st, *line) + pad_y),
                egui::Align2::LEFT_TOP,
                message,
                font.clone(),
                error.gamma_multiply(0.85),
            );
        }

        // Problems the language server found: a wavy line under each, and the worst one's
        // message at the end of its line.
        let mut worst: Vec<(usize, u8, &str)> = Vec::new();
        for diag in st.intel.diags.iter().filter(|d| d.severity <= 3) {
            let (first, last) = (diag.start.line as usize, diag.end.line as usize);
            if first >= buf.line_count() {
                continue;
            }
            let color = crate::intel::severity_color(diag.severity);
            let last_line = last.min(buf.line_count() - 1);
            for &line in visible.iter().filter(|&&l| l >= first && l <= last_line) {
                let text = buf.line(line);
                let from = if line == first {
                    crate::lsp::col_of(text, diag.start.ch)
                } else {
                    0
                };
                let to = if line == last {
                    crate::lsp::col_of(text, diag.end.ch)
                } else {
                    usize::MAX
                };
                let x0 = col_x(buf, Pos::new(line, from));
                let x1 = col_x(buf, Pos::new(line, to)).max(x0 + cw);
                squiggle(painter, x0, x1, line_y(st, line) + row_h - 1.5, color);
            }
            if diag.severity <= 2 {
                match worst.iter_mut().find(|(l, _, _)| *l == first) {
                    Some(entry) if entry.1 > diag.severity => {
                        *entry = (first, diag.severity, &diag.message)
                    }
                    Some(_) => {}
                    None => worst.push((first, diag.severity, &diag.message)),
                }
            }
        }
        for (line, severity, message) in &worst {
            if !visible.contains(line) || st.diags.iter().any(|(l, _)| l == line) {
                continue;
            }
            let message = message.lines().next().unwrap_or("");
            let x = col_x(buf, Pos::new(*line, usize::MAX)) + cw * 3.0;
            painter.text(
                pos2(x, line_y(st, *line) + pad_y),
                egui::Align2::LEFT_TOP,
                message,
                font.clone(),
                crate::intel::severity_color(*severity).gamma_multiply(0.8),
            );
        }

        // Who changed the cursor's line last, faintly at its end, while the text is still
        // as it was when that was looked up.
        let line = buf.cursor.line;
        let blame = st
            .blame
            .get(line)
            .filter(|_| prefs.blame && st.blame_revision == Some(buf.revision))
            .filter(|_| !st.diags.iter().any(|(l, _)| *l == line) && visible.contains(&line))
            .filter(|_| !worst.iter().any(|(l, _, _)| *l == line));
        if let Some(text) = blame {
            let x = col_x(buf, Pos::new(line, usize::MAX)) + cw * 4.0;
            painter.text(
                pos2(x, line_y(st, line) + pad_y),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                theme::COMMENT().gamma_multiply(0.7),
            );
        }

        // The gutter is painted last so it stays put over horizontally scrolled text.
        let gutter = Rect::from_min_size(view.min, vec2(gutter_w - cw * 0.5, view.height()));
        // Only needed to hide text that scrolled under the line numbers.
        if viewport.min.x > 0.0 {
            painter.rect_filled(gutter, 0.0, theme::EDITOR_BG());
        }
        for (start, count, kind) in &st.marks {
            let color = [theme::ANSI()[2], theme::ANSI()[3], error][*kind as usize % 3];
            let x = view.min.x + 2.0;
            let mark = if *kind == 2 {
                // Nothing is left of removed lines, so the place they were at is marked.
                Rect::from_min_size(pos2(x, line_y(st, start + 1) - 1.5), vec2(cw * 0.8, 3.0))
            } else {
                Rect::from_min_max(
                    pos2(x, line_y(st, *start)),
                    pos2(
                        x + 2.5,
                        line_y(st, start + count).max(line_y(st, *start) + row_h),
                    ),
                )
            };
            if mark.intersects(view) {
                painter.rect_filled(mark, 1.0, color);
            }
        }
        let numbers_right = fold_x - cw * 0.3;
        for &line in visible.iter().filter(|_| prefs.line_numbers) {
            let lsp_worst = worst.iter().find(|(l, _, _)| *l == line).map(|w| w.1);
            let color = if st.diags.iter().any(|(l, _)| *l == line) {
                error
            } else if let Some(severity) = lsp_worst {
                crate::intel::severity_color(severity)
            } else if line == buf.cursor.line {
                theme::TEXT()
            } else {
                theme::LINE_NUMBER()
            };
            painter.text(
                pos2(numbers_right, line_y(st, line) + pad_y),
                egui::Align2::RIGHT_TOP,
                line + 1,
                font.clone(),
                color,
            );
        }
        // Fold arrows: always on a folded block, on the others while the mouse is near.
        let near = resp.hover_pos().is_some_and(|p| p.x < text_x + cw * 4.0);
        for &line in &visible {
            let folded = st.folded.contains(&line);
            if !(folded || near && st.foldable(line)) {
                continue;
            }
            let glyph = if folded {
                crate::icons::CHEVRON_RIGHT
            } else {
                crate::icons::CHEVRON_DOWN
            };
            painter.text(
                pos2(fold_x + fold_w * 0.45, line_y(st, line) + row_h / 2.0),
                egui::Align2::CENTER_CENTER,
                glyph,
                FontId::proportional(prefs.code_size * 0.85),
                if folded {
                    theme::TEXT()
                } else {
                    theme::COMMENT()
                },
            );
        }

        // Sticky scroll: the first lines of the blocks the top of the view is in, pinned.
        let sticky = if prefs.sticky_scroll && first_row > 0 {
            sticky_lines(st, first_row, prefs.sticky_lines)
        } else {
            Vec::new()
        };
        if !sticky.is_empty() {
            let area =
                Rect::from_min_size(view.min, vec2(view.width(), sticky.len() as f32 * row_h));
            painter.rect_filled(area, 0.0, theme::EDITOR_BG());
            let text_clip =
                Rect::from_min_max(pos2(view.min.x + gutter_w - cw * 0.5, area.min.y), area.max)
                    .intersect(painter.clip_rect());
            for (index, &line) in sticky.iter().enumerate() {
                let y = area.min.y + index as f32 * row_h;
                let text = buf.line(line);
                if text.len() <= LONG_LINE && line < st.states.len() {
                    let sem = line_sem(st, buf, line);
                    let job = layout_line(text, lang, st.states[line], &font, &sem, &[]);
                    let galley = painter.layout_job(job);
                    painter.with_clip_rect(text_clip).galley(
                        pos2(text_x, y + pad_y),
                        galley,
                        theme::TEXT(),
                    );
                }
                if prefs.line_numbers {
                    painter.text(
                        pos2(numbers_right, y + pad_y),
                        egui::Align2::RIGHT_TOP,
                        line + 1,
                        font.clone(),
                        theme::LINE_NUMBER(),
                    );
                }
            }
            painter.line_segment(
                [area.left_bottom(), area.right_bottom()],
                (1.0, theme::tint(28)),
            );
            let pinned = ui.interact(area, ui.id().with("sticky"), Sense::click());
            if pinned.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if let Some(p) = pinned.interact_pointer_pos().filter(|_| pinned.clicked()) {
                let index = ((p.y - area.min.y) / row_h).floor().max(0.0) as usize;
                if let Some(&line) = sticky.get(index) {
                    let indent = buf
                        .line(line)
                        .chars()
                        .take_while(|c| c.is_whitespace())
                        .count();
                    buf.set_cursor(Pos::new(line, indent), false);
                    st.scroll_to = Some(st.row_of(line) as f32 * row_h);
                    resp.request_focus();
                }
            }
        }

        if st.reveal_cursor {
            st.reveal_cursor = false;
            // Kept clear of the pinned lines above.
            let pinned = (sticky.len() as f32 + 1.0) * row_h;
            let target = Rect::from_min_max(
                cursor_rect.min - vec2(gutter_w + cw * 2.0, pinned),
                cursor_rect.max + vec2(cw * 4.0, row_h),
            );
            ui.scroll_to_rect(target, None);
        }
    });
    action
}
