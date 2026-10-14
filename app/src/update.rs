//! Updates. When it opens, and every hour while it is open, ride reads `latest.json` from the newest GitHub release of the
//! project, and when it names a newer version, downloads that release's setup program in
//! the background and checks it against the SHA-256 the manifest gives. The setup then
//! runs when ride closes, or at once from the status bar: it waits for ride to exit,
//! swaps in the new ride.exe and starts it again (see `--update` in the installer).
//!
//! Nothing here depends on a particular computer: the releases are built and published
//! by GitHub Actions (`.github/workflows/release.yml`), and every check talks to GitHub alone.

use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MANIFEST_URL: &str =
    "https://github.com/raisen1337/rideide/releases/latest/download/latest.json";
pub const RELEASES_URL: &str = "https://github.com/raisen1337/rideide/releases";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How often ride looks again while it stays open.
const RECHECK: u64 = 60 * 60;
/// A setup program is a few tens of megabytes; anything far bigger is not one.
const MAX_SETUP: u64 = 400 * 1024 * 1024;

/// What `latest.json` holds, as the release workflow writes it.
#[derive(Deserialize)]
struct Manifest {
    version: String,
    url: String,
    sha256: String,
    size: u64,
}

pub enum State {
    Idle,
    Checking,
    UpToDate,
    Downloading(String),
    /// A newer version, downloaded and checked, waiting to be installed.
    Ready {
        version: String,
        setup: PathBuf,
    },
    Failed(String),
}

enum Done {
    UpToDate,
    Ready(String, PathBuf),
    Failed(String),
}

pub struct Updater {
    pub state: State,
    rx: Option<Receiver<Done>>,
    /// The version being downloaded, sent once the check has found it.
    downloading: Option<Receiver<String>>,
    /// Whether the check was asked for by hand, so its outcome is worth a word.
    by_hand: bool,
    next_check: Option<SystemTime>,
    started: SystemTime,
    /// Set when an update got ready right after ride opened: ride restarts into it at once.
    pub apply_now: bool,
}

/// An update that is ready within this long of ride opening is put in straight away,
/// before any real work has started; a later one waits for ride to close.
const AT_OPEN: Duration = Duration::from_secs(90);

/// Where downloaded setups and the time of the last check are kept.
fn folder() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("HOME"))
        .unwrap_or_else(|| ".".into());
    PathBuf::from(base).join("ride").join("updates")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// "1.2.3", "v1.2.3" or "1.2.3-beta" as numbers to compare; a suffix is ignored.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim().trim_start_matches('v');
    let text = text.split(['-', '+']).next()?;
    let mut parts = text.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    Some((major, minor, patch))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Fetches a URL. Mozilla's root certificates are tried first, as they work the same on
/// every computer; if that fails, Windows' own are, which is what works behind a company
/// proxy that inspects traffic.
fn fetch(url: &str, limit: u64, timeout: Duration) -> Result<Vec<u8>, String> {
    let get = |platform: bool| -> Result<Vec<u8>, ureq::Error> {
        let mut config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .user_agent(format!("ride/{VERSION}"));
        if platform {
            config = config.tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            );
        }
        let agent: ureq::Agent = config.build().into();
        agent
            .get(url)
            .call()?
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
    };
    get(false)
        .or_else(|first| get(true).map_err(|_| first))
        .map_err(|e| e.to_string())
}

/// The whole check, on a worker thread: is there a newer version, and if so, get it.
fn check(report: impl Fn(String)) -> Done {
    let manifest = match fetch(MANIFEST_URL, 64 * 1024, Duration::from_secs(30))
        .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).map_err(|e| e.to_string()))
    {
        Ok(manifest) => manifest,
        Err(error) => return Done::Failed(format!("Couldn't check for updates: {error}")),
    };
    let _ = std::fs::create_dir_all(folder());
    let _ = std::fs::write(folder().join("last-check"), now().to_string());
    if !is_newer(&manifest.version, VERSION) {
        return Done::UpToDate;
    }
    let version = manifest.version.trim_start_matches('v').to_string();
    let want = manifest.sha256.trim().to_lowercase();
    let setup = folder().join(format!("ride-setup-{version}.exe"));
    // Already downloaded by an earlier check.
    if std::fs::read(&setup).is_ok_and(|bytes| hex(&Sha256::digest(&bytes)) == want) {
        return Done::Ready(version, setup);
    }
    if !manifest.url.starts_with("https://github.com/") || manifest.size > MAX_SETUP {
        return Done::Failed("The update's manifest doesn't look right; it was ignored.".into());
    }
    report(version.clone());
    let bytes = match fetch(&manifest.url, MAX_SETUP, Duration::from_secs(15 * 60)) {
        Ok(bytes) => bytes,
        Err(error) => return Done::Failed(format!("Couldn't download ride {version}: {error}")),
    };
    if bytes.len() as u64 != manifest.size || hex(&Sha256::digest(&bytes)) != want {
        return Done::Failed(format!(
            "The download of ride {version} didn't match its checksum, so it was thrown away."
        ));
    }
    let partial = setup.with_extension("part");
    let written = std::fs::write(&partial, &bytes).and_then(|()| std::fs::rename(&partial, &setup));
    match written {
        Ok(()) => Done::Ready(version, setup),
        Err(error) => Done::Failed(format!("Couldn't save the update: {error}")),
    }
}

