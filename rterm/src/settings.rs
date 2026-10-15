//! rterm's settings, kept as JSON in %APPDATA%\rterm\settings.json. The terminal widget
//! shared with ride reads its font size and scrollback through `with`, as it does in ride.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::PathBuf;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The terminal's font size, in points.
    pub term_size: f32,
    /// Lines kept above the screen.
    pub scrollback: usize,
    /// The profile new tabs open with; empty for the first one found.
    pub default_profile: String,
    /// "graphite", "slate" or "day".
    pub theme: String,
    /// A blurred view of what is behind the window, under see-through panes.
    pub glass: bool,
    /// How opaque the panes are over that blur, from 0 to 1.
    pub glass_opacity: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            term_size: 14.0,
            scrollback: 10_000,
            default_profile: String::new(),
            theme: "graphite".into(),
            glass: false,
            glass_opacity: 0.6,
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Settings> = RefCell::new(load());
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("rterm").join("settings.json"))
}

fn load() -> Settings {
    path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn with<T>(read: impl FnOnce(&Settings) -> T) -> T {
    CURRENT.with(|current| read(&current.borrow()))
}

/// Changes the settings and writes them down when anything actually changed.
pub fn update(change: impl FnOnce(&mut Settings)) {
    CURRENT.with(|current| {
        let mut settings = current.borrow_mut();
        let before = settings.clone();
        change(&mut settings);
        settings.term_size = settings.term_size.clamp(8.0, 32.0);
        settings.glass_opacity = settings.glass_opacity.clamp(0.0, 1.0);
        if *settings != before {
            if let (Some(path), Ok(json)) = (path(), serde_json::to_vec_pretty(&*settings)) {
                let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
                let _ = std::fs::write(path, json);
            }
        }
    });
}
