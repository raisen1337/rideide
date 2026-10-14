//! The AI features layered on the editor: inline edits, the Changes panel, `@` mentions,
//! AI search, commit messages, the team graph and scheduled tasks.

use super::*;
use crate::diff;

/// What a background Claude or shell run is for; decides what happens with its output.
pub(super) enum JobKind {
    InlineEdit {
        tab_uid: u64,
        start: Pos,
        end: Pos,
        revision: u64,
        old: String,
    },
    AiSearch,
    /// The "Let's catch up" summary shown after connecting.
    CatchUp,
    /// `git diff`, whose output is then turned into a commit message.
    GitDiff,
    CommitMessage,
    /// A syntax check of a tab's file.
    Check {
        tab_uid: u64,
        tool: &'static str,
    },
    GitStatus,
    /// `git diff -U0` of a tab's file, for the change marks beside the code.
    GitMarks {
        tab_uid: u64,
    },
    /// A diff to show in a tab with this title.
    GitShow {
        title: String,
    },
    /// A git command whose last line is reported, named by `what`.
    GitRun {
        what: String,
    },
    Monitor,
    /// A formatter given a tab's text; with save, the tab is saved after.
    Format {
        tab_uid: u64,
        revision: u64,
        save: bool,
    },
    /// A command run for what it does; its output isn't looked at.
    Quiet,
}

pub(super) struct Job {
    proc: u64,
    kind: JobKind,
    out: Vec<u8>,
    err: String,
    /// A Claude run ends at its `result` event; a shell command when it exits.
    claude: bool,
}

/// A replacement Claude proposed for part of a file, waiting to be accepted or rejected.
pub(super) struct InlineEdit {
    tab_uid: u64,
    start: Pos,
    end: Pos,
    /// The buffer revision the proposal was made against.
    revision: u64,
    new: String,
    diff: Vec<diff::Line>,
}

pub(super) struct Scheduled {
    prompt: String,
    every_min: u32,
    /// App time, in seconds, of the next run.
    next: f64,
    /// The agent that runs it, created on the first run.
    agent: Option<u64>,
}

#[derive(Default)]
pub(super) struct Ai {
    jobs: Vec<Job>,
    /// The instruction being typed in the inline-edit bar, while it is open.
    inline_prompt: Option<String>,
    focus_inline: bool,
    inline_busy: bool,
    inline: Option<InlineEdit>,
    /// The Agents view shows the team graph instead of a chat.
    team_view: bool,
    scheduled: Vec<Scheduled>,
    /// The agents status card is folded down to a pill.
    widget_compact: bool,
    /// Agents were working and have all finished, and the user hasn't looked yet.
    widget_finished: bool,
    widget_was_busy: bool,
    /// The catch-up screen, while it covers the window.
    catchup: Option<CatchUp>,
}

pub(super) struct CatchUp {
    /// Claude's summary so far; it arrives a piece at a time.
    text: String,
    done: bool,
}

fn user_line(text: &str) -> Vec<u8> {
    let msg = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
    });
    format!("{msg}\n").into_bytes()
}

/// Claude sometimes wraps code in a Markdown fence even when asked not to.
fn strip_fences(text: &str) -> String {
    let mut lines: Vec<&str> = text.trim_matches('\n').lines().collect();
    if lines
        .first()
        .is_some_and(|l| l.trim_start().starts_with("```"))
    {
        lines.remove(0);
        if lines.last().is_some_and(|l| l.trim() == "```") {
            lines.pop();
        }
    }
    lines.join("\n")
}

/// Reads `path:line — reason` lines from an AI search answer.
fn parse_ai_hits(root: &str, text: &str) -> Vec<SearchHit> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim().trim_start_matches(['-', '*', ' ']);
            let (location, reason) = line.split_once(" — ").or_else(|| line.split_once(" - "))?;
            let (path, number) = location.trim().trim_matches('`').rsplit_once(':')?;
            let number: u32 = number.trim().parse().ok()?;
            let path = if path.starts_with('/') {
                path.to_string()
            } else {
                join(root, path)
            };
            Some(SearchHit {
                path,
                line: number.saturating_sub(1),
                col: 0,
                text: reason.trim().to_string(),
            })
        })
        .collect()
}

impl App {
    // ---- background runs ----

    fn start_claude_job(&mut self, kind: JobKind, prompt: String, model: &str) {
        // A catch-up is shown while it is written, so it asks for the text as it comes.
        let partial = if matches!(kind, JobKind::CatchUp) {
            " --include-partial-messages"
        } else {
            ""
        };
        if !self.online() {
            return;
        }
        // Default permission mode: it can read and search, but not change anything.
        let command = format!(
            "exec claude -p --input-format stream-json --output-format stream-json --verbose \
             --permission-mode default --model {model}{partial}"
        );
        let proc = self.request(Request::ProcOpen {
            command,
            cwd: self.root.clone(),
        });
        self.request(Request::ProcInput {
            proc,
            data: user_line(&prompt),
        });
        self.ai.jobs.push(Job {
            proc,
            kind,
            out: Vec::new(),
            err: String::new(),
            claude: true,
        });
    }

    /// Runs a command on the host; its output goes to the job's handler. The process id,
    /// for feeding it input.
    pub(super) fn start_shell_job(&mut self, kind: JobKind, command: String) -> Option<u64> {
        if !self.online() {
            return None;
        }
        let proc = self.request(Request::ProcOpen {
            command,
            cwd: self.root.clone(),
        });
        self.ai.jobs.push(Job {
            proc,
            kind,
            out: Vec::new(),
            err: String::new(),
            claude: false,
        });
        Some(proc)
    }