impl Updater {
    pub fn new() -> Self {
        let dir = folder();
        // What an update left behind: the replaced program, setups already installed.
        if let Ok(exe) = std::env::current_exe() {
            if let Some(old) = exe.parent().map(|d| d.join("ride.old.exe")) {
                let _ = std::fs::remove_file(old);
            }
        }
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name
                .strip_prefix("ride-setup-")
                .and_then(|rest| rest.strip_suffix(".exe").or(rest.strip_suffix(".part")))
                .or(name.strip_prefix("tried-"));
            if version.is_some_and(|v| !is_newer(v, VERSION)) || name.ends_with(".part") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        // Every start checks; it is one small file. While ride stays open, every hour.
        Self {
            state: State::Idle,
            rx: None,
            downloading: None,
            by_hand: false,
            next_check: Some(SystemTime::now()),
            started: SystemTime::now(),
            apply_now: false,
        }
    }

    /// Why the last update didn't install, once; the setup program leaves it behind.
    pub fn take_last_error() -> Option<String> {
        let path = folder().join("last-error.txt");
        let text = std::fs::read_to_string(&path).ok()?;
        let _ = std::fs::remove_file(path);
        Some(text.trim().to_string()).filter(|t| !t.is_empty())
    }

    pub fn check_now(&mut self, ctx: &Context) {
        if matches!(self.state, State::Checking | State::Downloading(_)) {
            return;
        }
        self.by_hand = true;
        self.start(ctx);
    }

    fn start(&mut self, ctx: &Context) {
        let (tx, rx) = channel();
        let (report_tx, report_rx) = channel::<String>();
        self.state = State::Checking;
        self.rx = Some(rx);
        self.downloading = Some(report_rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let waker = ctx.clone();
            let done = check(move |version| {
                let _ = report_tx.send(version);
                waker.request_repaint();
            });
            let _ = tx.send(done);
            ctx.request_repaint();
        });
    }

    /// Called every frame: starts the daily check when it is due, and takes in results.
    /// Returns a line for the status bar when there is something to say.
    pub fn tick(&mut self, ctx: &Context) -> Option<String> {
        if let Some(report) = &self.downloading {
            if let Ok(version) = report.try_recv() {
                self.state = State::Downloading(version);
            }
        }
        if let Some(rx) = &self.rx {
            let Ok(done) = rx.try_recv() else {
                return None;
            };
            self.rx = None;
            self.downloading = None;
            let by_hand = std::mem::take(&mut self.by_hand);
            self.next_check = Some(SystemTime::now() + Duration::from_secs(RECHECK));
            return match done {
                Done::UpToDate => {
                    self.state = State::UpToDate;
                    by_hand.then(|| format!("ride {VERSION} is the newest version."))
                }
                Done::Ready(version, setup) => {
                    let early = self.started.elapsed().is_ok_and(|age| age < AT_OPEN);
                    // Tried once at an opening already and still here: that install failed,
                    // so it waits for ride to close rather than restarting it again and again.
                    let tried = folder().join(format!("tried-{version}"));
                    self.apply_now = early && settings::with(|s| s.auto_update) && !tried.exists();
                    if self.apply_now {
                        let _ = std::fs::write(&tried, "");
                    }
                    let line = if self.apply_now {
                        format!("Updating to ride {version}…")
                    } else {
                        format!("ride {version} is ready; it installs when you close ride.")
                    };
                    self.state = State::Ready { version, setup };
                    Some(line)
                }
                Done::Failed(error) => {
                    // Offline or GitHub unreachable: try again in a few hours, quietly.
                    self.next_check = Some(SystemTime::now() + Duration::from_secs(3 * 60 * 60));
                    self.state = State::Failed(error.clone());
                    by_hand.then_some(error)
                }
            };
        }
        let auto = settings::with(|s| s.auto_update);
        let due = self.next_check.is_some_and(|at| SystemTime::now() >= at);
        if auto && due && !matches!(self.state, State::Ready { .. }) {
            self.start(ctx);
        }
        // Wake up for the next check even when nothing else repaints.
        if let Some(wait) = self
            .next_check
            .and_then(|at| at.duration_since(SystemTime::now()).ok())
        {
            ctx.request_repaint_after(wait.min(Duration::from_secs(3600)));
        }
        None
    }

    pub fn ready(&self) -> Option<&str> {
        match &self.state {
            State::Ready { version, .. } => Some(version),
            _ => None,
        }
    }

    /// Starts the downloaded setup, which waits for this ride to exit before it replaces
    /// it; with `relaunch`, it starts ride again afterwards. True if it was started.
    pub fn install(&mut self, relaunch: bool) -> bool {
        let State::Ready { setup, .. } = &self.state else {
            return false;
        };
        let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
        else {
            return false;
        };
        let mut command = Command::new(setup);
        command
            .arg("--update")
            .arg(format!("--dir={}", dir.display()))
            .arg(format!("--wait={}", std::process::id()));
        if relaunch {
            command.arg("--launch");
        }
        let started = spawn_detached(&mut command);
        if started {
            // Installed once; a second close mustn't start it again.
            self.state = State::Idle;
        }
        started
    }
}

