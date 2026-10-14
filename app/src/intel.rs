//! Code intelligence in the editor, from the language servers: completions as you type,
//! what a name is when the mouse rests on it, the arguments of the call being typed,
//! problems, go to definition, references, rename and quick fixes. The popups are drawn
//! with the editor (`popups`); what they ask for is carried out by the app.

use super::*;
use crate::lsp::{self, CodeAction, Diag, Event, Item, LPos, Location};
use eframe::egui::Rect;

/// A button of the note above the code about the file's language server.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NoteAction {
    Install,
    Start,
    Hide,
}

pub struct Note {
    text: String,
    buttons: Vec<(&'static str, NoteAction)>,
}

pub struct Completion {
    pub items: Vec<Item>,
    /// The items that match what has been typed, best first.
    shown: Vec<usize>,
    selected: usize,
    /// Where the word being completed starts.
    pub start: Pos,
    pub incomplete: bool,
    /// What `shown` was worked out for.
    filtered_for: Option<String>,
    scroll: bool,
    /// The item whose details were last asked for.
    resolving: Option<usize>,
}

pub struct Hover {
    /// The text it is about.
    pub range: (Pos, Pos),
    pub text: String,
    /// The char it was asked for, on screen.
    pub anchor: Rect,
}

pub struct Actions {
    pub list: Vec<CodeAction>,
    selected: usize,
}

/// What the language server has to show in one tab, and what the editor noticed for it.
#[derive(Default)]
pub struct Intel {
    pub completion: Option<Completion>,
    pub hover: Option<Hover>,
    pub signature: Option<lsp::Signature>,
    /// The line signature help was asked on; it closes when the cursor leaves it.
    pub signature_line: usize,
    pub actions: Option<Actions>,
    pub diags: Vec<Diag>,
    diag_gen: u64,
    pub note: Option<Note>,
    // Filled in by the editor each frame.
    /// The cursor on screen, while it is in view and the editor has the keyboard.
    pub cursor_rect: Option<Rect>,
    /// A char typed this frame.
    pub typed: Option<char>,
    /// Keys meant for the open popup.
    pub keys: Vec<Key>,
    /// The char under the mouse and where it is on screen, and since when it is there.
    pub mouse: Option<(Pos, Rect)>,
    pub mouse_since: f64,
    /// The place hover was last asked about.
    hover_asked: Option<Pos>,
    /// Where the hover popup was last drawn: the mouse may move onto it.
    hover_rect: Option<Rect>,
    /// A completion taken whose edits elsewhere (an import) are still to come: its label
    /// and the revision right after it went in.
    accepted: Option<(String, u64)>,
    /// A completion whose details the popup wants.
    want_resolve: Option<usize>,
}

impl Intel {
    /// Whether a list (completions, quick fixes) is open and takes the arrow keys.
    pub fn list_open(&self) -> bool {
        self.actions.is_some()
            || self
                .completion
                .as_ref()
                .is_some_and(|c| !c.shown.is_empty())
    }
}

pub fn severity_color(severity: u8) -> Color32 {
    match severity {
        1 => theme::ANSI()[1],
        2 => theme::ANSI()[3],
        _ => theme::ANSI()[4],
    }
}

fn severity_icon(severity: u8) -> char {
    match severity {
        1 => icons::ERROR,
        2 => '\u{ea6c}',
        _ => '\u{ea74}',
    }
}

/// A completion's kind, as an icon and its colour.
fn kind_icon(kind: u8) -> (char, Color32) {
    match kind {
        2..=4 => ('\u{ea8c}', theme::FUNC()),
        5 => ('\u{eb5f}', theme::TEXT()),
        10 => ('\u{eb65}', theme::TEXT()),
        6 => ('\u{ea88}', theme::ACCENT()),
        7 => ('\u{eb5b}', theme::TYPE()),
        8 => ('\u{eb61}', theme::TYPE()),
        9 => ('\u{ea8b}', theme::TYPE()),
        13 => ('\u{ea95}', theme::TYPE()),
        22 => ('\u{ea91}', theme::TYPE()),
        25 => ('\u{ea92}', theme::TYPE()),
        20 | 21 => ('\u{eb5d}', theme::NUMBER()),
        14 => ('\u{eb62}', theme::KEYWORD()),
        15 => ('\u{eb66}', theme::COMMENT()),
        17 => ('\u{ea7b}', theme::COMMENT()),
        19 => ('\u{ea83}', theme::COMMENT()),
        23 => ('\u{ea86}', theme::FUNC()),
        24 => ('\u{eb64}', theme::PUNCT()),
        _ => ('\u{ea93}', theme::COMMENT()),
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Where the word that ends at `pos` starts.
fn word_start(buf: &Buffer, pos: Pos) -> Pos {
    let line = buf.line(pos.line);
    let before = line.chars().take(pos.col).collect::<Vec<_>>();
    let len = before.iter().rev().take_while(|c| is_ident(**c)).count();
    Pos::new(pos.line, pos.col - len)
}

/// The word at `pos` (or just before it): where it starts and ends, and the word.
fn word_at(buf: &Buffer, pos: Pos) -> Option<(Pos, Pos, String)> {
    let chars: Vec<char> = buf.line(pos.line).chars().collect();
    let col = pos.col.min(chars.len());
    let start = col
        - chars[..col]
            .iter()
            .rev()
            .take_while(|c| is_ident(**c))
            .count();
    let end = col + chars[col..].iter().take_while(|c| is_ident(**c)).count();
    (start < end).then(|| {
        (
            Pos::new(pos.line, start),
            Pos::new(pos.line, end),
            chars[start..end].iter().collect(),
        )
    })
}

/// A server's place in a buffer.
fn pos_in(buf: &Buffer, at: LPos) -> Pos {
    let line = at.line as usize;
    if line >= buf.line_count() {
        let last = buf.line_count() - 1;
        return Pos::new(last, buf.line_len(last));
    }
    Pos::new(line, lsp::col_of(buf.line(line), at.ch))
}

fn lpos_in(buf: &Buffer, pos: Pos) -> LPos {
    lsp::lpos(buf.line(pos.line), pos)
}

/// How well a completion matches what has been typed; lower is better.
fn score(filter: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(3);
    }
    if filter.starts_with(query) {
        return Some(0);
    }
    let (filter, query) = (filter.to_lowercase(), query.to_lowercase());
    if filter.starts_with(&query) {
        return Some(1);
    }
    // The typed letters in order, anywhere in the name.
    let mut rest = filter.chars();
    query.chars().all(|q| rest.any(|f| f == q)).then_some(2)
}

fn refilter(completion: &mut Completion, query: &str) {
    let mut scored: Vec<(u8, usize)> = completion
        .items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| Some((score(&item.filter, query)?, i)))
        .collect();
    let items = &completion.items;
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| items[a.1].sort.cmp(&items[b.1].sort))
            .then_with(|| items[a.1].label.cmp(&items[b.1].label))
    });
    completion.shown = scored.into_iter().map(|(_, i)| i).collect();
    completion.selected = 0;
    completion.scroll = true;
}

