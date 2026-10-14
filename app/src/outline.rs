//! What a file is made of, read from its text alone, for any language: the functions,
//! classes and types it declares (the outline and the breadcrumbs), and the blocks that
//! can be folded (by indentation, which every language's code follows).

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Function,
    Class,
    Type,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub line: usize,
    /// Indentation width, which also tells what contains what.
    pub depth: usize,
    pub kind: Kind,
    pub name: String,
    /// The last line of its body.
    pub end: usize,
}

fn indent_of(line: &str) -> Option<usize> {
    if line.trim().is_empty() {
        return None;
    }
    Some(
        line.chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum(),
    )
}

/// For each line, the last line of the block that starts there: the lines below it that
/// are indented deeper. `None` where nothing is.
pub fn fold_ends(lines: &[&str]) -> Vec<Option<usize>> {
    let mut ends = vec![None; lines.len()];
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut last_text = 0;
    for (index, line) in lines.iter().enumerate() {
        let Some(depth) = indent_of(line) else {
            continue;
        };
        while let Some(&(d, start)) = open.last() {
            if d < depth {
                break;
            }
            open.pop();
            if last_text > start {
                ends[start] = Some(last_text);
            }
        }
        open.push((depth, index));
        last_text = index;
    }
    for (_, start) in open {
        if last_text > start {
            ends[start] = Some(last_text);
        }
    }
    ends
}

/// The leading words that say nothing about what is declared.
const MODIFIERS: [&str; 19] = [
    "export",
    "default",
    "async",
    "pub",
    "pub(crate)",
    "pub(super)",
    "local",
    "static",
    "public",
    "private",
    "protected",
    "internal",
    "abstract",
    "final",
    "override",
    "virtual",
    "unsafe",
    "const",
    "extern",
];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The identifier at the start of `text`, dotted and colon names included (`a.b:c`).
fn name_at(text: &str) -> Option<&str> {
    let end = text
        .find(|c: char| !(is_ident(c) || c == '.' || c == ':'))
        .unwrap_or(text.len());
    let name = text[..end].trim_end_matches([':', '.']);
    (!name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit())).then_some(name)
}

/// Words that open a statement, not a declaration, when followed by `(`.
const NOT_NAMES: [&str; 13] = [
    "if", "for", "while", "switch", "catch", "return", "foreach", "elseif", "else", "until",
    "with", "match", "function",
];

