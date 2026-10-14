//! Features that work by running a command on the host and reading what it prints:
//! syntax checks, git, and the server monitor.

use super::*;
use crate::ai::JobKind;

/// Quotes text as one argument for the shell on the connected host: a POSIX shell on a
/// server, PowerShell on this computer.
pub(super) fn quote(text: &str) -> String {
    if chat::local_shell() {
        format!("'{}'", text.replace('\'', "''"))
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

// ---- syntax checks ----

/// The command that checks a file's syntax, by its extension, and the name of the tool.
fn checker(path: &str) -> Option<(&'static str, String)> {
    let q = quote(path);
    let name = file_name(path).to_lowercase();
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    Some(match ext {
        "lua" => ("lua", format!("luac -p {q}")),
        "js" | "mjs" | "cjs" => ("node", format!("node --check {q}")),
        // Windows has no `python3`, only a stub of that name that opens the Store.
        "py" if chat::local_shell() => ("python", format!("python -m py_compile {q}")),
        "py" => ("python", format!("python3 -m py_compile {q}")),
        "sh" | "bash" => ("bash", format!("bash -n {q}")),
        "php" => ("php", format!("php -l {q}")),
        _ => return None,
    })
}

/// JSON is checked here rather than on the host: the text is at hand and needs no tool.
/// Files that allow comments, as VS Code's and TypeScript's do, are left alone.
fn check_json(path: &str, text: &str) -> Option<Vec<(usize, String)>> {
    let name = file_name(path).to_lowercase();
    if !name.ends_with(".json") {
        return None;
    }
    let with_comments =
        name.starts_with("tsconfig") || name.starts_with("jsconfig") || path.contains("/.vscode/");
    if with_comments || text.trim().is_empty() {
        return Some(Vec::new());
    }
    Some(match serde_json::from_str::<serde::de::IgnoredAny>(text) {
        Ok(_) => Vec::new(),
        Err(error) => {
            let message = error.to_string();
            // The message ends with the place, which the editor already shows.
            let message = message
                .split(" at line ")
                .next()
                .unwrap_or(&message)
                .to_string();
            vec![(error.line().saturating_sub(1), message)]
        }
    })
}

/// The number that follows `marker` in `text`, if any.
fn number_after(text: &str, marker: &str) -> Option<usize> {
    let rest = &text[text.find(marker)? + marker.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// A `:LINE:` or `:LINE` at a line's end, as compilers print after a file name.
fn colon_number(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    (0..bytes.len())
        .filter(|i| bytes[*i] == b':')
        .find_map(|i| {
            let digits: String = text[i + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            let end = i + 1 + digits.len();
            let closes = matches!(bytes.get(end), None | Some(b':' | b'\n' | b'\r'));
            (!digits.is_empty() && closes).then(|| (digits.parse().unwrap_or(1), end))
        })
}

/// Reads the error a syntax checker printed, as a 0-based line and a message. Empty when
/// the file is fine, or the tool isn't installed on the host.
pub(super) fn parse_check(tool: &str, output: &str) -> Vec<(usize, String)> {
    let output = output.trim();
    let missing = [
        "not found",
        "not recognized",
        "No such file or directory",
        "cannot find",
    ];
    if output.is_empty() || missing.iter().any(|m| output.contains(m)) && !output.contains("rror:")
    {
        return Vec::new();
    }
    let last_error = output
        .lines()
        .rev()
        .find(|l| l.contains("Error"))
        .unwrap_or("")
        .trim();
    let (line, message) = match tool {
        // luac: path:LINE: message
        "lua" => match colon_number(output) {
            Some((line, end)) => (
                line,
                output[end..]
                    .trim_start_matches(':')
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
            ),
            None => return Vec::new(),
        },
        // path:LINE, a code frame, then "SyntaxError: message"
        // When node itself fails, the first line names one of its own files instead.
        "node" => match output
            .lines()
            .next()
            .filter(|first| !first.starts_with("node:"))
            .and_then(colon_number)
        {
            Some((line, _)) => (line, last_error),
            None => return Vec::new(),
        },
        // bash: path: line N: message
        "bash" => match number_after(output, "line ") {
            Some(line) => (
                line,
                output
                    .split(&format!("line {line}:"))
                    .nth(1)
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
            ),
            None => return Vec::new(),
        },
        // PHP Parse error: message in path on line N
        "php" => match number_after(output, "on line ") {
            Some(line) if output.contains("rror") => (
                line,
                output
                    .lines()
                    .find(|l| l.contains("rror"))
                    .unwrap_or("")
                    .trim(),
            ),
            _ => return Vec::new(),
        },
        // python: `line N` somewhere, and a last line naming the error
        _ => match number_after(output, "line ") {
            Some(line) if !last_error.is_empty() => (line, last_error),
            _ => return Vec::new(),
        },
    };
    let message: String = message.chars().take(160).collect();
    vec![(
        line.saturating_sub(1),
        if message.is_empty() {
            "syntax error".into()
        } else {
            message
        },
    )]
}

// ---- git ----

pub(super) struct GitEntry {
    /// Path from the repository's top folder.
    path: String,
    /// The two status letters git prints: staged, then not staged.
    staged: char,
    unstaged: char,
}

#[derive(Default)]
pub(super) struct Git {
    pub(super) repo: bool,
    /// The repository's top folder on the host.
    top: String,
    branch: String,
    entries: Vec<GitEntry>,
    /// Status letter per changed file, and the folders that contain a change, by full path.
    by_path: HashMap<String, char>,
    changed_dirs: HashSet<String>,
    /// The commit message being written.
    pub(super) message: String,
    busy: bool,
    /// A file whose discard button was pressed once and waits for the second press.
    confirm_discard: Option<String>,
}

/// Reads `git rev-parse --show-toplevel` followed by `git status --porcelain=v1 -b`.
fn parse_status(output: &str) -> Option<(String, String, Vec<GitEntry>)> {
    let mut lines = output.lines();
    let top = lines
        .next()
        .filter(|l| !l.starts_with("fatal") && !l.trim().is_empty())?
        .trim()
        .replace('\\', "/");
    let mut branch = String::new();
    let mut entries = Vec::new();
    for line in lines {
        if let Some(head) = line.strip_prefix("## ") {
            branch = head.split("...").next().unwrap_or(head).trim().to_string();
        } else if let Some(rest) = line.get(3..).filter(|rest| !rest.is_empty()) {
            let mut letters = line.chars();
            let (staged, unstaged) = (letters.next()?, letters.next()?);
            // A rename is shown as "old -> new"; the new name is the file that exists.
            let path = rest
                .rsplit(" -> ")
                .next()
                .unwrap_or("")
                .trim_matches('"')
                .trim_end_matches('/')
                .to_string();
            entries.push(GitEntry {
                path,
                staged,
                unstaged,
            });
        }
    }
    Some((top, branch, entries))
}

/// Turns `git diff -U0` output into the line ranges that changed in the new file:
/// (0-based first line, line count, kind) with kind 0 added, 1 changed, 2 removed.
pub(super) fn parse_hunks(diff: &str) -> Vec<(usize, usize, u8)> {
    diff.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("@@ -")?;
            let (old, rest) = rest.split_once(" +")?;
            let new = rest.split(' ').next()?;
            let count = |range: &str| {
                range
                    .split_once(',')
                    .map_or(Some(1), |(_, n)| n.parse::<usize>().ok())
            };
            let start: usize = new.split(',').next()?.parse().ok()?;
            let (removed, added) = (count(old)?, count(new)?);
            Some(match (removed, added) {
                (_, 0) => (start.saturating_sub(1).max(0), 0, 2),
                (0, added) => (start - 1, added, 0),
                (_, added) => (start - 1, added, 1),
            })
        })
        .collect()
}

/// Turns a unified diff into the lines the diff view draws.
fn parse_unified(text: &str) -> Vec<diff::Line> {
    text.lines()
        .filter(|l| {
            !l.starts_with("diff --git")
                && !l.starts_with("index ")
                && !l.starts_with("--- ")
                && !l.starts_with("+++ ")
        })
        .map(|line| match line.as_bytes().first() {
            Some(b'+') => (diff::Op::Added, line[1..].to_string()),
            Some(b'-') => (diff::Op::Removed, line[1..].to_string()),
            Some(b' ') => (diff::Op::Same, line[1..].to_string()),
            _ => (diff::Op::Same, line.to_string()),
        })
        .collect()
}

// ---- server monitor ----

#[derive(Default)]
pub(super) struct Monitor {
    /// Share of CPU time in use between the last two readings.
    cpu: Option<f32>,
    /// (busy, total) CPU ticks of the last reading.
    ticks: Option<(u64, u64)>,
    /// (used, total) in bytes.
    memory: (u64, u64),
    disk: (u64, u64),
    load: String,
    /// The busiest processes: CPU %, memory %, name.
    processes: Vec<(String, String, String)>,
    /// pm2's processes: name, status, CPU %, memory in bytes.
    pm2: Vec<(String, String, f64, u64)>,
    /// What listens on the host: port and the program's name.
    listening: Vec<(u16, String)>,
    /// The port typed into the forward box.
    port_input: String,
    /// App time of the next reading.
    next: f64,
    waiting: bool,
    note: String,
}

const MONITOR_COMMAND: &str = "head -1 /proc/stat; echo @@; head -3 /proc/meminfo; echo @@; df -B1 -P / | tail -1; echo @@; \
     cat /proc/loadavg; echo @@; ps -eo pcpu,pmem,comm --sort=-pcpu | head -8; echo @@; pm2 jlist 2>/dev/null; echo @@; \
     ss -ltnpH 2>/dev/null";

fn parse_monitor(monitor: &mut Monitor, output: &str) {
    let parts: Vec<&str> = output.split("@@").collect();
    let numbers = |text: &str| -> Vec<u64> {
        text.split_whitespace()
            .filter_map(|w| w.parse().ok())
            .collect()
    };
    let Some(cpu) = parts.first().filter(|p| p.trim_start().starts_with("cpu")) else {
        monitor.note = "The monitor reads Linux's /proc, which this host doesn't have.".into();
        return;
    };
    monitor.note.clear();
    let ticks = numbers(cpu);
    if ticks.len() >= 5 {
        let total: u64 = ticks.iter().sum();
        let idle = ticks[3] + ticks[4];
        if let Some((before_busy, before_total)) = monitor.ticks {
            let span = total.saturating_sub(before_total).max(1);
            monitor.cpu = Some((total - idle).saturating_sub(before_busy) as f32 / span as f32);
        }
        monitor.ticks = Some((total - idle, total));
    }
    if let Some(memory) = parts.get(1) {
        let kb = |key: &str| {
            memory
                .lines()
                .find(|l| l.starts_with(key))
                .map(numbers)
                .and_then(|n| n.first().copied())
                .unwrap_or(0)
                * 1024
        };
        let (total, available) = (kb("MemTotal"), kb("MemAvailable"));
        monitor.memory = (total.saturating_sub(available), total);
    }
    if let Some(disk) = parts.get(2).map(|d| numbers(d)) {
        if disk.len() >= 2 {
            monitor.disk = (disk[1], disk[0]);
        }
    }
    if let Some(load) = parts.get(3) {
        monitor.load = load
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join("  ");
    }
    if let Some(processes) = parts.get(4) {
        monitor.processes = processes
            .lines()
            .filter(|l| !l.contains("%CPU") && !l.trim().is_empty())
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                Some((
                    words.next()?.to_string(),
                    words.next()?.to_string(),
                    words.collect::<Vec<_>>().join(" "),
                ))
            })
            .filter(|(_, _, name)| name != "ps" && name != "head")
            .take(6)
            .collect();
    }
    monitor.pm2 = parts
        .get(5)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json.trim()).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap_or("?").to_string(),
                p["pm2_env"]["status"].as_str().unwrap_or("?").to_string(),
                p["monit"]["cpu"].as_f64().unwrap_or(0.0),
                p["monit"]["memory"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    monitor.listening = parts
        .get(6)
        .map(|text| parse_listening(text))
        .unwrap_or_default();
}

/// Reads `ss -ltnpH`: the local address is the fourth word, the program is in `users:(("name",`.
fn parse_listening(text: &str) -> Vec<(u16, String)> {
    let mut ports: Vec<(u16, String)> = text
        .lines()
        .filter_map(|line| {
            let address = line.split_whitespace().nth(3)?;
            let port = address.rsplit(':').next()?.parse().ok()?;
            let name = line
                .split("((\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or("");
            Some((port, name.to_string()))
        })
        .collect();
    // A program listening on IPv4 and IPv6 shows up once per address.
    ports.sort();
    ports.dedup_by_key(|(port, _)| *port);
    ports
}

/// A port of the host made reachable on this computer, through an `ssh -L` that lives
/// as long as this does.
pub(super) struct Forward {
    remote: u16,
    local: u16,
    /// Missing on this computer itself, where the port is reachable as it is.
    tunnel: Option<std::process::Child>,
}

impl Drop for Forward {
    fn drop(&mut self) {
        if let Some(tunnel) = &mut self.tunnel {
            let _ = tunnel.kill();
            let _ = tunnel.wait();
        }
    }
}

/// The port to listen on here: the same number as on the host when it is free.
fn free_local_port(wanted: u16) -> Option<u16> {
    let free =
        |port: u16| std::net::TcpListener::bind(("127.0.0.1", port)).and_then(|l| l.local_addr());
    free(wanted)
        .or_else(|_| free(0))
        .ok()
        .map(|address| address.port())
}

/// A path is put into a URL that goes to the system; only what a URL path is made of passes.
fn url_path(path: &str) -> String {
    let clean: String = path
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "/-_.~?=&%#+:@,;".contains(*c))
        .collect();
    if clean.starts_with('/') {
        clean
    } else {
        format!("/{clean}")
    }
}

pub(crate) fn open_in_browser(url: &str) {
    // Handed over as one argument, with no shell in between to read anything into it.
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// What is added to Claude Code's own notes on a host, once, so it knows about previews.
const CLAUDE_NOTE: &str = "## Previews and demos (ride)\n\
The user works on this machine through the ride editor, over SSH, so a server started here is not reachable from their browser by itself. \
When the user asks for a demo or a preview of something that runs on a port: start it listening on 127.0.0.1 (in the background, so it keeps running), \
then call the `preview` tool of the `ride` MCP server (`mcp__ride__preview`) with the port and, if needed, the path. \
It forwards the port to the user's computer and opens their browser there. Use `forward_port` to forward without opening a browser. \
If the tool answers that ride is not connected, tell the user which port the server is on instead.";

fn bar(ui: &mut Ui, label: &str, part: f32, detail: String) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(theme::STRONG()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(detail).small().color(theme::COMMENT()));
        });
    });
    let (track, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter().rect_filled(track, 3, theme::tint(18));
    let part = part.clamp(0.0, 1.0);
    // The colour says how close to full it is.
    let color = if part > 0.9 {
        theme::ANSI()[1]
    } else if part > 0.7 {
        theme::ANSI()[3]
    } else {
        theme::ANSI()[2]
    };
    ui.painter().rect_filled(
        egui::Rect::from_min_size(track.min, vec2(track.width() * part, 6.0)),
        3,
        color,
    );
    ui.add_space(8.0);
}

