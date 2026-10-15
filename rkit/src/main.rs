#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! rkit: one place for the apps of the suite. It starts ride and rterm (each also starts on
//! its own, from the Start menu or Explorer), reopens recent folders, and shows and installs
//! each app's updates with its changelog.

// The look is ride's, and the window parts and shell profiles are rterm's, shared as files.
#[allow(dead_code)]
#[path = "../../app/src/icons.rs"]
mod icons;
#[allow(dead_code)]
#[path = "../../rterm/src/os.rs"]
mod os;
#[allow(dead_code)]
#[path = "../../rterm/src/profiles.rs"]
mod profiles;
#[allow(dead_code)]
#[path = "../../app/src/theme.rs"]
mod theme;
#[allow(dead_code)]
#[path = "../../rterm/src/window.rs"]
mod window;

mod updates;

use eframe::egui::{
    self, vec2, Align, Color32, Context, CornerRadius, Frame, Layout, Margin, Panel, RichText,
    ScrollArea, Sense, Stroke, TextureHandle, Ui, ViewportBuilder, ViewportCommand,
};
use profiles::Profile;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use updates::{Manifest, Release};
use window::{resize_edges, title_drag, window_buttons};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How ride names folders on this computer in its list of recent ones.
const LOCAL: &str = "This computer";

fn main() -> eframe::Result {
    if std::env::args().any(|arg| arg == "--version") {
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "{VERSION}");
        return Ok(());
    }
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        setup.device_descriptor = Arc::new(|_adapter| eframe::wgpu::DeviceDescriptor {
            label: Some("rkit"),
            required_limits: eframe::wgpu::Limits {
                max_texture_dimension_2d: 8192,
                ..Default::default()
            },
            memory_hints: eframe::wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        });
    }
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([1000.0, 660.0])
            .with_min_inner_size([760.0, 480.0])
            .with_title("rkit")
            .with_icon(egui::IconData {
                rgba: include_bytes!(concat!(env!("OUT_DIR"), "/icon-128.rgba")).to_vec(),
                width: 128,
                height: 128,
            })
            // The system frame stays in the window's style and is hidden by `os::adopt_window`.
            .with_decorations(true),
        wgpu_options,
        persistence_path: std::env::var_os("APPDATA")
            .map(|dir| PathBuf::from(dir).join("rkit").join("window.ron")),
        ..Default::default()
    };
    eframe::run_native("rkit", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Ride,
    Rterm,
    Rdrop,
    Rkit,
}

/// A download and swap in progress for one app.
struct Job {
    progress: Arc<Mutex<f32>>,
    done: Receiver<Result<String, String>>,
}

struct Card {
    kind: Kind,
    key: &'static str,
    name: &'static str,
    blurb: &'static str,
    exe: PathBuf,
    icon: TextureHandle,
    /// The installed version, once asked; `None` inside when it isn't installed.
    installed: Option<Option<String>>,
    job: Option<Job>,
    /// What the last update did, and whether that went well.
    message: Option<(String, bool)>,
    notes_open: bool,
    /// Updated in this session: the running copy is the old one until it starts again.
    updated: bool,
}

/// A folder opened lately in ride.
struct Recent {
    host: String,
    root: String,
}

struct App {
    cards: Vec<Card>,
    manifest: Option<Result<Manifest, String>>,
    manifest_rx: Option<Receiver<Result<Manifest, String>>>,
    versions_rx: Option<Receiver<(Kind, Option<String>)>>,
    recent: Vec<Recent>,
    hosts: Vec<String>,
    profiles: Vec<Profile>,
    more_profiles: Option<Receiver<Vec<Profile>>>,
    adopted: bool,
    status: String,
}

fn texture(ctx: &Context, name: &str, rgba: &[u8]) -> TextureHandle {
    let image = egui::ColorImage::from_rgba_unmultiplied([96, 96], rgba);
    ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
}