    /// Feeds a process message to the job it belongs to. False if it isn't a job's.
    pub(super) fn job_message(&mut self, id: u64, resp: &Response) -> bool {
        let Some(index) = self.ai.jobs.iter().position(|j| j.proc == id) else {
            return false;
        };
        let job = &mut self.ai.jobs[index];
        let mut outcome = None;
        match resp {
            Response::ProcOutput { data } => {
                job.out.extend_from_slice(data);
                let mut streamed = String::new();
                if job.claude {
                    for line in String::from_utf8_lossy(&job.out).lines() {
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                            continue;
                        };
                        if v["type"] == "stream_event"
                            && v["event"]["delta"]["type"] == "text_delta"
                        {
                            streamed.push_str(v["event"]["delta"]["text"].as_str().unwrap_or(""));
                        }
                        if v["type"] == "result" {
                            let text = v["result"].as_str().unwrap_or("").to_string();
                            let failed = v["is_error"].as_bool().unwrap_or(false);
                            outcome = Some(if failed { Err(text) } else { Ok(text) });
                        }
                    }
                }
                if let (JobKind::CatchUp, Some(catchup)) = (&job.kind, &mut self.ai.catchup) {
                    catchup.text = streamed;
                }
            }
            Response::ProcStderr { data } => job.err.push_str(&String::from_utf8_lossy(data)),
            // A formatter that fails says why on its error stream; its output is no use.
            Response::ProcExit { code }
                if matches!(job.kind, JobKind::Format { .. }) && *code != Some(0) =>
            {
                let error = job.err.trim();
                outcome = Some(Err(if error.is_empty() {
                    format!("exited with code {}", code.unwrap_or(-1))
                } else {
                    error.to_string()
                }));
            }
            Response::ProcExit { .. } if job.claude => {
                let error = job.err.trim();
                let error = if error.is_empty() {
                    "Claude exited without an answer"
                } else {
                    error
                };
                outcome = Some(Err(error.to_string()));
            }
            Response::ProcExit { .. } => {
                let mut text = String::from_utf8_lossy(&job.out).into_owned();
                // Checkers and git say what went wrong on the error stream.
                if matches!(job.kind, JobKind::Check { .. } | JobKind::GitRun { .. }) {
                    text.push_str(&job.err);
                }
                outcome = Some(Ok(text))
            }
            Response::Error { message } => outcome = Some(Err(message.clone())),
            _ => {}
        }
        if let Some(outcome) = outcome {
            let job = self.ai.jobs.remove(index);
            if job.claude {
                self.request(Request::ProcClose { proc: job.proc });
            }
            self.finish_job(job.kind, outcome);
        }
        true
    }

    fn finish_job(&mut self, kind: JobKind, outcome: Result<String, String>) {
        match (kind, outcome) {
            (
                JobKind::InlineEdit {
                    tab_uid,
                    start,
                    end,
                    revision,
                    old,
                },
                outcome,
            ) => {
                self.ai.inline_busy = false;
                let text = match outcome {
                    Ok(text) => text,
                    Err(error) => {
                        self.status = format!("Inline edit failed: {error}");
                        return;
                    }
                };
                let new = strip_fences(&text);
                let unchanged = self
                    .tabs
                    .iter()
                    .any(|t| t.uid == tab_uid && t.buf.revision == revision);
                if !unchanged {
                    self.status =
                        "The file changed while Claude was working; the proposal was dropped"
                            .into();
                } else if new == old {
                    self.status = "Claude proposed no change".into();
                } else {
                    let diff = diff::lines(&old, &new);
                    self.ai.inline = Some(InlineEdit {
                        tab_uid,
                        start,
                        end,
                        revision,
                        new,
                        diff,
                    });
                }
            }
            (JobKind::AiSearch, outcome) => {
                self.search.ai_busy = false;
                match outcome {
                    Ok(text) => {
                        self.search.hits = parse_ai_hits(&self.root, &text);
                        if self.search.hits.is_empty() {
                            self.status = "Claude found nothing it could point to".into();
                        }
                    }
                    Err(error) => self.status = format!("AI search failed: {error}"),
                }
            }
            (JobKind::GitDiff, Ok(diff)) if diff.trim().is_empty() => {
                self.status =
                    "No commit message: there are no uncommitted changes, or this folder isn't a git repository".into();
            }
            (JobKind::GitDiff, Ok(diff)) => {
                let diff: String = diff.chars().take(60_000).collect();
                let prompt = format!(
                    "Write a git commit message for the diff below: a subject line of at most 72 \
                     characters, a blank line, then a short body saying what changed and why. \
                     Reply with only the commit message. Do not use tools.\n\n{diff}"
                );
                self.start_claude_job(JobKind::CommitMessage, prompt, "haiku");
            }
            // With the Git panel there to take it, the message goes into its commit box.
            (JobKind::CommitMessage, Ok(text)) if self.git.repo => {
                self.status.clear();
                self.git.message = strip_fences(&text);
                self.side = Side::Git;
                self.show_side = true;
            }
            (JobKind::CommitMessage, Ok(text)) => {
                self.status.clear();
                self.dialog = Some(Dialog::Text {
                    title: "Commit message".into(),
                    text: strip_fences(&text),
                });
            }
            (JobKind::CatchUp, Ok(text)) => {
                if let Some(catchup) = &mut self.ai.catchup {
                    catchup.text = text;
                    catchup.done = true;
                }
            }
            // No Claude on this host, or it isn't logged in: then there is no catch-up at all.
            (JobKind::CatchUp, Err(_)) => self.ai.catchup = None,
            (JobKind::GitDiff | JobKind::CommitMessage, Err(error)) => {
                self.status = format!("Commit message failed: {error}");
            }
            (
                JobKind::Format {
                    tab_uid,
                    revision,
                    save,
                },
                outcome,
            ) => self.format_done(tab_uid, revision, save, outcome),
            (kind, outcome) => self.tool_job_done(kind, outcome.unwrap_or_else(|error| error)),
        }
    }

    // ---- inline edit ----

    /// The selection of a tab, or the cursor's whole line when nothing is selected.
    fn selection_or_line(&self, index: usize) -> (Pos, Pos, String) {
        let buf = &self.tabs[index].buf;
        let (mut start, mut end) = buf.selection();
        if start == end {
            start = Pos::new(start.line, 0);
            end = Pos::new(end.line, buf.line_len(end.line));
        }
        (start, end, buf.text_range(start, end))
    }

    pub(super) fn editor_action(&mut self, index: usize, action: editor::Action) {
        match action {
            editor::Action::InlineEdit if self.tabs[index].detached => {
                self.status = "Dock this tab into the main window to edit it with Claude".into();
            }
            editor::Action::InlineEdit => {
                self.active = index;
                self.ai.inline = None;
                self.ai.inline_prompt = Some(String::new());
                self.ai.focus_inline = true;
            }
            editor::Action::Explain => {
                let (start, end, code) = self.selection_or_line(index);
                let path = &self.tabs[index].path;
                let message = format!(
                    "Explain this code from {path} (lines {}-{}):\n```\n{code}\n```",
                    start.line + 1,
                    end.line + 1
                );
                self.show_chat = true;
                self.chat_send(message);
            }
            editor::Action::AddToChat => {
                self.chat.add_to_input("@selection");
                self.show_chat = true;
            }
        }
    }

    fn start_inline_edit(&mut self, index: usize, instruction: String) {
        let (start, end, old) = self.selection_or_line(index);
        let tab = &self.tabs[index];
        let file = tab.buf.text();
        let file = if file.len() > 80_000 {
            "(the file is too large to include; only the selected code is given)".to_string()
        } else {
            file
        };
        let prompt = format!(
            "You are an inline code editor. Rewrite the selected code from the file below as instructed.\n\
             Reply with ONLY the replacement for the selected code: no explanation and no Markdown fences. \
             Keep the indentation of the original. Do not use tools.\n\n\
             File: {}\nInstruction: {instruction}\n\n<file>\n{file}\n</file>\n\n\
             <selected lines=\"{}-{}\">\n{old}\n</selected>",
            tab.path,
            start.line + 1,
            end.line + 1
        );
        let kind = JobKind::InlineEdit {
            tab_uid: tab.uid,
            start,
            end,
            revision: tab.buf.revision,
            old,
        };
        self.ai.inline_busy = true;
        self.start_claude_job(kind, prompt, &settings::with(|s| s.edit_model.clone()));
    }

    /// The inline-edit prompt and, once Claude answered, its proposal as a diff.
    pub(super) fn inline_bar(&mut self, ui: &mut Ui, active: usize) {
        if self.ai.inline_prompt.is_some() {
            let (mut submit, mut close) = (false, false);
            Frame::NONE
                .inner_margin(Margin::symmetric(10, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::SPARKLE).color(theme::CLAUDE()));
                        let text = self.ai.inline_prompt.as_mut().unwrap();
                        let edit =
                            ui.add(TextEdit::singleline(text).desired_width(440.0).hint_text(
                                "Describe the change to the selection, then press Enter",
                            ));
                        if std::mem::take(&mut self.ai.focus_inline) {
                            edit.request_focus();
                        }
                        let (enter, escape) =
                            ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                        submit = edit.lost_focus() && enter && !text.trim().is_empty();
                        if self.ai.inline_busy {
                            ui.spinner();
                            ui.label(
                                RichText::new("Claude is writing…")
                                    .small()
                                    .color(theme::COMMENT()),
                            );
                        }
                        close = icon_button(ui, icons::CLOSE, "Close (Esc)")
                            || (escape && self.ai.inline.is_none());
                    });
                });
            if submit && !self.ai.inline_busy {
                let instruction = self.ai.inline_prompt.clone().unwrap_or_default();
                self.ai.inline = None;
                self.start_inline_edit(active, instruction);
            }
            if close {
                self.ai.inline_prompt = None;
                self.tabs[active].view.request_focus = true;
            }
        }

        let Some(edit) = &self.ai.inline else { return };
        if self.tabs[active].uid != edit.tab_uid {
            return;
        }
        let (added, removed) = diff::stats(&edit.diff);
        let (mut accept, mut reject) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        Frame::NONE
            .fill(theme::PANEL_BG())
            .stroke((1.0, theme::CLAUDE().gamma_multiply(0.45)))
            .corner_radius(12)
            .inner_margin(Margin::same(10))
            .outer_margin(Margin {
                left: 10,
                right: 10,
                top: 4,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icons::SPARKLE)
                            .color(theme::CLAUDE())
                            .size(15.0),
                    );
                    ui.label(RichText::new("Proposed edit").strong());
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("+{added}"))
                            .color(Color32::from_rgb(0x9a, 0xd8, 0x8a)),
                    );
                    ui.label(
                        RichText::new(format!("−{removed}"))
                            .color(Color32::from_rgb(0xf0, 0x8a, 0x8a)),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        reject |= ui.button("Reject   Esc").clicked();
                        let button = egui::Button::new(
                            RichText::new("Accept   Ctrl+Enter").color(Color32::WHITE),
                        )
                        .fill(theme::STRING().gamma_multiply(0.45));
                        accept |= ui.add(button).clicked();
                    });
                });
                ui.add_space(6.0);
                // The diff sits in its own inset box, clear of the header row.
                Frame::NONE
                    .fill(Color32::from_black_alpha(90))
                    .corner_radius(8)
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        diff::show(ui, &edit.diff, "inline_edit", 240.0);
                    });
            });

        if accept {
            let edit = self.ai.inline.take().unwrap();
            let tab = &mut self.tabs[active];
            if tab.buf.revision != edit.revision {
                self.status = "The file changed since the proposal was made; ask again".into();
                return;
            }
            tab.buf.set_cursor(edit.start, false);
            tab.buf.set_cursor(edit.end, true);
            tab.buf.insert(&edit.new);
            tab.view.request_focus = true;
            self.ai.inline_prompt = None;
        } else if reject {
            self.ai.inline = None;
        }
    }

    // ---- @ mentions ----

    /// Context for the `@` mentions in a message: the selection, the terminal, file paths.
    pub(super) fn mention_context(&self, text: &str) -> Option<String> {
        let mut parts = Vec::new();
        let mut files = Vec::new();
        for word in text
            .split_whitespace()
            .filter(|w| w.starts_with('@') && w.len() > 1)
        {
            let name = word[1..].trim_end_matches([',', '.', ';', ':', ')', '?', '!']);
            match name {
                "selection" => {
                    if let Some(index) = self.docked_active() {
                        let (start, end, code) = self.selection_or_line(index);
                        parts.push(format!(
                            "Selected code from {} (lines {}-{}):\n```\n{code}\n```",
                            self.tabs[index].path,
                            start.line + 1,
                            end.line + 1
                        ));
                    }
                }
                "terminal" => {
                    if let Some(term) = self.terms.get(self.active_term) {
                        parts.push(format!(
                            "Current terminal contents:\n```\n{}\n```",
                            term.visible_text()
                        ));
                    }
                }
                "" => {}
                path if path.starts_with('/') => files.push(path.to_string()),
                path => files.push(join(&self.root, path)),
            }
        }
        if !files.is_empty() {
            parts.push(format!(
                "Files the user refers to (read them as needed): {}",
                files.join(", ")
            ));
        }
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    /// Looks up files for the mention being typed in the side chat, or in an agent's chat.
    pub(super) fn find_files(&mut self, query: String, agent: Option<u64>) {
        let request = Request::FindFiles {
            root: self.root.clone(),
            query: query.clone(),
        };
        self.request_then(request, Pending::Mentions { query, agent });
    }

    pub(super) fn mentions_found(&mut self, query: &str, agent: Option<u64>, paths: Vec<String>) {
        let chat = match agent {
            None => Some(&mut self.chat),
            Some(uid) => self
                .agents
                .iter_mut()
                .find(|a| a.uid == uid)
                .map(|a| &mut a.chat),
        };
        if let Some(chat) = chat {
            chat.set_suggestions(query, paths);
        }
    }

    pub(super) fn add_path_to_chat(&mut self, path: &str) {
        let prefix = format!("{}/", self.root.trim_end_matches('/'));
        let relative = path.strip_prefix(&prefix).unwrap_or(path);
        self.chat.add_to_input(&format!("@{relative}"));
        self.show_chat = true;
    }

    // ---- changes ----

    /// Every edit Claude made, in the side chat and by each agent, with who made it.
    fn all_changes(&self) -> Vec<(String, chat::Change)> {
        let side = self
            .chat
            .changes
            .iter()
            .map(|c| ("Claude".to_string(), c.clone()));
        let team = self
            .agents
            .iter()
            .flat_map(|a| a.chat.changes.iter().map(|c| (a.name.clone(), c.clone())));
        side.chain(team).collect()
    }

    pub(super) fn changes_panel(&mut self, ui: &mut Ui) {
        let (mut clear, mut commit) = (false, false);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("CHANGES")
                    .small()
                    .strong()
                    .color(theme::COMMENT()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                clear = icon_button(
                    ui,
                    icons::CLEAR,
                    "Clear this list (the files stay as they are)",
                );
                commit = icon_button(
                    ui,
                    icons::COMMIT,
                    "Write a commit message from the git diff",
                );
            });
        });
        let changes = self.all_changes();
        if changes.is_empty() {
            ui.label(
                RichText::new("Edits Claude makes to files are listed here.")
                    .small()
                    .color(theme::COMMENT()),
            );
        }

        let prefix = format!("{}/", self.root.trim_end_matches('/'));
        let (mut open, mut revert, mut goto) = (None, None, None);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            // Newest first.
            for (n, (who, change)) in changes.iter().enumerate().rev() {
                let shown = change.path.strip_prefix(&prefix).unwrap_or(&change.path);
                let (icon, color) = icons::for_entry(file_name(&change.path), false, false);
                let mut clicked_button = false;
                let row = Frame::NONE
                    .fill(theme::tint(5))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(9, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if !change.whole_file
                                    && !change.new.is_empty()
                                    && icon_button(ui, icons::DISCARD, "Revert this edit")
                                {
                                    revert = Some(n);
                                    clicked_button = true;
                                }
                                if icon_button(ui, icons::EDIT, "Open the file") {
                                    goto = Some(n);
                                    clicked_button = true;
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    let label = pieces(&[
                                        (&format!("{icon}  "), color),
                                        (shown, theme::TEXT()),
                                    ]);
                                    ui.add(egui::Label::new(label).selectable(false));
                                });
                            });
                        });
                        let summary = if change.whole_file {
                            format!(
                                "written whole · {} lines · {who}",
                                change.new.lines().count()
                            )
                        } else {
                            format!(
                                "+{}  −{}  ·  {who}",
                                change.new.lines().count(),
                                change.old.lines().count()
                            )
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(summary).small().color(theme::COMMENT()),
                            )
                            .selectable(false),
                        );
                    })
                    .response;
                if row.contains_pointer() {
                    ui.painter().rect_stroke(
                        row.rect,
                        10,
                        (1.0, theme::tint(50)),
                        egui::StrokeKind::Inside,
                    );
                    if !clicked_button && ui.input(|i| i.pointer.primary_clicked()) {
                        open = Some(n);
                    }
                }
            }
        });

        if let Some(n) = open {
            let (who, change) = &changes[n];
            let lines = diff::lines(&change.old, &change.new);
            let title = format!("{} · {who}", change.path);
            self.push_preview_tab(title, preview::Preview::Diff(lines));
        }
        if let Some(n) = goto {
            self.open_file(changes[n].1.path.clone(), None);
        }
        if let Some(n) = revert {
            let change = changes[n].1.clone();
            let path = change.path.clone();
            self.request_then(Request::ReadFile { path }, Pending::Revert { change });
        }
        if clear {
            self.chat.changes.clear();
            for agent in &mut self.agents {
                agent.chat.changes.clear();
            }
        }
        if commit {
            self.commit_message();
        }
    }

    /// Proposes a commit message for the uncommitted changes in the open folder.
    pub(super) fn commit_message(&mut self) {
        self.status = "Reading the git diff…".into();
        self.start_shell_job(JobKind::GitDiff, "git diff HEAD 2>/dev/null".into());
    }

    pub(super) fn push_preview_tab(&mut self, path: String, preview: preview::Preview) {
        if let Some(index) = self.tabs.iter().position(|t| t.path == path) {
            self.tabs[index].preview = Some(preview);
            if !self.tabs[index].detached {
                self.active = index;
            }
            return;
        }
        self.tabs.push(Tab {
            uid: self.next_uid,
            path,
            buf: Buffer::new(""),
            view: EditorState::default(),
            preview: Some(preview),
            pane: self.focus_pane(),
            detached: false,
            place_at: None,
            float: Float::default(),
            edited: (0, 0.0, 0),
        });
        self.next_uid += 1;
        self.active = self.tabs.len() - 1;
    }

    /// Undoes one edit in the file as it is now, if the edited text can still be found.
    pub(super) fn apply_revert(&mut self, change: chat::Change, data: Vec<u8>) {
        let Ok(text) = String::from_utf8(data) else {
            self.status = "Can't revert: the file isn't text any more".into();
            return;
        };
        if text.matches(&change.new).count() != 1 {
            self.status =
                "Can't revert automatically: that text was edited again, or appears more than once"
                    .into();
            return;
        }
        let restored = text.replacen(&change.new, &change.old, 1);
        let request = Request::WriteFile {
            path: change.path.clone(),
            data: restored.into_bytes(),
        };
        self.request_then(request, Pending::Reverted { change });
    }

    pub(super) fn forget_change(&mut self, change: &chat::Change) {
        let same =
            |c: &chat::Change| c.path == change.path && c.old == change.old && c.new == change.new;
        let lists = std::iter::once(&mut self.chat.changes)
            .chain(self.agents.iter_mut().map(|a| &mut a.chat.changes));
        for list in lists {
            if let Some(index) = list.iter().position(same) {
                list.remove(index);
                return;
            }
        }
    }

    // ---- AI search ----

    pub(super) fn ai_search(&mut self) {
        let query = self.search.query.trim().to_string();
        if query.is_empty() || self.search.ai_busy {
            return;
        }
        self.search.hits.clear();
        self.search.ai_busy = true;
        let prompt = format!(
            "Search the project in the current directory for: {query}\n\
             Use your search and read tools; do not change anything. Reply with ONLY up to 15 \
             lines, most relevant first, each in exactly this form and nothing else:\n\
             <path relative to the current directory>:<line number> — <short reason>"
        );
        self.start_claude_job(
            JobKind::AiSearch,
            prompt,
            &settings::with(|s| s.search_model.clone()),
        );
    }

    // ---- team ----

    /// The header above an agent's chat: who it is, how it is doing, and the switch
    /// to the team graph. Returns true while the graph is shown.
    pub(super) fn team_header(&mut self, ui: &mut Ui) -> bool {
        let agent = &self.agents[self.active_agent];
        let model = if agent.chat.model_choice.is_empty() {
            "default model"
        } else {
            agent.chat.model_choice
        };
        let state = if agent.chat.busy {
            "working"
        } else if agent.chat.proc.is_some() {
            "idle"
        } else {
            "not started"
        };
        let details = format!(
            "{model}  ·  {state}  ·  {} turns  ·  ${:.2}",
            agent.chat.turns,
            agent.chat.cost()
        );
        let (name, model_choice, busy) =
            (agent.name.clone(), agent.chat.model_choice, agent.chat.busy);

        Frame::NONE
            .inner_margin(Margin {
                left: 18,
                right: 14,
                top: 12,
                bottom: 8,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if self.ai.team_view {
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Team").size(18.0).color(theme::STRONG()));
                            ui.label(
                                RichText::new("Who created whom. Click an agent to open its chat.")
                                    .small()
                                    .color(theme::COMMENT()),
                            );
                        });
                    } else {
                        let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                        avatar(ui, rect, &name, model_choice, busy);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(RichText::new(&name).size(17.0).color(theme::STRONG()));
                            ui.label(RichText::new(details).small().color(theme::COMMENT()));
                        });
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // A two-way switch, drawn as one control.
                        Frame::NONE
                            .fill(theme::tint(10))
                            .corner_radius(8)
                            .inner_margin(Margin::same(2))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                ui.selectable_value(
                                    &mut self.ai.team_view,
                                    true,
                                    format!("{}  Team", icons::TASKS),
                                );
                                ui.selectable_value(
                                    &mut self.ai.team_view,
                                    false,
                                    format!("{}  Chat", icons::SPARKLE),
                                );
                            });
                    });
                });
            });
        ui.painter().hline(
            ui.max_rect().x_range(),
            ui.cursor().top(),
            (1.0, theme::LINE()),
        );
        self.ai.team_view
    }

    /// Who delegated to whom: each agent a card, under the agent that created it.
    pub(super) fn team_view(&mut self, ui: &mut Ui) {
        const NODE: egui::Vec2 = vec2(236.0, 92.0);
        const GAP: egui::Vec2 = vec2(28.0, 58.0);

        let mut rows: Vec<Vec<usize>> = Vec::new();
        for (index, depth) in self.agent_order() {
            if rows.len() <= depth {
                rows.resize(depth + 1, Vec::new());
            }
            rows[depth].push(index);
        }
        let widest = rows.iter().map(Vec::len).max().unwrap_or(1) as f32;
        let mut select = None;

        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let size = vec2(
                (widest * (NODE.x + GAP.x) + 48.0).max(ui.available_width()),
                (rows.len() as f32 * (NODE.y + GAP.y) + 48.0).max(ui.available_height()),
            );
            let (area, _) = ui.allocate_exact_size(size, Sense::hover());
            let mut rects = vec![egui::Rect::NOTHING; self.agents.len()];
            for (depth, row) in rows.iter().enumerate() {
                let row_width = row.len() as f32 * NODE.x + (row.len() as f32 - 1.0) * GAP.x;
                let left = area.center().x - row_width / 2.0;
                let top = area.top() + 30.0 + depth as f32 * (NODE.y + GAP.y);
                for (n, index) in row.iter().enumerate() {
                    let min = egui::pos2(left + n as f32 * (NODE.x + GAP.x), top);
                    rects[*index] = egui::Rect::from_min_size(min, NODE);
                }
            }

            // Links first, so the cards sit on top of them.
            for (index, agent) in self.agents.iter().enumerate() {
                let parent = agent
                    .parent
                    .and_then(|uid| self.agents.iter().position(|a| a.uid == uid));
                if let Some(parent) = parent {
                    let (from, to) = (rects[parent].center_bottom(), rects[index].center_top());
                    let mid = (from.y + to.y) / 2.0;
                    let color = if agent.chat.busy {
                        model_color(agent.chat.model_choice)
                    } else {
                        theme::tint(60)
                    };
                    let curve = egui::epaint::CubicBezierShape::from_points_stroke(
                        [from, egui::pos2(from.x, mid), egui::pos2(to.x, mid), to],
                        false,
                        Color32::TRANSPARENT,
                        (1.5, color),
                    );
                    ui.painter().add(curve);
                }
            }
            for (index, agent) in self.agents.iter().enumerate() {
                let rect = rects[index];
                let resp =
                    ui.interact(rect, ui.id().with(("team_node", agent.uid)), Sense::click());
                let busy = agent.chat.busy;
                let color = model_color(agent.chat.model_choice);
                let border = if busy {
                    color
                } else if resp.hovered() {
                    theme::tint(90)
                } else {
                    theme::LINE()
                };
                let fill = if index == self.active_agent {
                    theme::tint(16)
                } else {
                    theme::tint(7)
                };
                ui.painter()
                    .rect(rect, 12, fill, (1.0, border), egui::StrokeKind::Inside);

                let badge =
                    egui::Rect::from_min_size(rect.min + vec2(14.0, 14.0), vec2(34.0, 34.0));
                avatar(ui, badge, &agent.name, agent.chat.model_choice, busy);
                let text =
                    |pos: egui::Pos2, align: Align2, text: String, size: f32, color: Color32| {
                        ui.painter()
                            .text(pos, align, text, FontId::proportional(size), color);
                    };
                let left = rect.min.x + 60.0;
                text(
                    egui::pos2(left, rect.min.y + 13.0),
                    Align2::LEFT_TOP,
                    agent.name.clone(),
                    15.0,
                    theme::STRONG(),
                );
                let model = if agent.chat.model_choice.is_empty() {
                    "default"
                } else {
                    agent.chat.model_choice
                };
                let state = if busy {
                    "working"
                } else if agent.chat.proc.is_some() {
                    "idle"
                } else {
                    "not started"
                };
                text(
                    egui::pos2(left, rect.min.y + 34.0),
                    Align2::LEFT_TOP,
                    format!("{model}  ·  {state}"),
                    12.0,
                    color,
                );
                let stats = format!(
                    "{} turns   {} sent   ${:.2}",
                    agent.chat.turns,
                    agent.sent,
                    agent.chat.cost()
                );
                text(
                    egui::pos2(rect.min.x + 14.0, rect.max.y - 12.0),
                    Align2::LEFT_BOTTOM,
                    stats,
                    12.0,
                    theme::COMMENT(),
                );
                if resp.clicked() {
                    select = Some(index);
                }
            }
        });

        if let Some(index) = select {
            self.active_agent = index;
            self.ai.team_view = false;
        }
    }

    // ---- scheduled tasks ----

    pub(super) fn scheduled_section(&mut self, ui: &mut Ui) {
        let now = ui.input(|i| i.time);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("SCHEDULED")
                    .small()
                    .strong()
                    .color(theme::COMMENT()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, icons::ADD, "Run a prompt on a timer") {
                    self.dialog = Some(Dialog::Schedule {
                        prompt: String::new(),
                        minutes: "60".into(),
                    });
                }
            });
        });
        if self.ai.scheduled.is_empty() {
            ui.label(
                RichText::new("Run a prompt every few minutes, while ride is open and connected.")
                    .small()
                    .color(theme::COMMENT()),
            );
        }
        let mut remove = None;
        for (index, task) in self.ai.scheduled.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, icons::CLOSE, "Remove") {
                        remove = Some(index);
                    }
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        let first: String = task
                            .prompt
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(60)
                            .collect();
                        ui.add(egui::Label::new(first).selectable(false));
                        let wait = ((task.next - now) / 60.0).ceil().max(0.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "every {} min · next in {wait:.0} min",
                                    task.every_min
                                ))
                                .small()
                                .color(theme::COMMENT()),
                            )
                            .selectable(false),
                        );
                    });
                });
            });
        }
        if let Some(index) = remove {
            self.ai.scheduled.remove(index);
        }
    }

    pub(super) fn add_scheduled(&mut self, prompt: String, minutes: &str, now: f64) {
        let every_min = minutes.trim().parse::<u32>().unwrap_or(0);
        if prompt.trim().is_empty() || every_min == 0 {
            self.status = "A scheduled task needs a prompt and a number of minutes".into();
            return;
        }
        let next = now + every_min as f64 * 60.0;
        self.ai.scheduled.push(Scheduled {
            prompt: prompt.trim().to_string(),
            every_min,
            next,
            agent: None,
        });
    }

    pub(super) fn scheduled_json(&self) -> serde_json::Value {
        self.ai
            .scheduled
            .iter()
            .map(|t| serde_json::json!({ "prompt": t.prompt, "every_min": t.every_min, "agent": t.agent }))
            .collect()
    }

    /// Restores scheduled tasks; each first runs one full interval after the app starts.
    pub(super) fn load_scheduled(&mut self, v: &serde_json::Value) {
        self.ai.scheduled = v
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                let every_min = t["every_min"].as_u64()? as u32;
                Some(Scheduled {
                    prompt: t["prompt"].as_str()?.to_string(),
                    every_min,
                    next: every_min as f64 * 60.0,
                    agent: t["agent"].as_u64(),
                })
            })
            .collect();
    }

    /// Starts the scheduled tasks that are due. Each has its own agent in the Agents view.
    pub(super) fn schedule_tick(&mut self, ctx: &Context) {
        if self.ai.scheduled.is_empty() {
            return;
        }
        // Nothing else wakes an idle window up when a task comes due.
        ctx.request_repaint_after(std::time::Duration::from_secs(20));
        if !self.online() {
            return;
        }
        let now = ctx.input(|i| i.time);
        for n in 0..self.ai.scheduled.len() {
            let task = &self.ai.scheduled[n];
            if now < task.next {
                continue;
            }
            let prompt = task.prompt.clone();
            let existing = task
                .agent
                .and_then(|uid| self.agents.iter().position(|a| a.uid == uid));
            let index =
                existing.unwrap_or_else(|| self.new_agent("scheduled", "", chat::Mode::Ask));
            let uid = self.agents[index].uid;
            let task = &mut self.ai.scheduled[n];
            task.agent = Some(uid);
            task.next = now + task.every_min as f64 * 60.0;
            // A run that is still going isn't piled onto; this one is skipped.
            if !self.agents[index].chat.busy {
                self.status = format!(
                    "Scheduled task started in agent {}",
                    self.agents[index].name
                );
                self.agent_user_send(index, prompt);
            }
        }
    }
}

