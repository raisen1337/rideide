//! Finding your way in a file: the breadcrumbs above the code and the outline of its
//! functions, classes and types.

use super::*;
use outline::{Kind, Symbol};

pub(super) fn symbol_icon(kind: Kind) -> (char, Color32) {
    match kind {
        Kind::Function => (icons::METHOD, theme::FUNC()),
        Kind::Class => (icons::CLASS, theme::TYPE()),
        Kind::Type => (icons::STRUCTURE, theme::TYPE()),
    }
}

/// How deep each symbol is nested, counted in symbols around it rather than in spaces.
fn levels(symbols: &[Symbol]) -> Vec<usize> {
    let mut open: Vec<&Symbol> = Vec::new();
    symbols
        .iter()
        .map(|symbol| {
            open.retain(|outer| outer.end >= symbol.line && outer.depth < symbol.depth);
            let level = open.len();
            open.push(symbol);
            level
        })
        .collect()
}

/// Puts the cursor on a line of a tab and brings it into view.
pub(super) fn jump(tab: &mut Tab, line: usize) {
    let line = line.min(tab.buf.line_count().saturating_sub(1));
    let col = tab
        .buf
        .line(line)
        .chars()
        .take_while(|c| c.is_whitespace())
        .count();
    tab.buf.set_cursor(Pos::new(line, col), false);
    tab.view.reveal_cursor = true;
    tab.view.request_focus = true;
}

/// The bar above the code: the file's folders, its name, and the symbols the cursor is in.
/// Each symbol opens a list of all of them, to jump to another.
pub(super) fn breadcrumbs(ui: &mut Ui, tab: &mut Tab, root: &str) {
    let relative = tab
        .path
        .strip_prefix(root)
        .unwrap_or(&tab.path)
        .trim_start_matches('/');
    let mut jump_to = None;
    Frame::NONE
        .inner_margin(Margin {
            left: 12,
            right: 8,
            top: 3,
            bottom: 3,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                let separator = |ui: &mut Ui| {
                    ui.label(
                        RichText::new(icons::CHEVRON_RIGHT)
                            .small()
                            .color(theme::COMMENT()),
                    );
                };
                let parts: Vec<&str> = relative.split('/').filter(|p| !p.is_empty()).collect();
                for (index, part) in parts.iter().enumerate() {
                    if index > 0 {
                        separator(ui);
                    }
                    let last = index + 1 == parts.len();
                    let color = if last {
                        theme::TEXT()
                    } else {
                        theme::COMMENT()
                    };
                    ui.add(
                        egui::Label::new(RichText::new(*part).small().color(color))
                            .selectable(false),
                    );
                }
                let chain: Vec<Symbol> = outline::enclosing(&tab.view.symbols, tab.buf.cursor.line)
                    .into_iter()
                    .cloned()
                    .collect();
                let all = &tab.view.symbols;
                let shown: Vec<Symbol> = if chain.is_empty() && !all.is_empty() {
                    // Outside every symbol, the list is still a click away.
                    Vec::new()
                } else {
                    chain
                };
                for symbol in &shown {
                    separator(ui);
                    let (icon, color) = symbol_icon(symbol.kind);
                    let label =
                        pieces(&[(&format!("{icon} "), color), (&symbol.name, theme::TEXT())]);
                    ui.menu_button(label, |ui| symbol_menu(ui, all, &mut jump_to));
                }
                if shown.is_empty() && !all.is_empty() {
                    separator(ui);
                    let label = RichText::new("…").small().color(theme::COMMENT());
                    ui.menu_button(label, |ui| symbol_menu(ui, all, &mut jump_to))
                        .response
                        .on_hover_text("Functions and types in this file");
                }
                // Who else is editing this file live, at the right.
                if !tab.view.peers.is_empty() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        for mark in &tab.view.peers {
                            let size = vec2(18.0, 18.0);
                            let resp = match mark.avatar {
                                Some(texture) => ui.add(
                                    egui::Image::new(egui::load::SizedTexture::new(texture, size))
                                        .corner_radius(9),
                                ),
                                None => {
                                    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
                                    ui.painter().circle_filled(rect.center(), 9.0, mark.color);
                                    resp
                                }
                            };
                            // A ring in their cursor's colour ties the face to the cursor.
                            ui.painter()
                                .circle_stroke(resp.rect.center(), 9.5, (1.5, mark.color));
                            resp.on_hover_text(format!("{} is editing this file", mark.name));
                        }
                    });
                }
            });
        });
    if let Some(line) = jump_to {
        jump(tab, line);
    }
}

fn symbol_menu(ui: &mut Ui, symbols: &[Symbol], jump_to: &mut Option<usize>) {
    ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
        for (symbol, level) in symbols.iter().zip(levels(symbols)) {
            let (icon, color) = symbol_icon(symbol.kind);
            let label = pieces(&[
                (&" ".repeat(level * 3), theme::TEXT()),
                (&format!("{icon}  "), color),
                (&symbol.name, theme::TEXT()),
            ]);
            if ui.button(label).clicked() {
                *jump_to = Some(symbol.line);
                ui.close();
            }
        }
    });
}

impl App {
    /// The outline side tab: the open file's functions, classes and types, nested as they
    /// are in the code, with the one the cursor is in marked. A click goes there.
    pub(super) fn outline_panel(&mut self, ui: &mut Ui) {
        let Some(index) = self
            .docked_active()
            .filter(|&i| self.tabs[i].preview.is_none())
        else {
            ui.add_space(8.0);
            ui.label(RichText::new("Open a file to see its outline.").color(theme::COMMENT()));
            return;
        };
        Frame::NONE
            .fill(theme::tint(8))
            .corner_radius(6)
            .inner_margin(Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::SEARCH).color(theme::COMMENT()));
                    ui.add(
                        TextEdit::singleline(&mut self.outline_filter)
                            .desired_width(f32::INFINITY)
                            .frame(Frame::NONE)
                            .hint_text("Filter"),
                    );
                });
            });
        ui.add_space(4.0);
        let tab = &self.tabs[index];
        let current = outline::enclosing(&tab.view.symbols, tab.buf.cursor.line)
            .last()
            .map(|s| s.line);
        let filter = self.outline_filter.to_lowercase();
        let symbols = tab.view.symbols.clone();
        if symbols.is_empty() {
            ui.label(
                RichText::new("No functions or types found in this file.").color(theme::COMMENT()),
            );
            return;
        }
        let mut jump_to = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (symbol, level) in symbols.iter().zip(levels(&symbols)) {
                if !filter.is_empty() && !symbol.name.to_lowercase().contains(&filter) {
                    continue;
                }
                let (icon, color) = symbol_icon(symbol.kind);
                let indent = if filter.is_empty() {
                    level as f32 * 14.0
                } else {
                    0.0
                };
                ui.horizontal(|ui| {
                    ui.add_space(indent);
                    let label =
                        pieces(&[(&format!("{icon}  "), color), (&symbol.name, theme::TEXT())]);
                    let resp = ui.selectable_label(current == Some(symbol.line), label);
                    if resp
                        .on_hover_text(format!("line {}", symbol.line + 1))
                        .clicked()
                    {
                        jump_to = Some(symbol.line);
                    }
                });
            }
        });
        if let Some(line) = jump_to {
            jump(&mut self.tabs[index], line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_is_counted_in_symbols() {
        let text = "class A {\n    run() {\n    }\n    stop() {\n    }\n}\nfunction b() {\n}";
        let lines: Vec<&str> = text.lines().collect();
        let symbols = outline::symbols(&lines);
        assert_eq!(levels(&symbols), [0, 1, 1, 0]);
    }
}