fn popup_frame() -> Frame {
    Frame::NONE
        .fill(theme::PANEL_BG())
        .stroke((1.0, theme::LINE()))
        .corner_radius(8)
        .inner_margin(Margin::same(4))
        .shadow(egui::epaint::Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(90),
        })
}

/// A code block in hover text and documentation.
fn code_block(ui: &mut Ui, code: &str, _salt: (usize, usize)) {
    Frame::NONE
        .fill(theme::tint(8))
        .corner_radius(6)
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(code.trim_end())
                        .monospace()
                        .color(theme::TEXT()),
                )
                .wrap(),
            );
        });
}

/// The note above the code about the file's language server, with its buttons.
pub fn note_bar(ui: &mut Ui, intel: &Intel) -> Option<editor::Action> {
    let note = intel.note.as_ref()?;
    let mut chosen = None;
    Frame::NONE
        .fill(theme::tint(7))
        .corner_radius(6)
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icons::TOOLS.to_string()).color(theme::COMMENT()));
                ui.add(
                    egui::Label::new(RichText::new(&note.text).small().color(theme::TEXT())).wrap(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (label, action) in note.buttons.iter().rev() {
                        if ui.small_button(*label).clicked() {
                            chosen = Some(editor::Action::Server(*action));
                        }
                    }
                });
            });
        });
    ui.add_space(2.0);
    chosen
}

