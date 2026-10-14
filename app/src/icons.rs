//! Icon glyphs from the bundled Nerd Fonts symbol font, and the file-type → icon mapping.

use crate::theme;
use eframe::egui::Color32;

// Codicons, for the UI itself.
pub const FILES: char = '\u{eaf0}';
pub const SEARCH: char = '\u{ea6d}';
pub const TERMINAL: char = '\u{ea85}';
pub const CLOSE: char = '\u{ea76}';
pub const ADD: char = '\u{ea60}';
pub const SPARKLE: char = '\u{ec10}';
pub const CHEVRON_RIGHT: char = '\u{eab6}';
pub const CHEVRON_DOWN: char = '\u{eab4}';
pub const CHEVRON_UP: char = '\u{eab7}';
pub const REFRESH: char = '\u{eb37}';
pub const DOWNLOAD: char = '\u{eac2}';
pub const TRASH: char = '\u{ea81}';
pub const EDIT: char = '\u{ea73}';
pub const NEW_FILE: char = '\u{ea7f}';
pub const NEW_FOLDER: char = '\u{ea80}';
pub const COPY: char = '\u{ebcc}';
pub const DOT: char = '\u{ea71}';
pub const PLUG: char = '\u{eb2d}';
pub const DISCONNECT: char = '\u{ead0}';
pub const TOOLS: char = '\u{eb6d}';
pub const STOP: char = '\u{ead7}';
pub const SIGN_IN: char = '\u{ea6f}';
pub const ATTACH: char = '\u{ec34}';
pub const ARROW_UP: char = '\u{eaa1}';
pub const ARROW_DOWN: char = '\u{ea9a}';
pub const CHECK: char = '\u{eab2}';
pub const ERROR: char = '\u{ea87}';
pub const REMOTE: char = '\u{eb3a}';
pub const POP_OUT: char = '\u{eb14}';
pub const PIN: char = '\u{eb2b}';
pub const PINNED: char = '\u{eba0}';
pub const DOCK: char = '\u{ebf4}';
pub const FOLDER_ROOT: char = '\u{eb46}';
pub const EYE: char = '\u{ea70}';
pub const GLOBE: char = '\u{eb01}';
pub const TASKS: char = '\u{eb67}';
pub const CODE: char = '\u{eac4}';
pub const AGENTS: char = '\u{ec20}';
pub const HISTORY: char = '\u{ea82}';
pub const ARROW_LEFT: char = '\u{ea9b}';
pub const PLAY: char = '\u{eb2c}';
pub const PAUSE: char = '\u{ead1}';
pub const MUSIC: char = '\u{ec1b}';
pub const VOLUME: char = '\u{eb75}';
pub const DIFF: char = '\u{eae1}';
pub const DISCARD: char = '\u{eae2}';
pub const CLEAR: char = '\u{eabf}';
pub const COMMIT: char = '\u{ec6f}';
pub const PALETTE: char = '\u{eb5c}';
pub const STAR: char = '\u{eb59}';
pub const SAVE: char = '\u{eb4b}';
pub const GEAR: char = '\u{eaf8}';
pub const SOURCE_CONTROL: char = '\u{ea68}';
pub const PULSE: char = '\u{eb31}';
pub const METHOD: char = '\u{ea8c}';
pub const CLASS: char = '\u{eb5b}';
pub const STRUCTURE: char = '\u{ea91}';
pub const DATABASE: char = '\u{eace}';
pub const KEY: char = '\u{eb11}';
pub const COMPARE: char = '\u{eafd}';
pub const IMAGE: char = '\u{eaea}';
pub const WIN_MINIMIZE: char = '\u{eaba}';
pub const WIN_MAXIMIZE: char = '\u{eab9}';
pub const WIN_RESTORE: char = '\u{eabb}';
pub const WIN_CLOSE: char = '\u{eab8}';

const FOLDER: Color32 = Color32::from_rgb(0x8c, 0xaa, 0xee);
const GREY: Color32 = Color32::from_rgb(0x8a, 0x91, 0x9e);
const YELLOW: Color32 = Color32::from_rgb(0xe5, 0xc0, 0x5a);
const ORANGE: Color32 = Color32::from_rgb(0xe3, 0x8c, 0x4f);
const RED: Color32 = Color32::from_rgb(0xe0, 0x5d, 0x5d);
const GREEN: Color32 = Color32::from_rgb(0x8c, 0xc8, 0x6a);
const BLUE: Color32 = Color32::from_rgb(0x51, 0x9a, 0xe0);
const CYAN: Color32 = Color32::from_rgb(0x5c, 0xc8, 0xe0);
const PURPLE: Color32 = Color32::from_rgb(0xa5, 0x82, 0xd8);
const PINK: Color32 = Color32::from_rgb(0xf0, 0x6a, 0x9a);

