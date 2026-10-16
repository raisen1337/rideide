//! What makes a pane work as Explorer does: the keys, copy, cut and paste (with Explorer's
//! own clipboard), Delete to the Recycle Bin, renaming in place, opening files to look at
//! them or in their app, and a file's properties.

use super::*;
use crate::preview::{self, Preview};

/// Files copied or cut, to paste somewhere else.
pub(super) struct Clip {
    pub place: Place,
    pub paths: Vec<String>,
    pub cut: bool,
    /// The clipboard's number when these were put there: if it has moved on, something
    /// newer (copied in Explorer, say) is pasted instead.
    pub stamp: u32,
}

/// A name being edited in place.
pub(super) struct Renaming {
    pub pane: u64,
    pub name: String,
    pub text: String,
    pub focus: bool,
}

/// A file from a server opened in its app: when it is saved there, it goes back up.
pub(super) struct Opened {
    pub job: u64,
    pub local: PathBuf,
    pub place: Place,
    pub remote_dir: String,
    pub mtime: Option<std::time::SystemTime>,
    pub checked: f64,
}

/// A file's properties, with its permissions to change on a server.
pub(super) struct Props {
    pub pane: u64,
    pub name: String,
    pub item: Item,
    pub mode: u32,
    pub octal: String,
    /// Owner and group by name, and where a link points, asked of the server's shell.
    pub owner: String,
    pub group: String,
    pub owner_was: String,
    pub group_was: String,
    pub target: String,
    pub details: Option<std::sync::mpsc::Receiver<(String, String, String)>>,
    /// Changes go to everything inside a folder too.
    pub recursive: bool,
}