/// Draws the popups at the cursor and under the mouse, and takes the keys meant for them.
pub fn popups(
    ui: &mut Ui,
    buf: &mut Buffer,
    st: &mut editor::EditorState,
) -> Option<editor::Action> {
    let ctx = ui.ctx().clone();
    let intel = &mut st.intel;
    let mut action = None;

    // The completion list follows what is typed, and closes once the word is left.
    if let Some(completion) = &mut intel.completion {
        let cursor = buf.cursor;
        let inside = cursor.line == completion.start.line
            && cursor.col >= completion.start.col
            && !buf.has_selection();
        let query = if inside {
            buf.text_range(completion.start, cursor)
        } else {
            String::new()
        };
        if !inside || !query.chars().all(is_ident) {
            intel.completion = None;
        } else if completion.filtered_for.as_deref() != Some(query.as_str()) {
            refilter(completion, &query);
            completion.filtered_for = Some(query);
            if completion.shown.is_empty() && !completion.incomplete {
                intel.completion = None;
            }
        }
    }
    if intel.signature.is_some() && buf.cursor.line != intel.signature_line {
        intel.signature = None;
    }

    for key in std::mem::take(&mut intel.keys) {
        let list_len = match (&intel.actions, &intel.completion) {
            (Some(actions), _) => actions.list.len(),
            (None, Some(completion)) => completion.shown.len(),
            _ => 0,
        };
        let step = |selected: usize, by: isize| -> usize {
            if list_len == 0 {
                return 0;
            }
            (selected as isize + by).rem_euclid(list_len as isize) as usize
        };
        let by = match key {
            Key::ArrowUp => -1,
            Key::ArrowDown => 1,
            Key::PageUp => -8,
            Key::PageDown => 8,
            _ => 0,
        };
        match key {
            Key::Escape => {
                if intel.actions.take().is_none()
                    && intel.completion.take().is_none()
                    && intel.signature.take().is_none()
                {
                    intel.hover = None;
                }
            }
            Key::Enter | Key::Tab => {
                if let Some(actions) = intel.actions.take() {
                    action = Some(editor::Action::RunAction(actions.selected));
                } else if let Some(completion) = &intel.completion {
                    if let Some(&index) = completion.shown.get(completion.selected) {
                        action = Some(editor::Action::Accept(index));
                    }
                }
            }
            _ => {
                if let Some(actions) = &mut intel.actions {
                    actions.selected = step(actions.selected, by);
                } else if let Some(completion) = &mut intel.completion {
                    completion.selected = step(completion.selected, by);
                    completion.scroll = true;
                }
            }
        }
    }

    // ---- hover: what is under the mouse, and the problems there ----
    let pointer = ctx.pointer_hover_pos();
    let over_popup = pointer
        .zip(intel.hover_rect)
        .is_some_and(|(p, r)| r.expand(6.0).contains(p));
    let mouse = intel.mouse.map(|m| m.0);
    if let Some(hover) = &intel.hover {
        let inside = mouse.is_some_and(|p| p >= hover.range.0 && p <= hover.range.1);
        if !inside && !over_popup {
            intel.hover = None;
        }
    }
    let rested = ctx.input(|i| i.time) - intel.mouse_since >= 0.45;
    let problems: Vec<&Diag> = match intel.mouse {
        Some((pos, _)) if rested || over_popup => intel
            .diags
            .iter()
            .filter(|d| d.severity <= 3)
            .filter(|d| {
                let (start, end) = (pos_in(buf, d.start), pos_in(buf, d.end));
                // A problem with no width covers the char it is at.
                pos >= start && (pos < end || start == end && pos == start)
            })
            .collect(),
        _ => Vec::new(),
    };
    let anchor = intel
        .hover
        .as_ref()
        .map(|h| h.anchor)
        .or(intel.mouse.map(|m| m.1).filter(|_| !problems.is_empty()));
    let mut hover_rect = None;
    if let Some(anchor) = anchor {
        let text = intel.hover.as_ref().map(|h| h.text.clone());
        let shown = egui::Area::new(ui.id().with("lsp_hover"))
            .order(egui::Order::Tooltip)
            .fixed_pos(anchor.left_bottom() + vec2(0.0, 2.0))
            .constrain(true)
            .show(&ctx, |ui| {
                popup_frame().inner_margin(Margin::same(8)).show(ui, |ui| {
                    ui.set_max_width(560.0);
                    ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for diag in &problems {
                            ui.horizontal_top(|ui| {
                                ui.label(
                                    RichText::new(severity_icon(diag.severity).to_string())
                                        .color(severity_color(diag.severity)),
                                );
                                let source = if diag.source.is_empty() {
                                    String::new()
                                } else {
                                    format!("  {}", diag.source)
                                };
                                ui.add(
                                    egui::Label::new(pieces(&[
                                        (&diag.message, theme::TEXT()),
                                        (&source, theme::COMMENT()),
                                    ]))
                                    .wrap(),
                                );
                            });
                        }
                        if let Some(text) = &text {
                            if !problems.is_empty() {
                                ui.separator();
                            }
                            crate::markdown::render(ui, text, 0, &code_block);
                        }
                    });
                });
            });
        hover_rect = Some(shown.response.rect);
    }
    intel.hover_rect = hover_rect;
    if intel.mouse.is_some() && !rested {
        ctx.request_repaint_after(std::time::Duration::from_millis(460));
    }

    let Some(cursor) = intel.cursor_rect else {
        return action;
    };
    let screen = ctx.content_rect();

    // ---- the arguments of the call being typed, above the cursor ----
    if let Some(signature) = &intel.signature {
        egui::Area::new(ui.id().with("lsp_signature"))
            .order(egui::Order::Foreground)
            .fixed_pos(cursor.left_top() - vec2(4.0, 4.0))
            .pivot(egui::Align2::LEFT_BOTTOM)
            .constrain(true)
            .interactable(false)
            .show(&ctx, |ui| {
                popup_frame()
                    .inner_margin(Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        ui.set_max_width(600.0);
                        let mut job = egui::text::LayoutJob::default();
                        let font = egui::FontId::monospace(13.0);
                        let chars: Vec<char> = signature.label.chars().collect();
                        let (a, b) = signature.active.unwrap_or((0, 0));
                        for (part, strong) in [
                            (&chars[..a.min(chars.len())], false),
                            (&chars[a.min(chars.len())..b.min(chars.len())], true),
                            (&chars[b.min(chars.len())..], false),
                        ] {
                            let text: String = part.iter().collect();
                            let mut format = egui::TextFormat::simple(
                                font.clone(),
                                if strong {
                                    theme::ACCENT()
                                } else {
                                    theme::TEXT()
                                },
                            );
                            if strong {
                                format.underline = egui::Stroke::new(1.0, theme::ACCENT());
                            }
                            job.append(&text, 0.0, format);
                        }
                        job.wrap.max_width = 600.0;
                        ui.label(job);
                        let doc: String = signature
                            .doc
                            .lines()
                            .take_while(|l| !l.trim().is_empty())
                            .take(3)
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !doc.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(doc).small().color(theme::COMMENT()),
                                )
                                .wrap(),
                            );
                        }
                    });
            });
    }

    // ---- quick fixes, below the cursor ----
    if let Some(actions) = &mut intel.actions {
        egui::Area::new(ui.id().with("lsp_actions"))
            .order(egui::Order::Foreground)
            .fixed_pos(cursor.left_bottom() + vec2(-4.0, 4.0))
            .constrain(true)
            .show(&ctx, |ui| {
                popup_frame().show(ui, |ui| {
                    ui.set_min_width(260.0);
                    ui.set_max_width(520.0);
                    for (index, fix) in actions.list.iter().enumerate() {
                        let selected = index == actions.selected;
                        let icon = if fix.preferred {
                            icons::STAR
                        } else {
                            '\u{ea61}'
                        };
                        let label = pieces(&[
                            (&format!("{icon}  "), theme::ANSI()[3]),
                            (&fix.title, theme::TEXT()),
                        ]);
                        if ui.selectable_label(selected, label).clicked() {
                            action = Some(editor::Action::RunAction(index));
                        }
                    }
                });
            });
        if matches!(action, Some(editor::Action::RunAction(_))) {
            intel.actions = None;
        }
    }

    // ---- completions, below the cursor ----
    if let Some(completion) = &mut intel.completion {
        if completion.shown.is_empty() {
            return action;
        }
        const ROW: f32 = 22.0;
        let rows = completion.shown.len().min(10);
        let height = rows as f32 * ROW + 10.0;
        let below = cursor.left_bottom() + vec2(-6.0, 4.0);
        let place = if below.y + height > screen.bottom() && cursor.top() - height > screen.top() {
            cursor.left_top() - vec2(6.0, height + 4.0)
        } else {
            below
        };
        let query = completion.filtered_for.clone().unwrap_or_default();
        egui::Area::new(ui.id().with("lsp_completion"))
            .order(egui::Order::Foreground)
            .fixed_pos(place)
            .constrain(true)
            .show(&ctx, |ui| {
                ui.horizontal_top(|ui| {
                    popup_frame().show(ui, |ui| {
                        ui.set_width(400.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ScrollArea::vertical()
                            .max_height(rows as f32 * ROW)
                            .auto_shrink([false, true])
                            .show_rows(ui, ROW, completion.shown.len(), |ui, range| {
                                // Down the list: the popups sit side by side, so the
                                // layout here would otherwise run across.
                                ui.vertical(|ui| {
                                    for row in range {
                                        let index = completion.shown[row];
                                        let item = &completion.items[index];
                                        let (rect, resp) = ui.allocate_exact_size(
                                            vec2(ui.available_width(), ROW),
                                            Sense::click(),
                                        );
                                        let selected = row == completion.selected;
                                        if selected {
                                            ui.painter().rect_filled(rect, 5.0, theme::SELECTION());
                                            if completion.scroll {
                                                ui.scroll_to_rect(rect, None);
                                            }
                                        } else if resp.hovered() {
                                            ui.painter().rect_filled(rect, 5.0, theme::tint(10));
                                        }
                                        let (icon, color) = kind_icon(item.kind);
                                        let painter = ui.painter_at(rect);
                                        painter.text(
                                            rect.left_center() + vec2(8.0, 0.0),
                                            egui::Align2::LEFT_CENTER,
                                            icon,
                                            egui::FontId::proportional(13.0),
                                            color,
                                        );
                                        // The typed part of the name stands out.
                                        let matched = if item
                                            .label
                                            .to_lowercase()
                                            .starts_with(&query.to_lowercase())
                                        {
                                            query.chars().count()
                                        } else {
                                            0
                                        };
                                        let head: String =
                                            item.label.chars().take(matched).collect();
                                        let tail: String =
                                            item.label.chars().skip(matched).collect();
                                        let mut job = egui::text::LayoutJob::default();
                                        let font = egui::FontId::monospace(13.0);
                                        job.append(
                                            &head,
                                            0.0,
                                            egui::TextFormat::simple(font.clone(), theme::ACCENT()),
                                        );
                                        job.append(
                                            &tail,
                                            0.0,
                                            egui::TextFormat::simple(font, theme::TEXT()),
                                        );
                                        let label = painter.layout_job(job);
                                        let label_w = label.size().x;
                                        painter.galley(
                                            rect.left_center() + vec2(28.0, -label.size().y / 2.0),
                                            label,
                                            theme::TEXT(),
                                        );
                                        // Its detail (a type, a module) to the right, if there's room.
                                        let room = rect.width() - 28.0 - label_w - 24.0;
                                        if room > 40.0 && !item.detail.is_empty() {
                                            let detail: String = item
                                                .detail
                                                .lines()
                                                .next()
                                                .unwrap_or("")
                                                .chars()
                                                .take(80)
                                                .collect();
                                            let galley = painter.layout(
                                                detail,
                                                egui::FontId::proportional(11.5),
                                                theme::COMMENT(),
                                                room,
                                            );
                                            let size = galley.size();
                                            if galley.rows.len() == 1 {
                                                painter.galley(
                                                    rect.right_center()
                                                        - vec2(size.x + 8.0, size.y / 2.0),
                                                    galley,
                                                    theme::COMMENT(),
                                                );
                                            }
                                        }
                                        if resp.clicked() {
                                            action = Some(editor::Action::Accept(index));
                                        }
                                    }
                                });
                            });
                    });
                    // The selected one's documentation beside the list.
                    let Some(&index) = completion.shown.get(completion.selected) else {
                        return;
                    };
                    let item = &completion.items[index];
                    if !item.resolved && completion.resolving != Some(index) {
                        completion.resolving = Some(index);
                        intel.want_resolve = Some(index);
                    }
                    if item.doc.trim().is_empty() && item.detail.trim().is_empty() {
                        return;
                    }
                    popup_frame().inner_margin(Margin::same(8)).show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                            ui.vertical(|ui| {
                                if !item.detail.trim().is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(item.detail.trim())
                                                .monospace()
                                                .size(12.0)
                                                .color(theme::TYPE()),
                                        )
                                        .wrap(),
                                    );
                                }
                                if !item.doc.trim().is_empty() {
                                    crate::markdown::render(ui, &item.doc, 1, &code_block);
                                }
                            })
                        });
                    });
                });
            });
        completion.scroll = false;
    }
    action
}

