//! The conversations Claude Code had in a folder, read from the records it keeps of them
//! (`~/.claude/projects/<folder>/<session>.jsonl`), so the app can list and resume the
//! ones started in a terminal too.

use ride_proto::{ClaudeMessage, ClaudeSession};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

/// The newest this many conversations are listed.
const SESSIONS: usize = 40;
/// The last this many messages of each are kept.
const MESSAGES: usize = 300;
/// Longer messages are cut to this many characters.
const TEXT: usize = 6000;

/// Claude Code names a folder's records after the folder's path, with everything but
/// letters and digits made a dash: `/root/site` → `-root-site`, `C:/Users/me` → `C--Users-me`.
fn folder_key(cwd: &str) -> String {
    cwd.trim_end_matches(['/', '\\'])
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".claude"))
}

pub fn sessions(cwd: &str) -> Vec<ClaudeSession> {
    let Some(dir) = config_dir().map(|d| d.join("projects").join(folder_key(cwd))) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            let secs = modified
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            Some((secs, e.path()))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files
        .into_iter()
        .filter_map(|(modified, path)| read(&path, modified))
        .take(SESSIONS)
        .collect()
}

fn clip(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(TEXT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// What Claude Code adds to the conversation itself (slash commands and their output,
/// reminders), rather than what was typed.
fn is_noise(text: &str) -> bool {
    const TAGS: [&str; 12] = [
        "<command-name>",
        "<command-message>",
        "<command-args>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<local-command-caveat>",
        "<system-reminder>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
        "<user-prompt-submit-hook>",
        "<task-notification>",
    ];
    let text = text.trim_start();
    text.is_empty()
        || TAGS.iter().any(|tag| text.starts_with(tag))
        || text.starts_with("Caveat: The messages below")
        || text.starts_with("[Request interrupted")
}

/// What a tool was used on, in a few words.
fn tool_detail(input: &Value) -> String {
    for key in [
        "file_path",
        "command",
        "pattern",
        "url",
        "query",
        "description",
        "path",
    ] {
        if let Some(value) = input[key].as_str() {
            return value
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(200)
                .collect();
        }
    }
    String::new()
}

fn read(path: &std::path::Path, modified: u64) -> Option<ClaudeSession> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let file = std::fs::File::open(path).ok()?;
    let (mut title, mut summary) = (None, None);
    let mut messages = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Run headless (`claude -p`): ride's own chats, already in its history, and the
        // small jobs it gives Claude. Only conversations typed in a terminal are wanted.
        if entry["entrypoint"].as_str() == Some("sdk-cli") {
            return None;
        }
        if entry["isSidechain"].as_bool() == Some(true) || entry["isMeta"].as_bool() == Some(true) {
            continue;
        }
        let content = &entry["message"]["content"];
        match entry["type"].as_str() {
            Some("summary") => {
                summary = entry["summary"].as_str().map(str::to_string);
            }
            Some("custom-title") => {
                title = entry["customTitle"].as_str().map(str::to_string);
            }
            Some("user") => {
                let text = match content {
                    Value::String(text) => text.clone(),
                    Value::Array(blocks) => blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                if !is_noise(&text) {
                    messages.push(ClaudeMessage::User(clip(&text)));
                }
            }
            Some("assistant") => {
                for block in content.as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or("");
                            if !text.trim().is_empty() {
                                messages.push(ClaudeMessage::Claude(clip(text)));
                            }
                        }
                        Some("tool_use") => messages.push(ClaudeMessage::Tool {
                            name: block["name"].as_str().unwrap_or("tool").to_string(),
                            detail: tool_detail(&block["input"]),
                        }),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let first_ask = messages.iter().find_map(|m| match m {
        ClaudeMessage::User(text) => Some(text.lines().next().unwrap_or("").to_string()),
        _ => None,
    })?;
    let title = title
        .or(summary)
        .unwrap_or(first_ask)
        .chars()
        .take(70)
        .collect();
    let skip = messages.len().saturating_sub(MESSAGES);
    messages.drain(..skip);
    Some(ClaudeSession {
        id,
        title,
        modified,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RIDE_HISTORY_CWD=FOLDER lists the real conversations of a folder, by their shape
    /// only, to check the reading against what Claude Code actually writes.
    #[test]
    fn real_records_when_asked() {
        let Ok(cwd) = std::env::var("RIDE_HISTORY_CWD") else {
            return;
        };
        for session in sessions(&cwd) {
            let count =
                |f: fn(&ClaudeMessage) -> bool| session.messages.iter().filter(|m| f(m)).count();
            println!(
                "{} title={} user={} claude={} tool={}",
                session.id,
                session.title.chars().count(),
                count(|m| matches!(m, ClaudeMessage::User(_))),
                count(|m| matches!(m, ClaudeMessage::Claude(_))),
                count(|m| matches!(m, ClaudeMessage::Tool { .. })),
            );
        }
    }

    #[test]
    fn folders_are_named_as_claude_code_names_them() {
        assert_eq!(folder_key("/root/my.site"), "-root-my-site");
        assert_eq!(folder_key("C:/Users/me/"), "C--Users-me");
    }

    #[test]
    fn a_record_becomes_a_conversation() {
        let dir = std::env::temp_dir().join(format!("ride-claude-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc-123.jsonl");
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"fix the login page"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","name":"Read","input":{"file_path":"/srv/login.php"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"..."}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"hidden"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed."}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let session = read(&path, 7).unwrap();
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(session.id, "abc-123");
        assert_eq!(session.title, "fix the login page");
        let seen: Vec<String> = session
            .messages
            .iter()
            .map(|m| match m {
                ClaudeMessage::User(t) => format!("user {t}"),
                ClaudeMessage::Claude(t) => format!("claude {t}"),
                ClaudeMessage::Tool { name, detail } => format!("tool {name} {detail}"),
            })
            .collect();
        assert_eq!(
            seen,
            [
                "user fix the login page",
                "claude Looking.",
                "tool Read /srv/login.php",
                "claude Fixed."
            ]
        );
    }
}