/// What line `text` declares, if anything.
fn declaration(text: &str) -> Option<(Kind, String)> {
    let mut rest = text.trim();
    loop {
        let word = rest.split_whitespace().next().unwrap_or("");
        if MODIFIERS.contains(&word) && rest.len() > word.len() {
            rest = rest[word.len()..].trim_start();
        } else {
            break;
        }
    }
    let (word, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let after = after.trim_start();
    let named = |kind| name_at(after).map(|n| (kind, n.to_string()));
    match word {
        "function" | "def" | "fn" | "fun" | "func" | "sub" | "proc" => {
            // Go's methods: `func (r *T) Name(`.
            let after = if word == "func" && after.starts_with('(') {
                after
                    .split_once(')')
                    .map_or(after, |(_, rest)| rest.trim_start())
            } else {
                after
            };
            // `function(` with no name is a value, not a declaration.
            return name_at(after.trim_start_matches('*')).map(|n| (Kind::Function, n.to_string()));
        }
        "class" | "module" | "object" | "namespace" => return named(Kind::Class),
        // `impl<T> Trait for Type` is named in full, generics left out.
        "impl" => {
            let after = match after.strip_prefix('<') {
                Some(generic) => generic
                    .split_once('>')
                    .map_or("", |(_, rest)| rest)
                    .trim_start(),
                None => after,
            };
            let name = after.split(['{', ';']).next().unwrap_or("");
            let name = name.split(" where").next().unwrap_or(name).trim();
            return (!name.is_empty()).then(|| (Kind::Type, name.to_string()));
        }
        "struct" | "enum" | "trait" | "interface" | "type" | "union" | "record" => {
            return named(Kind::Type)
        }
        _ => {}
    }
    // `name = function(`, `const name = (a) =>`, `name: function(`.
    let target = match word {
        "let" | "var" | "val" => after,
        _ => rest,
    };
    if let Some((left, right)) = assignment(target) {
        let right = right.trim_start().trim_start_matches("async").trim_start();
        let is_function = right.starts_with("function")
            || right.starts_with("lambda")
            || (right.starts_with('(') && right.contains("=>"))
            || right
                .split_once("=>")
                .is_some_and(|(arg, _)| name_at(arg.trim()) == Some(arg.trim()));
        let left = left.trim();
        if is_function && name_at(left) == Some(left) {
            return Some((Kind::Function, left.to_string()));
        }
    }
    // C-like functions and class methods: `type name(args) {`, with nothing after the
    // parameters but a return type. A call (`a.b(x).c(|y| {`) has dots, operators or
    // more parentheses, a statement starts with a keyword.
    if !rest.ends_with('{') {
        return None;
    }
    let open = rest.find('(')?;
    let before = rest[..open].trim_end();
    let close = open + rest[open..].find(')')?;
    let tail = &rest[close + 1..];
    let declared_like = before
        .chars()
        .all(|c| is_ident(c) || " :<>,*&~[]".contains(c))
        && !tail.contains(['(', '=', ';'])
        && !rest[open + 1..close].contains('(');
    let first = before.split_whitespace().next().unwrap_or("");
    let name = before.rsplit([' ', '*', '&']).next().unwrap_or("");
    if declared_like
        && !NOT_NAMES.contains(&first)
        && !NOT_NAMES.contains(&name)
        && name_at(name) == Some(name)
    {
        return Some((Kind::Function, name.to_string()));
    }
    None
}

/// `left = right` or `left: right`, where `=` isn't part of `==` or `=>` and `:` isn't
/// part of `::`.
fn assignment(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| bytes[p]);
        let next = bytes.get(i + 1).copied();
        let lone_eq = b == b'='
            && !matches!(next, Some(b'=' | b'>'))
            && !matches!(prev, Some(b'=' | b'!' | b'<' | b'>'));
        let lone_colon = b == b':' && next != Some(b':') && prev != Some(b':');
        if lone_eq || lone_colon {
            return Some((&text[..i], &text[i + 1..]));
        }
        if b == b'(' || b == b'"' || b == b'\'' {
            return None;
        }
    }
    None
}
/// The declarations in a file, top to bottom, each with the line its body ends on.
pub fn symbols(lines: &[&str]) -> Vec<Symbol> {
    let ends = fold_ends(lines);
    let mut found = Vec::new();
    for (line, text) in lines.iter().enumerate() {
        let trimmed = text.trim_start();
        if trimmed.starts_with("//")
            || trimmed.starts_with('#') && !trimmed.starts_with("#[")
            || trimmed.starts_with("--")
            || trimmed.starts_with('*')
            || trimmed.starts_with("/*")
        {
            continue;
        }
        let Some((kind, name)) = declaration(text) else {
            continue;
        };
        let depth = indent_of(text).unwrap_or(0);
        let mut end = ends[line].unwrap_or(line);
        // The line that closes the body (`}`, `end`) belongs to it too.
        if let Some((next, closing)) = lines
            .iter()
            .enumerate()
            .skip(end + 1)
            .find(|(_, l)| !l.trim().is_empty())
        {
            let t = closing.trim_start();
            let closes = t.starts_with(['}', ']', ')']) || t.starts_with("end");
            if closes && indent_of(closing) == Some(depth) {
                end = next;
            }
        }
        found.push(Symbol {
            line,
            depth,
            kind,
            name,
            end,
        });
    }
    found
}

