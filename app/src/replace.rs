//! Replace in the whole folder: what the folder search found, changed in every file at
//! once. Open files are changed in their tabs (an undo takes it back); the others are read,
//! changed and written back on the host.

use super::*;

/// What a replacement is: the search as it was when it was asked for, and the new text.
#[derive(Clone, Debug)]
pub(super) struct Replacement {
    pattern: String,
    regex: bool,
    case_sensitive: bool,
    with: String,
}

impl Replacement {
    fn compile(&self) -> Option<regex::Regex> {
        let pattern = if self.regex {
            self.pattern.clone()
        } else {
            regex::escape(&self.pattern)
        };
        regex::RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            .build()
            .ok()
    }

    /// What one match turns into: `$1` and the like name its groups when the search is a
    /// regular expression; otherwise the text goes in as typed.
    fn for_match(&self, caps: &regex::Captures) -> String {
        if self.regex {
            let mut out = String::new();
            caps.expand(&self.with, &mut out);
            out
        } else {
            self.with.clone()
        }
    }

    /// `text` with every match replaced, and how many there were.
    fn apply(&self, re: &regex::Regex, text: &str) -> (String, usize) {
        let mut count = 0;
        let new = re.replace_all(text, |caps: &regex::Captures| {
            count += 1;
            self.for_match(caps)
        });
        (new.into_owned(), count)
    }
}

/// A result line as it would read after the replacement: the text before the match, the
/// match, what replaces it, and the rest.
pub(super) fn preview(
    re: &regex::Regex,
    replacement: &str,
    regex: bool,
    text: &str,
    col: usize,
) -> Option<(String, String, String, String)> {
    let from = text.char_indices().nth(col).map_or(text.len(), |(i, _)| i);
    let caps = re.captures_at(text, from)?;
    let found = caps.get(0)?;
    let new = Replacement {
        pattern: String::new(),
        regex,
        case_sensitive: true,
        with: replacement.to_string(),
    }
    .for_match(&caps);
    Some((
        text[..found.start()].to_string(),
        found.as_str().to_string(),
        new,
        text[found.end()..].to_string(),
    ))
}

impl App {
    /// The folder search as a regular expression, matched the way the host matched it.
    pub(super) fn search_regex(&mut self) -> Option<regex::Regex> {
        let key = (
            self.search.query.clone(),
            self.search.regex,
            self.search.case_sensitive,
        );
        if self.search.compiled.as_ref().map(|(k, _)| k) != Some(&key) {
            let replacement = self.replacement();
            self.search.compiled = replacement.compile().map(|re| (key, re));
        }
        self.search.compiled.as_ref().map(|(_, re)| re.clone())
    }

    fn replacement(&self) -> Replacement {
        Replacement {
            pattern: self.search.query.clone(),
            regex: self.search.regex,
            case_sensitive: self.search.case_sensitive,
            with: self.search.replace.clone(),
        }
    }

    /// Asks before replacing in every file the search found.
    pub(super) fn ask_replace_all(&mut self) {
        let mut paths: Vec<String> = Vec::new();
        for hit in &self.search.hits {
            if !paths.contains(&hit.path) {
                paths.push(hit.path.clone());
            }
        }
        if paths.is_empty() {
            return;
        }
        let lines = self.search.hits.len();
        self.dialog = Some(Dialog::ConfirmReplace { paths, lines });
    }

    /// Replaces the search's matches in these files.
    pub(super) fn replace_in(&mut self, paths: Vec<String>) {
        let replacement = self.replacement();
        let Some(re) = replacement.compile() else {
            self.status = "The search isn't a valid regular expression".into();
            return;
        };
        self.search.replaced = (0, 0);
        for path in &paths {
            let open = self
                .tabs
                .iter()
                .position(|t| &t.path == path && t.preview.is_none());
            match open {
                Some(index) => {
                    let count = self.replace_in_tab(index, &re, &replacement);
                    if count > 0 {
                        self.search.replaced.0 += count;
                        self.search.replaced.1 += 1;
                    }
                }
                None => self.request_then(
                    Request::ReadFile { path: path.clone() },
                    Pending::ReplaceIn {
                        path: path.clone(),
                        replacement: replacement.clone(),
                    },
                ),
            }
        }
        self.search.hits.retain(|h| !paths.contains(&h.path));
        self.replace_status();
    }

