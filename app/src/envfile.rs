//! `.env` files as a table of keys and values, the values hidden until asked for. The
//! table edits the tab's text directly, keeping comments, blank lines and order, so
//! saving, undo and "Edit as text" work as for any file.

use super::*;

/// Whether a file is a dotenv file: `.env`, `.env.local`, `production.env`…
pub(super) fn is_env_file(path: &str) -> bool {
    let name = file_name(path).to_lowercase();
    name == ".env" || name.starts_with(".env.") || name.ends_with(".env")
}

#[derive(Clone, PartialEq, Debug)]
enum Line {
    /// A comment, a blank line, or anything that isn't `KEY=value`, kept as it is.
    Other(String),
    Entry {
        export: bool,
        key: String,
        value: String,
        /// The quote the value was written in, if any.
        quote: Option<char>,
    },
}

fn parse(text: &str) -> Vec<Line> {
    text.split('\n')
        .map(|raw| {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return Line::Other(line.to_string());
            }
            let (export, body) = match trimmed.strip_prefix("export ") {
                Some(rest) => (true, rest.trim_start()),
                None => (false, trimmed),
            };
            let Some((key, value)) = body.split_once('=') else {
                return Line::Other(line.to_string());
            };
            let key = key.trim();
            if key.is_empty() || key.contains(char::is_whitespace) {
                return Line::Other(line.to_string());
            }
            let value = value.trim();
            let quote = value
                .chars()
                .next()
                .filter(|q| (*q == '"' || *q == '\'') && value.len() >= 2 && value.ends_with(*q));
            let value = match quote {
                Some(_) => value[1..value.len() - 1].to_string(),
                None => value.to_string(),
            };
            Line::Entry {
                export,
                key: key.to_string(),
                value,
                quote,
            }
        })
        .collect()
}

/// The variables of a dotenv file, values unquoted.
pub(super) fn pairs(text: &str) -> Vec<(String, String)> {
    parse(text)
        .into_iter()
        .filter_map(|line| match line {
            Line::Entry { key, value, .. } => Some((key, value)),
            Line::Other(_) => None,
        })
        .collect()
}

fn write(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| match line {
            Line::Other(text) => text.clone(),
            Line::Entry {
                export,
                key,
                value,
                quote,
            } => {
                // A value that would be misread bare gets quotes.
                let needs = value.contains([' ', '#', '"', '\'']) || value.contains('\t');
                let quote = quote.or(needs.then_some('"'));
                let value = match quote {
                    Some(q) => format!("{q}{value}{q}"),
                    None => value.clone(),
                };
                format!("{}{key}={value}", if *export { "export " } else { "" })
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The table, editing the tab's text.
pub(super) fn show(ui: &mut Ui, tab: &mut Tab) {
    let text = tab.buf.text();
    let mut lines = parse(&text);
    let view = &mut tab.view;
    let count = lines
        .iter()
        .filter(|l| matches!(l, Line::Entry { .. }))
        .count();
    let mut changed = false;
    let mut remove = None;

    Frame::NONE
        .inner_margin(Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(pieces(&[
                    (&format!("{}  ", icons::KEY), theme::COMMENT()),
                    (&format!("{count} variables"), theme::STRONG()),
                ]));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Edit as text").clicked() {
                        view.env_as_text = true;
                    }
                    if ui.button(format!("{}  Add", icons::ADD)).clicked() {
                        lines.push(Line::Entry {
                            export: false,
                            key: "NEW_KEY".into(),
                            value: String::new(),
                            quote: None,
                        });
                        view.env_revealed.insert(lines.len() - 1);
                        changed = true;
                    }
                    let label = if view.env_show_all {
                        "Hide values"
                    } else {
                        "Show values"
                    };
                    if ui.button(format!("{}  {label}", icons::EYE)).clicked() {
                        view.env_show_all = !view.env_show_all;
                    }
                });
            });
            ui.add_space(8.0);
            let mono = egui::TextStyle::Monospace;
            ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for (index, line) in lines.iter_mut().enumerate() {
                    match line {
                        Line::Other(text) => {
                            if !text.trim().is_empty() {
                                ui.label(
                                    RichText::new(text.as_str())
                                        .monospace()
                                        .color(theme::COMMENT()),
                                );
                            }
                        }
                        Line::Entry { key, value, .. } => {
                            ui.horizontal(|ui| {
                                let height = ui.spacing().interact_size.y;
                                changed |= ui
                                    .add_sized(
                                        [240.0, height],
                                        TextEdit::singleline(key)
                                            .font(mono.clone())
                                            .clip_text(true)
                                            .text_color(theme::FUNC()),
                                    )
                                    .changed();
                                ui.label(RichText::new("=").monospace().color(theme::COMMENT()));
                                let shown = view.env_show_all || view.env_revealed.contains(&index);
                                // The buttons are placed from the right first; the value takes
                                // what is left, and a long one scrolls inside its box.
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if icon_button(ui, icons::TRASH, "Remove this variable") {
                                        remove = Some(index);
                                    }
                                    let eye = if shown { "Hide" } else { "Show" };
                                    if icon_button(ui, icons::EYE, eye)
                                        && !view.env_revealed.remove(&index)
                                    {
                                        view.env_revealed.insert(index);
                                    }
                                    let edit = ui.add_sized(
                                        [ui.available_width(), height],
                                        TextEdit::singleline(value)
                                            .font(mono.clone())
                                            .clip_text(true)
                                            .password(!shown)
                                            .hint_text("empty"),
                                    );
                                    changed |= edit.changed();
                                    // Typing into a hidden value shows it, so you see what you type.
                                    if edit.has_focus() && !shown {
                                        view.env_revealed.insert(index);
                                    }
                                });
                            });
                        }
                    }
                }
                if count == 0 {
                    ui.label(
                        RichText::new("No variables yet. Add one, or edit the file as text.")
                            .color(theme::COMMENT()),
                    );
                }
            });
        });

    if let Some(index) = remove {
        lines.remove(index);
        view.env_revealed.clear();
        changed = true;
    }
    if changed {
        let mut new = write(&lines);
        // Windows line ends stay Windows line ends.
        if text.contains("\r\n") {
            new = new.replace('\n', "\r\n");
        }
        if new != text {
            // One edit for the whole text, so undo and saving work as in the editor.
            let cursor = tab.buf.cursor;
            tab.buf.select_all();
            tab.buf.insert(&new);
            tab.buf.set_cursor(cursor, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_files_are_recognised() {
        assert!(is_env_file("/srv/app/.env"));
        assert!(is_env_file("/srv/app/.env.local"));
        assert!(is_env_file("/srv/app/production.env"));
        assert!(!is_env_file("/srv/app/environment.ts"));
    }

    #[test]
    fn a_file_comes_back_as_it_was() {
        let text =
            "# database\nDB_HOST=localhost\nexport DB_PASS=\"p a ss\"\n\nTOKEN='x#y'\nnot a pair\n";
        let lines = parse(text);
        assert_eq!(write(&lines), text);
        assert_eq!(
            lines[2],
            Line::Entry {
                export: true,
                key: "DB_PASS".into(),
                value: "p a ss".into(),
                quote: Some('"'),
            }
        );
    }

    #[test]
    fn values_that_need_quotes_get_them() {
        let mut lines = parse("A=1");
        if let Line::Entry { value, .. } = &mut lines[0] {
            *value = "two words".into();
        }
        assert_eq!(write(&lines), "A=\"two words\"");
    }
}