impl App {
    /// A small card in the bottom-right corner that shows what the agents are doing
    /// while the user is in another view. It folds down to a pill.
    pub(super) fn agents_widget(&mut self, ctx: &Context) {
        let busy: Vec<usize> = (0..self.agents.len())
            .filter(|i| self.agents[*i].chat.busy)
            .collect();
        // Remember that work was going on, so its end can be announced once.
        if !busy.is_empty() {
            self.ai.widget_was_busy = true;
            self.ai.widget_finished = false;
        } else if std::mem::take(&mut self.ai.widget_was_busy) {
            self.ai.widget_finished = true;
        }
        if busy.is_empty() && !self.ai.widget_finished {
            return;
        }

        let (mut open, mut toggle, mut dismiss) = (None, false, false);
        egui::Area::new(egui::Id::new("agents_widget"))
            .order(egui::Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, [-16.0, -38.0])
            .show(ctx, |ui| {
                let compact = self.ai.widget_compact;
                Frame::NONE
                    .fill(ui.visuals().window_fill)
                    .stroke((1.0, theme::tint(34)))
                    .corner_radius(if compact { 18 } else { 12 })
                    .shadow(egui::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 0,
                        color: Color32::from_black_alpha(110),
                    })
                    .inner_margin(if compact {
                        Margin::symmetric(12, 6)
                    } else {
                        Margin::same(10)
                    })
                    .show(ui, |ui| {
                        let summary = match busy.len() {
                            0 => "Agents finished".to_string(),
                            1 => "1 agent working".to_string(),
                            n => format!("{n} agents working"),
                        };
                        if compact {
                            // The pill: one line, click to unfold.
                            let pill = ui
                                .horizontal(|ui| {
                                    if busy.is_empty() {
                                        ui.label(
                                            RichText::new(icons::CHECK).color(theme::STRING()),
                                        );
                                    } else {
                                        ui.add(egui::Spinner::new().size(12.0));
                                    }
                                    ui.add(
                                        egui::Label::new(RichText::new(&summary).small())
                                            .selectable(false),
                                    );
                                    let chevron = RichText::new(icons::CHEVRON_UP)
                                        .size(11.0)
                                        .color(theme::COMMENT());
                                    ui.add(egui::Label::new(chevron).selectable(false));
                                })
                                .response;
                            if pill.contains_pointer() && ui.input(|i| i.pointer.primary_clicked())
                            {
                                toggle = true;
                            }
                            return;
                        }

                        ui.set_width(300.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icons::AGENTS).color(theme::CLAUDE()));
                            ui.add(
                                egui::Label::new(RichText::new(&summary).strong())
                                    .selectable(false),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if busy.is_empty() {
                                    dismiss = icon_button(ui, icons::CLOSE, "Dismiss");
                                }
                                toggle = icon_button(ui, icons::CHEVRON_DOWN, "Fold down");
                            });
                        });
                        ui.add_space(2.0);
                        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                        // Working agents; once they have all finished, everyone, so results are a click away.
                        let listed: Vec<usize> = if busy.is_empty() {
                            (0..self.agents.len()).collect()
                        } else {
                            busy.clone()
                        };
                        for index in listed.into_iter().take(5) {
                            let agent = &self.agents[index];
                            let row = Frame::NONE
                                .corner_radius(8)
                                .inner_margin(Margin::symmetric(6, 5))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        let (badge, _) = ui
                                            .allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                                        avatar(
                                            ui,
                                            badge,
                                            &agent.name,
                                            agent.chat.model_choice,
                                            agent.chat.busy,
                                        );
                                        ui.vertical(|ui| {
                                            ui.spacing_mut().item_spacing.y = 0.0;
                                            let name =
                                                RichText::new(&agent.name).color(theme::STRONG());
                                            ui.add(egui::Label::new(name).selectable(false));
                                            let snippet = agent.chat.snippet();
                                            let activity =
                                                match (agent.chat.busy, snippet.is_empty()) {
                                                    (true, true) => "Working…".to_string(),
                                                    (false, true) => "Nothing yet".to_string(),
                                                    (_, false) => snippet,
                                                };
                                            let activity = RichText::new(activity)
                                                .small()
                                                .color(theme::COMMENT());
                                            ui.add(egui::Label::new(activity).selectable(false));
                                        });
                                    });
                                })
                                .response;
                            if row.contains_pointer() {
                                ui.painter().rect_filled(row.rect, 8, theme::tint(12));
                                if ui.input(|i| i.pointer.primary_clicked()) {
                                    open = Some(index);
                                }
                            }
                        }
                        if busy.len() > 5 {
                            let more = RichText::new(format!("and {} more", busy.len() - 5))
                                .small()
                                .color(theme::COMMENT());
                            ui.label(more);
                        }
                    });
            });

        if let Some(index) = open {
            self.active_agent = index;
            self.view = View::Agents;
            self.ai.widget_finished = false;
        } else if dismiss {
            self.ai.widget_finished = false;
        } else if toggle {
            self.ai.widget_compact = !self.ai.widget_compact;
        }
        // The avatars pulse and the activity lines change while agents work.
        if !busy.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }
}