impl App {
    /// The name of the language server of a tab's file, once it is ready to answer.
    fn intel_server(&self, index: usize) -> Option<String> {
        let tab = self.tabs.get(index).filter(|t| t.preview.is_none())?;
        let spec = settings::server_for(&tab.path)?;
        self.lsp.ready(&spec.name).map(|_| spec.name)
    }

    /// Whether a language server checks this tab's file, so the simpler syntax check isn't needed.
    pub(super) fn intel_covers(&self, index: usize) -> bool {
        self.intel_server(index).is_some()
    }

    /// Hands what the client has to send to the connection.
    fn intel_flush(&mut self) {
        for request in self.lsp.outgoing() {
            self.request(request);
        }
    }

    /// Tells the server what the tab holds now, if it doesn't know yet.
    fn intel_sync(&mut self, index: usize, name: &str) {
        let tab = &self.tabs[index];
        if self.lsp.synced(name, &tab.path) == Some(tab.buf.revision) {
            return;
        }
        let (path, text, revision) = (tab.path.clone(), tab.buf.plain_text(), tab.buf.revision);
        self.lsp.sync(
            name,
            &path,
            &lsp::language_id(&path),
            text,
            revision,
            self.now,
        );
    }

    fn start_server(&mut self, spec: &settings::LangServer) {
        // Programs installed for one user often land in these, which a login shell may not
        // have on its PATH.
        let command = if chat::local_shell() {
            spec.command.clone()
        } else {
            format!(
                "PATH=\"$PATH:$HOME/.local/bin:$HOME/go/bin:$HOME/.cargo/bin\" exec {}",
                spec.command
            )
        };
        let proc = self.request(Request::ProcOpen {
            command,
            cwd: self.root.clone(),
        });
        let root = self.root.clone();
        self.lsp.started(&spec.name, proc, &root);
    }

    /// Every frame: starts the servers the open files need, keeps them up to date with the
    /// text, and asks what the editor wants to know.
    pub(super) fn intel_tick(&mut self, ctx: &Context) {
        let usable = self.online() && !self.guest() && settings::with(|s| s.language_servers_on);
        if !usable {
            if self.online() && !self.lsp.servers.is_empty() {
                self.lsp.stop_all();
                self.intel_flush();
            }
            for tab in &mut self.tabs {
                tab.view.intel.note = None;
                tab.view.intel.diags.clear();
            }
            return;
        }
        let mut open: HashMap<String, HashSet<String>> = HashMap::new();
        for index in 0..self.tabs.len() {
            let tab = &self.tabs[index];
            let spec = settings::server_for(&tab.path).filter(|_| tab.preview.is_none());
            let Some(spec) = spec else {
                self.tabs[index].view.intel.note = None;
                continue;
            };
            let name = spec.name.clone();
            open.entry(name.clone())
                .or_default()
                .insert(tab.path.clone());
            let state = self.lsp.server(&name).map(|s| s.state.clone());
            let installing = self.lsp.installing.contains(&name);
            let hidden = self.lsp.dismissed.contains(&name);
            if state.is_none() && !installing && !hidden {
                self.start_server(&spec);
            }
            let can_install = !spec.install.trim().is_empty() && !chat::local_shell();
            let note = match (&state, installing, hidden) {
                (_, _, true) => None,
                (_, true, _) => Some(Note {
                    text: format!(
                        "Installing the {name} language server in the terminal. Start it once that's done."
                    ),
                    buttons: vec![("Start it", NoteAction::Start), ("Hide", NoteAction::Hide)],
                }),
                (Some(lsp::State::Missing), ..) => Some(Note {
                    text: if can_install {
                        format!(
                            "The {name} language server isn't installed on this server. It gives completion, problems, go to definition and rename in these files."
                        )
                    } else {
                        format!(
                            "The {name} language server ({}) isn't installed here. Its command can be changed in Settings → Editor.",
                            spec.command
                        )
                    },
                    buttons: if can_install {
                        vec![("Install", NoteAction::Install), ("Not now", NoteAction::Hide)]
                    } else {
                        vec![("Hide", NoteAction::Hide)]
                    },
                }),
                (Some(lsp::State::Failed(error)), ..) => Some(Note {
                    text: format!("The {name} language server stopped: {error}"),
                    buttons: if can_install {
                        vec![
                            ("Restart", NoteAction::Start),
                            ("Install again", NoteAction::Install),
                            ("Hide", NoteAction::Hide),
                        ]
                    } else {
                        vec![("Restart", NoteAction::Start), ("Hide", NoteAction::Hide)]
                    },
                }),
                _ => None,
            };
            self.tabs[index].view.intel.note = note;
            if state != Some(lsp::State::Ready) {
                continue;
            }
            self.intel_sync(index, &name);
            self.intel_tab(index, &name, ctx);
        }
        let names: Vec<String> = self.lsp.servers.iter().map(|s| s.name.clone()).collect();
        for name in names {
            let paths = open.get(&name);
            let paths: HashSet<&str> = paths.into_iter().flatten().map(String::as_str).collect();
            self.lsp.close_others(&name, &paths);
        }
        self.lsp.tick(self.now);
        if self.lsp.waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        self.intel_flush();
    }

