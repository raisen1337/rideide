//! Markdown as Claude writes it, drawn in the chat: headings, lists (numbered, nested,
//! with checkboxes), bold, italic, struck-through and code spans, links that open in the
//! browser, quotes, rules, tables and code blocks.

use crate::{icons, theme, tools};
use eframe::egui::{
    self, text::LayoutJob, Align, FontId, Frame, Layout, Margin, ScrollArea, Sense, Stroke,
    TextFormat, Ui,
};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// A run of styled text, and the links in it (by character range).
#[derive(Default)]
struct Inline {
    job: LayoutJob,
    chars: usize,
    links: Vec<(std::ops::Range<usize>, String)>,
}

impl Inline {
    fn is_empty(&self) -> bool {
        self.job.text.trim().is_empty()
    }
}

struct Table {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<Inline>>,
    row: Vec<Inline>,
    head: bool,
}

#[derive(Default)]
struct State {
    bold: u32,
    italic: u32,
    strike: u32,
    heading: Option<HeadingLevel>,
    link: Option<(usize, String)>,
    /// The next number of each list open, `None` for a bulleted one.
    lists: Vec<Option<u64>>,
    /// The marker of the list item whose first line hasn't been drawn yet.
    marker: Option<String>,
    quote: u32,
    code: Option<String>,
    table: Option<Table>,
    blocks: usize,
}

const BODY: f32 = 14.0;

fn heading_size(level: HeadingLevel) -> f32 {
    match level {
        HeadingLevel::H1 => 20.0,
        HeadingLevel::H2 => 17.5,
        HeadingLevel::H3 => 15.5,
        _ => 14.5,
    }
}

impl State {
    fn format(&self, code: bool) -> TextFormat {
        if code {
            return TextFormat {
                background: theme::tint(20),
                ..TextFormat::simple(FontId::monospace(BODY - 1.0), theme::TYPE())
            };
        }
        let size = self.heading.map_or(BODY, heading_size);
        let strong =
            self.bold > 0 || self.heading.is_some() || self.table.as_ref().is_some_and(|t| t.head);
        let color = if self.link.is_some() {
            theme::ACCENT()
        } else if strong {
            theme::STRONG()
        } else if self.quote > 0 {
            theme::COMMENT()
        } else {
            theme::TEXT()
        };
        let mut format = TextFormat::simple(FontId::proportional(size), color);
        format.italics = self.italic > 0;
        if self.strike > 0 {
            format.strikethrough = Stroke::new(1.0, color);
        }
        if self.link.is_some() {
            format.underline = Stroke::new(1.0, theme::ACCENT().gamma_multiply(0.6));
        }
        format
    }
}

fn append(inline: &mut Inline, text: &str, format: TextFormat) {
    inline.job.append(text, 0.0, format);
    inline.chars += text.chars().count();
}

/// Draws a run of text, wrapped; its links open in the browser on a click.
fn show_inline(ui: &mut Ui, inline: Inline) {
    let Inline { job, links, .. } = inline;
    let label = egui::Label::new(job).wrap().sense(Sense::click());
    let (pos, galley, resp) = label.layout_in_ui(ui);
    ui.painter()
        .add(egui::Shape::galley(pos, galley.clone(), theme::TEXT()));
    if links.is_empty() {
        return;
    }
    let link_at = |point: egui::Pos2| {
        let index = galley.cursor_from_pos(point - pos).index.0;
        links
            .iter()
            .find(|(range, _)| range.contains(&index))
            .map(|(_, url)| url.clone())
    };
    if let Some(url) = resp.hover_pos().and_then(link_at) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        resp.clone().on_hover_text(&url);
        if resp.clicked() {
            tools::open_in_browser(&url);
        }
    }
}

impl State {
    /// Puts the text gathered so far on screen: as a paragraph, a heading or a list item.
    fn flush(&mut self, ui: &mut Ui, inline: &mut Inline) {
        let inline = std::mem::take(inline);
        if inline.is_empty() {
            return;
        }
        let depth = self.lists.len().saturating_sub(1) as f32;
        let marker = self.marker.take();
        let quote = self.quote;
        let resp = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if quote > 0 {
                    ui.add_space(14.0 * quote as f32);
                }
                if !self.lists.is_empty() {
                    ui.add_space(4.0 + depth * 18.0);
                    // The marker in its own column, so wrapped lines line up after it.
                    let marker = marker.unwrap_or_default();
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(18.0, BODY + 4.0), Sense::hover());
                    if !marker.is_empty() {
                        ui.painter().text(
                            rect.left_top() + egui::vec2(0.0, 1.0),
                            egui::Align2::LEFT_TOP,
                            marker,
                            FontId::proportional(BODY),
                            theme::COMMENT(),
                        );
                    }
                }
                ui.vertical(|ui| show_inline(ui, inline));
            })
            .response;
        if quote > 0 {
            let x = resp.rect.left() + 3.0;
            ui.painter().line_segment(
                [
                    egui::pos2(x, resp.rect.top()),
                    egui::pos2(x, resp.rect.bottom()),
                ],
                Stroke::new(3.0, theme::tint(40)),
            );
        }
    }
}

