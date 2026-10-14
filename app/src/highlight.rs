//! A small hand-written lexer for syntax colouring. It works a line at a time and
//! carries a `State` across lines for block comments and multi-line strings.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Keyword,
    Type,
    Func,
    Str,
    Number,
    Comment,
    Punct,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum State {
    #[default]
    Normal,
    BlockComment,
    /// Inside the multi-line string opened by `Lang::multiline[n]`.
    Str(u8),
}

pub struct Lang {
    pub line_comment: &'static [&'static str],
    pub block_comment: Option<(&'static str, &'static str)>,
    pub quotes: &'static [char],
    /// String delimiters that may span lines.
    pub multiline: &'static [&'static str],
    /// `'a` is a lifetime, not the start of a char literal.
    pub lifetimes: bool,
    pub keywords: &'static [&'static str],
}

const PLAIN: Lang = Lang {
    line_comment: &[],
    block_comment: None,
    quotes: &[],
    multiline: &[],
    lifetimes: false,
    keywords: &[],
};

const RUST: Lang = Lang {
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    lifetimes: true,
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ],
    ..PLAIN
};

const PYTHON: Lang = Lang {
    line_comment: &["#"],
    quotes: &['"', '\''],
    multiline: &["\"\"\"", "'''"],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self",
        "True", "try", "while", "with", "yield",
    ],
    ..PLAIN
};

const JS: Lang = Lang {
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    multiline: &["`"],
    keywords: &[
        "abstract",
        "any",
        "as",
        "async",
        "await",
        "boolean",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "number",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "static",
        "string",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "yield",
    ],
    ..PLAIN
};

const GO: Lang = Lang {
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    multiline: &["`"],
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "false",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "nil",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "true",
        "type",
        "var",
    ],
    ..PLAIN
};

const C_LIKE: Lang = Lang {
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "abstract",
        "auto",
        "bool",
        "boolean",
        "break",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "extern",
        "false",
        "final",
        "finally",
        "float",
        "for",
        "if",
        "implements",
        "import",
        "inline",
        "int",
        "interface",
        "long",
        "namespace",
        "new",
        "null",
        "nullptr",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "struct",
        "super",
        "switch",
        "template",
        "this",
        "throw",
        "true",
        "try",
        "typedef",
        "union",
        "unsigned",
        "using",
        "virtual",
        "void",
        "volatile",
        "while",
    ],
    ..PLAIN
};

const PHP: Lang = Lang {
    line_comment: &["//", "#"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "abstract",
        "array",
        "as",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "echo",
        "else",
        "elseif",
        "extends",
        "false",
        "final",
        "finally",
        "fn",
        "for",
        "foreach",
        "function",
        "if",
        "implements",
        "interface",
        "namespace",
        "new",
        "null",
        "private",
        "protected",
        "public",
        "require",
        "require_once",
        "return",
        "static",
        "switch",
        "throw",
        "trait",
        "true",
        "try",
        "use",
        "while",
    ],
    ..PLAIN
};

const SHELL: Lang = Lang {
    line_comment: &["#"],
    quotes: &['"', '\''],
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "exit", "export", "fi", "for", "function",
        "if", "in", "local", "return", "then", "until", "while",
    ],
    ..PLAIN
};

const CONFIG: Lang = Lang {
    line_comment: &["#"],
    quotes: &['"', '\''],
    keywords: &["true", "false", "null", "yes", "no", "on", "off"],
    ..PLAIN
};

const JSON: Lang = Lang {
    quotes: &['"'],
    keywords: &["true", "false", "null"],
    ..PLAIN
};

const CSS: Lang = Lang {
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    line_comment: &[],
    ..PLAIN
};

const MARKUP: Lang = Lang {
    block_comment: Some(("<!--", "-->")),
    quotes: &['"', '\''],
    ..PLAIN
};

const SQL: Lang = Lang {
    line_comment: &["--"],
    block_comment: Some(("/*", "*/")),
    quotes: &['\'', '"'],
    keywords: &[
        "ALTER", "AND", "AS", "BY", "CREATE", "DELETE", "DROP", "FROM", "GROUP", "INDEX", "INSERT",
        "INTO", "JOIN", "LEFT", "LIMIT", "NOT", "NULL", "ON", "OR", "ORDER", "SELECT", "SET",
        "TABLE", "UPDATE", "VALUES", "WHERE", "alter", "and", "as", "by", "create", "delete",
        "drop", "from", "group", "index", "insert", "into", "join", "left", "limit", "not", "null",
        "on", "or", "order", "select", "set", "table", "update", "values", "where",
    ],
    ..PLAIN
};

const LUA: Lang = Lang {
    line_comment: &["--"],
    quotes: &['"', '\''],
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
        "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ],
    ..PLAIN
};

const RUBY: Lang = Lang {
    line_comment: &["#"],
    quotes: &['"', '\''],
    keywords: &[
        "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure", "false",
        "for", "if", "in", "module", "next", "nil", "require", "rescue", "return", "self", "then",
        "true", "unless", "until", "when", "while", "yield",
    ],
    ..PLAIN
};

pub fn lang_for(path: &str) -> &'static Lang {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "rs" => &RUST,
        "py" | "pyi" => &PYTHON,
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => &JS,
        "go" => &GO,
        "c" | "h" | "cpp" | "cc" | "hpp" | "java" | "cs" | "kt" | "swift" | "dart" | "scala" => {
            &C_LIKE
        }
        "php" => &PHP,
        "sh" | "bash" | "zsh" | "fish" => &SHELL,
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "env" | "service" => &CONFIG,
        "json" | "jsonc" => &JSON,
        "css" | "scss" | "less" => &CSS,
        "html" | "htm" | "xml" | "svg" | "vue" | "svelte" => &MARKUP,
        "sql" => &SQL,
        "lua" => &LUA,
        "rb" => &RUBY,
        _ if name == "dockerfile" || name == "makefile" || name.starts_with('.') => &CONFIG,
        _ => &PLAIN,
    }
}