    /// What a tab with a ready server wants: its problems, completions after typing, hover.
    fn intel_tab(&mut self, index: usize, name: &str, ctx: &Context) {
        // RIDE_INTEL_COMPLETE asks for completions at the cursor of the first file a server
        // is ready for, as Ctrl+Space would, to look at the list.
        static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if std::env::var_os("RIDE_INTEL_COMPLETE").is_some()
            && !ASKED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            self.intel_complete(index, name, None);
        }
        let gen = self.lsp.diag_gen;
        let tab = &mut self.tabs[index];
        if tab.view.intel.diag_gen != gen {
            tab.view.intel.diag_gen = gen;
            tab.view.intel.diags = self.lsp.diags_for(&tab.path).cloned().unwrap_or_default();
        }
        let Some(server) = self.lsp.ready(name) else {
            return;
        };
        let (completion_triggers, signature_triggers) =
            (server.completion_triggers(), server.signature_triggers());
        let can_hover = server.can("hoverProvider");

        if let Some(typed) = tab.view.intel.typed.take() {
            let open = tab.view.intel.completion.as_ref();
            if is_ident(typed) {
                if open.is_none_or(|c| c.incomplete) {
                    self.intel_complete(index, name, None);
                }
            } else if completion_triggers.contains(&typed) && !typed.is_whitespace() {
                self.intel_complete(index, name, Some(typed));
            } else {
                self.tabs[index].view.intel.completion = None;
            }
            if signature_triggers.contains(&typed) {
                self.intel_ask(index, name, "textDocument/signatureHelp", None);
            }
        }