fn read_recent() -> Vec<Recent> {
    let Some(base) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    let path = PathBuf::from(base).join("ride").join("recent.json");
    let list: Vec<serde_json::Value> = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    list.iter()
        .filter_map(|item| {
            Some(Recent {
                host: item["host"].as_str()?.to_string(),
                root: item["root"].as_str()?.to_string(),
            })
        })
        .take(12)
        .collect()
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map_or_else(|| PathBuf::from("C:\\"), PathBuf::from)
}

/// Starts an app on its own, so it keeps running when rkit closes.
fn launch(exe: &Path, args: &[String]) -> Result<(), String> {
    if !exe.exists() {
        return Err(format!(
            "{} isn't installed",
            exe.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    std::process::Command::new(exe)
        .args(args)
        .current_dir(home())
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install_fonts(ctx);
        theme::set(ctx, theme::GRAPHITE);
        let dir = updates::install_dir();
        let card = |kind, key, name, blurb, icon: &[u8]| Card {
            kind,
            key,
            name,
            blurb,
            exe: dir.join(format!("{key}.exe")),
            icon: texture(ctx, key, icon),
            installed: None,
            job: None,
            message: None,
            notes_open: false,
            updated: false,
        };
        let mut cards = vec![
            card(
                Kind::Ride,
                "ride",
                "ride",
                "Code editor for your servers, with Claude Code built in.",
                include_bytes!(concat!(env!("OUT_DIR"), "/ride-96.rgba")),
            ),
            card(
                Kind::Rterm,
                "rterm",
                "rterm",
                "Terminal for PowerShell, cmd, Git Bash, WSL and SSH, in tabs and panes.",
                include_bytes!(concat!(env!("OUT_DIR"), "/rterm-96.rgba")),
            ),
            card(
                Kind::Rdrop,
                "rdrop",
                "rdrop",
                "Files between this computer and your servers, and server to server, over SFTP.",
                include_bytes!(concat!(env!("OUT_DIR"), "/rdrop-96.rgba")),
            ),
            card(
                Kind::Rkit,
                "rkit",
                "rkit",
                "This app: starts the others and keeps them up to date.",
                include_bytes!(concat!(env!("OUT_DIR"), "/rkit-96.rgba")),
            ),
        ];
        cards[3].installed = Some(Some(VERSION.to_string()));

        let mut app = Self {
            cards,
            manifest: None,
            manifest_rx: None,
            versions_rx: None,
            recent: read_recent(),
            hosts: Vec::new(),
            profiles: profiles::local(),
            more_profiles: None,
            adopted: false,
            status: String::new(),
        };
        app.ask_versions(ctx);
        app.check_updates(ctx);
        let (tx, rx) = channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(profiles::slow());
            repaint.request_repaint();
        });
        app.more_profiles = Some(rx);
        app
    }

    fn ask_versions(&mut self, ctx: &Context) {
        let (tx, rx) = channel();
        let exes: Vec<(Kind, PathBuf)> = self
            .cards
            .iter()
            .filter(|c| c.kind != Kind::Rkit)
            .map(|c| (c.kind, c.exe.clone()))
            .collect();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            for (kind, exe) in exes {
                let _ = tx.send((kind, updates::installed_version(&exe)));
                repaint.request_repaint();
            }
        });
        self.versions_rx = Some(rx);
    }

    fn check_updates(&mut self, ctx: &Context) {
        let (tx, rx) = channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(updates::fetch_manifest());
            repaint.request_repaint();
        });
        self.manifest = None;
        self.manifest_rx = Some(rx);
    }

    fn release(&self, key: &str) -> Option<&Release> {
        self.manifest.as_ref()?.as_ref().ok()?.apps.get(key)
    }

    fn poll(&mut self, ctx: &Context) {
        if let Some(rx) = &self.manifest_rx {
            if let Ok(result) = rx.try_recv() {
                self.manifest = Some(result);
                self.manifest_rx = None;
            }
        }
        if let Some(rx) = &self.versions_rx {
            while let Ok((kind, version)) = rx.try_recv() {
                if let Some(card) = self.cards.iter_mut().find(|c| c.kind == kind) {
                    card.installed = Some(version);
                }
            }
        }
        if let Some(rx) = &self.more_profiles {
            if let Ok(found) = rx.try_recv() {
                self.hosts = found
                    .iter()
                    .filter_map(|p| p.name.strip_prefix("SSH: ").map(str::to_string))
                    .collect();
                self.profiles.extend(found);
                self.more_profiles = None;
            }
        }
        let mut refresh = false;
        for card in &mut self.cards {
            let Some(job) = &card.job else { continue };
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            if let Ok(result) = job.done.try_recv() {
                card.job = None;
                card.message = Some(match result {
                    Ok(version) => {
                        card.updated = true;
                        card.installed = Some(Some(version.clone()));
                        let more = match card.kind {
                            Kind::Rkit => "Restart rkit to use it.".to_string(),
                            _ => format!("Copies already open keep the old version until you start {} again.", card.name),
                        };
                        (format!("Updated to {version}. {more}"), true)
                    }
                    Err(error) => (error, false),
                });
                refresh = true;
            }
        }
        if refresh {
            self.ask_versions(ctx);
        }
    }

    fn start_update(&mut self, index: usize, ctx: &Context) {
        let Some(release) = self.release(self.cards[index].key).cloned() else {
            return;
        };
        let card = &mut self.cards[index];
        let progress = Arc::new(Mutex::new(0.0));
        let (tx, done) = channel();
        let (target, report, repaint) = (card.exe.clone(), progress.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = updates::install(&release, &target, |p| {
                if let Ok(mut at) = report.lock() {
                    *at = p;
                }
            })
            .map(|()| release.version.clone());
            let _ = tx.send(result);
            repaint.request_repaint();
        });
        card.message = None;
        card.job = Some(Job { progress, done });
    }

    fn sync_frame(&mut self, frame: &eframe::Frame) {
        if !std::mem::replace(&mut self.adopted, true) {
            os::adopt_window(frame);
            os::set_backdrop(frame, None, true);
        }
    }

    fn title_bar(&mut self, ui: &mut Ui) {
        Panel::top("title")
            .exact_size(44.0)
            .frame(Frame::NONE.fill(theme::WINDOW_BG()).inner_margin(Margin {
                left: 14,
                right: 4,
                top: 8,
                bottom: 0,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                title_drag(ui);
                ui.horizontal(|ui| {
                    let icon = &self.cards[3].icon;
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(
                        icon.id(),
                        vec2(22.0, 22.0),
                    )));
                    ui.label(
                        RichText::new("rkit")
                            .size(15.0)
                            .strong()
                            .color(theme::TEXT()),
                    );
                    ui.label(
                        RichText::new("ride, rterm and what comes next")
                            .size(12.5)
                            .color(theme::COMMENT()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Min), window_buttons);
                });
            });
    }

    fn card(&mut self, ui: &mut Ui, index: usize) {
        let ctx = ui.ctx().clone();
        let (kind, name, blurb, exe) = {
            let c = &self.cards[index];
            (c.kind, c.name, c.blurb, c.exe.clone())
        };
        let release = self.release(self.cards[index].key).cloned();
        let installed = self.cards[index].installed.clone();
        let current = installed.clone().flatten();
        let newer = release
            .as_ref()
            .filter(|r| {
                current
                    .as_deref()
                    .is_none_or(|v| updates::is_newer(&r.version, v))
            })
            .filter(|_| installed.is_some());
        let mut launch_args: Option<Vec<String>> = None;
        let mut update = false;

        Frame::NONE
            .fill(theme::tint(7))
            .stroke(Stroke::new(1.0, theme::LINE()))
            .corner_radius(CornerRadius::same(14))
            .inner_margin(Margin::same(18))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let icon = &self.cards[index].icon;
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(
                        icon.id(),
                        vec2(56.0, 56.0),
                    )));
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(name)
                                    .size(21.0)
                                    .strong()
                                    .color(theme::STRONG()),
                            );
                            let version = match &installed {
                                None => "…".to_string(),
                                Some(None) => "not installed".to_string(),
                                Some(Some(v)) => v.clone(),
                            };
                            ui.label(RichText::new(version).size(13.0).color(theme::COMMENT()));
                            match (&newer, &self.manifest) {
                                (Some(r), _) => {
                                    chip(ui, &format!("{} available", r.version), theme::ACCENT())
                                }
                                (None, Some(Ok(_))) if current.is_some() => {
                                    chip(ui, "up to date", Color32::from_rgb(0x7f, 0xd0, 0x9a))
                                }
                                _ => {}
                            }
                        });
                        ui.label(RichText::new(blurb).size(13.0).color(theme::MUTED()));
                    });
                    if kind != Kind::Rkit && current.is_some() {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if primary(ui, "Open") {
                                launch_args = Some(Vec::new());
                            }
                        });
                    }
                });

                // What each app can be started straight into.
                if current.is_some() {
                    match kind {
                        Kind::Ride => {
                            ui.add_space(10.0);
                            ui.horizontal_wrapped(|ui| {
                                if secondary(ui, &format!("{}  Open folder…", icons::FOLDER_ROOT))
                                {
                                    if let Some(folder) = rfd::FileDialog::new()
                                        .set_title("Open in ride")
                                        .pick_folder()
                                    {
                                        launch_args = Some(vec![folder.display().to_string()]);
                                    }
                                }
                            });
                        }
                        Kind::Rterm => {
                            ui.add_space(10.0);
                            ui.horizontal_wrapped(|ui| {
                                for profile in self
                                    .profiles
                                    .iter()
                                    .filter(|p| !p.name.starts_with("SSH: "))
                                {
                                    if secondary(ui, &format!("{}  {}", profile.icon, profile.name))
                                    {
                                        launch_args = Some(vec!["-p".into(), profile.name.clone()]);
                                    }
                                }
                            });
                        }
                        Kind::Rdrop | Kind::Rkit => {}
                    }
                }

                // Updates: this app's own, with what is new in it.
                let card = &self.cards[index];
                if let Some(job) = &card.job {
                    ui.add_space(10.0);
                    let at = job.progress.lock().map_or(0.0, |p| *p);
                    ui.add(egui::ProgressBar::new(at).desired_height(6.0));
                    ui.label(
                        RichText::new(format!("Downloading {name}… {:.0}%", at * 100.0))
                            .size(12.5)
                            .color(theme::COMMENT()),
                    );
                } else if let Some(release) = &release {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if let Some(r) = &newer {
                            let label = if current.is_some() {
                                format!("Update to {}", r.version)
                            } else {
                                format!("Install {}", r.version)
                            };
                            if primary(ui, &label) {
                                update = true;
                            }
                        }
                        if !release.changelog.is_empty() {
                            let open = self.cards[index].notes_open;
                            let label = if newer.is_some() && current.is_some() {
                                "What's new"
                            } else {
                                "Changelog"
                            };
                            if secondary(
                                ui,
                                &format!(
                                    "{label}  {}",
                                    if open {
                                        icons::CHEVRON_DOWN
                                    } else {
                                        icons::CHEVRON_RIGHT
                                    }
                                ),
                            ) {
                                self.cards[index].notes_open = !open;
                            }
                        }
                        if kind == Kind::Rkit
                            && self.cards[index].updated
                            && secondary(ui, "Restart rkit")
                        {
                            if launch(&exe, &[]).is_ok() {
                                ctx.send_viewport_cmd(ViewportCommand::Close);
                            }
                        }
                    });
                }
                if let Some((text, good)) = &self.cards[index].message {
                    let color = if *good {
                        theme::COMMENT()
                    } else {
                        Color32::from_rgb(0xf0, 0x80, 0x8a)
                    };
                    ui.label(RichText::new(text).size(12.5).color(color));
                }
                if self.cards[index].notes_open {
                    if let Some(release) = &release {
                        // Before an update, what it brings; after, the newest entries.
                        let shown = if newer.is_some() && current.is_some() {
                            updates::whats_new(release, current.as_deref())
                        } else {
                            release.changelog.iter().take(6).collect()
                        };
                        ui.add_space(8.0);
                        notes(ui, &shown);
                    }
                }
            });

        if update {
            self.start_update(index, &ctx);
        }
        if let Some(args) = launch_args {
            if let Err(error) = launch(&exe, &args) {
                self.status = error;
            }
        }
    }

    /// The right column: recent folders from ride, and the servers in the SSH config.
    fn side(&mut self, ui: &mut Ui) {
        let ride = self.cards[0].exe.clone();
        let rterm = self.cards[1].exe.clone();
        let rdrop = self.cards[2].exe.clone();
        let mut run: Option<(PathBuf, Vec<String>)> = None;
        heading(ui, "Recent in ride");
        if self.recent.is_empty() {
            ui.label(
                RichText::new("Folders you open in ride show up here.")
                    .size(12.5)
                    .color(theme::COMMENT()),
            );
        }
        for item in &self.recent {
            let name = item
                .root
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&item.root);
            let place = if item.host == LOCAL {
                "this computer".to_string()
            } else {
                item.host.clone()
            };
            let resp = row(
                ui,
                icons::FOLDER_ROOT,
                if name.is_empty() { &item.root } else { name },
                &format!("{place}  ·  {}", item.root),
            );
            if resp.clicked() {
                let args = if item.host == LOCAL {
                    vec![item.root.clone()]
                } else {
                    vec![
                        format!("--host={}", item.host),
                        format!("--root={}", item.root),
                    ]
                };
                run = Some((ride.clone(), args));
            }
        }
        ui.add_space(18.0);
        heading(ui, "Servers");
        if self.hosts.is_empty() {
            let text = if self.more_profiles.is_some() {
                "Reading ~/.ssh/config…"
            } else {
                "Hosts in ~/.ssh/config show up here."
            };
            ui.label(RichText::new(text).size(12.5).color(theme::COMMENT()));
        }
        for host in &self.hosts {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icons::REMOTE).color(theme::COMMENT()));
                ui.label(RichText::new(host).size(14.0).color(theme::TEXT()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if small(ui, "rdrop", "This server's files in rdrop") {
                        run = Some((rdrop.clone(), vec![format!("--host={host}")]));
                    }
                    if small(ui, "rterm", "An SSH session in rterm") {
                        run = Some((rterm.clone(), vec!["-p".into(), format!("SSH: {host}")]));
                    }
                    if small(ui, "ride", "Connect ride to this server") {
                        run = Some((ride.clone(), vec![format!("--host={host}")]));
                    }
                });
            });
        }
        if let Some((exe, args)) = run {
            if let Err(error) = launch(&exe, &args) {
                self.status = error;
            }
        }
    }

    fn footer(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        Panel::bottom("footer")
            .exact_size(34.0)
            .frame(
                Frame::NONE
                    .fill(theme::WINDOW_BG())
                    .inner_margin(Margin::symmetric(16, 6)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let text = match &self.manifest {
                        None => "Checking for updates…".to_string(),
                        Some(Ok(_)) => "Checked for updates just now".to_string(),
                        Some(Err(error)) => error.clone(),
                    };
                    ui.label(RichText::new(text).size(12.5).color(theme::COMMENT()));
                    if !self.status.is_empty() {
                        ui.label(
                            RichText::new(format!("·  {}", self.status))
                                .size(12.5)
                                .color(Color32::from_rgb(0xf0, 0x80, 0x8a)),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.manifest.is_some()
                            && small(ui, "Check again", "Look for updates now")
                        {
                            self.check_updates(&ctx);
                            self.recent = read_recent();
                        }
                    });
                });
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.sync_frame(frame);
        self.poll(&ctx);
        self.title_bar(ui);
        self.footer(ui);
        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(theme::PANEL_BG())
                    .inner_margin(Margin::same(20)),
            )
            .show(ui, |ui| {
                let side_w = 300.0;
                let main_w = ui.available_width() - side_w - 24.0;
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        vec2(main_w, ui.available_height()),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ScrollArea::vertical().id_salt("cards").show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 14.0;
                                for index in 0..self.cards.len() {
                                    self.card(ui, index);
                                }
                            });
                        },
                    );
                    ui.add_space(24.0);
                    ui.allocate_ui_with_layout(
                        vec2(side_w, ui.available_height()),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ScrollArea::vertical()
                                .id_salt("side")
                                .show(ui, |ui| self.side(ui));
                        },
                    );
                });
            });
        resize_edges(&ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::WINDOW_BG().to_normalized_gamma_f32()
    }
}

