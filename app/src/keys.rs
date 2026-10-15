//! Keyboard shortcuts: every command that has one, its default keys, and what the user
//! changed them to in Settings â†’ Keyboard shortcuts.

use eframe::egui::{Event, InputState, Key, Modifiers};
use std::cell::Cell;

/// Where a shortcut works.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum When {
    /// Anywhere in the window, but not while a terminal has the keyboard.
    Window,
    /// Even while a terminal has the keyboard.
    Always,
    /// While the code editor has the keyboard.
    Editor,
}

pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    /// Its keys out of the box; empty for none.
    pub default: &'static str,
    pub when: When,
}

const fn c(id: &'static str, label: &'static str, default: &'static str, when: When) -> Command {
    Command {
        id,
        label,
        default,
        when,
    }
}

pub const COMMANDS: &[Command] = &[
    c(
        "palette.commands",
        "Show all commands",
        "Ctrl+Shift+P",
        When::Always,
    ),
    c("view.settings", "Open settings", "Ctrl+,", When::Always),
    c("view.terminal", "Toggle terminal", "Ctrl+`", When::Always),
    c("palette.files", "Go to file", "Ctrl+P", When::Window),
    c(
        "palette.symbols",
        "Go to symbol in file",
        "Ctrl+Shift+O",
        When::Window,
    ),
    c("palette.line", "Go to line", "Ctrl+G", When::Window),
    c(
        "search.folder",
        "Search in folder",
        "Ctrl+Shift+F",
        When::Window,
    ),
    c(
        "search.replace",
        "Replace in folder",
        "Ctrl+Shift+H",
        When::Window,
    ),
    c("editor.find", "Find in file", "Ctrl+F", When::Window),
    c("file.save", "Save", "Ctrl+S", When::Window),
    c("file.close", "Close tab", "Ctrl+W", When::Window),
    c(
        "editor.format",
        "Format document",
        "Shift+Alt+F",
        When::Window,
    ),
    c("view.files", "Toggle side bar", "Ctrl+B", When::Window),
    c(
        "view.chat",
        "Toggle Claude chat",
        "Ctrl+Shift+I",
        When::Window,
    ),
    c("chat.new", "New Claude chat", "", When::Window),
    c("terminal.new", "New terminal", "", When::Always),
    c(
        "editor.inlayHints",
        "Turn inlay hints on or off",
        "",
        When::Window,
    ),
    c(
        "view.minimap",
        "Turn the minimap on or off",
        "",
        When::Window,
    ),
    c(
        "view.stickyScroll",
        "Turn sticky scroll on or off",
        "",
        When::Window,
    ),
    c(
        "editor.inlineEdit",
        "Edit with Claude",
        "Ctrl+K",
        When::Editor,
    ),
    c("editor.definition", "Go to definition", "F12", When::Editor),
    c(
        "editor.references",
        "Find references",
        "Shift+F12",
        When::Editor,
    ),
    c("editor.rename", "Rename symbol", "F2", When::Editor),
    c("editor.quickFix", "Quick fix", "Ctrl+.", When::Editor),
    c(
        "editor.complete",
        "Show completions",
        "Ctrl+Space",
        When::Editor,
    ),
    c(
        "editor.comment",
        "Toggle line comment",
        "Ctrl+/",
        When::Editor,
    ),
    c("editor.fold", "Fold", "Ctrl+Shift+[", When::Editor),
    c("editor.unfold", "Unfold", "Ctrl+Shift+]", When::Editor),
    c("editor.foldAll", "Fold all", "", When::Window),
    c("editor.unfoldAll", "Unfold all", "", When::Window),
    c(
        "editor.duplicateLine",
        "Copy line down",
        "Shift+Alt+Down",
        When::Editor,
    ),
    c("editor.moveLineUp", "Move line up", "Alt+Up", When::Editor),
    c(
        "editor.moveLineDown",
        "Move line down",
        "Alt+Down",
        When::Editor,
    ),
    c(
        "editor.deleteLine",
        "Delete line",
        "Ctrl+Shift+K",
        When::Editor,
    ),
    c("view.shortcuts", "Keyboard shortcuts", "", When::Always),
];

/// A key with the modifiers held for it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Combo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: Key,
}

impl Combo {
    pub fn parse(text: &str) -> Option<Combo> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let (mut ctrl, mut shift, mut alt) = (false, false, false);
        // The key is what follows the last `+`, unless the key is `+` itself.
        let (mods, key) = match text.strip_suffix("++") {
            Some(mods) => (mods, "+"),
            None => text.rsplit_once('+').unwrap_or(("", text)),
        };
        for part in mods.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" => ctrl = true,
                "shift" => shift = true,
                "alt" | "option" => alt = true,
                _ => return None,
            }
        }
        let key = key_named(key.trim())?;
        Some(Combo {
            ctrl,
            shift,
            alt,
            key,
        })
    }

    pub fn from_event(key: Key, modifiers: Modifiers) -> Combo {
        Combo {
            ctrl: modifiers.command || modifiers.ctrl,
            shift: modifiers.shift,
            alt: modifiers.alt,
            key,
        }
    }

    pub fn matches(&self, key: Key, modifiers: Modifiers) -> bool {
        *self == Combo::from_event(key, modifiers)
    }

    fn modifiers(&self) -> Modifiers {
        let mut m = Modifiers::NONE;
        if self.ctrl {
            m = m | Modifiers::COMMAND;
        }
        if self.shift {
            m = m | Modifiers::SHIFT;
        }
        if self.alt {
            m = m | Modifiers::ALT;
        }
        m
    }
}