        let tab = &mut self.tabs[index];
        let intel = &mut tab.view.intel;
        if let Some((pos, _)) = intel.mouse {
            let covered = intel
                .hover
                .as_ref()
                .is_some_and(|h| pos >= h.range.0 && pos <= h.range.1);
            if can_hover && !covered && intel.hover_asked != Some(pos) {
                let waited = self.now - intel.mouse_since;
                if waited >= 0.35 {
                    intel.hover_asked = Some(pos);
                    let at = lpos_in(&tab.buf, pos);
                    let pending = lsp::Pending::Hover {
                        tab: tab.uid,
                        at: pos,
                    };
                    let path = tab.path.clone();
                    self.lsp.at(name, "textDocument/hover", &path, at, pending);
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(0.36 - waited));
                }
            }
        }

        let tab = &mut self.tabs[index];
        if let Some(wanted) = tab.view.intel.want_resolve.take() {
            if let Some(item) = tab
                .view
                .intel
                .completion
                .as_ref()
                .and_then(|c| c.items.get(wanted))
            {
                let item = item.clone();
                let uid = tab.uid;
                self.lsp.resolve_completion(name, uid, &item);
            }
        }
    }

    /// Asks for completions at the cursor.
    fn intel_complete(&mut self, index: usize, name: &str, trigger: Option<char>) {
        self.intel_sync(index, name);
        let tab = &self.tabs[index];
        let cursor = tab.buf.cursor;
        let at = lpos_in(&tab.buf, cursor);
        let pending = lsp::Pending::Completion {
            tab: tab.uid,
            at: cursor,
        };
        let path = tab.path.clone();
        self.lsp.complete(name, &path, at, trigger, pending);
    }

    /// Asks the server something about the cursor (or `pos`) of a tab.
    fn intel_ask(&mut self, index: usize, name: &str, method: &str, pos: Option<Pos>) {
        self.intel_sync(index, name);
        let tab = &mut self.tabs[index];
        let pos = pos.unwrap_or(tab.buf.cursor);
        let at = lpos_in(&tab.buf, pos);
        let uid = tab.uid;
        let pending = match method {
            "textDocument/definition" => lsp::Pending::Definition,
            "textDocument/signatureHelp" => {
                tab.view.intel.signature_line = pos.line;
                lsp::Pending::Signature { tab: uid }
            }
            "textDocument/references" => lsp::Pending::References {
                name: word_at(&tab.buf, pos).map(|w| w.2).unwrap_or_default(),
            },
            _ => lsp::Pending::Other,
        };
        let path = tab.path.clone();
        self.lsp.at(name, method, &path, at, pending);
    }

    /// Carries out what the editor asked of the language server.
    pub(super) fn intel_action(&mut self, index: usize, action: editor::Action) {
        use editor::Action as A;
        if let A::Server(button) = action {
            self.intel_note(index, button);
            return;
        }
        let Some(name) = self.intel_server(index) else {
            let has = settings::server_for(&self.tabs[index].path).is_some();
            self.status = if has && settings::with(|s| s.language_servers_on) {
                "The language server for this file isn't running yet".into()
            } else {
                "No language server is set for this kind of file (Settings → Editor)".into()
            };
            return;
        };
        match action {
            A::Definition(pos) => {
                self.intel_ask(index, &name, "textDocument/definition", Some(pos))
            }
            A::References => self.intel_ask(index, &name, "textDocument/references", None),
            A::Complete => self.intel_complete(index, &name, None),
            A::Rename => {
                let tab = &self.tabs[index];
                let Some((_, _, word)) = word_at(&tab.buf, tab.buf.cursor) else {
                    self.status = "Put the cursor on a name to rename it".into();
                    return;
                };
                self.dialog = Some(Dialog::Prompt {
                    title: format!("Rename {word} to"),
                    value: word,
                    action: PromptAction::RenameSymbol {
                        tab: tab.uid,
                        pos: tab.buf.cursor,
                    },
                });
            }
            A::CodeActions => {
                self.intel_sync(index, &name);
                let tab = &self.tabs[index];
                let (start, end) = tab.buf.selection();
                let (start, end) = (lpos_in(&tab.buf, start), lpos_in(&tab.buf, end));
                let (uid, path, line) = (tab.uid, tab.path.clone(), tab.buf.cursor.line as u32);
                self.lsp.code_actions(&name, uid, &path, start, end, line);
            }
            A::Accept(item) => self.intel_accept(index, &name, item),
            A::RunAction(chosen) => {
                let Some(actions) = self.tabs[index].view.intel.actions.take() else {
                    return;
                };
                let Some(fix) = actions.list.get(chosen) else {
                    return;
                };
                let mut events = Vec::new();
                self.lsp.apply_action(&name, fix, &mut events);
                self.intel_events(events);
            }
            _ => {}
        }
        self.intel_flush();
    }

    /// Renames what is at `pos` in the tab `uid`, once the new name is typed.
    pub(super) fn intel_rename(&mut self, uid: u64, pos: Pos, new_name: &str) {
        let Some(index) = self.tabs.iter().position(|t| t.uid == uid) else {
            return;
        };
        let Some(name) = self.intel_server(index) else {
            return;
        };
        self.intel_sync(index, &name);
        let tab = &self.tabs[index];
        let at = lpos_in(&tab.buf, tab.buf.clamp(pos));
        let path = tab.path.clone();
        let pending = lsp::Pending::Rename {
            name: new_name.to_string(),
        };
        self.lsp
            .at(&name, "textDocument/rename", &path, at, pending);
        self.intel_flush();
    }

    /// Formats with the language server, for files that have no formatter of their own.
    pub(super) fn intel_format(&mut self, index: usize) -> bool {
        let Some(name) = self.intel_server(index) else {
            return false;
        };
        if !self
            .lsp
            .ready(&name)
            .is_some_and(|s| s.can("documentFormattingProvider"))
        {
            return false;
        }
        self.intel_sync(index, &name);
        let tab = &self.tabs[index];
        let (path, uid, revision) = (tab.path.clone(), tab.uid, tab.buf.revision);
        let (width, spaces) = settings::with(|s| (s.tab_width, s.indent != settings::Indent::Tabs));
        self.lsp.format(&name, &path, uid, revision, width, spaces);
        self.intel_flush();
        true
    }

    pub(super) fn intel_saved(&mut self, path: &str) {
        if let Some(spec) = settings::server_for(path) {
            self.lsp.saved(&spec.name, path);
            self.intel_flush();
        }
    }

    fn intel_note(&mut self, index: usize, button: NoteAction) {
        let Some(spec) = settings::server_for(&self.tabs[index].path) else {
            return;
        };
        let name = spec.name.clone();
        match button {
            NoteAction::Install => {
                // In a terminal, where its progress shows and anything it asks can be answered.
                self.new_term();
                if let Some(term) = self.terms.last() {
                    let term = term.id;
                    let data = format!("{}\r", spec.install).into_bytes();
                    self.request(Request::TermInput { term, data });
                }
                self.lsp.forget(&name);
                self.lsp.installing.insert(name);
            }
            NoteAction::Start => {
                self.lsp.installing.remove(&name);
                self.lsp.forget(&name);
            }
            NoteAction::Hide => {
                self.lsp.dismissed.insert(name);
            }
        }
        self.intel_flush();
    }

    /// Puts a completion in: what it replaces, its snippet filled in, and the edits that
    /// come with it.
    fn intel_accept(&mut self, index: usize, name: &str, chosen: usize) {
        let tab = &mut self.tabs[index];
        let Some(completion) = tab.view.intel.completion.take() else {
            return;
        };
        let Some(item) = completion.items.get(chosen).cloned() else {
            return;
        };
        let buf = &mut tab.buf;
        let cursor = buf.cursor;
        let start = match item.range {
            Some((from, _)) if from.line as usize == cursor.line => pos_in(buf, from).min(cursor),
            _ => completion.start,
        };
        let (text, place) = if item.snippet {
            lsp::expand_snippet(&item.insert)
        } else {
            (item.insert.clone(), None)
        };
        // The lines of a snippet are indented like the line it goes into.
        let indent: String = buf
            .line(cursor.line)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let indented = |offset: usize| -> usize {
            let breaks = text.chars().take(offset).filter(|c| *c == '\n').count();
            offset + breaks * indent.chars().count()
        };
        let place = place.map(|(a, b)| (indented(a), indented(b)));
        let text = text.replace('\n', &format!("\n{indent}"));
        let total = text.chars().count();
        let mut edits = vec![(start, cursor, text)];
        edits.extend(
            item.extra
                .iter()
                .map(|e| (pos_in(buf, e.start), pos_in(buf, e.end), e.text.clone())),
        );
        buf.apply_edits(edits);
        // The cursor went to the end of what was put in; a snippet's first place is chosen.
        if let Some((a, b)) = place {
            let end = buf.offset_of(buf.cursor);
            let from = end - total;
            buf.anchor = buf.pos_at(from + a);
            buf.cursor = buf.pos_at(from + b);
        }
        tab.view.reveal_cursor = true;
        tab.view.request_focus = true;
        if item.extra.is_empty() && !item.resolved {
            // The edits elsewhere (an import) may only come with the details.
            tab.view.intel.accepted = Some((item.label.clone(), tab.buf.revision));
            let uid = tab.uid;
            self.lsp.resolve_completion(name, uid, &item);
        }
        let opens_call = item.insert.contains('(');
        if opens_call
            && self
                .lsp
                .ready(name)
                .is_some_and(|s| s.can("signatureHelpProvider"))
        {
            self.intel_ask(index, name, "textDocument/signatureHelp", None);
        }
    }

    /// Acts on what the servers said.
    fn intel_events(&mut self, events: Vec<Event>) {
        for event in events {
            match event {
                Event::Completion {
                    tab,
                    at,
                    items,
                    incomplete,
                } => {
                    let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == tab) else {
                        continue;
                    };
                    let cursor = tab.buf.cursor;
                    if cursor.line != at.line || items.is_empty() {
                        if !incomplete {
                            tab.view.intel.completion = None;
                        }
                        continue;
                    }
                    let start = word_start(&tab.buf, cursor);
                    tab.view.intel.completion = Some(Completion {
                        items,
                        shown: Vec::new(),
                        selected: 0,
                        start,
                        incomplete,
                        filtered_for: None,
                        scroll: true,
                        resolving: None,
                    });
                }
                Event::Resolved { tab, item } => {
                    let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == tab) else {
                        continue;
                    };
                    let accepted = tab.view.intel.accepted.take();
                    match accepted {
                        Some((label, revision)) if label == item.label => {
                            if tab.buf.revision == revision && !item.extra.is_empty() {
                                let buf = &mut tab.buf;
                                let edits = item
                                    .extra
                                    .iter()
                                    .map(|e| {
                                        (pos_in(buf, e.start), pos_in(buf, e.end), e.text.clone())
                                    })
                                    .collect();
                                buf.apply_edits(edits);
                            }
                        }
                        other => tab.view.intel.accepted = other,
                    }
                    if let Some(completion) = &mut tab.view.intel.completion {
                        if let Some(old) = completion
                            .items
                            .iter_mut()
                            .find(|i| i.label == item.label && !i.resolved)
                        {
                            if !item.detail.is_empty() {
                                old.detail = item.detail;
                            }
                            if !item.doc.is_empty() {
                                old.doc = item.doc;
                            }
                            if !item.extra.is_empty() {
                                old.extra = item.extra;
                            }
                            old.resolved = true;
                        }
                    }
                }
                Event::Hover {
                    tab,
                    at,
                    text,
                    range,
                } => {
                    let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == tab) else {
                        continue;
                    };
                    let intel = &mut tab.view.intel;
                    let Some((mouse, anchor)) = intel.mouse.filter(|m| m.0 == at) else {
                        continue;
                    };
                    let range = match range {
                        Some((a, b)) => (pos_in(&tab.buf, a), pos_in(&tab.buf, b)),
                        None => word_at(&tab.buf, at)
                            .map(|w| (w.0, w.1))
                            .unwrap_or((mouse, mouse)),
                    };
                    intel.hover = Some(Hover {
                        range,
                        text,
                        anchor,
                    });
                }
                Event::Signature { tab, signature } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == tab) {
                        tab.view.intel.signature = signature;
                    }
                }
                Event::Definition { locations } => match locations.len() {
                    0 => self.status = "No definition found".into(),
                    1 => self.goto_location(&locations[0]),
                    n => self.show_locations(format!("{n} definitions"), locations),
                },
                Event::References { name, locations } => {
                    if locations.is_empty() {
                        self.status = "No references found".into();
                    } else {
                        let n = locations.len();
                        let what = if n == 1 { "reference" } else { "references" };
                        self.show_locations(format!("{n} {what} to {name}"), locations);
                    }
                }
                Event::Edit { edit, what } => self.apply_workspace_edit(edit, what),
                Event::Actions { tab, actions } => {
                    let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == tab) else {
                        continue;
                    };
                    if actions.is_empty() {
                        self.status = "No quick fixes here".into();
                    } else {
                        let selected = actions.iter().position(|a| a.preferred).unwrap_or(0);
                        tab.view.intel.actions = Some(Actions {
                            list: actions,
                            selected,
                        });
                    }
                }
                Event::Format {
                    tab,
                    revision,
                    edits,
                } => {
                    let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == tab) else {
                        continue;
                    };
                    if tab.buf.revision != revision {
                        self.status =
                            "The file changed while it was being formatted, so it was left as it was"
                                .into();
                    } else if !edits.is_empty() {
                        let buf = &mut tab.buf;
                        let edits = edits
                            .iter()
                            .map(|e| (pos_in(buf, e.start), pos_in(buf, e.end), e.text.clone()))
                            .collect();
                        buf.apply_edits(edits);
                    }
                }
                Event::Diagnostics => {}
                Event::Message(text) => self.status = text,
            }
        }
        self.intel_flush();
    }

    /// Takes a message from a language server's process. False if it isn't one's.
    pub(super) fn intel_message(&mut self, id: u64, resp: &Response) -> bool {
        let Some(events) = self.lsp.feed(id, resp) else {
            return false;
        };
        self.intel_events(events);
        true
    }

    fn goto_location(&mut self, location: &Location) {
        let open = self
            .tabs
            .iter()
            .position(|t| lsp::same_path(&t.path, &location.path));
        match open {
            Some(index) => {
                let pos = pos_in(&self.tabs[index].buf, location.start);
                let path = self.tabs[index].path.clone();
                self.open_file(path, Some(pos));
            }
            None => {
                let pos = Pos::new(location.start.line as usize, location.start.ch as usize);
                self.open_file(location.path.clone(), Some(pos));
            }
        }
    }

    /// Lists places in the search results above the file tree, with their lines.
    fn show_locations(&mut self, title: String, locations: Vec<Location>) {
        if let Some(id) = self.search.id.take() {
            self.request(Request::Cancel { target: id });
        }
        let mut unread = Vec::new();
        self.search.hits = locations
            .iter()
            .map(|l| {
                let tab = self
                    .tabs
                    .iter()
                    .find(|t| lsp::same_path(&t.path, &l.path) && t.preview.is_none());
                let (path, col, text) = match tab {
                    Some(tab) => {
                        let line = (l.start.line as usize).min(tab.buf.line_count() - 1);
                        let text = tab.buf.line(line);
                        (
                            tab.path.clone(),
                            lsp::col_of(text, l.start.ch),
                            text.trim_end().to_string(),
                        )
                    }
                    None => {
                        if !unread.contains(&l.path) {
                            unread.push(l.path.clone());
                        }
                        (l.path.clone(), l.start.ch as usize, String::new())
                    }
                };
                SearchHit {
                    path,
                    line: l.start.line,
                    col: col as u32,
                    text: text.chars().take(400).collect(),
                }
            })
            .collect();
        // Grouped by file, as the search results are.
        self.search
            .hits
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        self.search.title = Some(title);
        self.view = View::Code;
        self.show_side = true;
        self.side = Side::Search;
        for path in unread {
            self.request_then(
                Request::ReadFile { path: path.clone() },
                Pending::HitLines { path },
            );
        }
    }

    /// The lines of a file that wasn't open, for the places listed in it.
    pub(super) fn fill_hit_lines(&mut self, path: &str, data: &[u8]) {
        let text = String::from_utf8_lossy(data);
        let lines: Vec<&str> = text.lines().collect();
        for hit in self.search.hits.iter_mut().filter(|h| h.path == path) {
            if let Some(line) = lines.get(hit.line as usize) {
                hit.text = line.trim_end().chars().take(400).collect();
                hit.col = lsp::col_of(line, hit.col) as u32;
            }
        }
    }

    /// Makes the edits a rename or a quick fix asks for: in the editor for open files, on
    /// disk for the others.
    fn apply_workspace_edit(&mut self, edit: lsp::WorkspaceEdit, what: String) {
        let files = edit.len();
        for (path, edits) in edit {
            let open = self
                .tabs
                .iter()
                .position(|t| lsp::same_path(&t.path, &path) && t.preview.is_none());
            match open {
                Some(index) => {
                    let tab = &mut self.tabs[index];
                    let was_saved = !tab.buf.is_dirty();
                    let buf = &mut tab.buf;
                    let edits = edits
                        .iter()
                        .map(|e| (pos_in(buf, e.start), pos_in(buf, e.end), e.text.clone()))
                        .collect();
                    buf.apply_edits(edits);
                    // A change across files leaves saved files saved, so they agree with
                    // the ones changed on disk.
                    if was_saved && files > 1 {
                        self.write_tab(index);
                    }
                }
                None => self.request_then(
                    Request::ReadFile { path: path.clone() },
                    Pending::LspEdit { path, edits },
                ),
            }
        }
        self.status = if files > 1 {
            format!("{what} in {files} files")
        } else {
            what
        };
    }

    /// A file a rename reaches that isn't open: changed and written back.
    pub(super) fn edit_file_on_disk(
        &mut self,
        path: String,
        edits: Vec<lsp::TextEdit>,
        data: Vec<u8>,
    ) {
        let Ok(text) = String::from_utf8(data) else {
            self.status = format!("{} isn't text; it was left as it was", file_name(&path));
            return;
        };
        let data = lsp::apply_to_text(&text, &edits).into_bytes();
        let refresh = parent(&path).to_string();
        self.request_then(Request::WriteFile { path, data }, Pending::FsOp { refresh });
    }

    /// The problems side tab: every problem the language servers found, by file.
    pub(super) fn problems_panel(&mut self, ui: &mut Ui) {
        let mut files: Vec<(&String, &Vec<Diag>)> = self
            .lsp
            .all_diags()
            .filter(|(_, d)| d.iter().any(|d| d.severity <= 3))
            .collect();
        files.sort_by_key(|(path, diags)| {
            (
                diags.iter().map(|d| d.severity).min().unwrap_or(4),
                (*path).clone(),
            )
        });
        if files.is_empty() {
            ui.add_space(12.0);
            let text = if self.lsp.servers.is_empty() {
                "Problems the language servers find show here, once a file they know is open."
            } else {
                "No problems found in the open files."
            };
            ui.label(RichText::new(text).small().color(theme::COMMENT()));
            return;
        }
        let prefix = format!("{}/", lsp::key(self.root.trim_end_matches('/')));
        let mut goto = None;
        ScrollArea::vertical()
            .id_salt("problems")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                for (path, diags) in files {
                    let shown = path.strip_prefix(&prefix).unwrap_or(path);
                    let (icon, color) = icons::for_entry(file_name(path), false, false);
                    let count = diags.iter().filter(|d| d.severity <= 3).count();
                    ui.add_space(4.0);
                    ui.label(pieces(&[
                        (&format!("{icon}  "), color),
                        (shown, theme::TEXT()),
                        (&format!("  {count}"), theme::COMMENT()),
                    ]));
                    let mut sorted: Vec<&Diag> = diags.iter().filter(|d| d.severity <= 3).collect();
                    sorted.sort_by_key(|d| (d.severity, d.start));
                    for diag in sorted {
                        let message = diag.message.lines().next().unwrap_or("");
                        let label = pieces(&[
                            (
                                &format!("  {}  ", severity_icon(diag.severity)),
                                severity_color(diag.severity),
                            ),
                            (message, theme::TEXT()),
                            (
                                &format!(
                                    "  {} [{}:{}]",
                                    diag.source,
                                    diag.start.line + 1,
                                    diag.start.ch + 1
                                ),
                                theme::COMMENT(),
                            ),
                        ]);
                        let row = ui
                            .add(egui::Button::selectable(false, label).truncate())
                            .on_hover_text(&diag.message);
                        if row.clicked() {
                            goto = Some(Location {
                                path: path.clone(),
                                start: diag.start,
                                end: diag.end,
                            });
                        }
                    }
                }
            });
        if let Some(location) = goto {
            // A problem's path is the server's: an open tab is found by it, whatever its case.
            let tab = self
                .tabs
                .iter()
                .find(|t| lsp::key(&t.path) == location.path)
                .map(|t| t.path.clone());
            let location = Location {
                path: tab.unwrap_or(location.path),
                ..location
            };
            self.goto_location(&location);
        }
    }

    /// The problems in the open files, for the status bar: errors and warnings.
    pub(super) fn problem_counts(&self) -> (usize, usize) {
        let mut counts = (0, 0);
        for (_, diags) in self.lsp.all_diags() {
            for diag in diags {
                match diag.severity {
                    1 => counts.0 += 1,
                    2 => counts.1 += 1,
                    _ => {}
                }
            }
        }
        counts
    }

    /// What a language server is busy with, for the status bar.
    pub(super) fn intel_busy(&self) -> Option<String> {
        self.lsp.servers.iter().find_map(|s| match &s.state {
            lsp::State::Starting => Some(format!("Starting the {} language server…", s.name)),
            lsp::State::Ready => s.busy().map(|b| format!("{}: {b}", s.name)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_are_ranked_by_how_they_match() {
        assert_eq!(score("value", "val"), Some(0));
        assert_eq!(score("Value", "val"), Some(1));
        assert_eq!(score("getValue", "gv"), Some(2));
        assert_eq!(score("abc", "x"), None);
        let item = |label: &str, sort: &str| Item {
            label: label.into(),
            filter: label.into(),
            sort: sort.into(),
            ..Default::default()
        };
        let mut completion = Completion {
            items: vec![
                item("getValue", "1"),
                item("value", "2"),
                item("Values", "0"),
            ],
            shown: Vec::new(),
            selected: 0,
            start: Pos::default(),
            incomplete: false,
            filtered_for: None,
            scroll: false,
            resolving: None,
        };
        refilter(&mut completion, "val");
        // The exact prefix first, then the prefix in another case, then letters in order.
        assert_eq!(completion.shown, [1, 2, 0]);
    }

    #[test]
    fn words_are_found_around_the_cursor() {
        let buf = Buffer::new("let $foo_bar = 1;");
        assert_eq!(word_start(&buf, Pos::new(0, 8)), Pos::new(0, 4));
        let (start, end, word) = word_at(&buf, Pos::new(0, 6)).unwrap();
        assert_eq!((start.col, end.col, word.as_str()), (4, 12, "$foo_bar"));
        assert!(word_at(&buf, Pos::new(0, 13)).is_none());
    }
}