impl App {
    // ---- ports ----

    /// Makes a port of the host reachable on this computer and returns the local port.
    pub(super) fn forward_port(&mut self, remote: u16) -> Result<u16, String> {
        self.forwards.retain_mut(|f| {
            f.tunnel
                .as_mut()
                .is_none_or(|t| matches!(t.try_wait(), Ok(None)))
        });
        if let Some(forward) = self.forwards.iter().find(|f| f.remote == remote) {
            return Ok(forward.local);
        }
        if !self.online() {
            return Err("ride isn't connected".into());
        }
        if chat::local_shell() {
            self.forwards.push(Forward {
                remote,
                local: remote,
                tunnel: None,
            });
            return Ok(remote);
        }
        let local = free_local_port(remote).ok_or("no free port on the user's computer")?;
        let mut command = std::process::Command::new("ssh");
        command
            .args([
                "-N",
                "-o",
                "BatchMode=yes",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=30",
            ])
            .args([
                "-L",
                &format!("127.0.0.1:{local}:127.0.0.1:{remote}"),
                self.host.trim(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let tunnel = command
            .spawn()
            .map_err(|error| format!("couldn't start ssh: {error}"))?;
        os::end_with_app(&tunnel);
        self.forwards.push(Forward {
            remote,
            local,
            tunnel: Some(tunnel),
        });
        Ok(local)
    }

    /// Carries out what a program on the host asked for, and words the answer it gets.
    pub(super) fn host_ask(&mut self, text: &str) -> String {
        let Ok(ask) = serde_json::from_str::<serde_json::Value>(text) else {
            return "error: ride didn't understand the request".into();
        };
        let Some(remote) = ask["port"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port > 0)
        else {
            return "error: that isn't a port number".into();
        };
        let local = match self.forward_port(remote) {
            Ok(local) => local,
            Err(error) => return format!("error: {error}"),
        };
        if ask["tool"] == "preview" {
            let url = format!(
                "http://localhost:{local}{}",
                url_path(ask["path"].as_str().unwrap_or("/"))
            );
            open_in_browser(&url);
            self.status = format!("Claude opened {url}");
            format!("Port {remote} is forwarded and {url} was opened in the user's browser.")
        } else {
            self.status = format!("Claude forwarded port {remote} to localhost:{local}");
            format!("Port {remote} of this host is now reachable on the user's computer at http://localhost:{local}")
        }
    }

    /// One-time setup of a host: Claude Code there is told about ride's MCP server, and
    /// gets a note in its own memory on when to use it.
    pub(super) fn set_up_host(&mut self) {
        if chat::local_shell() || !settings::with(|s| s.claude_previews) {
            return;
        }
        let note: Vec<String> = CLAUDE_NOTE
            .lines()
            .map(|line| format!("'{}'", line.replace('\'', "'\\''")))
            .collect();
        let command = format!(
            "r=$HOME/.cache/ride; [ -f $r/mcp-v1 ] || {{ command -v claude >/dev/null 2>&1 && [ -x $r/ride-agent ] \
             && {{ claude mcp get ride >/dev/null 2>&1 || claude mcp add --scope user ride -- $r/ride-agent mcp >/dev/null 2>&1; }} \
             && mkdir -p $HOME/.claude \
             && {{ grep -q 'Previews and demos (ride)' $HOME/.claude/CLAUDE.md 2>/dev/null || printf '%s\\n' '' {} >> $HOME/.claude/CLAUDE.md; }} \
             && touch $r/mcp-v1; }}",
            note.join(" ")
        );
        self.start_shell_job(JobKind::Quiet, command);
    }

    fn ports_section(&mut self, ui: &mut Ui) {
        enum Act {
            Forward(u16),
            Open(u16),
            Stop(u16),
        }
        let mut act = None;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("PORTS")
                    .small()
                    .strong()
                    .color(theme::COMMENT()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let typed = self
                    .monitor
                    .port_input
                    .trim()
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port > 0);
                if ui
                    .add_enabled(typed.is_some(), egui::Button::new("Forward").small())
                    .clicked()
                {
                    act = typed.map(Act::Forward);
                    self.monitor.port_input.clear();
                }
                ui.add(
                    TextEdit::singleline(&mut self.monitor.port_input)
                        .desired_width(52.0)
                        .hint_text("port"),
                );
            });
        });
        let forwarded: HashMap<u16, u16> =
            self.forwards.iter().map(|f| (f.remote, f.local)).collect();
        // Forwarded ports first, then whatever else listens on the host.
        let mut rows: Vec<(u16, String)> = self
            .forwards
            .iter()
            .map(|f| (f.remote, String::new()))
            .collect();
        for (port, name) in &self.monitor.listening {
            match rows.iter_mut().find(|(p, _)| p == port) {
                Some(row) => row.1 = name.clone(),
                None => rows.push((*port, name.clone())),
            }
        }
        for (port, name) in rows {
            ui.horizontal(|ui| {
                let local = forwarded.get(&port);
                let color = if local.is_some() {
                    theme::ANSI()[2]
                } else {
                    theme::COMMENT()
                };
                ui.label(RichText::new(icons::DOT).size(9.0).color(color));
                ui.label(RichText::new(port.to_string()).monospace());
                ui.label(RichText::new(name).small().color(theme::COMMENT()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| match local {
                    Some(local) => {
                        if icon_button(ui, icons::CLOSE, "Stop forwarding") {
                            act = Some(Act::Stop(port));
                        }
                        if icon_button(
                            ui,
                            icons::GLOBE,
                            &format!("Open http://localhost:{local} in the browser"),
                        ) {
                            act = Some(Act::Open(*local));
                        }
                        if local != &port {
                            ui.label(
                                RichText::new(format!("→ {local}"))
                                    .small()
                                    .monospace()
                                    .color(theme::COMMENT()),
                            );
                        }
                    }
                    None => {
                        if icon_button(ui, icons::REMOTE, "Forward to this computer") {
                            act = Some(Act::Forward(port));
                        }
                    }
                });
            });
        }
        ui.add_space(8.0);
        match act {
            Some(Act::Forward(port)) => match self.forward_port(port) {
                Ok(local) => self.status = format!("Port {port} is at http://localhost:{local}"),
                Err(error) => self.status = format!("Couldn't forward port {port}: {error}"),
            },
            Some(Act::Open(local)) => open_in_browser(&format!("http://localhost:{local}/")),
            Some(Act::Stop(port)) => self.forwards.retain(|f| f.remote != port),
            None => {}
        }
    }