static CHANGELOG: &str = include_str!("../../CHANGELOG.md");

/// The changelog entries after `seen` up to this version, newest first.
pub fn changes_since(seen: &str) -> Vec<(String, String)> {
    CHANGELOG
        .split("\n## ")
        .skip(1)
        .filter_map(|entry| {
            let (version, body) = entry.split_once('\n')?;
            let version = version.trim().to_string();
            let wanted = is_newer(&version, seen) && !is_newer(&version, VERSION);
            wanted.then(|| (version, body.trim().to_string()))
        })
        .collect()
}

impl App {
    /// The "What's new" screen, once after an update, before anything else.
    /// Like the catch-up screen, it takes the whole window.
    pub(super) fn whats_new_screen(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, theme::EDITOR_BG());
        Panel::top("whats_new_title")
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

        let mut close = ctx.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Escape)
                || i.consume_key(Modifiers::NONE, Key::Enter)
        });
        Panel::bottom("whats_new_buttons")
            .frame(Frame::NONE.inner_margin(Margin {
                left: 24,
                right: 24,
                top: 10,
                bottom: 22,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let button = egui::Button::new(
                        RichText::new("Continue")
                            .size(15.0)
                            .color(theme::EDITOR_BG()),
                    )
                    .fill(theme::TEXT())
                    .corner_radius(8)
                    .min_size(vec2(150.0, 36.0));
                    close |= ui.add(button).on_hover_text("Enter").clicked();
                });
            });

        let entries = self.whats_new.clone();
        CentralPanel::default_margins()
            .frame(Frame::NONE)
            .show(ui, |ui| {
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    let width = (ui.available_width() - 48.0).min(640.0);
                    let side = ((ui.available_width() - width) / 2.0).max(0.0);
                    ui.horizontal(|ui| {
                        ui.add_space(side);
                        ui.vertical(|ui| {
                            ui.set_width(width);
                            ui.add_space((ui.ctx().content_rect().height() * 0.07).max(12.0));
                            ui.label(
                                RichText::new("What's new")
                                    .size(32.0)
                                    .color(theme::STRONG()),
                            );
                            ui.label(
                                RichText::new(format!("ride is now {VERSION}"))
                                    .color(theme::COMMENT()),
                            );
                            for (version, body) in &entries {
                                ui.add_space(22.0);
                                ui.label(
                                    RichText::new(version)
                                        .small()
                                        .strong()
                                        .color(theme::COMMENT()),
                                );
                                ui.add_space(6.0);
                                ui.spacing_mut().item_spacing.y = 8.0;
                                chat::markdown(ui, body, usize::MAX);
                            }
                            ui.add_space(24.0);
                        });
                    });
                });
            });
        resize_edges(&ctx);
        if close {
            self.whats_new.clear();
        }
    }
}

/// Starts a program that outlives ride: outside any job ride itself was started in
/// (where Windows allows that), and without a console window.
fn spawn_detached(command: &mut Command) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        command.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB);
        if command.spawn().is_ok() {
            return true;
        }
        command.creation_flags(flags);
    }
    command.spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn the_changelog_lists_what_is_newer_than_seen() {
        let all = changes_since("0.0.0");
        assert!(!all.is_empty());
        assert_eq!(
            all[0].0, VERSION,
            "CHANGELOG.md needs an entry for {VERSION}"
        );
        assert!(all.iter().all(|(_, body)| !body.is_empty()));
        assert!(changes_since(VERSION).is_empty());
    }

    #[test]
    fn the_manifest_reads_as_the_workflow_writes_it() {
        let text = r#"{"version":"0.2.0","url":"https://github.com/raisen1337/rideide/releases/download/v0.2.0/ride-setup.exe","sha256":"AB12","size":123,"notes":"x"}"#;
        let manifest: Manifest = serde_json::from_str(text).unwrap();
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(manifest.size, 123);
        assert_eq!(hex(&[0xab, 0x01]), "ab01");
    }
}