// ---- small pieces ----

fn chip(ui: &mut Ui, text: &str, color: Color32) {
    Frame::NONE
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.5).color(color));
        });
}

fn primary(ui: &mut Ui, text: &str) -> bool {
    let button = egui::Button::new(
        RichText::new(text)
            .size(13.5)
            .strong()
            .color(Color32::from_rgb(0x0b, 0x0b, 0x0f)),
    )
    .fill(theme::ACCENT())
    .corner_radius(8)
    .min_size(vec2(84.0, 32.0));
    ui.add(button).clicked()
}

fn secondary(ui: &mut Ui, text: &str) -> bool {
    let button = egui::Button::new(RichText::new(text).size(13.0))
        .corner_radius(8)
        .min_size(vec2(0.0, 30.0));
    ui.add(button).clicked()
}

fn small(ui: &mut Ui, text: &str, hint: &str) -> bool {
    let button = egui::Button::new(RichText::new(text).size(12.0)).corner_radius(6);
    ui.add(button).on_hover_text(hint).clicked()
}

fn heading(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .size(11.5)
            .strong()
            .color(theme::COMMENT()),
    );
    ui.add_space(6.0);
}

/// A clickable line with an icon, a name and a quieter line under it.
fn row(ui: &mut Ui, icon: char, name: &str, detail: &str) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 46.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 8.0, theme::tint(10));
    }
    let painter = ui.painter_at(rect);
    painter.text(
        rect.left_center() + vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(16.0),
        theme::COMMENT(),
    );
    painter.text(
        rect.left_top() + vec2(36.0, 6.0),
        egui::Align2::LEFT_TOP,
        name,
        egui::FontId::proportional(14.0),
        theme::TEXT(),
    );
    painter.text(
        rect.left_top() + vec2(36.0, 26.0),
        egui::Align2::LEFT_TOP,
        detail,
        egui::FontId::proportional(11.5),
        theme::COMMENT(),
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Changelog entries, from their markdown: a heading per version and a bullet per line.
fn notes(ui: &mut Ui, entries: &[&updates::Entry]) {
    for entry in entries {
        ui.label(
            RichText::new(entry.version.as_str())
                .size(13.5)
                .strong()
                .color(theme::TEXT()),
        );
        for line in entry.notes.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let plain = line
                .trim_start_matches("- ")
                .replace("**", "")
                .replace('`', "");
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("•").color(theme::ACCENT()));
                ui.add(
                    egui::Label::new(RichText::new(plain).size(12.5).color(theme::MUTED())).wrap(),
                );
            });
        }
        ui.add_space(6.0);
    }
}