impl App {
    /// The keys, as Explorer has them, for the pane in focus.
    pub(super) fn keys(&mut self, ctx: &Context) {
        // Ctrl+V as Windows sees it: egui only reports a paste when the clipboard holds
        // text, and files copied in Explorer aren't text. The press is caught as it starts.
        let paste_down = winshell::keys_down(&[0x11, 0x56]) && ctx.input(|i| i.focused);
        let paste_pressed = paste_down && !std::mem::replace(&mut self.paste_down, paste_down);
        if !paste_down {
            self.paste_down = false;
        }
        // Keys go to the panes unless something else wants them: a dialog, the preview, a
        // name being edited, or a text box being typed in. A picked row having the focus
        // doesn't count.
        if self.dialog.is_some()
            || self.auth.is_some()
            || self.preview.is_some()
            || self.props.is_some()
            || self.renaming.is_some()
            || self.compare.is_some()
            || self.palette.is_some()
            || typing(ctx)
        {
            return;
        }
        let id = self.focus;
        let Some(pane) = self.pane(id) else { return };
        let names = pane.selected_names();
        let up = pane.up();
        let pressed = |k: Key, m: Modifiers| ctx.input_mut(|i| i.consume_key(m, k));
        let ctrl = Modifiers::COMMAND;
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        // egui turns Ctrl+C and Ctrl+X into events of their own.
        let (copy, cut) = ctx.input(|i| {
            (
                i.events.iter().any(|e| matches!(e, Event::Copy)),
                i.events.iter().any(|e| matches!(e, Event::Cut)),
            )
        });
        if copy && !names.is_empty() {
            self.set_clip(id, &names, false);
        }
        if cut && !names.is_empty() {
            self.set_clip(id, &names, true);
        }
        if paste_pressed {
            self.paste(id);
        }
        if pressed(Key::F5, Modifiers::NONE) && !names.is_empty() {
            if let Some(to) = self.other_pane(id) {
                self.copy(id, &names, to, None);
            }
        }
        if pressed(Key::N, ctrl_shift) || pressed(Key::F7, Modifiers::NONE) {
            self.new_folder(id);
        }
        if pressed(Key::E, ctrl) {
            if let Some(name) = names.first() {
                self.open_in_ride(id, name);
            }
        }
        if pressed(Key::F2, Modifiers::NONE) {
            if let Some(name) = names.first() {
                self.start_rename(id, name);
            }
        }
        if pressed(Key::Delete, Modifiers::SHIFT) && !names.is_empty() {
            self.dialog = Some(Dialog::Delete(id, names.clone()));
        } else if pressed(Key::Delete, Modifiers::NONE) && !names.is_empty() {
            self.delete_soft(id, names.clone());
        }
        if pressed(Key::Enter, Modifiers::ALT) {
            if let Some(name) = names.first() {
                self.open_props(id, name);
            }
        }
        let searching = self.pane(id).is_some_and(|p| p.search.is_some());
        if pressed(Key::F, ctrl) || pressed(Key::F3, Modifiers::NONE) {
            if let Some(p) = self.pane_mut(id) {
                p.focus_search = true;
            }
        }
        if searching && pressed(Key::Backspace, Modifiers::NONE) {
            self.end_search(id);
        } else if pressed(Key::Backspace, Modifiers::NONE) || pressed(Key::ArrowUp, Modifiers::ALT)
        {
            if let Some(up) = up {
                self.go(id, up);
            }
        }
        if pressed(Key::ArrowLeft, Modifiers::ALT) {
            if let Some(back) = self.pane_mut(id).and_then(|p| p.back.pop()) {
                self.open(id, back);
            }
        }
        if pressed(Key::Tab, Modifiers::NONE) {
            let ids = self.layout.panes();
            if let Some(at) = ids.iter().position(|p| *p == id) {
                self.set_focus(ids[(at + 1) % ids.len()]);
            }
        }
        if pressed(Key::A, ctrl) {
            let hidden = self.settings.show_hidden;
            if let Some(p) = self.pane_mut(id) {
                p.selected = p.visible(hidden).iter().map(|i| i.name.clone()).collect();
            }
        }
        if pressed(Key::H, ctrl) {
            self.settings.show_hidden = !self.settings.show_hidden;
        }
        if pressed(Key::R, ctrl) || pressed(Key::F5, Modifiers::CTRL) {
            self.refresh(id);
        }
        if pressed(Key::Enter, Modifiers::NONE) {
            if let [name] = names.as_slice() {
                self.open_item(id, name);
            }
        }
        // Moving through the list with the arrows, Shift taking a range.
        let hidden = self.settings.show_hidden;
        let rows = self.pane(id).map_or(0, |p| p.visible(hidden).len());
        if rows > 0 {
            let page = 12usize;
            let moves = [
                (Key::ArrowDown, 1isize),
                (Key::ArrowUp, -1),
                (Key::PageDown, page as isize),
                (Key::PageUp, -(page as isize)),
                (Key::Home, isize::MIN / 2),
                (Key::End, isize::MAX / 2),
            ];
            for (key, step) in moves {
                for shift in [false, true] {
                    let mods = if shift {
                        Modifiers::SHIFT
                    } else {
                        Modifiers::NONE
                    };
                    if pressed(key, mods) {
                        self.step_selection(id, step, shift);
                    }
                }
            }
        }
        // Typing jumps to the first name that starts with what was typed.
        let typed: String = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect()
        });
        if !typed.is_empty() {
            let now = ctx.input(|i| i.time);
            if let Some(p) = self.pane_mut(id) {
                if now - p.jump.1 > 1.0 {
                    p.jump.0.clear();
                }
                p.jump.0.push_str(&typed.to_lowercase());
                p.jump.1 = now;
                let want = p.jump.0.clone();
                if let Some(item) = p
                    .visible(hidden)
                    .into_iter()
                    .find(|i| i.name.to_lowercase().starts_with(&want))
                {
                    p.selected = [item.name.clone()].into();
                    p.scroll_to = Some(item.name);
                }
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            let searching = self
                .pane(id)
                .is_some_and(|p| p.search.is_some() && p.selected.is_empty());
            if searching {
                self.end_search(id);
            } else if let Some(p) = self.pane_mut(id) {
                p.selected.clear();
            }
        }
    }

    /// Opens an item in ride, where it is: a file here or on a server to edit (saving
    /// writes it straight back), a folder as ride's folder. ride reaches servers by their
    /// name in the SSH config, so saved sites can't go to it.
    pub(super) fn open_in_ride(&mut self, id: u64, name: &str) {
        let Some(pane) = self.pane(id) else { return };
        let Some(item) = pane.items.iter().find(|i| i.name == name) else {
            return;
        };
        let path = pane.child(name);
        let mut args: Vec<String> = Vec::new();
        match &pane.place {
            Some(Place::Local) => args.push(path.clone()),
            Some(Place::Server(host)) if host.starts_with("site:") => {
                self.status = "ride connects to servers in your SSH config; add this one there to edit in ride".into();
                return;
            }
            Some(Place::Server(host)) => {
                args.push(format!("--host={host}"));
                if item.is_dir {
                    args.push(format!("--root={path}"));
                } else {
                    args.push(format!("--root={}", pane.path));
                    args.push(format!("--open={path}"));
                }
            }
            None => return,
        }
        // ride sits beside rdrop once installed; otherwise Windows finds it by its App Path.
        let beside = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("ride.exe")))
            .filter(|p| p.exists());
        let started = match beside {
            Some(exe) => std::process::Command::new(exe)
                .args(&args)
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string()),
            None => winshell::start("ride", &args),
        };
        match started {
            Ok(()) => self.note = format!("Opened {name} in ride"),
            Err(e) => self.status = format!("ride couldn't be started: {e}"),
        }
    }

    fn step_selection(&mut self, id: u64, step: isize, extend: bool) {
        let hidden = self.settings.show_hidden;
        let Some(p) = self.pane_mut(id) else { return };
        let items = p.visible(hidden);
        let current = items.iter().rposition(|i| p.selected.contains(&i.name));
        let at = match current {
            None => {
                if step > 0 {
                    0
                } else {
                    items.len() - 1
                }
            }
            Some(c) => (c as isize + step).clamp(0, items.len() as isize - 1) as usize,
        };
        if extend {
            let anchor = p
                .anchor
                .unwrap_or(current.unwrap_or(at))
                .min(items.len() - 1);
            let (a, b) = (anchor.min(at), anchor.max(at));
            p.selected = items[a..=b].iter().map(|i| i.name.clone()).collect();
        } else {
            p.selected = [items[at].name.clone()].into();
            p.anchor = Some(at);
        }
        p.scroll_to = Some(items[at].name.clone());
    }

    // ---- clipboard ----

    pub(super) fn set_clip(&mut self, id: u64, names: &[String], cut: bool) {
        let Some(pane) = self.pane(id) else { return };
        let Some(place) = pane.place.clone() else {
            return;
        };
        let paths: Vec<String> = names.iter().map(|n| pane.child(n)).collect();
        // Files here go on Windows' clipboard too, so they paste into Explorer.
        if place == Place::Local {
            let list: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
            winshell::set_clipboard_files(&list, cut);
        }
        let what = match paths.len() {
            1 => names[0].clone(),
            n => format!("{n} items"),
        };
        self.note = format!("{} {what}", if cut { "Cut" } else { "Copied" });
        self.clip = Some(Clip {
            place,
            paths,
            cut,
            stamp: winshell::clipboard_stamp(),
        });
    }

    /// Pastes into a pane's folder: what rdrop copied, or files copied in Explorer.
    pub(super) fn paste(&mut self, id: u64) {
        let Some(dst) = self.pane(id) else { return };
        if !dst.ready() || (dst.is_local() && dst.path.is_empty()) {
            self.status = "Open a folder to paste into".into();
            return;
        }
        let (to, dir) = (dst.place.clone().unwrap(), dst.path.clone());
        let _ = dst;
        let ours = self
            .clip
            .as_ref()
            .filter(|c| c.place != Place::Local || c.stamp == winshell::clipboard_stamp());
        let (from, paths, cut) = match ours {
            Some(c) => (c.place.clone(), c.paths.clone(), c.cut),
            None => match winshell::clipboard_files() {
                Some((files, cut)) => (
                    Place::Local,
                    files.iter().map(|f| f.display().to_string()).collect(),
                    cut,
                ),
                None => {
                    self.note = "Nothing to paste".into();
                    return;
                }
            },
        };
        // A move within the same place is a rename, done at once.
        if cut && from == to {
            let mut renames = Vec::new();
            for path in &paths {
                let name = path
                    .trim_end_matches(['/', '\\'])
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(path);
                match &to {
                    Place::Local => {
                        let target = PathBuf::from(&dir).join(name);
                        if let Err(e) = std::fs::rename(path, &target) {
                            // Another drive: copy, then remove.
                            if e.raw_os_error() == Some(17) {
                                self.queue.add_move(
                                    from.clone(),
                                    path.clone(),
                                    to.clone(),
                                    dir.clone(),
                                );
                            } else {
                                self.status = format!("{name}: {e}");
                            }
                        }
                    }
                    Place::Server(_) => renames.push((path.clone(), sftp::join(&dir, name))),
                }
            }
            if let Some(c) = self.pane(id).and_then(|p| p.conn.as_ref()) {
                for (from, to) in renames {
                    c.ask(Ask::Rename(from, to));
                }
            }
            self.clip = None;
            self.refresh_all(&from);
            return;
        }
        for path in paths {
            if cut {
                self.queue
                    .add_move(from.clone(), path, to.clone(), dir.clone());
            } else {
                self.queue.add(from.clone(), path, to.clone(), dir.clone());
            }
        }
        if cut {
            self.clip = None;
        }
    }

    /// Lists again every pane that shows `place`.
    pub(super) fn refresh_all(&mut self, place: &Place) {
        let ids: Vec<u64> = self
            .panes
            .iter()
            .filter(|p| p.place.as_ref() == Some(place))
            .map(|p| p.id)
            .collect();
        for id in ids {
            self.refresh(id);
        }
    }

    // ---- delete ----

    /// Delete as Explorer does it: here to the Recycle Bin; on a server, after asking, as
    /// servers have no Recycle Bin.
    pub(super) fn delete_soft(&mut self, id: u64, names: Vec<String>) {
        let Some(pane) = self.pane(id) else { return };
        if !pane.is_local() {
            self.dialog = Some(Dialog::Delete(id, names));
            return;
        }
        let paths: Vec<String> = names.iter().map(|n| pane.child(n)).collect();
        match winshell::recycle(&paths) {
            Ok(()) => {
                self.note = match names.as_slice() {
                    [one] => format!("{one} went to the Recycle Bin"),
                    many => format!("{} items went to the Recycle Bin", many.len()),
                };
            }
            Err(e) => self.status = e,
        }
        self.refresh(id);
    }

    // ---- renaming in place ----

    pub(super) fn start_rename(&mut self, id: u64, name: &str) {
        self.slow_click = None;
        self.renaming = Some(Renaming {
            pane: id,
            name: name.to_string(),
            text: name.to_string(),
            focus: true,
        });
    }

    pub(super) fn finish_rename(&mut self, keep: bool) {
        let Some(r) = self.renaming.take() else {
            return;
        };
        let new = r.text.trim().to_string();
        if keep && new != r.name {
            if valid_name(&new) {
                self.rename(r.pane, &r.name, &new);
                if let Some(p) = self.pane_mut(r.pane) {
                    p.selected = [new].into();
                }
            } else {
                self.status = format!("\"{new}\" can't be a file name");
            }
        }
    }

    /// A new folder named "New folder", ready to be named, as Ctrl+Shift+N does in Explorer.
    pub(super) fn new_folder(&mut self, id: u64) {
        let Some(pane) = self.pane(id) else { return };
        if !pane.ready() || (pane.is_local() && pane.path.is_empty()) {
            return;
        }
        let mut name = "New folder".to_string();
        let mut n = 2;
        while pane
            .items
            .iter()
            .any(|i| i.name.eq_ignore_ascii_case(&name))
        {
            name = format!("New folder ({n})");
            n += 1;
        }
        let path = pane.child(&name);
        if pane.is_local() {
            if let Err(e) = std::fs::create_dir(&path) {
                self.status = e.to_string();
                return;
            }
            self.refresh(id);
        } else if let Some(c) = &pane.conn {
            c.ask(Ask::Mkdir(path));
        }
        self.pending_rename = Some((id, name));
    }

    /// Once a new folder shows up in its pane, its name is edited.
    pub(super) fn pending_renames(&mut self) {
        let Some((id, name)) = &self.pending_rename else {
            return;
        };
        let (id, name) = (*id, name.clone());
        if self
            .pane(id)
            .is_some_and(|p| p.items.iter().any(|i| i.name == name))
        {
            self.pending_rename = None;
            if let Some(p) = self.pane_mut(id) {
                p.selected = [name.clone()].into();
                p.scroll_to = Some(name.clone());
            }
            self.start_rename(id, &name);
        }
    }

    // ---- opening ----

    /// Enter or a double click: a folder opens, a file shows.
    pub(super) fn open_item(&mut self, id: u64, name: &str) {
        let Some(pane) = self.pane(id) else { return };
        let Some(item) = pane.items.iter().find(|i| i.name == name).cloned() else {
            // The list of drives has no items of its own.
            if pane.is_local() && pane.path.is_empty() {
                self.go(id, name.to_string());
            }
            return;
        };
        let path = if pane.is_local() && pane.path.is_empty() {
            item.name.clone()
        } else {
            pane.child(&item.name)
        };
        if item.is_dir {
            self.go(id, path);
        } else {
            self.open_preview(id, &item);
        }
    }

    pub(super) fn open_preview(&mut self, id: u64, item: &Item) {
        let Some(pane) = self.pane(id) else { return };
        let Some(place) = pane.place.clone() else {
            return;
        };
        let path = pane.child(&item.name);
        let mut p = Preview::new(id, &item.name, &path, place.label(), Some(item.size));
        p.live = preview::is_log(&item.name)
            && preview::kind_of(&item.name) == preview::Kind::Other
            && item.size > preview::TAIL_BYTES;
        self.preview = Some(p);
        self.read_preview();
    }

    /// Reads what the preview needs: the start of the file, or the end of a log it follows.
    pub(super) fn read_preview(&mut self) {
        let Some(p) = &mut self.preview else { return };
        if !p.wants_data() {
            return;
        }
        let limit = if p.live {
            preview::TAIL_BYTES
        } else {
            preview::read_limit(p.kind)
        };
        if limit == 0 {
            return;
        }
        p.asked_at = self.now;
        let (pane, path, tail) = (p.pane, p.path.clone(), p.live);
        let Some(owner) = self.panes.iter().find(|x| x.id == pane) else {
            return;
        };
        if owner.is_local() {
            let read = (|| -> std::io::Result<(Vec<u8>, u64)> {
                use std::io::{Read, Seek};
                let mut f = std::fs::File::open(&path)?;
                let size = f.metadata()?.len();
                if tail {
                    f.seek(std::io::SeekFrom::Start(size.saturating_sub(limit)))?;
                }
                let mut data = Vec::new();
                f.take(limit).read_to_end(&mut data)?;
                Ok((data, size))
            })();
            let ctx = self.ctx.clone();
            match read {
                Ok((data, size)) => self.preview.as_mut().unwrap().set_data(&ctx, data, size),
                Err(e) => self.preview.as_mut().unwrap().content = Some(Err(e.to_string())),
            }
        } else if let Some(c) = &owner.conn {
            c.ask(Ask::Read { path, limit, tail });
        }
    }

    /// What the preview window asked for, and following a log.
    pub(super) fn preview_tick(&mut self, ctx: &Context) {
        let Some(p) = &mut self.preview else { return };
        let act = preview::show(ctx, p);
        let live = p.live;
        let due = live && self.now - p.asked_at > 1.0;
        if live {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        match act {
            preview::Act::None => {}
            preview::Act::Close => self.preview = None,
            preview::Act::Live(on) => {
                let p = self.preview.as_mut().unwrap();
                p.live = on;
                p.content = None;
                self.read_preview();
            }
            preview::Act::OpenElsewhere => {
                let (pane, name) = (p.pane, p.name.clone());
                self.open_with_app(pane, &name);
            }
            preview::Act::Next | preview::Act::Previous => {
                let (pane, name) = (p.pane, p.name.clone());
                let hidden = self.settings.show_hidden;
                if let Some(owner) = self.pane(pane) {
                    let files: Vec<Item> = owner
                        .visible(hidden)
                        .into_iter()
                        .filter(|i| !i.is_dir)
                        .collect();
                    if let Some(at) = files.iter().position(|i| i.name == name) {
                        let next = if act == preview::Act::Next {
                            (at + 1) % files.len()
                        } else {
                            (at + files.len() - 1) % files.len()
                        };
                        let item = files[next].clone();
                        if let Some(o) = self.pane_mut(pane) {
                            o.selected = [item.name.clone()].into();
                            o.scroll_to = Some(item.name.clone());
                        }
                        self.open_preview(pane, &item);
                    }
                }
                return;
            }
        }
        if due {
            self.read_preview();
        }
    }

    /// Opens a file in the app Windows has for it. One from a server is fetched first, and
    /// when it is saved there, it goes back to the server.
    pub(super) fn open_with_app(&mut self, id: u64, name: &str) {
        let Some(pane) = self.pane(id) else { return };
        let Some(place) = pane.place.clone() else {
            return;
        };
        if place == Place::Local {
            let _ = std::process::Command::new("explorer")
                .arg(pane.child(name))
                .spawn();
            return;
        }
        let (source, remote_dir) = (pane.child(name), pane.path.clone());
        let dir = std::env::temp_dir()
            .join("rdrop")
            .join(place.label().replace(['/', '\\', ':', '@'], "_"))
            .join(format!("{:x}", self.next_open()));
        let _ = std::fs::create_dir_all(&dir);
        let job = self.queue.add(
            place.clone(),
            source,
            Place::Local,
            dir.display().to_string(),
        );
        self.opened.push(Opened {
            job,
            local: dir.join(name),
            place,
            remote_dir,
            mtime: None,
            checked: self.now,
        });
        self.note = format!("Opening {name}…");
    }

    fn next_open(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Files opened in their app: open them once fetched, and send back what is saved.
    pub(super) fn opened_tick(&mut self) {
        let now = self.now;
        let mut back = Vec::new();
        for o in &mut self.opened {
            if o.mtime.is_none() {
                let done = self
                    .queue
                    .jobs
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|j| j.id == o.job)
                    .map(|j| j.state.clone());
                if done == Some(State::Done) {
                    o.mtime = std::fs::metadata(&o.local).and_then(|m| m.modified()).ok();
                    let _ = std::process::Command::new("explorer").arg(&o.local).spawn();
                }
                continue;
            }
            if now - o.checked < 1.5 {
                continue;
            }
            o.checked = now;
            let current = std::fs::metadata(&o.local).and_then(|m| m.modified()).ok();
            if current.is_some() && current != o.mtime {
                o.mtime = current;
                back.push((
                    o.local.display().to_string(),
                    o.place.clone(),
                    o.remote_dir.clone(),
                ));
            }
        }
        for (local, place, dir) in back {
            let name = PathBuf::from(&local)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.queue.add(Place::Local, local, place.clone(), dir);
            self.note = format!("Saved {name}: sending it back to {}", place.label());
        }
    }

    // ---- properties ----

    pub(super) fn open_props(&mut self, id: u64, name: &str) {
        let Some(pane) = self.pane(id) else { return };
        let Some(item) = pane.items.iter().find(|i| i.name == name).cloned() else {
            return;
        };
        let mode = item.perms.unwrap_or(0) & 0o7777;
        // Owner, group and a link's target need the server's shell: asked on the side.
        let details = match &pane.place {
            Some(Place::Server(host)) => {
                let (tx, rx) = std::sync::mpsc::channel();
                let (host, path, repaint) = (host.clone(), pane.child(name), self.ctx.clone());
                std::thread::spawn(move || {
                    let q = sftp::quote(&path);
                    let script = format!("stat -c '%U\t%G' -- {q}; readlink -- {q}; true");
                    if let Some(out) = sftp::run(&host, &script) {
                        let mut lines = out.lines();
                        let (owner, group) = lines
                            .next()
                            .and_then(|l| l.split_once('\t'))
                            .map(|(o, g)| (o.to_string(), g.to_string()))
                            .unwrap_or_default();
                        let target = lines.next().unwrap_or("").to_string();
                        let _ = tx.send((owner, group, target));
                        repaint.request_repaint();
                    }
                });
                Some(rx)
            }
            _ => None,
        };
        self.props = Some(Props {
            pane: id,
            name: name.to_string(),
            item,
            mode,
            octal: format!("{mode:03o}"),
            owner: String::new(),
            group: String::new(),
            owner_was: String::new(),
            group_was: String::new(),
            target: String::new(),
            details,
            recursive: false,
        });
    }

    pub(super) fn props_dialog(&mut self, ctx: &Context) {
        let Some(props) = &mut self.props else { return };
        if let Some((owner, group, target)) =
            props.details.as_ref().and_then(|rx| rx.try_recv().ok())
        {
            props.details = None;
            props.owner_was = owner.clone();
            props.group_was = group.clone();
            props.owner = owner;
            props.group = group;
            props.target = target;
        }
        let remote = self
            .panes
            .iter()
            .find(|p| p.id == props.pane)
            .is_some_and(|p| !p.is_local());
        let (place, path) = self
            .panes
            .iter()
            .find(|p| p.id == props.pane)
            .map(|p| {
                (
                    p.place
                        .as_ref()
                        .map_or(String::new(), |pl| pl.label().to_string()),
                    p.child(&props.name),
                )
            })
            .unwrap_or_default();
        let offset = self.offset;
        let original = props.item.perms.unwrap_or(0) & 0o7777;
        let owned = !props.owner_was.is_empty();
        let ownership_changed = owned
            && !props.owner.trim().is_empty()
            && !props.group.trim().is_empty()
            && (props.owner.trim() != props.owner_was || props.group.trim() != props.group_was);
        let can = remote
            && props.item.perms.is_some()
            && (props.mode != original || ownership_changed || props.recursive);
        let answer = ui::Dialog {
            id: "props",
            icon: if props.item.is_dir {
                icons::FOLDER_ROOT
            } else {
                icons::FILES
            },
            title: &props.name,
            subtitle: &place,
            confirm: if remote { "Apply" } else { "Close" },
            other: None,
            tone: ui::Tone::Normal,
            can: can || !remote,
            width: 460.0,
        }
        .show(ctx, |ui| {
            egui::Grid::new("props")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    let row = |ui: &mut Ui, k: &str, v: String| {
                        ui.label(RichText::new(k).size(12.5).color(theme::COMMENT()));
                        ui.add(
                            egui::Label::new(RichText::new(v).size(12.5).color(theme::TEXT()))
                                .wrap(),
                        );
                        ui.end_row();
                    };
                    row(ui, "Where", path.clone());
                    row(
                        ui,
                        "Kind",
                        if props.item.is_dir {
                            "Folder".into()
                        } else {
                            "File".into()
                        },
                    );
                    if !props.item.is_dir {
                        row(
                            ui,
                            "Size",
                            format!(
                                "{}  ({} bytes)",
                                size_text(props.item.size),
                                props.item.size
                            ),
                        );
                    }
                    row(ui, "Modified", time_text(props.item.mtime, offset));
                    if props.item.link {
                        let text = if props.target.is_empty() {
                            "A symbolic link".to_string()
                        } else {
                            format!("A symbolic link to {}", props.target)
                        };
                        row(ui, "Link", text);
                    }
                });
            if remote {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Owned by")
                        .size(12.0)
                        .strong()
                        .color(theme::MUTED()),
                );
                if props.details.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Asking the server…")
                                .size(12.0)
                                .color(theme::COMMENT()),
                        );
                    });
                } else if owned {
                    ui::pair(
                        ui,
                        |ui| {
                            ui::field(ui, "owner", "User", &mut props.owner, "root", false);
                        },
                        |ui| {
                            ui::field(ui, "group", "Group", &mut props.group, "root", false);
                        },
                        170.0,
                    );
                } else {
                    ui.label(
                        RichText::new("This server doesn't say who owns its files (no shell).")
                            .size(12.0)
                            .color(theme::COMMENT()),
                    );
                }
            }
            if remote && props.item.perms.is_some() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Permissions")
                        .size(12.0)
                        .strong()
                        .color(theme::MUTED()),
                );
                egui::Grid::new("perms")
                    .num_columns(4)
                    .spacing([18.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("");
                        for h in ["Read", "Write", "Run"] {
                            ui.label(RichText::new(h).size(12.0).color(theme::COMMENT()));
                        }
                        ui.end_row();
                        for (who, shift) in [("Owner", 6), ("Group", 3), ("Others", 0)] {
                            ui.label(RichText::new(who).size(12.5).color(theme::TEXT()));
                            for bit in [4, 2, 1] {
                                let mask = bit << shift;
                                let mut on = props.mode & mask != 0;
                                if ui.checkbox(&mut on, "").changed() {
                                    props.mode = if on {
                                        props.mode | mask
                                    } else {
                                        props.mode & !mask
                                    };
                                    props.octal = format!("{:03o}", props.mode);
                                }
                            }
                            ui.end_row();
                        }
                    });
                let r = ui::field(ui, "octal", "As a number", &mut props.octal, "755", false);
                if r.changed() {
                    if let Ok(m) = u32::from_str_radix(props.octal.trim(), 8) {
                        props.mode = m & 0o7777;
                    }
                }
                ui.label(
                    RichText::new(mode_text(props.mode, props.item.is_dir))
                        .monospace()
                        .size(12.5)
                        .color(theme::COMMENT()),
                );
                if props.item.is_dir && owned {
                    ui::switch(
                        ui,
                        &mut props.recursive,
                        "Also everything inside",
                        "chmod -R and chown -R",
                    );
                }
            }
            false
        });
        match answer {
            ui::Answer::Waiting => {}
            ui::Answer::Cancel => self.props = None,
            ui::Answer::Confirm | ui::Answer::Other => {
                let props = self.props.take().unwrap();
                let host = match self.pane(props.pane).and_then(|p| p.place.clone()) {
                    Some(Place::Server(h)) => h,
                    _ => return,
                };
                if props.recursive || ownership_changed {
                    // Through the shell: SFTP has no "everything inside", and takes owners
                    // only as numbers.
                    let q = sftp::quote(&path);
                    let r = if props.recursive { "-R " } else { "" };
                    let mut script = String::from("set -e; ");
                    if props.mode != original || props.recursive {
                        script.push_str(&format!("chmod {r}{:o} -- {q}; ", props.mode));
                    }
                    if ownership_changed || props.recursive && owned {
                        let who = format!("{}:{}", props.owner.trim(), props.group.trim());
                        script.push_str(&format!("chown {r}{} -- {q}; ", sftp::quote(&who)));
                    }
                    script.push_str("echo rdrop-ok");
                    let (tx, rx) = std::sync::mpsc::channel();
                    let repaint = self.ctx.clone();
                    let name = props.name.clone();
                    std::thread::spawn(move || {
                        let ok = sftp::run(&host, &script).is_some_and(|o| o.contains("rdrop-ok"));
                        let _ = tx.send(if ok {
                            Ok(format!("Changed {name}"))
                        } else {
                            Err(format!("{name} couldn't be changed: the server refused (only root can give files away)"))
                        });
                        repaint.request_repaint();
                    });
                    self.shell_jobs.push((props.pane, rx));
                } else if props.mode != original {
                    if let Some(c) = self.pane(props.pane).and_then(|p| p.conn.as_ref()) {
                        c.ask(Ask::Chmod(path, props.mode));
                    }
                }
            }
        }
    }
}

/// Permissions as `ls -l` writes them.
pub(super) fn mode_text(mode: u32, dir: bool) -> String {
    let mut s = String::from(if dir { "d" } else { "-" });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_read_as_ls_writes_them() {
        assert_eq!(mode_text(0o755, true), "drwxr-xr-x");
        assert_eq!(mode_text(0o640, false), "-rw-r-----");
    }
}