impl App {
    /// What ride knows about where the user left off: their chats, the agents and what
    /// they were asked, the files Claude changed. Empty when there is nothing to tell.
    fn catchup_notes(&self) -> String {
        let mut notes = String::new();
        if !self.chat.is_empty() {
            notes.push_str(&format!(
                "## The chat that was open\n{}\n\n",
                self.chat.digest(14)
            ));
        }
        for (title, outline) in self.chat.history_digest(6) {
            notes.push_str(&format!("## An earlier chat: {title}\n{outline}\n\n"));
        }
        for agent in &self.agents {
            if agent.chat.is_empty() {
                continue;
            }
            let model = if agent.chat.model_choice.is_empty() {
                "default"
            } else {
                agent.chat.model_choice
            };
            let state = if agent.chat.awaiting_answer() {
                "its last request has no answer: it was interrupted or never ran"
            } else {
                "it answered its last request"
            };
            notes.push_str(&format!(
                "## Agent {} (model: {model}; {state})\n{}\n\n",
                agent.name,
                agent.chat.digest(8)
            ));
        }
        let changes = self.all_changes();
        if !changes.is_empty() {
            notes.push_str("## Files Claude edited (oldest first)\n");
            let skip = changes.len().saturating_sub(20);
            for (who, change) in &changes[skip..] {
                notes.push_str(&format!("{} (by {who})\n", change.path));
            }
            notes.push('\n');
        }
        if !self.ai.scheduled.is_empty() {
            notes.push_str("## Scheduled tasks\n");
            for task in &self.ai.scheduled {
                notes.push_str(&format!("every {} min: {}\n", task.every_min, task.prompt));
            }
        }
        notes
    }