impl std::fmt::Display for Combo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mods = self.modifiers();
        if mods.command {
            write!(f, "Ctrl+")?;
        }
        if mods.shift {
            write!(f, "Shift+")?;
        }
        if mods.alt {
            write!(f, "Alt+")?;
        }
        write!(f, "{}", key_name(self.key))
    }
}

fn key_named(name: &str) -> Option<Key> {
    let named = match name.to_ascii_lowercase().as_str() {
        "up" => Some(Key::ArrowUp),
        "down" => Some(Key::ArrowDown),
        "left" => Some(Key::ArrowLeft),
        "right" => Some(Key::ArrowRight),
        "esc" => Some(Key::Escape),
        "del" => Some(Key::Delete),
        "pgup" => Some(Key::PageUp),
        "pgdn" => Some(Key::PageDown),
        _ => None,
    };
    named.or_else(|| Key::from_name(name)).or_else(|| {
        // Letters typed in lower case.
        let mut chars = name.chars();
        let c = chars.next()?;
        if chars.next().is_none() && c.is_ascii_alphabetic() {
            Key::from_name(&c.to_ascii_uppercase().to_string())
        } else {
            None
        }
    })
}

fn key_name(key: Key) -> String {
    match key {
        Key::ArrowUp => "Up".into(),
        Key::ArrowDown => "Down".into(),
        Key::ArrowLeft => "Left".into(),
        Key::ArrowRight => "Right".into(),
        _ => key.symbol_or_name().to_string(),
    }
}

thread_local! {
    /// While Settings waits for the keys of a shortcut, none of them run.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

pub fn set_recording(on: bool) {
    RECORDING.with(|r| r.set(on));
}

pub fn recording() -> bool {
    RECORDING.with(Cell::get)
}

pub fn command(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// The keys a command has now: the user's, or else its default. An empty string set by
/// the user removes its shortcut.
pub fn combo(id: &str) -> Option<Combo> {
    let set = crate::settings::with(|s| s.keybindings.get(id).cloned());
    match set {
        Some(text) => Combo::parse(&text),
        None => command(id).and_then(|c| Combo::parse(c.default)),
    }
}

/// How a command's keys are shown in menus and the palette; empty when it has none.
pub fn shown(id: &str) -> String {
    combo(id).map(|c| c.to_string()).unwrap_or_default()
}

/// True once, when the keys of `id` were pressed this frame; the key press is taken so
/// nothing else acts on it.
pub fn take(input: &mut InputState, id: &str) -> bool {
    if recording() {
        return false;
    }
    let Some(combo) = combo(id) else {
        return false;
    };
    let found = input.events.iter().position(|event| {
        matches!(event, Event::Key { key, pressed: true, modifiers, .. }
            if combo.matches(*key, *modifiers))
    });
    match found {
        Some(index) => {
            input.events.remove(index);
            true
        }
        None => false,
    }
}

/// The editor command a key press is the shortcut of.
pub fn editor_command(key: Key, modifiers: Modifiers) -> Option<&'static str> {
    if recording() {
        return None;
    }
    COMMANDS
        .iter()
        .filter(|c| c.when == When::Editor)
        .find(|c| combo(c.id).is_some_and(|combo| combo.matches(key, modifiers)))
        .map(|c| c.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_read_and_written_back_alike() {
        for command in COMMANDS.iter().filter(|c| !c.default.is_empty()) {
            let combo = Combo::parse(command.default)
                .unwrap_or_else(|| panic!("{} doesn't parse", command.default));
            assert_eq!(combo.to_string(), command.default, "{}", command.id);
        }
        let combo = Combo::parse("ctrl+shift+p").unwrap();
        assert!(combo.ctrl && combo.shift && !combo.alt && combo.key == Key::P);
        assert_eq!(Combo::parse("Ctrl++").unwrap().key, Key::Plus);
        assert!(Combo::parse("Hyper+X").is_none());
        assert!(Combo::parse("").is_none());
    }

    #[test]
    fn a_shortcut_needs_exactly_its_modifiers() {
        let find = Combo::parse("Ctrl+F").unwrap();
        assert!(find.matches(Key::F, Modifiers::COMMAND));
        assert!(!find.matches(Key::F, Modifiers::COMMAND | Modifiers::SHIFT));
        assert!(!find.matches(Key::F, Modifiers::NONE));
    }

    #[test]
    fn no_two_default_shortcuts_collide() {
        for (i, a) in COMMANDS.iter().enumerate() {
            for b in &COMMANDS[i + 1..] {
                if !a.default.is_empty() {
                    assert!(a.default != b.default, "{} and {}", a.id, b.id);
                }
            }
        }
    }
}