    // ---- syntax checks ----

    /// Checks a tab's file on the host for syntax errors; they show up in the editor.
    pub(super) fn check_file(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index).filter(|t| t.preview.is_none()) else {
            return;
        };
        if let Some(diags) = check_json(&tab.path, &tab.buf.text()) {
            self.tabs[index].view.diags = diags;
        } else if let Some((tool, command)) = checker(&tab.path) {
            let kind = JobKind::Check {
                tab_uid: tab.uid,
                tool,
            };
            self.start_shell_job(kind, command);
        }
        self.git_marks(index);
    }

    // ---- formatting ----

    /// Formats a tab's text with its formatter, on the host. With `save` it is saved
    /// afterwards, formatted or, if formatting failed, as it was. False when the file has
    /// no formatter, so nothing was started.
    pub(super) fn format_tab(&mut self, index: usize, save: bool) -> bool {
        let Some(tab) = self.tabs.get(index).filter(|t| t.preview.is_none()) else {
            return false;
        };
        let Some(command) = settings::formatter_for(&tab.path) else {
            return false;
        };
        let kind = JobKind::Format {
            tab_uid: tab.uid,
            revision: tab.buf.revision,
            save,
        };
        let text = tab.buf.text().into_bytes();
        let Some(proc) = self.start_shell_job(kind, command) else {
            return false;
        };
        self.request(Request::ProcInput { proc, data: text });
        // Empty input closes the formatter's input, so it knows the text is complete.
        self.request(Request::ProcInput {
            proc,
            data: Vec::new(),
        });
        true
    }

    /// Shift+Alt+F: formats without saving.
    pub(super) fn format_document(&mut self, index: usize) {
        if !self.format_tab(index, false) {
            self.status =
                "No formatter is set for this kind of file; add one in Settings → Editor".into();
        }
    }

    pub(super) fn format_done(
        &mut self,
        tab_uid: u64,
        revision: u64,
        save: bool,
        outcome: Result<String, String>,
    ) {
        let Some(index) = self.tabs.iter().position(|t| t.uid == tab_uid) else {
            return;
        };
        match outcome {
            Ok(text) if self.tabs[index].buf.revision == revision => {
                let buf = &mut self.tabs[index].buf;
                if !text.trim().is_empty() && text != buf.text() {
                    // One edit, so a single undo takes the formatting back.
                    let cursor = buf.cursor;
                    buf.select_all();
                    buf.insert(&text);
                    buf.set_cursor(cursor, false);
                }
            }
            Ok(_) => {
                self.status =
                    "The file changed while it was being formatted, so it was left as it was".into()
            }
            Err(error) => {
                let first = error.lines().next().unwrap_or("").trim();
                self.status = format!("Couldn't format: {first}");
            }
        }
        if save {
            self.write_tab(index);
        }
    }

    // ---- git ----

    pub(super) fn git_refresh(&mut self) {
        if self.online() {
            self.start_shell_job(
                JobKind::GitStatus,
                "git rev-parse --show-toplevel; git status --porcelain=v1 -b".into(),
            );
        }
    }

    /// Marks, beside the code, the lines that differ from the last commit.
    fn git_marks(&mut self, index: usize) {
        if !self.git.repo {
            return;
        }
        let Some(tab) = self.tabs.get(index).filter(|t| t.preview.is_none()) else {
            return;
        };
        let (kind, command) = (
            JobKind::GitMarks { tab_uid: tab.uid },
            format!("git diff -U0 -- {}", quote(&tab.path)),
        );
        self.start_shell_job(kind, command);
    }

    fn git_run(&mut self, what: &str, command: String) {
        self.git.busy = true;
        self.status = format!("{what}…");
        self.start_shell_job(
            JobKind::GitRun {
                what: what.to_string(),
            },
            command,
        );
    }

    /// The colour a tree entry's name gets from its git status.
    pub(super) fn git_color(&self, path: &str, is_dir: bool) -> Color32 {
        if is_dir {
            let changed =
                self.git.changed_dirs.contains(path) || self.git.by_path.contains_key(path);
            return if changed {
                theme::ANSI()[3]
            } else {
                theme::TEXT()
            };
        }
        match self.git.by_path.get(path) {
            Some('?' | 'A') => theme::ANSI()[2],
            Some('D') => theme::ANSI()[1],
            Some(_) => theme::ANSI()[3],
            None => theme::TEXT(),
        }
    }

    /// Takes the output of a finished command of this module.
    pub(super) fn tool_job_done(&mut self, kind: JobKind, output: String) {
        match kind {
            JobKind::Check { tab_uid, tool } => {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == tab_uid) {
                    tab.view.diags = parse_check(tool, &output);
                }
            }
            JobKind::GitMarks { tab_uid } => {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == tab_uid) {
                    tab.view.marks = parse_hunks(&output);
                }
            }
            JobKind::GitStatus => {
                let parsed = parse_status(&output);
                let was_repo = std::mem::replace(&mut self.git.repo, parsed.is_some());
                let (top, branch, entries) = parsed.unwrap_or_default();
                self.git.by_path.clear();
                self.git.changed_dirs.clear();
                for entry in &entries {
                    let full = join(&top, &entry.path);
                    let letter = if entry.unstaged != ' ' {
                        entry.unstaged
                    } else {
                        entry.staged
                    };
                    self.git
                        .by_path
                        .insert(full.trim_end_matches('/').to_string(), letter);
                    let mut dir = parent(&full);
                    while dir.len() >= top.len() && self.git.changed_dirs.insert(dir.to_string()) {
                        dir = parent(dir);
                    }
                }
                (self.git.top, self.git.branch, self.git.entries) = (top, branch, entries);
                // Tabs opened before the repository was known get their change marks now.
                if self.git.repo && !was_repo {
                    for index in 0..self.tabs.len() {
                        self.git_marks(index);
                    }
                }
            }
            JobKind::GitShow { title } => {
                self.push_preview_tab(title, preview::Preview::Diff(parse_unified(&output)));
            }
            JobKind::GitRun { what } => {
                self.git.busy = false;
                let failed = output
                    .lines()
                    .any(|l| l.starts_with("fatal") || l.starts_with("error"));
                let summary = output
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("done")
                    .trim();
                self.status = if failed {
                    format!("{what} failed: {summary}")
                } else {
                    format!("{what}: {summary}")
                };
                if what == "Commit" && !failed {
                    self.git.message.clear();
                }
                self.sync_after_claude();
            }
            JobKind::Monitor => {
                self.monitor.waiting = false;
                parse_monitor(&mut self.monitor, &output);
            }
            _ => {}
        }
    }

    pub(super) fn git_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let branch = if self.git.branch.is_empty() {
                "git"
            } else {
                &self.git.branch
            };
            ui.label(pieces(&[
                (&format!("{}  ", icons::COMMIT), theme::COMMENT()),
                (branch, theme::STRONG()),
            ]));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, icons::REFRESH, "Refresh") {
                    self.git_refresh();
                }
                if self.git.repo && icon_button(ui, icons::ARROW_UP, "Push") {
                    self.git_run("Push", "git push".into());
                }
                if self.git.repo && icon_button(ui, icons::ARROW_DOWN, "Pull (fast-forward only)") {
                    self.git_run("Pull", "git pull --ff-only".into());
                }
                if self.git.busy {
                    ui.spinner();
                }
            });
        });
        if !self.git.repo {
            ui.label(
                RichText::new("The open folder isn't in a git repository.")
                    .small()
                    .color(theme::COMMENT()),
            );
            return;
        }

        ui.add(
            TextEdit::multiline(&mut self.git.message)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Commit message"),
        );
        ui.horizontal(|ui| {
            let ready = !self.git.message.trim().is_empty()
                && self
                    .git
                    .entries
                    .iter()
                    .any(|e| e.staged != ' ' && e.staged != '?');
            let commit = egui::Button::new(RichText::new("Commit").color(theme::EDITOR_BG()))
                .fill(theme::TEXT());
            if ui
                .add_enabled(ready, commit)
                .on_disabled_hover_text("Stage a file and write a message first")
                .clicked()
            {
                let command = format!("git commit -m {}", quote(self.git.message.trim()));
                self.git_run("Commit", command);
            }
            if ui
                .button(format!("{}  Write it", icons::SPARKLE))
                .on_hover_text("Let Claude write the message from the changes")
                .clicked()
            {
                self.commit_message();
            }
        });
        ui.add_space(4.0);

        enum Act {
            Stage(String),
            Unstage(String),
            Discard(String),
            AskDiscard(String),
            Show(String, bool),
            StageAll,
        }
        let mut act = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for staged_section in [true, false] {
                let rows: Vec<&GitEntry> = self
                    .git
                    .entries
                    .iter()
                    .filter(|e| {
                        if staged_section {
                            e.staged != ' ' && e.staged != '?'
                        } else {
                            e.unstaged != ' '
                        }
                    })
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let title = if staged_section { "STAGED" } else { "CHANGES" };
                    ui.label(
                        RichText::new(format!("{title}  {}", rows.len()))
                            .small()
                            .strong()
                            .color(theme::COMMENT()),
                    );
                    if !staged_section {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, icons::ADD, "Stage all changes") {
                                act = Some(Act::StageAll);
                            }
                        });
                    }
                });
                for entry in rows {
                    let letter = if staged_section {
                        entry.staged
                    } else {
                        entry.unstaged
                    };
                    let color = match letter {
                        '?' | 'A' => theme::ANSI()[2],
                        'D' => theme::ANSI()[1],
                        _ => theme::ANSI()[3],
                    };
                    ui.horizontal(|ui| {
                        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if staged_section {
                                if icon_button(ui, icons::CLOSE, "Unstage") {
                                    act = Some(Act::Unstage(entry.path.clone()));
                                }
                            } else {
                                if icon_button(ui, icons::ADD, "Stage") {
                                    act = Some(Act::Stage(entry.path.clone()));
                                }
                                if letter != '?' {
                                    if self.git.confirm_discard.as_deref()
                                        == Some(entry.path.as_str())
                                    {
                                        if ui
                                            .small_button("Discard?")
                                            .on_hover_text(
                                                "Throws away this file's changes for good",
                                            )
                                            .clicked()
                                        {
                                            act = Some(Act::Discard(entry.path.clone()));
                                        }
                                    } else if icon_button(ui, icons::DISCARD, "Discard changes…")
                                    {
                                        act = Some(Act::AskDiscard(entry.path.clone()));
                                    }
                                }
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let label = pieces(&[
                                    (&format!("{letter}  "), color),
                                    (&entry.path, theme::TEXT()),
                                ]);
                                if ui
                                    .selectable_label(false, label)
                                    .on_hover_text("Show the changes")
                                    .clicked()
                                {
                                    act = Some(Act::Show(entry.path.clone(), staged_section));
                                }
                            });
                        });
                    });
                }
            }
            if self.git.entries.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Nothing has changed since the last commit.")
                        .small()
                        .color(theme::COMMENT()),
                );
            }
        });

        // Paths are from the repository's top, so the commands run there whatever folder is open.
        let top = self.git.top.clone();
        let at = |path: &str| quote(&join(&top, path));
        match act {
            Some(Act::Stage(path)) => self.git_run("Stage", format!("git add -- {}", at(&path))),
            Some(Act::StageAll) => self.git_run("Stage", "git add -A".into()),
            Some(Act::Unstage(path)) => {
                self.git_run("Unstage", format!("git restore --staged -- {}", at(&path)))
            }
            Some(Act::AskDiscard(path)) => self.git.confirm_discard = Some(path),
            Some(Act::Discard(path)) => {
                self.git.confirm_discard = None;
                self.git_run("Discard", format!("git checkout -- {}", at(&path)));
            }
            // A file git doesn't know yet has nothing to compare with, so it is just opened.
            Some(Act::Show(path, _))
                if self
                    .git
                    .entries
                    .iter()
                    .any(|e| e.path == path && e.staged == '?') =>
            {
                self.open_file(join(&top, &path), None);
            }
            Some(Act::Show(path, staged)) => {
                let flag = if staged { "--cached " } else { "" };
                let command = format!("git diff {flag}-- {}", at(&path));
                let title = format!(
                    "{} · {}",
                    join(&self.git.top, &path),
                    if staged { "staged" } else { "changes" }
                );
                self.start_shell_job(JobKind::GitShow { title }, command);
            }
            None => {}
        }
    }

    // ---- server monitor ----

    pub(super) fn monitor_panel(&mut self, ui: &mut Ui) {
        // A reading every few seconds, only while the panel is looked at.
        let now = ui.input(|i| i.time);
        if now >= self.monitor.next
            && !self.monitor.waiting
            && self.monitor.note.is_empty()
            && self.online()
        {
            self.monitor.next = now + 3.0;
            self.monitor.waiting = true;
            self.start_shell_job(JobKind::Monitor, MONITOR_COMMAND.into());
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));

        ui.label(
            RichText::new("SERVER")
                .small()
                .strong()
                .color(theme::COMMENT()),
        );
        ui.add_space(4.0);
        let m = &self.monitor;
        if !m.note.is_empty() {
            ui.label(RichText::new(&m.note).small().color(theme::COMMENT()));
            return;
        }
        let gb = |bytes: u64| format!("{:.1} GB", bytes as f64 / 1_073_741_824.0);
        let share = |(used, total): (u64, u64)| {
            if total == 0 {
                0.0
            } else {
                used as f32 / total as f32
            }
        };
        bar(
            ui,
            "CPU",
            m.cpu.unwrap_or(0.0),
            m.cpu.map_or("measuring…".into(), |c| {
                format!("{:.0}%   load {}", c * 100.0, m.load)
            }),
        );
        bar(
            ui,
            "Memory",
            share(m.memory),
            format!("{} of {}", gb(m.memory.0), gb(m.memory.1)),
        );
        bar(
            ui,
            "Disk",
            share(m.disk),
            format!("{} of {}", gb(m.disk.0), gb(m.disk.1)),
        );

        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
            self.ports_section(ui);
            let m = &self.monitor;
            if !m.pm2.is_empty() {
                ui.label(
                    RichText::new("PM2")
                        .small()
                        .strong()
                        .color(theme::COMMENT()),
                );
                for (name, status, cpu, memory) in &m.pm2 {
                    let color = if status == "online" {
                        theme::ANSI()[2]
                    } else {
                        theme::ANSI()[1]
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::DOT).size(9.0).color(color));
                        ui.label(name);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let detail =
                                format!("{cpu:.0}%  {:.0} MB", *memory as f64 / 1_048_576.0);
                            ui.label(
                                RichText::new(detail)
                                    .small()
                                    .monospace()
                                    .color(theme::COMMENT()),
                            );
                            if status != "online" {
                                ui.label(RichText::new(status).small().color(color));
                            }
                        });
                    });
                }
                ui.add_space(8.0);
            }
            ui.label(
                RichText::new("BUSIEST PROCESSES")
                    .small()
                    .strong()
                    .color(theme::COMMENT()),
            );
            for (cpu, memory, name) in &m.processes {
                ui.horizontal(|ui| {
                    ui.label(name);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{cpu}% cpu  {memory}% mem"))
                                .small()
                                .monospace()
                                .color(theme::COMMENT()),
                        );
                    });
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checker_output_becomes_a_line_and_a_message() {
        let lua = "luac: /srv/app/menu.lua:42: unexpected symbol near '}'";
        assert_eq!(
            parse_check("lua", lua),
            [(41, "unexpected symbol near '}'".to_string())]
        );

        let node = "/srv/app/x.js:7\n  let = ;\n      ^\n\nSyntaxError: Unexpected token '='\n    at wrapSafe (node:internal)\n";
        assert_eq!(
            parse_check("node", node),
            [(6, "SyntaxError: Unexpected token '='".to_string())]
        );

        let python =
            "  File \"/srv/a.py\", line 3\n    def f(:\n          ^\nSyntaxError: invalid syntax\n";
        assert_eq!(
            parse_check("python", python),
            [(2, "SyntaxError: invalid syntax".to_string())]
        );

        let own = "node:internal/modules/package_json_reader:193\n  const result = x;\n\nError: Invalid package config\n";
        assert!(
            parse_check("node", own).is_empty(),
            "node failing isn't an error in the file"
        );

        let json = check_json("/srv/package.json", "{\n  \"a\": 1\n  \"b\": 2\n}\n").unwrap();
        assert_eq!(json, [(2, "expected `,` or `}`".to_string())]);
        assert!(check_json("/srv/package.json", "{\"a\": [1, 2]}")
            .unwrap()
            .is_empty());
        assert!(check_json("/srv/tsconfig.json", "{ // comment\n}")
            .unwrap()
            .is_empty());
        assert!(check_json("/srv/a.lua", "x").is_none());

        let bash = "/srv/run.sh: line 9: syntax error near unexpected token `fi'";
        assert_eq!(
            parse_check("bash", bash),
            [(8, "syntax error near unexpected token `fi'".to_string())]
        );

        let php = "PHP Parse error:  syntax error, unexpected '}' in /srv/a.php on line 12";
        assert_eq!(parse_check("php", php)[0].0, 11);

        // A clean file, and a host without the tool, give nothing to show.
        assert!(parse_check("php", "No syntax errors detected in /srv/a.php").is_empty());
        assert!(parse_check("lua", "bash: luac: command not found").is_empty());
        assert!(parse_check("lua", "").is_empty());
    }

    #[test]
    fn git_status_is_read() {
        let (top, branch, entries) =
            parse_status("/srv/app\n## main...origin/main [ahead 1]\n M src/a.lua\nA  new.txt\n?? notes.md\nR  old.rs -> new.rs\n").unwrap();
        assert_eq!((top.as_str(), branch.as_str()), ("/srv/app", "main"));
        let seen: Vec<(char, char, &str)> = entries
            .iter()
            .map(|e| (e.staged, e.unstaged, e.path.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                (' ', 'M', "src/a.lua"),
                ('A', ' ', "new.txt"),
                ('?', '?', "notes.md"),
                ('R', ' ', "new.rs")
            ]
        );
        assert!(parse_status(
            "fatal: not a git repository (or any of the parent directories): .git\n"
        )
        .is_none());
    }

    #[test]
    fn diff_hunks_become_line_marks() {
        let diff =
            "@@ -3 +3 @@ fn a\n-x\n+y\n@@ -10,0 +11,2 @@\n+a\n+b\n@@ -20,3 +21,0 @@\n-a\n-b\n-c\n";
        assert_eq!(parse_hunks(diff), [(2, 1, 1), (10, 2, 0), (20, 0, 2)]);
    }

    #[test]
    fn monitor_output_is_read() {
        let mut monitor = Monitor::default();
        let reading = |busy: u64, idle: u64| {
            format!(
                "cpu  {busy} 0 0 {idle} 0 0 0 0\n@@\nMemTotal:       8000000 kB\nMemFree:        1000000 kB\nMemAvailable:   6000000 kB\n@@\n/dev/sda1 100000000000 40000000000 60000000000 40% /\n@@\n0.50 0.40 0.30 1/200 999\n@@\n%CPU %MEM COMMAND\n12.5  3.1 node\n 1.0  0.5 nginx\n@@\n[{{\"name\":\"api\",\"pm2_env\":{{\"status\":\"online\"}},\"monit\":{{\"cpu\":2.5,\"memory\":104857600}}}}]"
            )
        };
        parse_monitor(&mut monitor, &reading(100, 900));
        assert!(
            monitor.cpu.is_none(),
            "one reading can't tell how busy the CPU is"
        );
        parse_monitor(&mut monitor, &reading(150, 950));
        assert_eq!(monitor.cpu, Some(0.5));
        assert_eq!(monitor.memory, (2_000_000 * 1024, 8_000_000 * 1024));
        assert_eq!(monitor.disk, (40_000_000_000, 100_000_000_000));
        assert_eq!(
            monitor.processes[0],
            ("12.5".to_string(), "3.1".to_string(), "node".to_string())
        );
        assert_eq!(
            monitor.pm2[0],
            ("api".to_string(), "online".to_string(), 2.5, 104_857_600)
        );

        let ss = "LISTEN 0 511 0.0.0.0:80 0.0.0.0:* users:((\"nginx\",pid=7,fd=17),(\"nginx\",pid=8,fd=17))\n\
                  LISTEN 0 511 [::]:80 [::]:* users:((\"nginx\",pid=7,fd=18))\n\
                  LISTEN 0 4096 127.0.0.1:3000 0.0.0.0:* users:((\"node\",pid=9,fd=3))\n";
        assert_eq!(
            parse_listening(ss),
            [(80, "nginx".to_string()), (3000, "node".to_string())]
        );
        assert_eq!(url_path("/a/b.html?x=1"), "/a/b.html?x=1");
        assert_eq!(url_path("x\" & calc"), "/x&calc");
    }
}