    /// Opens the catch-up screen and asks Claude on the connected host to write it.
    /// Does nothing when there is no earlier work to catch up on.
    pub(super) fn start_catchup(&mut self) {
        let notes = self.catchup_notes();
        if notes.trim().is_empty() {
            return;
        }
        let notes: String = notes.chars().take(14_000).collect();
        let prompt = format!(
            "The user is coming back to their code editor. Write a short catch-up for them from the notes below, \
             which are outlines of their earlier conversations with you and your agents.\n\n\
             Use these sections as Markdown headings, in this order, and leave out any that would be empty:\n\
             ## Where you left off\n## Recent chats\n## What got done\n## Finished while you were away\n## Still open\n\n\
             \"Finished while you were away\" is for tasks the user handed to an agent that now have an answer. \
             \"Still open\" is for questions waiting on the user, tasks that were interrupted or never ran, and things worth checking.\n\
             Two to five short bullets per section, plain words, with file names where they help. \
             Say only what the notes support. Do not use tools. Write in the language the user writes in.\n\n\
             Host: {}\nFolder: {}\n\n<notes>\n{notes}\n</notes>",
            self.host.trim(),
            self.root
        );
        self.ai.catchup = Some(CatchUp {
            text: String::new(),
            done: false,
        });
        self.start_claude_job(
            JobKind::CatchUp,
            prompt,
            &settings::with(|s| s.catchup_model.clone()),
        );
    }

