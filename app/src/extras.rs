//! Smaller conveniences around the editor: uploading files, restoring the last session,
//! desktop notifications, and team templates for the Agents view.

use super::*;

// ---- uploads ----

/// Files larger than this are skipped; a file travels as a single message.
const MAX_UPLOAD_FILE: u64 = 200 * 1024 * 1024;
const MAX_UPLOAD_FILES: usize = 5000;

enum UploadItem {
    /// An empty folder, to be created as such.
    Dir(String),
    File(String, Vec<u8>),
    Skipped,
    Done,
}

pub(super) struct Upload {
    rx: Receiver<UploadItem>,
    /// The folder on the host the files go into.
    dir: String,
    files: usize,
    skipped: usize,
}

/// Reads `path` (a file, or a folder with everything in it) and hands it over piece by piece.
fn read_for_upload(path: &Path, relative: String, tx: &Sender<UploadItem>, count: &mut usize) {
    if *count >= MAX_UPLOAD_FILES {
        let _ = tx.send(UploadItem::Skipped);
        return;
    }
    if path.is_dir() {
        let entries: Vec<_> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .collect();
        if entries.is_empty() {
            let _ = tx.send(UploadItem::Dir(relative));
            return;
        }
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            read_for_upload(&entry.path(), format!("{relative}/{name}"), tx, count);
        }
        return;
    }
    let small_enough = path.metadata().is_ok_and(|m| m.len() <= MAX_UPLOAD_FILE);
    match std::fs::read(path).ok().filter(|_| small_enough) {
        Some(data) => {
            *count += 1;
            let _ = tx.send(UploadItem::File(relative, data));
        }
        None => {
            let _ = tx.send(UploadItem::Skipped);
        }
    }
}

// ---- session ----

/// A tab to reopen when the host it was on is connected again.
pub(super) struct SavedTab {
    pos: Pos,
    /// The text of a tab that had unsaved edits, so they survive a restart.
    unsaved: Option<String>,
}

// ---- team templates ----

#[derive(Clone)]
pub(super) struct TeamAgent {
    name: String,
    /// A model alias, or empty for Claude Code's default.
    model: String,
    /// What this agent is for, added to its instructions.
    role: String,
    /// The name of the agent it reports to, for the team graph.
    parent: Option<String>,
}

#[derive(Clone)]
pub(super) struct Team {
    name: String,
    agents: Vec<TeamAgent>,
    built_in: bool,
}

pub(super) enum TeamAction {
    Apply(Team),
    Remove(String),
    SaveCurrent,
}

fn member(name: &str, model: &str, role: &str, parent: Option<&str>) -> TeamAgent {
    TeamAgent {
        name: name.into(),
        model: model.into(),
        role: role.into(),
        parent: parent.map(str::to_string),
    }
}

fn built_in_teams() -> Vec<Team> {
    let lead = "lead";
    vec![
        Team {
            name: "Build crew".into(),
            built_in: true,
            agents: vec![
                member(lead, "opus", "Plan the work, split it between the coders, and check what comes back before reporting to the user.", None),
                member("coder-1", "sonnet", "Implement what the lead assigns. Report exactly what you changed and what you could not do.", Some(lead)),
                member("coder-2", "sonnet", "Implement what the lead assigns. Report exactly what you changed and what you could not do.", Some(lead)),
                member("reviewer", "sonnet", "Review changes for bugs and regressions when asked. Read the code; do not edit it.", Some(lead)),
            ],
        },
        Team {
            name: "Research".into(),
            built_in: true,
            agents: vec![
                member(lead, "sonnet", "Break the question down, send the scouts to look, and combine what they find into one answer.", None),
                member("scout-1", "haiku", "Find and summarise what the lead asks about. Quote file paths and line numbers. Do not edit anything.", Some(lead)),
                member("scout-2", "haiku", "Find and summarise what the lead asks about. Quote file paths and line numbers. Do not edit anything.", Some(lead)),
            ],
        },
        Team {
            name: "Solo".into(),
            built_in: true,
            agents: vec![member(lead, "", "Do the work yourself; bring in a helper only when it clearly pays off.", None)],
        },
    ]
}

fn teams_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("ride").join("teams.json"))
}

