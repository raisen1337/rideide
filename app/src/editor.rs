//! The code editor widget: draws a `Buffer` and turns input events into edits.

use crate::buffer::{Buffer, Pos};
use crate::highlight::{lex, Kind, Lang, State};
use crate::theme;
use eframe::egui::{
    self, pos2, text::LayoutJob, vec2, Color32, CursorIcon, Event, EventFilter, FontId, Key, Rect,
    ScrollArea, Sense, TextFormat, Ui,
};

/// Lines longer than this are drawn without colours, and only their visible part.
const LONG_LINE: usize = 2000;

/// Something the user asked for in the editor that the app has to carry out.
#[derive(Clone, Copy)]
pub enum Action {
    /// Rewrite the selection with Claude (Ctrl+K).
    InlineEdit,
    Explain,
    AddToChat,
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
    /// Lexer state at the start of each line, filled lazily down to the last visible one.
    states: Vec<State>,
    max_cols: usize,
    cached_revision: Option<u64>,
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

fn layout_line(line: &str, lang: &Lang, state: State, font: &FontId) -> LayoutJob {
    let mut spans = Vec::new();
    lex(lang, line, state, &mut spans);

    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let (mut start, mut vcol) = (0, 0);
    for (end, kind) in spans {
        let format = TextFormat::simple(font.clone(), color(kind));
        let segment = &line[start..end];
        if segment.contains('\t') {
            let mut expanded = String::new();
            for c in segment.chars() {
                let w = char_width(c, vcol);
                if c == '\t' {
                    expanded.extend(std::iter::repeat_n(' ', w));
                } else {
                    expanded.push(c);
                }
                vcol += w;
            }
            job.append(&expanded, 0.0, format);
        } else {
            vcol += segment.chars().count();
            job.append(segment, 0.0, format);
        }
        start = end;
    }
    job
}

pub fn show(ui: &mut Ui, buf: &mut Buffer, st: &mut EditorState, lang: &Lang) -> Option<Action> {
    let prefs = crate::settings::get();
    let font = FontId::monospace(prefs.code_size);
    let (cw, glyph_h) = ui
        .ctx()
        .fonts_mut(|f| (f.glyph_width(&font, 'm'), f.row_height(&font)));
    let row_h = (glyph_h + prefs.line_gap).round();
    let pad_y = ((row_h - glyph_h) / 2.0).floor();

    if st.cached_revision != Some(buf.revision) {
        st.cached_revision = Some(buf.revision);
        st.states.clear();
        st.max_cols = (0..buf.line_count())
            .map(|i| visual_col(buf.line(i), usize::MAX))
            .max()
            .unwrap_or(0);
    }

    let digits = buf.line_count().to_string().len().max(3);
    // Without line numbers a narrow margin is left, so text doesn't touch the edge.
    let gutter_w = if prefs.line_numbers {
        (digits as f32 + 2.5) * cw
    } else {
        cw * 2.0
    };
    let mut focused = false;
    let mut action = None;

    ScrollArea::both()
        .auto_shrink(false)
        .show_viewport(ui, |ui, viewport| {
            // Room below the last line, so it can be scrolled up to the middle of the view.
            let size = vec2(
                gutter_w + (st.max_cols + 8) as f32 * cw,
                buf.line_count() as f32 * row_h + viewport.height() - row_h * 3.0,
            )
            .max(viewport.size());
            let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
            let origin = rect.min;
            let text_x = origin.x + gutter_w;
            let view = Rect::from_min_size(origin + viewport.min.to_vec2(), viewport.size());

            let first = (viewport.min.y / row_h).floor().max(0.0) as usize;
            let last = ((viewport.max.y / row_h).ceil() as usize).min(buf.line_count());

            if st.request_focus {
                st.request_focus = false;
                resp.request_focus();
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Text);
            }
            resp.context_menu(|ui| {
                for (label, choice) in [
                    ("Edit with Claude    Ctrl+K", Action::InlineEdit),
                    ("Explain with Claude", Action::Explain),
                    ("Add selection to chat", Action::AddToChat),
                ] {
                    if ui.button(label).clicked() {
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

            let before = (buf.cursor, buf.revision);

            // ---- mouse ----
            let hit = |buf: &Buffer, p: egui::Pos2| {
                let line = (((p.y - origin.y) / row_h).floor().max(0.0) as usize)
                    .min(buf.line_count() - 1);
                let vcol = ((p.x - text_x) / cw).round().max(0.0) as usize;
                Pos::new(line, col_at_visual(buf.line(line), vcol))
            };
            if let Some(p) = resp.interact_pointer_pos() {
                let pos = hit(buf, p);
                let (pressed, down, shift) = ui.input(|i| {
                    (
                        i.pointer.primary_pressed(),
                        i.pointer.primary_down(),
                        i.modifiers.shift,
                    )
                });
                if pressed {
                    resp.request_focus();
                    buf.set_cursor(pos, shift);
                } else if down && resp.dragged() {
                    buf.set_cursor(pos, true);
                }
                if resp.double_clicked() {
                    buf.select_word_at(pos);
                } else if resp.triple_clicked() {
                    buf.select_line(pos.line);
                }
            }

            // ---- keyboard ----
            if resp.has_focus() {
                focused = true;
                let filter = EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                };
                ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, filter));
                let page = ((viewport.height() / row_h) as isize - 1).max(1);
                for event in ui.input(|i| i.events.clone()) {
                    match event {
                        Event::Text(text) | Event::Paste(text) => buf.insert(&text),
                        Event::Copy if buf.has_selection() => {
                            ui.ctx().copy_text(buf.selected_text())
                        }
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
                            match key {
                                Key::ArrowLeft if cmd => buf.move_word_left(sel),
                                Key::ArrowRight if cmd => buf.move_word_right(sel),
                                Key::ArrowLeft => buf.move_left(sel),
                                Key::ArrowRight => buf.move_right(sel),
                                Key::ArrowUp => buf.move_lines(-1, sel),
                                Key::ArrowDown => buf.move_lines(1, sel),
                                Key::PageUp => buf.move_lines(-page, sel),
                                Key::PageDown => buf.move_lines(page, sel),
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
                                Key::K if cmd => action = Some(Action::InlineEdit),
                                Key::Slash if cmd => {
                                    if let Some(prefix) = lang.line_comment.first() {
                                        buf.toggle_comment(prefix);
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
            if before != (buf.cursor, buf.revision) {
                st.reveal_cursor = true;
            }

            // An edit above may have changed the text; the cache is rebuilt next frame,
            // so only trust the lexer states when they still match.
            if st.cached_revision != Some(buf.revision) {
                st.states.clear();
            }
            if st.states.is_empty() {
                st.states.push(State::Normal);
            }
            let last = last.min(buf.line_count());
            let mut scratch = Vec::new();
            while st.states.len() <= last && st.states.len() <= buf.line_count() {
                let i = st.states.len() - 1;
                scratch.clear();
                let next = lex(lang, buf.line(i), st.states[i], &mut scratch);
                st.states.push(next);
            }

            // ---- paint ----
            let painter = ui.painter();
            let line_y = |line: usize| origin.y + line as f32 * row_h;
            let col_x =
                |buf: &Buffer, p: Pos| text_x + visual_col(buf.line(p.line), p.col) as f32 * cw;

            if !buf.has_selection() && prefs.highlight_line {
                let y = line_y(buf.cursor.line);
                painter.rect_filled(
                    Rect::from_min_max(pos2(view.min.x, y), pos2(view.max.x, y + row_h)),
                    0.0,
                    theme::CURRENT_LINE(),
                );
            }

            let (sel_start, sel_end) = buf.selection();
            for line in first..last {
                let y = line_y(line);
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
                if text.is_empty() {
                    continue;
                }
                let pos = pos2(text_x, y + pad_y);
                if text.len() > LONG_LINE {
                    let skip = ((view.min.x - text_x) / cw).floor().max(0.0) as usize;
                    let take = (view.width() / cw).ceil() as usize + 2;
                    let visible: String = text
                        .chars()
                        .skip(skip)
                        .take(take)
                        .map(|c| if c == '\t' { ' ' } else { c })
                        .collect();
                    let galley = painter.layout_no_wrap(visible, font.clone(), theme::TEXT());
                    painter.galley(pos + vec2(skip as f32 * cw, 0.0), galley, theme::TEXT());
                } else {
                    let galley =
                        painter.layout_job(layout_line(text, lang, st.states[line], &font));
                    painter.galley(pos, galley, theme::TEXT());
                }
            }

            let cursor_x = col_x(buf, buf.cursor);
            let cursor_rect = Rect::from_min_size(
                pos2(cursor_x - 1.0, line_y(buf.cursor.line)),
                vec2(2.0, row_h),
            );
            if focused {
                let blink_on =
                    !prefs.cursor_blink || ui.input(|i| i.time) % 1.0 < 0.5 || st.reveal_cursor;
                if blink_on {
                    painter.rect_filled(cursor_rect, 0.0, theme::CURSOR());
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
            }

            let error = theme::ANSI()[1];
            for (line, message) in st.diags.iter().filter(|(l, _)| (first..last).contains(l)) {
                let indent = buf
                    .line(*line)
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .count();
                let x0 = col_x(buf, Pos::new(*line, indent));
                let x1 = col_x(buf, Pos::new(*line, usize::MAX)).max(x0 + cw);
                let y = line_y(*line) + row_h - 1.5;
                painter.line_segment([pos2(x0, y), pos2(x1, y)], (1.5, error));
                painter.text(
                    pos2(x1 + cw * 3.0, line_y(*line) + pad_y),
                    egui::Align2::LEFT_TOP,
                    message,
                    font.clone(),
                    error.gamma_multiply(0.85),
                );
            }

            // The gutter is painted last so it stays put over horizontally scrolled text.
            let gutter = Rect::from_min_size(view.min, vec2(gutter_w - cw, view.height()));
            // Only needed to hide text that scrolled under the line numbers.
            if viewport.min.x > 0.0 {
                painter.rect_filled(gutter, 0.0, theme::EDITOR_BG());
            }
            for (start, count, kind) in &st.marks {
                let color = [theme::ANSI()[2], theme::ANSI()[3], error][*kind as usize % 3];
                let x = view.min.x + 2.0;
                let mark = if *kind == 2 {
                    // Nothing is left of removed lines, so the place they were at is marked.
                    Rect::from_min_size(pos2(x, line_y(start + 1) - 1.5), vec2(cw * 0.8, 3.0))
                } else {
                    Rect::from_min_max(
                        pos2(x, line_y(*start)),
                        pos2(x + 2.5, line_y(start + count)),
                    )
                };
                if mark.intersects(view) {
                    painter.rect_filled(mark, 1.0, color);
                }
            }
            for line in (first..last).filter(|_| prefs.line_numbers) {
                let color = if st.diags.iter().any(|(l, _)| *l == line) {
                    error
                } else if line == buf.cursor.line {
                    theme::TEXT()
                } else {
                    theme::LINE_NUMBER()
                };
                painter.text(
                    pos2(gutter.max.x - cw * 0.5, line_y(line) + pad_y),
                    egui::Align2::RIGHT_TOP,
                    line + 1,
                    font.clone(),
                    color,
                );
            }

            if st.reveal_cursor {
                st.reveal_cursor = false;
                let target = Rect::from_min_max(
                    cursor_rect.min - vec2(gutter_w + cw * 2.0, row_h),
                    cursor_rect.max + vec2(cw * 4.0, row_h),
                );
                ui.scroll_to_rect(target, None);
            }
        });
    action
}