fn push(out: &mut Vec<(usize, Kind)>, end: usize, kind: Kind) {
    match out.last_mut() {
        Some(last) if last.1 == kind => last.0 = end,
        _ => out.push((end, kind)),
    }
}

/// Byte offset just past the closing `quote`, honouring backslash escapes; the line end if unclosed.
fn string_end(line: &str, from: usize, quote: char) -> usize {
    let mut escaped = false;
    for (i, c) in line[from..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return from + i + c.len_utf8();
        }
    }
    line.len()
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Splits `line` into coloured spans, appended to `out` as `(end byte, kind)`.
/// Returns the state the next line starts in.
pub fn lex(lang: &Lang, line: &str, mut state: State, out: &mut Vec<(usize, Kind)>) -> State {
    let mut i = 0;
    while i < line.len() {
        let rest = &line[i..];
        match state {
            State::BlockComment => {
                let close = lang.block_comment.map_or("", |(_, c)| c);
                match rest.find(close) {
                    Some(at) if !close.is_empty() => {
                        i += at + close.len();
                        state = State::Normal;
                    }
                    _ => i = line.len(),
                }
                push(out, i, Kind::Comment);
                continue;
            }
            State::Str(n) => {
                let close = lang.multiline[n as usize];
                match rest.find(close) {
                    Some(at) => {
                        i += at + close.len();
                        state = State::Normal;
                    }
                    None => i = line.len(),
                }
                push(out, i, Kind::Str);
                continue;
            }
            State::Normal => {}
        }

        if lang.line_comment.iter().any(|p| rest.starts_with(p)) {
            push(out, line.len(), Kind::Comment);
            return State::Normal;
        }
        if let Some((open, _)) = lang
            .block_comment
            .filter(|(open, _)| rest.starts_with(open))
        {
            i += open.len();
            push(out, i, Kind::Comment);
            state = State::BlockComment;
            continue;
        }
        if let Some(n) = lang.multiline.iter().position(|d| rest.starts_with(d)) {
            i += lang.multiline[n].len();
            push(out, i, Kind::Str);
            state = State::Str(n as u8);
            continue;
        }

        let c = rest.chars().next().unwrap();
        if lang.quotes.contains(&c) {
            let end = string_end(line, i + 1, c);
            let is_lifetime = lang.lifetimes && c == '\'' && {
                let body: Vec<char> = rest.chars().skip(1).take(3).collect();
                !(body.first() == Some(&'\\') || body.get(1) == Some(&'\''))
            };
            if is_lifetime {
                i += 1;
                push(out, i, Kind::Punct);
            } else {
                i = end;
                push(out, i, Kind::Str);
            }
        } else if c.is_ascii_digit() {
            let len = rest.find(|c: char| !(c.is_alphanumeric() || c == '.' || c == '_'));
            i += len.unwrap_or(rest.len());
            push(out, i, Kind::Number);
        } else if is_ident(c) {
            let len = rest.find(|c: char| !is_ident(c)).unwrap_or(rest.len());
            let word = &rest[..len];
            let kind = if lang.keywords.contains(&word) {
                Kind::Keyword
            } else if lang.keywords.is_empty() {
                Kind::Text
            } else if rest[len..].trim_start().starts_with('(') {
                Kind::Func
            } else if c.is_uppercase() {
                Kind::Type
            } else {
                Kind::Text
            };
            i += len;
            push(out, i, kind);
        } else {
            i += c.len_utf8();
            push(
                out,
                i,
                if c.is_whitespace() {
                    Kind::Text
                } else {
                    Kind::Punct
                },
            );
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: &Lang, line: &str, state: State) -> (Vec<(String, Kind)>, State) {
        let mut out = Vec::new();
        let end = lex(lang, line, state, &mut out);
        let mut start = 0;
        let spans = out
            .into_iter()
            .map(|(e, k)| {
                let s = line[start..e].to_string();
                start = e;
                (s, k)
            })
            .collect();
        (spans, end)
    }

    #[test]
    fn rust_line() {
        let (spans, state) = kinds(
            lang_for("a.rs"),
            "let s: &'a str = foo(\"hi\"); // x",
            State::Normal,
        );
        assert_eq!(state, State::Normal);
        assert!(spans.contains(&("let".into(), Kind::Keyword)));
        assert!(spans.contains(&("foo".into(), Kind::Func)));
        assert!(spans.contains(&("\"hi\"".into(), Kind::Str)));
        assert!(spans.contains(&("// x".into(), Kind::Comment)));
        assert!(!spans
            .iter()
            .any(|(s, k)| *k == Kind::Str && s.contains("a str")));
    }

    #[test]
    fn block_comment_spans_lines() {
        let lang = lang_for("a.js");
        let (_, state) = kinds(lang, "x = 1; /* start", State::Normal);
        assert_eq!(state, State::BlockComment);
        let (spans, state) = kinds(lang, "end */ y", state);
        assert_eq!(state, State::Normal);
        assert_eq!(spans[0], ("end */".into(), Kind::Comment));
    }

    #[test]
    fn python_triple_quotes_and_unicode() {
        let lang = lang_for("a.py");
        let (_, state) = kinds(lang, "s = \"\"\"șir", State::Normal);
        assert_eq!(state, State::Str(0));
        let (spans, state) = kinds(lang, "gata\"\"\" # ț", state);
        assert_eq!(state, State::Normal);
        assert_eq!(spans[0].1, Kind::Str);
        assert_eq!(spans.last().unwrap().1, Kind::Comment);
    }
}