    pub(super) fn catching_up(&self) -> bool {
        self.ai.catchup.is_some()
    }

    fn close_catchup(&mut self) {
        self.ai.catchup = None;
        // If Claude is still writing, it is stopped.
        if let Some(index) = self
            .ai
            .jobs
            .iter()
            .position(|j| matches!(j.kind, JobKind::CatchUp))
        {
            let job = self.ai.jobs.remove(index);
            self.request(Request::ProcClose { proc: job.proc });
        }
    }

    /// The catch-up screen. It takes the whole window; nothing else is drawn under it.
    pub(super) fn catchup_screen(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, theme::EDITOR_BG());
        let Some(catchup) = &self.ai.catchup else {
            return;
        };
        let (text, done) = (catchup.text.clone(), catchup.done);

        // The window can still be moved, minimized and closed from here.
        Panel::top("catchup_title")
            .frame(Frame::NONE.inner_margin(Margin {
                left: 14,
                right: 0,
                top: 3,
                bottom: 3,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                title_drag(ui);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new("ride").strong().color(theme::COMMENT()))
                            .selectable(false),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        window_buttons(ui)
                    });
                });
            });

        let mut close = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        if done {
            close |= ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        }
        Panel::bottom("catchup_buttons")
            .frame(Frame::NONE.inner_margin(Margin {
                left: 24,
                right: 24,
                top: 10,
                bottom: 22,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let (label, hint) = if done {
                        ("Continue", "Enter")
                    } else {
                        ("Skip", "Esc")
                    };
                    let button = egui::Button::new(
                        RichText::new(label).size(15.0).color(theme::EDITOR_BG()),
                    )
                    .fill(if done {
                        theme::TEXT()
                    } else {
                        theme::COMMENT()
                    })
                    .corner_radius(8)
                    .min_size(vec2(150.0, 36.0));
                    close |= ui.add(button).on_hover_text(hint).clicked();
                });
            });

        CentralPanel::default_margins()
            .frame(Frame::NONE)
            .show(ui, |ui| {
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    // One readable column in the middle, however wide the window is.
                    let width = (ui.available_width() - 48.0).min(720.0);
                    let side = ((ui.available_width() - width) / 2.0).max(0.0);
                    ui.horizontal(|ui| {
                        ui.add_space(side);
                        ui.vertical(|ui| {
                            ui.set_width(width);
                            ui.add_space((ui.ctx().content_rect().height() * 0.07).max(12.0));
                            ui.label(
                                RichText::new(icons::SPARKLE)
                                    .size(30.0)
                                    .color(theme::CLAUDE()),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("Let's catch up")
                                    .size(32.0)
                                    .color(theme::STRONG()),
                            );
                            let place = format!("{}  ·  {}", self.host.trim(), self.root);
                            ui.label(RichText::new(place).color(theme::COMMENT()));
                            ui.add_space(20.0);
                            if text.trim().is_empty() {
                                writing_placeholder(ui, width);
                            } else {
                                ui.spacing_mut().item_spacing.y = 10.0;
                                chat::markdown(ui, &text, usize::MAX);
                            }
                            ui.add_space(24.0);
                        });
                    });
                });
            });
        resize_edges(&ctx);

        if close {
            self.close_catchup();
        } else if !done {
            ctx.request_repaint_after(std::time::Duration::from_millis(40));
        }
    }
}

/// Stand-in lines with a light sweeping across them, while Claude hasn't written anything yet.
fn writing_placeholder(ui: &mut Ui, width: f32) {
    ui.label(RichText::new("Claude is looking at where you left off…").color(theme::COMMENT()));
    ui.add_space(14.0);
    let time = ui.input(|i| i.time) as f32;
    for (row, part) in [0.42, 0.92, 0.78, 0.86, 0.5, 0.9, 0.66]
        .into_iter()
        .enumerate()
    {
        let (rect, _) = ui.allocate_exact_size(
            vec2(width * part, if row % 4 == 0 { 16.0 } else { 11.0 }),
            Sense::hover(),
        );
        ui.painter().rect_filled(rect, 5, theme::tint(12));
        let sweep = ((time * 0.7 - row as f32 * 0.06).fract() * 1.4 - 0.2) * width;
        let glow = egui::Rect::from_min_size(
            egui::pos2(rect.left() + sweep, rect.top()),
            vec2(90.0, rect.height()),
        )
        .intersect(rect);
        if glow.is_positive() {
            ui.painter().rect_filled(glow, 5, theme::tint(16));
        }
        ui.add_space(if row % 4 == 3 { 14.0 } else { 2.0 });
    }
}