fn show_table(ui: &mut Ui, table: Table, salt: (usize, usize)) {
    Frame::NONE
        .stroke(Stroke::new(1.0, theme::tint(16)))
        .corner_radius(8)
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ScrollArea::horizontal()
                .id_salt(("table", salt))
                .show(ui, |ui| {
                    egui::Grid::new(("table_grid", salt))
                        .striped(true)
                        .spacing([18.0, 6.0])
                        .show(ui, |ui| {
                            for row in table.rows {
                                for (column, cell) in row.into_iter().enumerate() {
                                    let align = match table.aligns.get(column) {
                                        Some(Alignment::Right) => Align::Max,
                                        Some(Alignment::Center) => Align::Center,
                                        _ => Align::Min,
                                    };
                                    ui.with_layout(Layout::top_down(align), |ui| {
                                        ui.set_max_width(360.0);
                                        show_inline(ui, cell);
                                    });
                                }
                                ui.end_row();
                            }
                        });
                });
        });
}

/// Draws `text` as markdown. `code_block` draws a fenced block (with its copy button);
/// `salt` keeps the ids of what is drawn apart from other messages'.
pub(crate) fn render(
    ui: &mut Ui,
    text: &str,
    salt: usize,
    code_block: &dyn Fn(&mut Ui, &str, (usize, usize)),
) {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut state = State::default();
    let mut inline = Inline::default();
    ui.spacing_mut().item_spacing.y = 6.0;
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    state.flush(ui, &mut inline);
                    state.heading = Some(level);
                    ui.add_space(4.0);
                }
                Tag::BlockQuote(_) => {
                    state.flush(ui, &mut inline);
                    state.quote += 1;
                }
                Tag::CodeBlock(kind) => {
                    state.flush(ui, &mut inline);
                    let _: CodeBlockKind = kind;
                    state.code = Some(String::new());
                }
                Tag::List(first) => {
                    state.flush(ui, &mut inline);
                    state.lists.push(first);
                }
                Tag::Item => {
                    state.flush(ui, &mut inline);
                    let marker = match state.lists.last_mut() {
                        Some(Some(next)) => {
                            let marker = format!("{next}.");
                            *next += 1;
                            marker
                        }
                        _ => "•".to_string(),
                    };
                    state.marker = Some(marker);
                }
                Tag::Emphasis => state.italic += 1,
                Tag::Strong => state.bold += 1,
                Tag::Strikethrough => state.strike += 1,
                Tag::Link { dest_url, .. } => {
                    state.link = Some((inline.chars, dest_url.to_string()))
                }
                Tag::Image { dest_url, .. } => {
                    state.link = Some((inline.chars, dest_url.to_string()))
                }
                Tag::Table(aligns) => {
                    state.flush(ui, &mut inline);
                    state.table = Some(Table {
                        aligns,
                        rows: Vec::new(),
                        row: Vec::new(),
                        head: false,
                    });
                }
                Tag::TableHead => {
                    if let Some(table) = &mut state.table {
                        table.head = true;
                    }
                }
                Tag::TableRow | Tag::TableCell => {}
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph => {
                    // In a list, an item's paragraphs stay in the item.
                    state.flush(ui, &mut inline);
                }
                TagEnd::Heading(_) => {
                    state.flush(ui, &mut inline);
                    state.heading = None;
                }
                TagEnd::BlockQuote(_) => {
                    state.flush(ui, &mut inline);
                    state.quote = state.quote.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    if let Some(code) = state.code.take() {
                        code_block(ui, &code, (salt, state.blocks));
                        state.blocks += 1;
                    }
                }
                TagEnd::List(_) => {
                    state.flush(ui, &mut inline);
                    state.lists.pop();
                }
                TagEnd::Item => state.flush(ui, &mut inline),
                TagEnd::Emphasis => state.italic = state.italic.saturating_sub(1),
                TagEnd::Strong => state.bold = state.bold.saturating_sub(1),
                TagEnd::Strikethrough => state.strike = state.strike.saturating_sub(1),
                TagEnd::Link | TagEnd::Image => {
                    if let Some((start, url)) = state.link.take() {
                        inline.links.push((start..inline.chars, url));
                    }
                }
                TagEnd::TableCell => {
                    let cell = std::mem::take(&mut inline);
                    if let Some(table) = &mut state.table {
                        table.row.push(cell);
                    }
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some(table) = &mut state.table {
                        let row = std::mem::take(&mut table.row);
                        table.rows.push(row);
                        table.head = false;
                    }
                }
                TagEnd::Table => {
                    if let Some(table) = state.table.take() {
                        show_table(ui, table, (salt, state.blocks));
                        state.blocks += 1;
                    }
                }
                _ => {}
            },
            Event::Text(text) => match &mut state.code {
                Some(code) => code.push_str(&text),
                None => {
                    let format = state.format(false);
                    append(&mut inline, &text, format);
                }
            },
            Event::Code(text) => {
                let format = state.format(true);
                append(&mut inline, &text, format);
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                let format = state.format(false);
                append(&mut inline, &text, format);
            }
            // A line break in a message is kept, as chats do.
            Event::SoftBreak | Event::HardBreak => {
                let format = state.format(false);
                append(&mut inline, "\n", format);
            }
            Event::Rule => {
                state.flush(ui, &mut inline);
                ui.separator();
            }
            Event::TaskListMarker(done) => {
                // The box takes the place of the bullet.
                let mark = if done {
                    icons::CHECK.to_string()
                } else {
                    "○".to_string()
                };
                state.marker = Some(mark);
            }
            _ => {}
        }
    }
    // A code block still streaming in, or text after the last block.
    if let Some(code) = state.code.take() {
        code_block(ui, &code, (salt, state.blocks));
    }
    if let Some(table) = state.table.take() {
        show_table(ui, table, (salt, state.blocks));
    }
    state.flush(ui, &mut inline);
}