    /// Replaces in an open tab, as one undo step; a tab that was saved is saved again.
    fn replace_in_tab(
        &mut self,
        index: usize,
        re: &regex::Regex,
        replacement: &Replacement,
    ) -> usize {
        let tab = &mut self.tabs[index];
        let text = tab.buf.plain_text();
        let mut edits = Vec::new();
        // Matches as char offsets, counted along as they come in order.
        let (mut byte, mut chars) = (0, 0);
        let mut char_at = |to: usize| {
            chars += text[byte..to].chars().count();
            byte = to;
            chars
        };
        for caps in re.captures_iter(&text) {
            let found = caps.get(0).unwrap();
            if found.is_empty() {
                continue;
            }
            let start = char_at(found.start());
            let end = char_at(found.end());
            edits.push((start, end, replacement.for_match(&caps)));
        }
        let count = edits.len();
        if count == 0 {
            return 0;
        }
        let was_saved = !tab.buf.is_dirty();
        let buf = &mut tab.buf;
        let edits = edits
            .into_iter()
            .map(|(a, b, new)| (buf.pos_at(a), buf.pos_at(b), new))
            .collect();
        buf.apply_edits(edits);
        if was_saved {
            self.write_tab(index);
        }
        count
    }

    /// A file that wasn't open, read to have its matches replaced.
    pub(super) fn replace_in_file(
        &mut self,
        path: String,
        replacement: Replacement,
        data: Vec<u8>,
    ) {
        let Ok(text) = String::from_utf8(data) else {
            return;
        };
        let Some(re) = replacement.compile() else {
            return;
        };
        let (new, count) = replacement.apply(&re, &text);
        if count == 0 {
            return;
        }
        self.search.replaced.0 += count;
        self.search.replaced.1 += 1;
        let refresh = parent(&path).to_string();
        self.request_then(
            Request::WriteFile {
                path,
                data: new.into_bytes(),
            },
            Pending::FsOp { refresh },
        );
        self.replace_status();
    }

    fn replace_status(&mut self) {
        let (matches, files) = self.search.replaced;
        let plural = |n: usize, one: &str, many: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        };
        self.status = format!(
            "Replaced {} in {}",
            plural(matches, "match", "matches"),
            plural(files, "file", "files")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replacement(pattern: &str, regex: bool, with: &str) -> Replacement {
        Replacement {
            pattern: pattern.into(),
            regex,
            case_sensitive: false,
            with: with.into(),
        }
    }

    #[test]
    fn plain_text_replaces_as_typed() {
        let r = replacement("a.b", false, "$1");
        let re = r.compile().unwrap();
        assert_eq!(r.apply(&re, "a.b axb A.B"), ("$1 axb $1".to_string(), 2));
    }

    #[test]
    fn regular_expressions_fill_in_their_groups() {
        let r = replacement(r"(\w+)\.get\((\w+)\)", true, "$1[$2]");
        let re = r.compile().unwrap();
        let (text, count) = r.apply(&re, "x = cfg.get(key) + cfg.get(b)");
        assert_eq!((text.as_str(), count), ("x = cfg[key] + cfg[b]", 2));
        let shown = preview(&re, "$1[$2]", true, "  y = cfg.get(key);", 6).unwrap();
        assert_eq!(
            shown,
            (
                "  y = ".into(),
                "cfg.get(key)".into(),
                "cfg[key]".into(),
                ";".into()
            )
        );
    }
}