pub(super) fn load_teams() -> Vec<Team> {
    let text = teams_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let Ok(serde_json::Value::Array(teams)) = serde_json::from_str::<serde_json::Value>(&text)
    else {
        return Vec::new();
    };
    teams
        .iter()
        .filter_map(|t| {
            let agents = t["agents"]
                .as_array()?
                .iter()
                .filter_map(|a| {
                    Some(TeamAgent {
                        name: a["name"].as_str()?.to_string(),
                        model: a["model"].as_str().unwrap_or("").to_string(),
                        role: a["role"].as_str().unwrap_or("").to_string(),
                        parent: a["parent"].as_str().map(str::to_string),
                    })
                })
                .collect();
            Some(Team {
                name: t["name"].as_str()?.to_string(),
                agents,
                built_in: false,
            })
        })
        .collect()
}

fn save_teams(teams: &[Team]) -> std::io::Result<()> {
    let Some(path) = teams_path() else {
        return Ok(());
    };
    let json: Vec<serde_json::Value> = teams
        .iter()
        .map(|team| {
            let agents: Vec<serde_json::Value> = team
                .agents
                .iter()
                .map(|a| serde_json::json!({ "name": a.name, "model": a.model, "role": a.role, "parent": a.parent }))
                .collect();
            serde_json::json!({ "name": team.name, "agents": agents })
        })
        .collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::Value::Array(json).to_string())
}

/// Days since 1970, to tell one day from the next.
pub(super) fn today() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400)
}

impl App {
    // ---- uploads ----

    /// Copies files and folders from this computer into `dir` on the host.
    pub(super) fn upload_into(&mut self, dir: String, paths: Vec<PathBuf>) {
        if !self.online() || paths.is_empty() {
            return;
        }
        if self.upload.is_some() {
            self.status = "An upload is already running; wait for it to finish".into();
            return;
        }
        let (tx, rx) = channel();
        thread::spawn(move || {
            let mut count = 0;
            for path in paths {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                read_for_upload(&path, name, &tx, &mut count);
            }
            let _ = tx.send(UploadItem::Done);
        });
        self.status = format!("Uploading into {}…", file_name(&dir));
        self.upload = Some(Upload {
            rx,
            dir,
            files: 0,
            skipped: 0,
        });
    }

    pub(super) fn pick_and_upload(&mut self, dir: String) {
        let picked = rfd::FileDialog::new()
            .set_title(format!("Upload into {}", file_name(&dir)))
            .pick_files();
        if let Some(paths) = picked {
            self.upload_into(dir, paths);
        }
    }