/// The symbols the cursor is inside, outermost first.
pub fn enclosing(symbols: &[Symbol], line: usize) -> Vec<&Symbol> {
    let mut chain: Vec<&Symbol> = Vec::new();
    for symbol in symbols.iter().filter(|s| s.line <= line && line <= s.end) {
        chain.retain(|outer| outer.depth < symbol.depth);
        chain.push(symbol);
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<String> {
        let lines: Vec<&str> = text.lines().collect();
        symbols(&lines).into_iter().map(|s| s.name).collect()
    }

    #[test]
    fn declarations_are_found_across_languages() {
        let lua = "local function helper(a)\n  return a\nend\nfunction M.run(x)\nend\nM.stop = function()\nend\nlocal cb = function() end";
        assert_eq!(names(lua), ["helper", "M.run", "M.stop", "cb"]);
        let js = "export async function load(a) {\n}\nconst save = async (x) => {\n}\nclass Store {\n  get(key) {\n    if (key) {\n    }\n  }\n}\nconst sq = x => x * x;";
        assert_eq!(names(js), ["load", "save", "Store", "get", "sq"]);
        let py = "class Thing:\n    def __init__(self):\n        pass\nasync def main():\n    pass";
        assert_eq!(names(py), ["Thing", "__init__", "main"]);
        let rust = "pub struct Panes {\n}\nimpl Default for Panes {\n    fn default() -> Self {\n    }\n}\npub(super) fn show(ui: &mut Ui) {\n}";
        assert_eq!(
            names(rust),
            ["Panes", "Default for Panes", "default", "show"]
        );
        let go = "func (s *Server) Start() error {\n}\nfunc main() {\n}";
        assert_eq!(names(go), ["Start", "main"]);
        let c = "static int count_lines(const char *s) {\n  return 0;\n}\nint main(void) {\n  printf(\"x\");\n}";
        assert_eq!(names(c), ["count_lines", "main"]);
        let sh = "deploy() {\n  echo hi\n}";
        assert_eq!(names(sh), ["deploy"]);
        let php = "class User {\n    public function save($x) {\n    }\n}";
        assert_eq!(names(php), ["User", "save"]);
    }

    #[test]
    fn calls_and_statements_are_not_declarations() {
        let js = "if (x) {\n}\nfor (let i = 0; i < 3; i++) {\n}\nfoo(bar);\nsetTimeout(() => {\n}, 10);\nwhile (true) {\n}\nfoo(function() {\n});\ndescribe('x', () => {\n});\napp.get('/', (req, res) => {\n});";
        assert!(names(js).is_empty(), "{:?}", names(js));
        let rust = "    Kind::Function => (icons::METHOD, theme::FUNC()),\n    Some(x) => {\n    }\n    let shown: Vec<S> = if chain.is_empty() && !all.is_empty() {\n    }\n    Vec::new()\n    if !filter.is_empty() && !name.contains(&filter) {\n    }\n    ScrollArea::vertical().max_height(420.0).show(ui, |ui| {\n    });\n    if ui.button(label).clicked() {\n    }";
        assert!(names(rust).is_empty(), "{:?}", names(rust));
    }

    #[test]
    fn blocks_fold_by_indentation() {
        let text = "a {\n  b\n\n  c {\n    d\n  }\n}\ne";
        let lines: Vec<&str> = text.lines().collect();
        let ends = fold_ends(&lines);
        assert_eq!(ends[0], Some(5));
        assert_eq!(ends[3], Some(4));
        assert_eq!(ends[1], None);
        assert_eq!(ends[7], None);
    }

    #[test]
    fn the_cursor_is_placed_in_its_symbols() {
        let text = "class A {\n  run() {\n    x\n  }\n}\nfunction b() {\n}";
        let lines: Vec<&str> = text.lines().collect();
        let all = symbols(&lines);
        let chain: Vec<&str> = enclosing(&all, 2).iter().map(|s| s.name.as_str()).collect();
        assert_eq!(chain, ["A", "run"]);
        assert!(enclosing(&all, 6).iter().any(|s| s.name == "b"));
    }
}