/// The icon and its colour for a tree entry, in the style of VS Code's Seti theme.
pub fn for_entry(name: &str, is_dir: bool, open: bool) -> (char, Color32) {
    let lower = name.to_lowercase();
    if is_dir {
        return match lower.as_str() {
            ".git" | ".github" => ('\u{e5fb}', ORANGE),
            "node_modules" => ('\u{e5fa}', GREEN),
            ".config" | ".vscode" | ".idea" | "config" => ('\u{e5fc}', GREY),
            _ if open => ('\u{e5fe}', FOLDER),
            _ => ('\u{e5ff}', FOLDER),
        };
    }

    match lower.as_str() {
        "dockerfile" | "docker-compose.yml" | "docker-compose.yaml" | ".dockerignore" => {
            return ('\u{e650}', CYAN)
        }
        "makefile" | "cmakelists.txt" => return ('\u{e673}', ORANGE),
        "license" | "license.md" | "license.txt" => return ('\u{e60a}', YELLOW),
        "package.json" | "package-lock.json" | ".npmrc" => return ('\u{e616}', RED),
        "tsconfig.json" => return ('\u{e69d}', BLUE),
        "cargo.toml" | "cargo.lock" => return ('\u{e68b}', ORANGE),
        ".gitignore" | ".gitattributes" | ".gitmodules" => return ('\u{e65d}', ORANGE),
        ".editorconfig" => return ('\u{e652}', GREY),
        _ => {}
    }

    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "rs" => ('\u{e68b}', ORANGE),
        "py" | "pyi" | "pyw" => ('\u{e606}', BLUE),
        "js" | "mjs" | "cjs" => ('\u{e60c}', YELLOW),
        "ts" | "mts" | "cts" => ('\u{e628}', BLUE),
        "jsx" | "tsx" => ('\u{e625}', CYAN),
        "json" | "jsonc" | "json5" => ('\u{e60b}', YELLOW),
        "html" | "htm" => ('\u{e60e}', ORANGE),
        "css" => ('\u{e614}', BLUE),
        "scss" | "sass" | "less" => ('\u{e603}', PINK),
        "md" | "mdx" | "markdown" => ('\u{e609}', BLUE),
        "php" => ('\u{e608}', PURPLE),
        "go" => ('\u{e627}', CYAN),
        "lua" => ('\u{e620}', BLUE),
        "yml" | "yaml" => ('\u{e6a8}', PURPLE),
        "toml" => ('\u{e6b2}', GREY),
        "c" | "h" => ('\u{e649}', BLUE),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => ('\u{e646}', BLUE),
        "cs" => ('\u{e648}', GREEN),
        "java" | "jar" | "class" => ('\u{e66d}', RED),
        "kt" | "kts" => ('\u{e634}', PURPLE),
        "swift" => ('\u{e699}', ORANGE),
        "dart" => ('\u{e64c}', CYAN),
        "rb" | "gemspec" => ('\u{e605}', RED),
        "vue" => ('\u{e6a0}', GREEN),
        "svelte" => ('\u{e697}', ORANGE),
        "sh" | "bash" | "zsh" | "fish" => ('\u{e691}', GREEN),
        "ps1" | "psm1" | "bat" | "cmd" => ('\u{e683}', BLUE),
        "sql" | "db" | "sqlite" | "sqlite3" => ('\u{e64d}', PINK),
        "xml" | "xsd" | "plist" => ('\u{e619}', ORANGE),
        "svg" => ('\u{e698}', YELLOW),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" => ('\u{e60d}', PURPLE),
        "mp4" | "mkv" | "mov" | "webm" | "avi" => ('\u{e69f}', PINK),
        "mp3" | "wav" | "ogg" | "flac" | "m4a" => ('\u{e638}', PURPLE),
        "ttf" | "otf" | "woff" | "woff2" => ('\u{e659}', RED),
        "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "rar" | "zst" => ('\u{e6aa}', YELLOW),
        "pdf" => ('\u{e67d}', RED),
        "csv" | "tsv" => ('\u{e64a}', GREEN),
        "lock" => ('\u{e672}', GREY),
        "env" | "ini" | "conf" | "cfg" | "config" | "service" | "properties" => ('\u{e615}', GREY),
        "log" | "txt" => ('\u{e64e}', GREY),
        _ if lower.starts_with(".env") => ('\u{e615}', YELLOW),
        _ => ('\u{ea7b}', theme::COMMENT()),
    }
}