    /// Takes files dropped on the window, and moves a running upload along.
    pub(super) fn poll_uploads(&mut self, ctx: &Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .filter(|path| path.exists())
                .collect()
        });
        // Files dropped on the window are for the connection shown, not for one behind it.
        if !dropped.is_empty() && self.online() && !self.background {
            // Dropped files go where the user is looking: the open folder.
            let dir = self.root.clone();
            self.upload_into(dir, dropped);
        }
        // A listing asked for right after the last file would race with the writes.
        if let Some((at, dir)) = self.refresh_later.clone() {
            if ctx.input(|i| i.time) >= at {
                self.refresh_later = None;
                self.sync_tree();
                self.load_dir(&dir);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }

        let Some(upload) = &mut self.upload else {
            return;
        };
        let mut requests = Vec::new();
        let mut done = false;
        // A batch per frame keeps the window responsive during a large upload.
        for item in upload.rx.try_iter().take(48) {
            match item {
                UploadItem::Dir(relative) => requests.push(Request::CreateDir {
                    path: join(&upload.dir, &relative),
                }),
                UploadItem::File(relative, data) => {
                    upload.files += 1;
                    requests.push(Request::WriteFile {
                        path: join(&upload.dir, &relative),
                        data,
                    });
                }
                UploadItem::Skipped => upload.skipped += 1,
                UploadItem::Done => done = true,
            }
        }
        let progress = format!("Uploading… {} files", upload.files);
        for request in requests {
            self.request(request);
        }
        if done {
            let upload = self.upload.take().unwrap();
            let skipped = if upload.skipped > 0 {
                format!(
                    ", {} skipped (unreadable, over 200 MB, or past the 5000-file limit)",
                    upload.skipped
                )
            } else {
                String::new()
            };
            self.status = format!(
                "Uploaded {} files into {}{skipped}",
                upload.files, upload.dir
            );
            self.refresh_later = Some((ctx.input(|i| i.time) + 0.8, upload.dir));
        } else {
            self.status = progress;
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    /// Reloads the folder listings that are open in the tree.
    pub(super) fn sync_tree(&mut self) {
        let dirs: Vec<String> = std::iter::once(self.root.clone())
            .chain(self.expanded.iter().cloned())
            .collect();
        for dir in dirs {
            self.load_dir(&dir);
        }
    }

    // ---- session ----

    /// The open tabs, for reopening them after a restart.
    pub(super) fn session_json(&self) -> serde_json::Value {
        let tabs: Vec<serde_json::Value> = self
            .tabs
            .iter()
            // Diff tabs and the like aren't files; there is nothing to reopen.
            .filter(|tab| tab.path.starts_with('/') || tab.path.chars().nth(1) == Some(':'))
            .filter(|tab| !tab.path.contains(" · "))
            .map(|tab| {
                let unsaved = (tab.buf.is_dirty() && tab.preview.is_none()).then(|| tab.buf.text());
                serde_json::json!({
                    "path": tab.path,
                    "line": tab.buf.cursor.line,
                    "col": tab.buf.cursor.col,
                    "unsaved": unsaved,
                })
            })
            .collect();
        serde_json::json!({
            "host": self.host.trim(),
            "tabs": tabs,
            "active": self.docked_active().map(|i| self.tabs[i].path.clone()),
        })
    }

    pub(super) fn load_session(&mut self, v: &serde_json::Value) {
        self.restore_host = v["host"].as_str().unwrap_or("").to_string();
        self.restore_active = v["active"].as_str().map(str::to_string);
        self.restore_order = Vec::new();
        for tab in v["tabs"].as_array().into_iter().flatten() {
            let Some(path) = tab["path"].as_str() else {
                continue;
            };
            let pos = Pos::new(
                tab["line"].as_u64().unwrap_or(0) as usize,
                tab["col"].as_u64().unwrap_or(0) as usize,
            );
            let unsaved = tab["unsaved"].as_str().map(str::to_string);
            self.restore
                .insert(path.to_string(), SavedTab { pos, unsaved });
            self.restore_order.push(path.to_string());
        }
    }

    /// Reopens the tabs of the last session, if this is the host they were on.
    pub(super) fn restore_session(&mut self) {
        if self.host.trim() != self.restore_host {
            self.restore.clear();
            return;
        }
        for path in std::mem::take(&mut self.restore_order) {
            if let Some(saved) = self.restore.get(&path) {
                self.open_file(path, Some(saved.pos));
            }
        }
    }

    /// Finishes restoring a tab that has just been opened: its unsaved text, and its turn as the active tab.
    pub(super) fn restored_tab_opened(&mut self, index: usize) {
        let path = self.tabs[index].path.clone();
        if let Some(saved) = self.restore.remove(&path) {
            if let Some(text) = saved.unsaved {
                let tab = &mut self.tabs[index];
                tab.buf = Buffer::new(&text);
                tab.buf.mark_dirty();
                tab.buf.set_cursor(saved.pos, false);
                tab.view = EditorState::default();
                tab.view.reveal_cursor = true;
            }
        }
        // Files arrive in any order; the tab that was active stays in front once it is there.
        if let Some(wanted) = &self.restore_active {
            if let Some(active) = self.tabs.iter().position(|t| t.path == *wanted) {
                self.active = active;
            }
            if self.restore.is_empty() {
                self.restore_active = None;
            }
        }
    }

    // ---- catch-up, once a day ----

    pub(super) fn daily_catchup(&mut self) {
        let today = today();
        // A window opened to look at one panel isn't a session to catch up on.
        if std::env::var_os("RIDE_SIDE").is_some() {
            return;
        }
        match settings::with(|s| s.catchup) {
            settings::CatchUp::Off => {}
            settings::CatchUp::EveryConnect => self.start_catchup(),
            settings::CatchUp::Daily if self.catchup_day != today => {
                self.catchup_day = today;
                self.start_catchup();
            }
            settings::CatchUp::Daily => {}
        }
    }

    // ---- notifications ----

    /// A desktop notification, shown only while ride isn't the window in front.
    pub(super) fn notify(&self, title: &str, body: &str) {
        if self.focused || !settings::with(|s| s.notifications) {
            return;
        }
        #[cfg(windows)]
        {
            let (title, body) = (
                title.to_string(),
                body.chars().take(180).collect::<String>(),
            );
            thread::spawn(move || {
                use tauri_winrt_notification::Toast;
                // Shown under PowerShell's identity: an app needs a registered one of its own
                // to post toasts, and an unregistered one is dropped without a word.
                let _ = Toast::new(Toast::POWERSHELL_APP_ID)
                    .title(&title)
                    .text1(&body)
                    .show();
            });
        }
    }

    // ---- team templates ----

    fn all_teams(&self) -> Vec<Team> {
        built_in_teams()
            .into_iter()
            .chain(self.teams.iter().cloned())
            .collect()
    }

    /// The templates menu in the Agents panel. What was chosen is carried out by `team_action`.
    pub(super) fn teams_menu(&self, ui: &mut Ui) -> Option<TeamAction> {
        let mut action = None;
        ui.menu_button(RichText::new(icons::TASKS).size(13.0), |ui| {
            ui.label(
                RichText::new("Start from a team")
                    .small()
                    .color(theme::COMMENT()),
            );
            for team in self.all_teams() {
                ui.horizontal(|ui| {
                    let models: Vec<&str> = team
                        .agents
                        .iter()
                        .map(|a| {
                            if a.model.is_empty() {
                                "default"
                            } else {
                                a.model.as_str()
                            }
                        })
                        .collect();
                    let label = format!("{}   ({})", team.name, models.join(", "));
                    if ui.button(label).clicked() {
                        action = Some(TeamAction::Apply(team.clone()));
                        ui.close();
                    }
                    if !team.built_in && icon_button(ui, icons::TRASH, "Remove this template") {
                        action = Some(TeamAction::Remove(team.name.clone()));
                        ui.close();
                    }
                });
            }
            ui.separator();
            if ui.button("Save the current team as a template…").clicked() {
                action = Some(TeamAction::SaveCurrent);
                ui.close();
            }
        })
        .response
        .on_hover_text("Team templates");
        action
    }

    pub(super) fn team_action(&mut self, action: TeamAction) {
        match action {
            TeamAction::Apply(team) => self.apply_team(&team),
            TeamAction::Remove(name) => {
                self.teams.retain(|t| t.name != name);
                if let Err(error) = save_teams(&self.teams) {
                    self.status = format!("Couldn't save the templates: {error}");
                }
            }
            TeamAction::SaveCurrent => {
                self.dialog = Some(Dialog::Prompt {
                    title: "Name this team".into(),
                    value: String::new(),
                    action: PromptAction::SaveTeam,
                });
            }
        }
    }

    /// Replaces the current agents with a fresh team from the template.
    fn apply_team(&mut self, team: &Team) {
        let mode = self.agents.first().map_or(chat::Mode::Ask, |a| a.chat.mode);
        for index in 0..self.agents.len() {
            self.stop_agent(index);
        }
        self.agents.clear();
        for member in &team.agents {
            let index = self.new_agent(&member.name, model_alias(&member.model), mode);
            if !member.role.is_empty() {
                let agent = &mut self.agents[index];
                let base = agent.chat.system.take().unwrap_or_default();
                agent.chat.system =
                    Some(format!("{base}\n\nYour role in this team: {}", member.role));
                agent.role = member.role.clone();
            }
        }
        // Parents are linked once everyone exists, since they are named, not numbered.
        for member in &team.agents {
            let parent = member
                .parent
                .as_ref()
                .and_then(|p| self.agents.iter().find(|a| a.name == *p))
                .map(|a| a.uid);
            if let Some(agent) = self.agents.iter_mut().find(|a| a.name == member.name) {
                agent.parent = parent;
            }
        }
        self.active_agent = 0;
        self.status = format!("Team \"{}\" is ready; give the lead a task", team.name);
    }

    pub(super) fn save_team(&mut self, name: &str) {
        let agents = self
            .agents
            .iter()
            .map(|agent| TeamAgent {
                name: agent.name.clone(),
                model: agent.chat.model_choice.to_string(),
                role: agent.role.clone(),
                parent: agent
                    .parent
                    .and_then(|uid| self.agents.iter().find(|a| a.uid == uid))
                    .map(|a| a.name.clone()),
            })
            .collect();
        self.teams.retain(|t| t.name != name);
        self.teams.push(Team {
            name: name.to_string(),
            agents,
            built_in: false,
        });
        match save_teams(&self.teams) {
            Ok(()) => self.status = format!("Saved the team as \"{name}\""),
            Err(error) => self.status = format!("Couldn't save the template: {error}"),
        }
    }
}
