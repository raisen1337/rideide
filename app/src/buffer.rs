//! The text buffer behind an editor tab: lines, cursor, selection and undo history.

/// A position in the buffer. `col` counts chars, not bytes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

struct Edit {
    start: Pos,
    removed: String,
    inserted: String,
    cursor_before: Pos,
    anchor_before: Pos,
}

pub struct Buffer {
    lines: Vec<String>,
    pub cursor: Pos,
    /// The other end of the selection; equal to `cursor` when nothing is selected.
    pub anchor: Pos,
    /// Column the cursor wants to return to while moving vertically across short lines.
    goal_col: Option<usize>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Bumped on every change to the text.
    pub revision: u64,
    saved_revision: u64,
    crlf: bool,
    use_tabs: bool,
    /// Whether the next typed char may be folded into the previous undo step.
    typing_run: bool,
}

/// Columns per indent step, from the settings.
fn tab_width() -> usize {
    crate::settings::with(|s| s.tab_width)
}

fn byte_idx(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

fn char_len(line: &str) -> usize {
    line.chars().count()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let crlf = text.contains("\r\n");
        let lines: Vec<String> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        let use_tabs = lines.iter().take(2000).any(|l| l.starts_with('\t'));
        Self {
            lines,
            cursor: Pos::default(),
            anchor: Pos::default(),
            goal_col: None,
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
            saved_revision: 0,
            crlf,
            use_tabs,
            typing_run: false,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join(if self.crlf { "\r\n" } else { "\n" })
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, i: usize) -> &str {
        &self.lines[i]
    }

    pub fn line_len(&self, i: usize) -> usize {
        char_len(&self.lines[i])
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Makes the buffer count as edited: its text isn't what is on disk.
    pub fn mark_dirty(&mut self) {
        self.revision += 1;
    }

    pub fn mark_saved(&mut self, revision: u64) {
        self.saved_revision = revision;
    }

    pub fn clamp(&self, pos: Pos) -> Pos {
        let line = pos.line.min(self.lines.len() - 1);
        Pos::new(line, pos.col.min(self.line_len(line)))
    }

    fn end(&self) -> Pos {
        let line = self.lines.len() - 1;
        Pos::new(line, self.line_len(line))
    }

    // ---- selection ----

    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    pub fn selection(&self) -> (Pos, Pos) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    pub fn selected_text(&self) -> String {
        let (start, end) = self.selection();
        self.text_range(start, end)
    }

    pub fn select_all(&mut self) {
        self.anchor = Pos::default();
        self.cursor = self.end();
        self.moved();
    }

    pub fn select_word_at(&mut self, pos: Pos) {
        let pos = self.clamp(pos);
        let chars: Vec<char> = self.lines[pos.line].chars().collect();
        if chars.is_empty() {
            self.set_cursor(pos, false);
            return;
        }
        let at = pos.col.min(chars.len() - 1);
        let kind = is_word(chars[at]);
        let same = |c: &char| is_word(*c) == kind && (kind || !c.is_whitespace());
        let start = at - chars[..at].iter().rev().take_while(|c| same(c)).count();
        let end = at + chars[at..].iter().take_while(|c| same(c)).count().max(1);
        self.anchor = Pos::new(pos.line, start);
        self.cursor = Pos::new(pos.line, end);
        self.moved();
    }

    pub fn select_line(&mut self, line: usize) {
        let line = line.min(self.lines.len() - 1);
        self.anchor = Pos::new(line, 0);
        self.cursor = if line + 1 < self.lines.len() {
            Pos::new(line + 1, 0)
        } else {
            self.end()
        };
        self.moved();
    }

    pub fn text_range(&self, start: Pos, end: Pos) -> String {
        if start.line == end.line {
            let line = &self.lines[start.line];
            return line[byte_idx(line, start.col)..byte_idx(line, end.col)].to_string();
        }
        let first = &self.lines[start.line];
        let mut out = first[byte_idx(first, start.col)..].to_string();
        for line in &self.lines[start.line + 1..end.line] {
            out.push('\n');
            out.push_str(line);
        }
        let last = &self.lines[end.line];
        out.push('\n');
        out.push_str(&last[..byte_idx(last, end.col)]);
        out
    }

    // ---- movement ----

    fn moved(&mut self) {
        self.goal_col = None;
        self.typing_run = false;
    }

    pub fn set_cursor(&mut self, pos: Pos, select: bool) {
        self.cursor = self.clamp(pos);
        if !select {
            self.anchor = self.cursor;
        }
        self.moved();
    }

    pub fn move_left(&mut self, select: bool) {
        if !select && self.has_selection() {
            return self.set_cursor(self.selection().0, false);
        }
        let c = self.cursor;
        let pos = if c.col > 0 {
            Pos::new(c.line, c.col - 1)
        } else if c.line > 0 {
            Pos::new(c.line - 1, self.line_len(c.line - 1))
        } else {
            c
        };
        self.set_cursor(pos, select);
    }

    pub fn move_right(&mut self, select: bool) {
        if !select && self.has_selection() {
            return self.set_cursor(self.selection().1, false);
        }
        let c = self.cursor;
        let pos = if c.col < self.line_len(c.line) {
            Pos::new(c.line, c.col + 1)
        } else if c.line + 1 < self.lines.len() {
            Pos::new(c.line + 1, 0)
        } else {
            c
        };
        self.set_cursor(pos, select);
    }

    /// Moves the cursor `delta` lines, keeping its column where the lines are long enough.
    pub fn move_lines(&mut self, delta: isize, select: bool) {
        let goal = self.goal_col.unwrap_or(self.cursor.col);
        let line = self
            .cursor
            .line
            .saturating_add_signed(delta)
            .min(self.lines.len() - 1);
        self.set_cursor(Pos::new(line, goal), select);
        self.goal_col = Some(goal);
    }

    /// Jumps between the first non-blank char and column 0, like most editors do.
    pub fn move_home(&mut self, select: bool) {
        let line = &self.lines[self.cursor.line];
        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        let col = if self.cursor.col == indent { 0 } else { indent };
        self.set_cursor(Pos::new(self.cursor.line, col), select);
    }

    pub fn move_end(&mut self, select: bool) {
        let line = self.cursor.line;
        self.set_cursor(Pos::new(line, self.line_len(line)), select);
    }

    pub fn move_doc_start(&mut self, select: bool) {
        self.set_cursor(Pos::default(), select);
    }

    pub fn move_doc_end(&mut self, select: bool) {
        self.set_cursor(self.end(), select);
    }

    fn word_left_of(&self, pos: Pos) -> Pos {
        if pos.col == 0 {
            return if pos.line > 0 {
                Pos::new(pos.line - 1, self.line_len(pos.line - 1))
            } else {
                pos
            };
        }
        let chars: Vec<char> = self.lines[pos.line].chars().collect();
        let mut col = pos.col;
        while col > 0 && chars[col - 1].is_whitespace() {
            col -= 1;
        }
        if col > 0 {
            let kind = is_word(chars[col - 1]);
            while col > 0 && is_word(chars[col - 1]) == kind && !chars[col - 1].is_whitespace() {
                col -= 1;
            }
        }
        Pos::new(pos.line, col)
    }

    fn word_right_of(&self, pos: Pos) -> Pos {
        let chars: Vec<char> = self.lines[pos.line].chars().collect();
        if pos.col >= chars.len() {
            return if pos.line + 1 < self.lines.len() {
                Pos::new(pos.line + 1, 0)
            } else {
                pos
            };
        }
        let mut col = pos.col;
        if !chars[col].is_whitespace() {
            let kind = is_word(chars[col]);
            while col < chars.len() && is_word(chars[col]) == kind && !chars[col].is_whitespace() {
                col += 1;
            }
        }
        while col < chars.len() && chars[col].is_whitespace() {
            col += 1;
        }
        Pos::new(pos.line, col)
    }

    pub fn move_word_left(&mut self, select: bool) {
        self.set_cursor(self.word_left_of(self.cursor), select);
    }

    pub fn move_word_right(&mut self, select: bool) {
        self.set_cursor(self.word_right_of(self.cursor), select);
    }

    // ---- editing ----

    /// Replaces `start..end` with `text` and returns the removed text and the end of the insertion.
    fn splice(&mut self, start: Pos, end: Pos, text: &str) -> (String, Pos) {
        let removed = self.text_range(start, end);
        let tail = {
            let last = &self.lines[end.line];
            last[byte_idx(last, end.col)..].to_string()
        };
        let head_len = byte_idx(&self.lines[start.line], start.col);
        self.lines[start.line].truncate(head_len);
        self.lines.drain(start.line + 1..=end.line);

        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or("");
        self.lines[start.line].push_str(first);
        let rest: Vec<String> = parts.map(str::to_string).collect();
        let new_end = match rest.last() {
            Some(last) => Pos::new(start.line + rest.len(), char_len(last)),
            None => Pos::new(start.line, start.col + char_len(first)),
        };
        self.lines.splice(start.line + 1..start.line + 1, rest);
        self.lines[new_end.line].push_str(&tail);
        self.revision += 1;
        (removed, new_end)
    }

    /// Applies an edit, records it for undo and leaves the cursor after the inserted text.
    fn edit(&mut self, start: Pos, end: Pos, text: &str) {
        let (cursor_before, anchor_before) = (self.cursor, self.anchor);
        let (removed, new_end) = self.splice(start, end, text);

        let single_char = removed.is_empty() && char_len(text) == 1 && text != "\n";
        let continues_typing = self.typing_run
            && single_char
            && self.undo.last().is_some_and(|last| {
                last.removed.is_empty() && self.end_of(last.start, &last.inserted) == start
            });
        if continues_typing {
            self.undo.last_mut().unwrap().inserted.push_str(text);
        } else {
            let inserted = text.to_string();
            self.undo.push(Edit {
                start,
                removed,
                inserted,
                cursor_before,
                anchor_before,
            });
        }
        self.redo.clear();
        self.cursor = new_end;
        self.anchor = new_end;
        self.goal_col = None;
        // A space ends the run, so undo goes back a word at a time.
        self.typing_run = single_char && !text.starts_with(char::is_whitespace);
    }

    /// Where an insertion of `text` at `start` ended, given the pre-splice state is gone.
    fn end_of(&self, start: Pos, text: &str) -> Pos {
        match text.rsplit_once('\n') {
            Some((head, last)) => {
                Pos::new(start.line + head.matches('\n').count() + 1, char_len(last))
            }
            None => Pos::new(start.line, start.col + char_len(text)),
        }
    }

    pub fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (start, end) = self.selection();
        self.edit(start, end, &text);
    }

    pub fn newline(&mut self) {
        if !crate::settings::with(|s| s.auto_indent) {
            return self.insert("\n");
        }
        let (start, _) = self.selection();
        let line = &self.lines[start.line];
        let indent: String = line
            .chars()
            .take(start.col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.insert(&format!("\n{indent}"));
    }

    pub fn backspace(&mut self, word: bool) {
        if !self.has_selection() {
            let start = if word {
                self.word_left_of(self.cursor)
            } else {
                self.left_of(self.cursor)
            };
            self.anchor = start;
        }
        let (start, end) = self.selection();
        if start != end {
            self.edit(start, end, "");
        }
    }

    pub fn delete(&mut self, word: bool) {
        if !self.has_selection() {
            let c = self.cursor;
            self.anchor = if word {
                self.word_right_of(c)
            } else if c.col < self.line_len(c.line) {
                Pos::new(c.line, c.col + 1)
            } else if c.line + 1 < self.lines.len() {
                Pos::new(c.line + 1, 0)
            } else {
                c
            };
        }
        let (start, end) = self.selection();
        if start != end {
            self.edit(start, end, "");
        }
    }

    fn left_of(&self, pos: Pos) -> Pos {
        if pos.col > 0 {
            // Backspacing inside leading spaces removes a whole indent step.
            let line = &self.lines[pos.line];
            let in_indent = line.chars().take(pos.col).all(|c| c == ' ');
            let width = tab_width();
            let step = if in_indent && pos.col % width == 0 {
                width
            } else {
                1
            };
            Pos::new(pos.line, pos.col - step)
        } else if pos.line > 0 {
            Pos::new(pos.line - 1, self.line_len(pos.line - 1))
        } else {
            pos
        }
    }

    /// Whether indentation is a tab: what the settings say, or else what the file uses.
    fn tabs(&self) -> bool {
        match crate::settings::with(|s| s.indent) {
            crate::settings::Indent::Auto => self.use_tabs,
            crate::settings::Indent::Spaces => false,
            crate::settings::Indent::Tabs => true,
        }
    }

    fn indent_unit(&self) -> String {
        if self.tabs() {
            "\t".to_string()
        } else {
            " ".repeat(tab_width())
        }
    }

    /// Lines touched by the selection; a selection ending at column 0 doesn't count that line.
    fn selected_lines(&self) -> (usize, usize) {
        let (start, end) = self.selection();
        let last = if end.col == 0 && end.line > start.line {
            end.line - 1
        } else {
            end.line
        };
        (start.line, last)
    }

    pub fn tab(&mut self) {
        let (start, end) = self.selection();
        if start.line == end.line {
            if self.tabs() {
                return self.insert("\t");
            }
            let spaces = tab_width() - start.col % tab_width();
            return self.insert(&" ".repeat(spaces));
        }
        let unit = self.indent_unit();
        self.rewrite_lines(|line| {
            if line.is_empty() {
                line.to_string()
            } else {
                format!("{unit}{line}")
            }
        });
    }

    pub fn outdent(&mut self) {
        self.rewrite_lines(|line| {
            if let Some(rest) = line.strip_prefix('\t') {
                return rest.to_string();
            }
            let spaces = line
                .chars()
                .take(tab_width())
                .take_while(|c| *c == ' ')
                .count();
            line[spaces..].to_string()
        });
    }

    pub fn toggle_comment(&mut self, prefix: &str) {
        let (first, last) = self.selected_lines();
        let all_commented = self.lines[first..=last]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.trim_start().starts_with(prefix));
        self.rewrite_lines(|line| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            let indent = line.len() - line.trim_start().len();
            let (head, body) = line.split_at(indent);
            if all_commented {
                let body = body.strip_prefix(prefix).unwrap_or(body);
                format!("{head}{}", body.strip_prefix(' ').unwrap_or(body))
            } else {
                format!("{head}{prefix} {body}")
            }
        });
    }

    /// Rewrites every selected line as one undo step and keeps those lines selected.
    fn rewrite_lines(&mut self, f: impl Fn(&str) -> String) {
        let (first, last) = self.selected_lines();
        let had_selection = self.has_selection();
        let cursor = self.cursor;
        let old_len = self.line_len(cursor.line);

        let new_text: Vec<String> = self.lines[first..=last].iter().map(|l| f(l)).collect();
        let start = Pos::new(first, 0);
        let end = Pos::new(last, self.line_len(last));
        self.edit(start, end, &new_text.join("\n"));
        self.typing_run = false;

        if had_selection {
            self.anchor = start;
            self.cursor = Pos::new(last, self.line_len(last));
        } else {
            let shift = self.line_len(cursor.line) as isize - old_len as isize;
            self.cursor = Pos::new(cursor.line, cursor.col.saturating_add_signed(shift));
            self.anchor = self.cursor;
        }
    }

    // ---- live editing ----

    /// The text with `\n` line ends, whatever the file uses: what is shared.
    pub fn plain_text(&self) -> String {
        self.lines.join("\n")
    }

    /// A place as a count of characters from the start of `plain_text`.
    pub fn offset_of(&self, pos: Pos) -> usize {
        let pos = self.clamp(pos);
        self.lines[..pos.line]
            .iter()
            .map(|l| char_len(l) + 1)
            .sum::<usize>()
            + pos.col
    }

    pub fn pos_at(&self, offset: usize) -> Pos {
        let mut left = offset;
        for (line, text) in self.lines.iter().enumerate() {
            let len = char_len(text);
            if left <= len {
                return Pos::new(line, left);
            }
            left -= len + 1;
        }
        let last = self.lines.len() - 1;
        Pos::new(last, char_len(&self.lines[last]))
    }

    /// Takes in an edit someone else made, which turned `plain_text` into `new`. The
    /// cursor and selection stay on the same text. Undo can't reach past it (its record of
    /// places no longer holds), so the history is started afresh.
    pub fn apply_remote(&mut self, new: &str) {
        let old = self.plain_text();
        if old == new {
            return;
        }
        let op = ride_proto::ot::TextOp::diff(&old, new);
        let (cursor, anchor) = (self.offset_of(self.cursor), self.offset_of(self.anchor));
        // The one region that changed, as places in the text before.
        let a: Vec<char> = old.chars().collect();
        let b: Vec<char> = new.chars().collect();
        let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        let suffix = a
            .iter()
            .rev()
            .zip(b.iter().rev())
            .take(a.len().min(b.len()) - prefix)
            .take_while(|(x, y)| x == y)
            .count();
        let (start, end) = (self.pos_at(prefix), self.pos_at(a.len() - suffix));
        let inserted: String = b[prefix..b.len() - suffix].iter().collect();
        self.splice(start, end, &inserted);
        self.cursor = self.pos_at(op.map(cursor, false));
        self.anchor = self.pos_at(op.map(anchor, false));
        self.undo.clear();
        self.redo.clear();
        self.typing_run = false;
        self.goal_col = None;
    }

    pub fn undo(&mut self) {
        let Some(edit) = self.undo.pop() else { return };
        let end = self.end_of(edit.start, &edit.inserted);
        self.splice(edit.start, end, &edit.removed);
        self.cursor = edit.cursor_before;
        self.anchor = edit.anchor_before;
        self.redo.push(edit);
        self.moved();
    }

    pub fn redo(&mut self) {
        let Some(edit) = self.redo.pop() else { return };
        let end = self.end_of(edit.start, &edit.removed);
        let (_, new_end) = self.splice(edit.start, end, &edit.inserted);
        self.cursor = new_end;
        self.anchor = new_end;
        self.undo.push(edit);
        self.moved();
    }

    // ---- search ----

    /// Finds the next (or previous) occurrence of `query` from `from`, wrapping around.
    pub fn find(
        &self,
        query: &str,
        from: Pos,
        forward: bool,
        case_sensitive: bool,
    ) -> Option<(Pos, Pos)> {
        if query.is_empty() {
            return None;
        }
        let needle = if case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };
        let needle_len = char_len(query);
        let n = self.lines.len();

        // Visit every line once, plus the starting line a second time for the wrap-around.
        for step in 0..=n {
            let i = if forward {
                (from.line + step) % n
            } else {
                (from.line + n - step % n) % n
            };
            let line = &self.lines[i];
            let hay = if case_sensitive {
                line.clone()
            } else {
                line.to_lowercase()
            };
            let cols = hay.match_indices(&needle).map(|(b, _)| char_len(&hay[..b]));
            let col = if step == 0 && forward {
                cols.filter(|c| *c >= from.col).next()
            } else if step == 0 {
                cols.filter(|c| c + needle_len <= from.col).last()
            } else if forward {
                cols.min()
            } else {
                cols.max()
            };
            if let Some(col) = col {
                return Some((Pos::new(i, col), Pos::new(i, col + needle_len)));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_from_others_keep_the_cursor_on_its_text() {
        let mut b = Buffer::new("let x = 1;\nfoo();");
        b.set_cursor(Pos::new(1, 3), false);
        assert_eq!(b.offset_of(b.cursor), 14);
        assert_eq!(b.pos_at(14), Pos::new(1, 3));
        // Someone adds a line above: the cursor moves down with "foo".
        b.apply_remote("// note\nlet x = 1;\nfoo();");
        assert_eq!(b.cursor, Pos::new(2, 3));
        assert_eq!(b.text(), "// note\nlet x = 1;\nfoo();");
        // Something after the cursor changes: it stays.
        b.apply_remote("// note\nlet x = 1;\nfoo(bar);");
        assert_eq!(b.cursor, Pos::new(2, 3));
    }

    #[test]
    fn insert_and_undo_multiline() {
        let mut b = Buffer::new("hello\nworld");
        b.set_cursor(Pos::new(0, 5), false);
        b.insert(" there\nbig");
        assert_eq!(b.text(), "hello there\nbig\nworld");
        assert_eq!(b.cursor, Pos::new(1, 3));
        b.undo();
        assert_eq!(b.text(), "hello\nworld");
        assert_eq!(b.cursor, Pos::new(0, 5));
        b.redo();
        assert_eq!(b.text(), "hello there\nbig\nworld");
        assert!(b.is_dirty());
    }

    #[test]
    fn replace_selection_across_lines() {
        let mut b = Buffer::new("one\ntwo\nthree");
        b.anchor = Pos::new(0, 1);
        b.cursor = Pos::new(2, 2);
        assert_eq!(b.selected_text(), "ne\ntwo\nth");
        b.insert("X");
        assert_eq!(b.text(), "oXree");
        b.undo();
        assert_eq!(b.text(), "one\ntwo\nthree");
    }

    #[test]
    fn typing_merges_into_one_undo_step_per_word() {
        let mut b = Buffer::new("");
        for c in "ab cd".chars() {
            b.insert(&c.to_string());
        }
        b.undo();
        assert_eq!(b.text(), "ab ");
        b.undo();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn backspace_joins_lines_and_handles_unicode() {
        let mut b = Buffer::new("aș\nț");
        b.set_cursor(Pos::new(1, 0), false);
        b.backspace(false);
        assert_eq!(b.text(), "așț");
        assert_eq!(b.cursor, Pos::new(0, 2));
        b.backspace(false);
        assert_eq!(b.text(), "aț");
    }

    #[test]
    fn indent_outdent_and_comment_selected_lines() {
        let mut b = Buffer::new("a\n\nb");
        b.select_all();
        b.tab();
        assert_eq!(b.text(), "    a\n\n    b");
        b.toggle_comment("//");
        assert_eq!(b.text(), "    // a\n\n    // b");
        b.toggle_comment("//");
        assert_eq!(b.text(), "    a\n\n    b");
        b.outdent();
        assert_eq!(b.text(), "a\n\nb");
    }

    #[test]
    fn crlf_round_trips() {
        let b = Buffer::new("a\r\nb\r\n");
        assert_eq!(b.line(0), "a");
        assert_eq!(b.text(), "a\r\nb\r\n");
    }

    #[test]
    fn find_wraps_in_both_directions() {
        let b = Buffer::new("foo bar\nFoo");
        let hit = b.find("foo", Pos::new(0, 1), true, false).unwrap();
        assert_eq!(hit.0, Pos::new(1, 0));
        let hit = b.find("foo", Pos::new(1, 3), true, true).unwrap();
        assert_eq!(hit.0, Pos::new(0, 0));
        let hit = b.find("foo", Pos::new(0, 0), false, false).unwrap();
        assert_eq!(hit.0, Pos::new(1, 0));
    }

    #[test]
    fn word_selection() {
        let mut b = Buffer::new("let foo_bar = 1;");
        b.select_word_at(Pos::new(0, 6));
        assert_eq!(b.selected_text(), "foo_bar");
    }
}
