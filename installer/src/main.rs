//! The setup program for rkit, the suite: ride, the editor, and rterm, the terminal. It
//! carries the programs inside itself, installs them for the current user, and doubles as
//! the uninstaller (`--uninstall`).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{
    self, pos2, vec2, Align, Align2, CentralPanel, Color32, Context, FontFamily, FontId, Frame,
    Layout, Margin, Panel, Rect, RichText, Sense, Stroke, TextEdit, Ui, ViewportBuilder,
};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The application, gzip-compressed, packed in by build.rs.
static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ride.exe.gz"));
/// rterm, the terminal that comes with ride, packed the same way.
static RTERM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rterm.exe.gz"));
/// rkit, which starts the apps and updates each on its own.
static RKIT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rkit.exe.gz"));
/// rdrop, for files between this computer and servers.
static RDROP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rdrop.exe.gz"));
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The icon as 128x128 RGBA pixels, for the window and the mark in the side rail.
static ICON: &[u8] = include_bytes!("../../app/assets/icon-128.rgba");
const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\ride";

const BG: Color32 = Color32::from_rgb(0x16, 0x16, 0x18);
const RAIL: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1f);
const LINE: Color32 = Color32::from_rgb(0x2c, 0x2c, 0x30);
const TEXT: Color32 = Color32::from_rgb(0xdc, 0xdc, 0xde);
const DIM: Color32 = Color32::from_rgb(0x86, 0x86, 0x8c);
const ACCENT: Color32 = Color32::from_rgb(0x87, 0xb4, 0xff);
const GOOD: Color32 = Color32::from_rgb(0x8c, 0xd0, 0x9a);
const BAD: Color32 = Color32::from_rgb(0xf0, 0x80, 0x8a);

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let uninstall = args.iter().any(|arg| arg == "--uninstall");
    // `--silent` does the whole job without a window, for scripted installs:
    //   rkit-setup --silent [--dir=PATH] [--portable]      (--portable: no shortcuts, not registered)
    //   uninstall --uninstall --silent [--remove-data]
    // `--preview` runs the whole interface without touching the computer: no files,
    // shortcuts or registry entries are written, and nothing is launched or removed.
    let preview = args.iter().any(|arg| arg == "--preview");
    // `--update` is what ride runs to update itself; see `update`.
    if !preview && args.iter().any(|arg| arg == "--update") {
        std::process::exit(if update(&args) { 0 } else { 1 });
    }
    if !preview && args.iter().any(|arg| arg == "--silent") {
        std::process::exit(if silent(&args, uninstall) { 0 } else { 1 });
    }
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([780.0, 500.0])
            .with_resizable(false)
            .with_decorations(false)
            .with_icon(egui::IconData {
                rgba: ICON.to_vec(),
                width: 128,
                height: 128,
            })
            .with_title(if uninstall {
                "Uninstall rkit"
            } else {
                "rkit Setup"
            }),
        centered: true,
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "rkit-setup",
        options,
        Box::new(move |cc| Ok(Box::new(Setup::new(cc, uninstall, preview)))),
    )
}

/// A command that never flashes a console window.
fn quiet(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn local_app_data() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| "C:\\".into()))
}

/// Where a fresh install goes: where rkit already is, if it is installed, so running setup
/// again updates that copy (installs from before the name rkit live in `Programs\ride`).
fn default_dir() -> PathBuf {
    registered_dir().unwrap_or_else(|| local_app_data().join("Programs").join("rkit"))
}

/// The folder Windows lists the install in, under "Installed apps".
fn registered_dir() -> Option<PathBuf> {
    let out = quiet("reg")
        .args(["query", UNINSTALL_KEY, "/v", "InstallLocation"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (_, value) = text.lines().find_map(|line| line.split_once("REG_SZ"))?;
    Some(PathBuf::from(value.trim())).filter(|dir| !dir.as_os_str().is_empty())
}

fn start_menu_link() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default())
        .join("Microsoft\\Windows\\Start Menu\\Programs\\ride.lnk")
}

fn rterm_link() -> PathBuf {
    start_menu_link().with_file_name("rterm.lnk")
}

fn rkit_link() -> PathBuf {
    start_menu_link().with_file_name("rkit.lnk")
}

fn rdrop_link() -> PathBuf {
    start_menu_link().with_file_name("rdrop.lnk")
}

/// A packed program, unpacked.
fn unpack(packed: &[u8]) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(packed)
        .read_to_end(&mut data)
        .map_err(|e| format!("The package is damaged: {e}"))?;
    Ok(data)
}

/// Puts a program in place even while a copy of it is running: Windows can rename a
/// program file that is open, but not overwrite it, so the old one is moved aside first.
/// A failure at any point leaves the old one working.
fn swap_in(target: &Path, data: &[u8]) -> Result<(), String> {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let fresh = target.with_extension("exe.new");
    let old = target.with_extension("old.exe");
    std::fs::write(&fresh, data).map_err(|e| format!("Can't write the new {name}: {e}"))?;
    let _ = std::fs::remove_file(&old);
    // Some antivirus scanners hold the file a moment longer.
    let mut moved = !target.exists();
    for _ in 0..20 {
        if moved || std::fs::rename(target, &old).is_ok() {
            moved = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !moved {
        let _ = std::fs::remove_file(&fresh);
        return Err(format!(
            "{name} stayed in use, so the update was left for later."
        ));
    }
    if let Err(error) = std::fs::rename(&fresh, target) {
        let _ = std::fs::rename(&old, target);
        let _ = std::fs::remove_file(&fresh);
        return Err(format!("Can't put the new {name} in place: {error}"));
    }
    Ok(())
}

/// Installs rterm beside ride, with a Start menu entry and "Open in rterm" on folders.
fn install_rterm(dir: &Path, start_menu: bool, context_menu: bool) -> Result<(), String> {
    let exe = dir.join("rterm.exe");
    swap_in(&exe, &unpack(RTERM)?)?;
    if start_menu {
        let script = format!(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({}); $s.TargetPath = {}; \
             $s.Description = 'rterm, a terminal'; $s.Save()",
            ps(&rterm_link().to_string_lossy()),
            ps(&exe.to_string_lossy()),
        );
        run_powershell(&script).map_err(|e| format!("Can't create rterm's shortcut: {e}"))?;
        // Lets Win+R and Explorer's address bar start it by name.
        let _ = quiet("reg")
            .args([
                "add",
                RTERM_APP_PATH,
                "/ve",
                "/d",
                &exe.to_string_lossy(),
                "/f",
            ])
            .status();
    }
    if context_menu {
        add_menu_entries(&exe, &RTERM_MENU_KEYS, "Open in rterm", "-d ")?;
    }
    Ok(())
}

/// Installs rkit beside the apps, with a Start menu entry.
fn install_rkit(dir: &Path, start_menu: bool) -> Result<(), String> {
    let exe = dir.join("rkit.exe");
    swap_in(&exe, &unpack(RKIT)?)?;
    if start_menu {
        let script = format!(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({}); $s.TargetPath = {}; \
             $s.Description = 'rkit: ride, rterm and their updates'; $s.Save()",
            ps(&rkit_link().to_string_lossy()),
            ps(&exe.to_string_lossy()),
        );
        run_powershell(&script).map_err(|e| format!("Can't create rkit's shortcut: {e}"))?;
        let _ = quiet("reg")
            .args([
                "add",
                RKIT_APP_PATH,
                "/ve",
                "/d",
                &exe.to_string_lossy(),
                "/f",
            ])
            .status();
    }
    Ok(())
}

/// Installs rdrop beside the others, with a Start menu entry and "Open in rdrop" on folders.
fn install_rdrop(dir: &Path, start_menu: bool, context_menu: bool) -> Result<(), String> {
    let exe = dir.join("rdrop.exe");
    swap_in(&exe, &unpack(RDROP)?)?;
    if start_menu {
        let script = format!(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({}); $s.TargetPath = {}; \
             $s.Description = 'rdrop: files between this computer and your servers'; $s.Save()",
            ps(&rdrop_link().to_string_lossy()),
            ps(&exe.to_string_lossy()),
        );
        run_powershell(&script).map_err(|e| format!("Can't create rdrop's shortcut: {e}"))?;
        let _ = quiet("reg")
            .args([
                "add",
                RDROP_APP_PATH,
                "/ve",
                "/d",
                &exe.to_string_lossy(),
                "/f",
            ])
            .status();
    }
    if context_menu {
        add_menu_entries(&exe, &RDROP_MENU_KEYS, "Open in rdrop", "")?;
    }
    Ok(())
}

const RDROP_APP_PATH: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\rdrop.exe";

/// rdrop's entries: a folder, and the empty space of a folder or the desktop.
const RDROP_MENU_KEYS: [(&str, &str); 2] = [
    (r"HKCU\Software\Classes\Directory\shell\rdrop", "%1"),
    (
        r"HKCU\Software\Classes\Directory\Background\shell\rdrop",
        "%V",
    ),
];

fn remove_rdrop(dir: &Path) {
    let _ = std::fs::remove_file(rdrop_link());
    let _ = quiet("reg").args(["delete", RDROP_APP_PATH, "/f"]).status();
    for (key, _) in RDROP_MENU_KEYS {
        let _ = quiet("reg").args(["delete", key, "/f"]).status();
    }
    let exe = dir.join("rdrop.exe");
    if std::fs::remove_file(&exe).is_err() {
        let _ = std::fs::rename(&exe, exe.with_extension("old.exe"));
    }
}

const RKIT_APP_PATH: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\rkit.exe";

fn remove_rkit(dir: &Path) {
    let _ = std::fs::remove_file(rkit_link());
    let _ = quiet("reg").args(["delete", RKIT_APP_PATH, "/f"]).status();
    let exe = dir.join("rkit.exe");
    if std::fs::remove_file(&exe).is_err() {
        let _ = std::fs::rename(&exe, exe.with_extension("old.exe"));
    }
}

const RTERM_APP_PATH: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\rterm.exe";

fn remove_rterm(dir: &Path) {
    let _ = std::fs::remove_file(rterm_link());
    let _ = quiet("reg").args(["delete", RTERM_APP_PATH, "/f"]).status();
    for (key, _) in RTERM_MENU_KEYS {
        let _ = quiet("reg").args(["delete", key, "/f"]).status();
    }
    let exe = dir.join("rterm.exe");
    // A running rterm keeps its file; it is moved aside to go with the folder.
    if std::fs::remove_file(&exe).is_err() {
        let _ = std::fs::rename(&exe, exe.with_extension("old.exe"));
    }
}

/// PowerShell quoting: a single-quoted string with its quotes doubled.
fn ps(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn run_powershell(script: &str) -> Result<(), String> {
    let out = quiet("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Creates shortcuts to the installed program with the Windows shell's own shortcut object.
fn create_shortcuts(exe: &Path, desktop: bool, start_menu: bool) -> Result<(), String> {
    let (exe, dir) = (
        exe.to_string_lossy(),
        exe.parent().unwrap_or(exe).to_string_lossy(),
    );
    let mut script = format!(
        "$shell = New-Object -ComObject WScript.Shell; \
         function Link($path) {{ $s = $shell.CreateShortcut($path); $s.TargetPath = {}; \
         $s.WorkingDirectory = {}; $s.Description = 'ride'; $s.Save() }}; ",
        ps(&exe),
        ps(&dir)
    );
    if start_menu {
        script.push_str(&format!(
            "Link {}; ",
            ps(&start_menu_link().to_string_lossy())
        ));
    }
    if desktop {
        // Asked from Windows, because the desktop may be redirected (to OneDrive, for one).
        script.push_str("Link (Join-Path ([Environment]::GetFolderPath('Desktop')) 'ride.lnk'); ");
    }
    run_powershell(&script)
}

fn remove_shortcuts() {
    let _ = std::fs::remove_file(start_menu_link());
    let _ = run_powershell(
        "Remove-Item (Join-Path ([Environment]::GetFolderPath('Desktop')) 'ride.lnk') -ErrorAction SilentlyContinue",
    );
}

/// Whether `dir` is the install Windows lists under "Installed apps". A portable copy
/// elsewhere is updated without touching that install's entry, shortcuts or menus.
fn is_registered(dir: &Path) -> bool {
    let plain = |p: &Path| {
        p.display()
            .to_string()
            .trim_end_matches('\\')
            .to_lowercase()
    };
    registered_dir().is_some_and(|listed| plain(&listed) == plain(dir))
}

/// Lists ride under "Installed apps", with this program as its uninstaller.
fn register(dir: &Path, size_kb: u64) -> Result<(), String> {
    let exe = dir.join("ride.exe");
    let uninstall = format!("\"{}\" --uninstall", dir.join("uninstall.exe").display());
    let values: [(&str, &str, String); 9] = [
        ("DisplayName", "REG_SZ", "rkit (ride, rterm)".into()),
        ("DisplayVersion", "REG_SZ", VERSION.into()),
        ("Publisher", "REG_SZ", "rkit".into()),
        ("InstallLocation", "REG_SZ", dir.display().to_string()),
        ("DisplayIcon", "REG_SZ", exe.display().to_string()),
        ("UninstallString", "REG_SZ", uninstall),
        ("EstimatedSize", "REG_DWORD", size_kb.to_string()),
        ("NoModify", "REG_DWORD", "1".into()),
        ("NoRepair", "REG_DWORD", "1".into()),
    ];
    for (name, kind, value) in values {
        let status = quiet("reg")
            .args([
                "add",
                UNINSTALL_KEY,
                "/v",
                name,
                "/t",
                kind,
                "/d",
                &value,
                "/f",
            ])
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("could not write {name} to the registry"));
        }
    }
    Ok(())
}

/// Where the right-click entries live: for any file, for a folder, and for the empty
/// space of a folder or the desktop. `%1` is what was clicked; `%V` the folder itself.
const MENU_KEYS: [(&str, &str); 3] = [
    (r"HKCU\Software\Classes\*\shell\ride", "%1"),
    (r"HKCU\Software\Classes\Directory\shell\ride", "%1"),
    (
        r"HKCU\Software\Classes\Directory\Background\shell\ride",
        "%V",
    ),
];

/// rterm's entries: a folder, and the empty space of a folder or the desktop.
const RTERM_MENU_KEYS: [(&str, &str); 2] = [
    (r"HKCU\Software\Classes\Directory\shell\rterm", "%1"),
    (
        r"HKCU\Software\Classes\Directory\Background\shell\rterm",
        "%V",
    ),
];

/// Adds "Open with ride" to Explorer's right-click menu, for this user only.
fn add_context_menu(exe: &Path) -> Result<(), String> {
    add_menu_entries(exe, &MENU_KEYS, "Open with ride", "")
}

/// Adds an entry named `label` to Explorer's right-click menus under `keys`, running `exe`
/// with `flag` and then what was clicked.
fn add_menu_entries(
    exe: &Path,
    keys: &[(&str, &str)],
    label: &str,
    flag: &str,
) -> Result<(), String> {
    let exe = exe.display().to_string();
    for &(key, argument) in keys {
        let command = format!("\"{exe}\" {flag}\"{argument}\"");
        let entries: [(&str, Vec<&str>); 3] = [
            (key, vec!["/ve", "/d", label]),
            (key, vec!["/v", "Icon", "/d", &exe]),
            (&format!("{key}\\command"), vec!["/ve", "/d", &command]),
        ];
        for (key, value) in entries {
            let status = quiet("reg")
                .arg("add")
                .arg(key)
                .args(value)
                .arg("/f")
                .status()
                .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err(format!("could not add \"{label}\" to the right-click menu"));
            }
        }
    }
    Ok(())
}

fn remove_context_menu() {
    for (key, _) in MENU_KEYS {
        let _ = quiet("reg").args(["delete", key, "/f"]).status();
    }
}

fn ride_is_running() -> bool {
    quiet("tasklist")
        .args(["/FI", "IMAGENAME eq ride.exe", "/NH"])
        .output()
        .is_ok_and(|out| {
            String::from_utf8_lossy(&out.stdout)
                .to_lowercase()
                .contains("ride.exe")
        })
}

#[derive(Clone)]
struct Options {
    dir: PathBuf,
    desktop: bool,
    start_menu: bool,
    /// List ride under Windows' installed apps.
    register: bool,
    /// Add "Open with ride" to the right-click menu of files, folders and the desktop.
    context_menu: bool,
    /// Install Tribib, the design app the Designer works in, from Tribib's own releases.
    tribib: bool,
    /// Install rterm, the terminal that comes with ride.
    rterm: bool,
}

/// Where Tribib publishes its Windows releases; its own app updates from here too.
const TRIBIB_RELEASES: &str = "https://s3.tribib.com/desktop/win";

fn tribib_exe() -> PathBuf {
    local_app_data()
        .join("Programs")
        .join("tribib-desktop")
        .join("Tribib.exe")
}

/// Downloads Tribib's latest installer from Tribib, checks it against the checksum Tribib
/// publishes with it, and runs it for this user, quietly.
fn install_tribib() -> Result<(), String> {
    use sha2::{Digest, Sha512};
    use std::io::Write;
    let manifest = ureq::get(&format!("{TRIBIB_RELEASES}/latest.yml"))
        .call()
        .map_err(|e| format!("couldn't reach Tribib: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let field = |key: &str| {
        manifest
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().trim_matches('\'').trim_matches('"').to_string())
    };
    let (Some(file), Some(checksum)) = (field("path:"), field("sha512:")) else {
        return Err("Tribib's release list isn't as expected".into());
    };
    if file.contains(['/', '\\']) || !file.ends_with(".exe") {
        return Err("Tribib's release list names an unexpected file".into());
    }
    let mut response = ureq::get(&format!("{TRIBIB_RELEASES}/{file}"))
        .call()
        .map_err(|e| format!("couldn't download Tribib: {e}"))?;
    let mut reader = response.body_mut().as_reader();
    let path = std::env::temp_dir().join(&file);
    let mut out = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut hasher = Sha512::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        out.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
    }
    drop(out);
    if base64(&hasher.finalize()) != checksum {
        let _ = std::fs::remove_file(&path);
        return Err("the download didn't match Tribib's checksum".into());
    }
    let status = quiet(&path.to_string_lossy())
        .arg("/S")
        .status()
        .map_err(|e| format!("couldn't run Tribib's installer: {e}"))?;
    let _ = std::fs::remove_file(&path);
    if !status.success() || !tribib_exe().exists() {
        return Err("Tribib's installer didn't finish".into());
    }
    Ok(())
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Runs an install or uninstall to the end without any interface. True on success.
fn silent(args: &[String], uninstalling: bool) -> bool {
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    let (tx, rx) = channel();
    let ctx = Context::default();
    if uninstalling {
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(default_dir);
        uninstall(dir.clone(), has("--remove-data"), tx, ctx);
        remove_folder_after_exit(&dir);
    } else {
        let dir = args
            .iter()
            .find_map(|arg| arg.strip_prefix("--dir="))
            .map_or_else(default_dir, PathBuf::from);
        let shortcuts = !has("--portable");
        install(
            Options {
                dir,
                desktop: shortcuts,
                start_menu: shortcuts,
                register: shortcuts,
                context_menu: shortcuts,
                // A silent install asks for it by name.
                tribib: has("--tribib"),
                rterm: !has("--no-rterm"),
            },
            tx,
            ctx,
        );
    }
    rx.try_iter()
        .any(|message| matches!(message, Progress::Done))
}

/// Updates an existing install in place, without a window:
///   rkit-setup --update --dir=PATH [--wait=PID] [--launch]
/// Waits for that ride (PID) to exit, swaps in the new ride.exe so that a failure at any
/// point leaves the old one working, refreshes the uninstaller and the version Windows
/// shows, and starts ride again with `--launch`. Shortcuts and menu entries the user has
/// kept or removed are left as they are. What went wrong is written to `last-error.txt`
/// in ride's update folder, for ride to show.
fn update(args: &[String]) -> bool {
    let value = |name: &str| args.iter().find_map(|arg| arg.strip_prefix(name));
    let dir = value("--dir=").map_or_else(default_dir, PathBuf::from);
    let log_dir = local_app_data().join("ride").join("updates");
    let _ = std::fs::create_dir_all(&log_dir);
    let _ = std::fs::remove_file(log_dir.join("last-error.txt"));

    if let Some(pid) = value("--wait=").and_then(|pid| pid.parse::<u32>().ok()) {
        wait_for_exit(pid);
    }
    let result = replace_app(&dir);
    if let Err(error) = &result {
        let _ = std::fs::write(log_dir.join("last-error.txt"), error);
    }
    if args.iter().any(|arg| arg == "--launch") {
        let mut command = Command::new(dir.join("ride.exe"));
        command.current_dir(&dir);
        let _ = command.spawn();
    }
    result.is_ok()
}

/// Waits up to 20 seconds for a process to end, then ends it.
fn wait_for_exit(pid: u32) {
    let running = || {
        quiet("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
    };
    let started = Instant::now();
    while running() {
        if started.elapsed() > Duration::from_secs(20) {
            let _ = quiet("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .status();
            std::thread::sleep(Duration::from_millis(800));
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn replace_app(dir: &Path) -> Result<(), String> {
    let app = unpack(PAYLOAD)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
    swap_in(&dir.join("ride.exe"), &app)?;
    let registered = is_registered(dir);
    // rterm is updated with ride. An install from before rterm gets it now, with its Start
    // menu entry and right-click entries when ride itself was installed with its own.
    let rterm = dir.join("rterm.exe");
    let first = !rterm.exists();
    let unwanted = dir.join("no-rterm").exists();
    if !unwanted {
        if let Err(error) = install_rterm(dir, first && registered, first && registered) {
            let log = local_app_data().join("ride").join("updates");
            let _ = std::fs::write(log.join("rterm-error.txt"), error);
        }
    }
    // rdrop likewise, with its Start menu and right-click entries the first time.
    let first = !dir.join("rdrop.exe").exists();
    if let Err(error) = install_rdrop(dir, first && registered, first && registered) {
        let log = local_app_data().join("ride").join("updates");
        let _ = std::fs::write(log.join("rdrop-error.txt"), error);
    }
    // rkit likewise, with its Start menu entry the first time.
    let first = !dir.join("rkit.exe").exists();
    if let Err(error) = install_rkit(dir, first && registered) {
        let log = local_app_data().join("ride").join("updates");
        let _ = std::fs::write(log.join("rkit-error.txt"), error);
    }
    if let Ok(me) = std::env::current_exe() {
        let uninstaller = dir.join("uninstall.exe");
        if me != uninstaller && uninstaller.exists() {
            let _ = std::fs::copy(me, uninstaller);
        }
    }
    if registered {
        let _ = quiet("reg")
            .args([
                "add",
                UNINSTALL_KEY,
                "/v",
                "DisplayVersion",
                "/t",
                "REG_SZ",
                "/d",
                VERSION,
                "/f",
            ])
            .status();
    }
    Ok(())
}

/// Writes a file that may be in use: if the first try fails, ride is asked to close
/// (and then made to), and the write is tried again.
fn write_over_running(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if std::fs::write(path, data).is_ok() {
        return Ok(());
    }
    let _ = quiet("taskkill").args(["/IM", "ride.exe"]).status();
    std::thread::sleep(Duration::from_millis(1500));
    if std::fs::write(path, data).is_ok() {
        return Ok(());
    }
    let _ = quiet("taskkill").args(["/IM", "ride.exe", "/F"]).status();
    std::thread::sleep(Duration::from_millis(700));
    std::fs::write(path, data)
}

enum Progress {
    Step(f32, &'static str),
    Done,
    Failed(String),
}

/// Each step is given at least this long on screen, so the progress can be followed.
const STEP_TIME: Duration = Duration::from_millis(420);

fn paced<T>(
    tx: &Sender<Progress>,
    ctx: &Context,
    at: f32,
    label: &'static str,
    work: impl FnOnce() -> T,
) -> T {
    let _ = tx.send(Progress::Step(at, label));
    ctx.request_repaint();
    let started = Instant::now();
    let result = work();
    std::thread::sleep(STEP_TIME.saturating_sub(started.elapsed()));
    result
}

/// Walks through the steps of an install or uninstall for `--preview`, doing none of them.
fn rehearse(uninstalling: bool, tx: Sender<Progress>, ctx: Context) {
    let steps: &[(f32, &'static str)] = if uninstalling {
        &[
            (0.3, "Removing shortcuts"),
            (0.6, "Removing the apps"),
            (0.9, "Unregistering from Windows"),
            (1.0, "Finishing"),
        ]
    } else {
        &[
            (0.05, "Preparing the folder"),
            (0.35, "Copying ride"),
            (0.6, "Adding the uninstaller"),
            (0.7, "Creating shortcuts"),
            (0.8, "Adding \"Open with ride\" to the right-click menu"),
            (0.83, "Installing rkit"),
            (0.84, "Installing rdrop"),
            (0.85, "Installing rterm"),
            (0.9, "Registering with Windows"),
            (1.0, "Finishing"),
        ]
    };
    for (at, label) in steps {
        paced(&tx, &ctx, *at, label, || {});
    }
    let _ = tx.send(Progress::Done);
    ctx.request_repaint();
}

fn install(options: Options, tx: Sender<Progress>, ctx: Context) {
    let run = || -> Result<(), String> {
        let dir = &options.dir;
        paced(&tx, &ctx, 0.05, "Preparing the folder", || {
            std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))
        })?;
        let size = paced(
            &tx,
            &ctx,
            0.35,
            "Copying ride",
            || -> Result<u64, String> {
                let app = unpack(PAYLOAD)?;
                // A copy of ride running from this folder would lock the file; one running from elsewhere is left alone.
                write_over_running(&dir.join("ride.exe"), &app)
                    .map_err(|e| format!("Can't write ride.exe: {e}"))?;
                Ok(app.len() as u64)
            },
        )?;
        paced(&tx, &ctx, 0.6, "Adding the uninstaller", || {
            let me = std::env::current_exe().map_err(|e| e.to_string())?;
            let target = dir.join("uninstall.exe");
            // Re-running the installed uninstaller as setup would copy it onto itself.
            if me != target {
                std::fs::copy(me, target)
                    .map_err(|e| format!("Can't write the uninstaller: {e}"))?;
            }
            Ok::<(), String>(())
        })?;
        paced(&tx, &ctx, 0.75, "Creating shortcuts", || {
            if options.desktop || options.start_menu {
                create_shortcuts(&dir.join("ride.exe"), options.desktop, options.start_menu)
                    .map_err(|e| format!("Can't create the shortcuts: {e}"))
            } else {
                Ok(())
            }
        })?;
        paced(
            &tx,
            &ctx,
            0.8,
            "Adding \"Open with ride\" to the right-click menu",
            || {
                if options.context_menu {
                    add_context_menu(&dir.join("ride.exe"))
                } else {
                    Ok(())
                }
            },
        )?;
        paced(&tx, &ctx, 0.83, "Installing rkit", || {
            install_rkit(dir, options.start_menu)
        })?;
        paced(&tx, &ctx, 0.84, "Installing rdrop", || {
            install_rdrop(dir, options.start_menu, options.context_menu)
        })?;
        paced(&tx, &ctx, 0.85, "Installing rterm", || {
            // Left out on purpose, it stays out on updates too.
            let marker = dir.join("no-rterm");
            if options.rterm {
                let _ = std::fs::remove_file(&marker);
                install_rterm(dir, options.start_menu, options.context_menu)
            } else {
                remove_rterm(dir);
                std::fs::write(&marker, "").map_err(|e| e.to_string())
            }
        })?;
        paced(&tx, &ctx, 0.9, "Registering with Windows", || {
            if options.register {
                register(dir, size / 1024)
            } else {
                Ok(())
            }
        })?;
        if options.tribib && !tribib_exe().exists() {
            // Tribib is a help to ride, not a part of it: if it can't be had now, ride is
            // installed anyway, and the Designer offers it again.
            paced(&tx, &ctx, 0.97, "Installing Tribib", || {
                if let Err(error) = install_tribib() {
                    let log = local_app_data().join("ride").join("updates");
                    let _ = std::fs::create_dir_all(&log);
                    let _ = std::fs::write(log.join("tribib-error.txt"), &error);
                }
            });
        }
        paced(&tx, &ctx, 1.0, "Finishing", || {});
        Ok(())
    };
    let _ = tx.send(match run() {
        Ok(()) => Progress::Done,
        Err(error) => Progress::Failed(error),
    });
    ctx.request_repaint();
}

fn uninstall(dir: PathBuf, remove_data: bool, tx: Sender<Progress>, ctx: Context) {
    paced(&tx, &ctx, 0.3, "Removing shortcuts", remove_shortcuts);
    paced(&tx, &ctx, 0.6, "Removing the apps", || {
        let app = dir.join("ride.exe");
        // If the file is locked, this install of ride is running: it is closed, then removed.
        if app.exists() && std::fs::remove_file(&app).is_err() && ride_is_running() {
            let _ = quiet("taskkill").args(["/IM", "ride.exe", "/F"]).status();
            std::thread::sleep(Duration::from_millis(800));
            let _ = std::fs::remove_file(&app);
        }
        remove_rterm(&dir);
        remove_rkit(&dir);
        remove_rdrop(&dir);
    });
    if remove_data {
        paced(
            &tx,
            &ctx,
            0.75,
            "Removing chats, themes and settings",
            || {
                if let Some(roaming) = std::env::var_os("APPDATA") {
                    let _ = std::fs::remove_dir_all(PathBuf::from(&roaming).join("ride"));
                    let _ = std::fs::remove_dir_all(PathBuf::from(&roaming).join("rterm"));
                    let _ = std::fs::remove_dir_all(PathBuf::from(&roaming).join("rkit"));
                    let _ = std::fs::remove_dir_all(PathBuf::from(roaming).join("rdrop"));
                }
            },
        );
    }
    paced(&tx, &ctx, 0.9, "Unregistering from Windows", || {
        remove_context_menu();
        let _ = quiet("reg").args(["delete", UNINSTALL_KEY, "/f"]).status();
        // The name and icon ride registers for its notifications.
        let _ = quiet("reg")
            .args([
                "delete",
                r"HKCU\Software\Classes\AppUserModelId\raisen1337.ride",
                "/f",
            ])
            .status();
    });
    paced(&tx, &ctx, 1.0, "Finishing", || {});
    let _ = tx.send(Progress::Done);
    ctx.request_repaint();
}

/// Deletes the install folder once this program, which lives in it, has exited.
fn remove_folder_after_exit(dir: &Path) {
    // cmd has its own quoting rules, so the command line is handed over exactly as written
    // here: the outer quotes are the ones `/C` strips, the inner ones protect the path.
    let command = format!(
        "/C \"ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"\"",
        dir.display()
    );
    let mut cmd = quiet("cmd");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(command);
    }
    let _ = cmd.current_dir(std::env::temp_dir()).spawn();
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Welcome,
    Options,
    Working,
    Finished,
}

struct Setup {
    uninstalling: bool,
    /// Shown for looking at only; see `--preview`.
    preview: bool,
    icon: egui::TextureHandle,
    page: Page,
    /// When the current page appeared, for its entrance animation.
    page_since: f64,
    options: Options,
    dir_text: String,
    launch: bool,
    remove_data: bool,
    has_ssh: bool,
    progress: Option<Receiver<Progress>>,
    fraction: f32,
    action: &'static str,
    error: Option<String>,
}

impl Setup {
    fn new(cc: &eframe::CreationContext<'_>, uninstalling: bool, preview: bool) -> Self {
        style(&cc.egui_ctx);
        let icon = cc.egui_ctx.load_texture(
            "icon",
            egui::ColorImage::from_rgba_unmultiplied([128, 128], ICON),
            egui::TextureOptions::LINEAR,
        );
        // As the uninstaller, this program sits in the folder it removes.
        let dir = if uninstalling {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .unwrap_or_else(default_dir)
        } else {
            default_dir()
        };
        let has_ssh = quiet("where")
            .arg("ssh")
            .output()
            .is_ok_and(|out| out.status.success());
        Self {
            uninstalling,
            preview,
            icon,
            page: Page::Welcome,
            page_since: 0.0,
            dir_text: dir.display().to_string(),
            options: Options {
                dir,
                desktop: true,
                start_menu: true,
                register: true,
                context_menu: true,
                tribib: true,
                rterm: true,
            },
            launch: true,
            remove_data: false,
            has_ssh,
            progress: None,
            fraction: 0.0,
            action: "",
            error: None,
        }
    }

    fn go(&mut self, ctx: &Context, page: Page) {
        self.page = page;
        self.page_since = ctx.input(|i| i.time);
    }

    fn start(&mut self, ctx: &Context) {
        let (tx, rx) = channel();
        self.progress = Some(rx);
        self.fraction = 0.0;
        self.error = None;
        let ctx_for_thread = ctx.clone();
        if self.preview {
            let uninstalling = self.uninstalling;
            std::thread::spawn(move || rehearse(uninstalling, tx, ctx_for_thread));
        } else if self.uninstalling {
            let (dir, remove_data) = (self.options.dir.clone(), self.remove_data);
            std::thread::spawn(move || uninstall(dir, remove_data, tx, ctx_for_thread));
        } else {
            self.options.dir = PathBuf::from(self.dir_text.trim());
            let options = self.options.clone();
            std::thread::spawn(move || install(options, tx, ctx_for_thread));
        }
        self.go(ctx, Page::Working);
    }

    fn poll(&mut self, ctx: &Context) {
        let Some(rx) = &self.progress else { return };
        while let Ok(message) = rx.try_recv() {
            match message {
                Progress::Step(fraction, action) => {
                    (self.fraction, self.action) = (fraction, action)
                }
                Progress::Done => {
                    self.progress = None;
                    self.go(ctx, Page::Finished);
                    return;
                }
                Progress::Failed(error) => {
                    self.progress = None;
                    self.error = Some(error);
                    self.go(ctx, Page::Finished);
                    return;
                }
            }
        }
    }

    fn close(&self, ctx: &Context) {
        if self.preview {
            // A preview installed nothing, so there is nothing to launch or clean up.
        } else if self.uninstalling && self.page == Page::Finished {
            remove_folder_after_exit(&self.options.dir);
        } else if !self.uninstalling
            && self.page == Page::Finished
            && self.error.is_none()
            && self.launch
        {
            // rkit, which starts the rest; ride when rkit isn't there.
            let rkit = self.options.dir.join("rkit.exe");
            let exe = if rkit.exists() {
                rkit
            } else {
                self.options.dir.join("ride.exe")
            };
            let _ = Command::new(&exe).current_dir(&self.options.dir).spawn();
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    // ---- the left rail ----

    fn rail(&self, ui: &mut Ui) {
        let steps: &[&str] = if self.uninstalling {
            &["Confirm", "Removing", "Done"]
        } else {
            &["Welcome", "Options", "Installing", "Done"]
        };
        let current = match (self.uninstalling, self.page) {
            (true, Page::Welcome | Page::Options) => 0,
            (true, Page::Working) => 1,
            (true, Page::Finished) => 2,
            (false, page) => page as usize,
        };

        ui.add_space(26.0);
        ui.horizontal(|ui| {
            ui.add_space(22.0);
            let size = vec2(38.0, 38.0);
            ui.add(egui::Image::new(egui::load::SizedTexture::new(
                self.icon.id(),
                size,
            )));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("rkit").size(18.0).color(Color32::WHITE));
                ui.label(
                    RichText::new(format!("version {VERSION}"))
                        .size(11.5)
                        .color(DIM),
                );
            });
        });
        ui.add_space(34.0);

        // The marker glides from step to step instead of jumping.
        let glide = ui
            .ctx()
            .animate_value_with_time(egui::Id::new("step"), current as f32, 0.35);
        let top = ui.cursor().top();
        let x = ui.max_rect().left() + 39.0;
        const SPACING: f32 = 44.0;
        let y_of = |index: f32| top + 10.0 + index * SPACING;
        let painter = ui.painter();
        painter.line_segment(
            [pos2(x, y_of(0.0)), pos2(x, y_of(steps.len() as f32 - 1.0))],
            Stroke::new(2.0, LINE),
        );
        painter.line_segment(
            [pos2(x, y_of(0.0)), pos2(x, y_of(glide))],
            Stroke::new(2.0, ACCENT),
        );
        for (index, name) in steps.iter().enumerate() {
            let center = pos2(x, y_of(index as f32));
            let reached = glide + 0.05 >= index as f32;
            painter.circle_filled(center, 6.0, if reached { ACCENT } else { RAIL });
            painter.circle_stroke(
                center,
                6.0,
                Stroke::new(2.0, if reached { ACCENT } else { LINE }),
            );
            let color = if index == current {
                Color32::WHITE
            } else if reached {
                TEXT
            } else {
                DIM
            };
            painter.text(
                center + vec2(22.0, 0.0),
                Align2::LEFT_CENTER,
                *name,
                FontId::proportional(14.5),
                color,
            );
        }
        // A halo follows the marker.
        painter.circle_stroke(
            pos2(x, y_of(glide)),
            10.0,
            Stroke::new(1.5, ACCENT.gamma_multiply(0.45)),
        );
    }

    // ---- pages ----

    fn heading(ui: &mut Ui, title: &str, text: &str) {
        ui.label(RichText::new(title).size(25.0).color(Color32::WHITE));
        ui.add_space(6.0);
        ui.label(RichText::new(text).size(14.5).color(DIM));
        ui.add_space(18.0);
    }

    fn welcome(&mut self, ui: &mut Ui) {
        if self.uninstalling {
            Self::heading(
                ui,
                "Uninstall rkit",
                "This removes ride, rterm, rdrop and rkit from this computer.",
            );
            ui.label(
                RichText::new(format!("Installed in {}", self.options.dir.display())).color(TEXT),
            );
            ui.add_space(14.0);
            ui.checkbox(
                &mut self.remove_data,
                "Also remove my chats, themes and settings",
            );
            ui.label(
                RichText::new("Leave this off to keep them for a later install.")
                    .size(12.5)
                    .color(DIM),
            );
            return;
        }
        Self::heading(
            ui,
            "Welcome to rkit",
            "A set of fast tools for your servers and for Windows, made to work together.",
        );
        for (title, text) in [
            (
                "ride, the editor",
                "Files, search, terminals and Claude Code on your servers, over your SSH setup.",
            ),
            (
                "rterm, the terminal",
                "PowerShell, cmd, Git Bash, WSL and SSH in tabs, in the same look as ride.",
            ),
            (
                "rkit, the hub",
                "Start any of them, reopen recent folders, and update each app on its own.",
            ),
        ] {
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(18.0, 20.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, ACCENT);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(title).size(15.0).color(TEXT));
                    ui.label(RichText::new(text).size(13.0).color(DIM));
                });
            });
            ui.add_space(8.0);
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("Everything is in this setup program. Nothing is downloaded.")
                .size(12.5)
                .color(DIM),
        );
    }

    fn options(&mut self, ui: &mut Ui) {
        Self::heading(
            ui,
            "Choose how to install",
            "rkit is installed for your user only; no administrator rights are needed.",
        );
        ui.label(RichText::new("Install in").size(13.0).color(DIM));
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut self.dir_text).desired_width(380.0));
            if ui.button("Browse…").clicked() {
                if let Some(folder) = rfd::FileDialog::new()
                    .set_title("Install rkit in")
                    .pick_folder()
                {
                    // The program gets a folder of its own inside the one picked.
                    self.dir_text = folder.join("rkit").display().to_string();
                }
            }
        });
        let megabytes = env!("RIDE_PAYLOAD_SIZE").parse::<f64>().unwrap_or(0.0) / 1_048_576.0 * 2.0;
        ui.label(
            RichText::new(format!("Needs about {megabytes:.0} MB"))
                .size(12.5)
                .color(DIM),
        );
        ui.add_space(16.0);
        ui.checkbox(&mut self.options.start_menu, "Add ride to the Start menu");
        ui.checkbox(&mut self.options.desktop, "Put a shortcut on the desktop");
        ui.checkbox(
            &mut self.options.context_menu,
            "Add \"Open with ride\" to the right-click menu",
        );
        ui.checkbox(
            &mut self.options.rterm,
            "Install rterm, a terminal for PowerShell, cmd, WSL and SSH",
        )
        .on_hover_text(
            "A separate app, with its own Start menu entry and \"Open in rterm\" on folders.",
        );
        if !tribib_exe().exists() {
            ui.checkbox(
                &mut self.options.tribib,
                "Install Tribib, the design app the Designer works in",
            )
            .on_hover_text("Downloaded from Tribib and checked before it runs. Tribib keeps itself up to date.");
        }
        ui.checkbox(&mut self.launch, "Open rkit when setup finishes");
        ui.add_space(14.0);
        let (color, text) = if self.has_ssh {
            (
                GOOD,
                "The OpenSSH client is available, so ride can connect to your servers.",
            )
        } else {
            (BAD, "The OpenSSH client (ssh) wasn't found. ride needs it to connect; add it from Windows' Optional features.")
        };
        ui.horizontal_wrapped(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(14.0, 18.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.5, color);
            ui.label(RichText::new(text).size(12.5).color(DIM));
        });
    }

    fn working(&mut self, ui: &mut Ui) {
        let title = if self.uninstalling {
            "Removing rkit"
        } else {
            "Installing rkit"
        };
        Self::heading(ui, title, "This takes a few seconds.");
        ui.add_space(24.0);
        // The bar eases toward the real progress instead of jumping to it.
        let shown =
            ui.ctx()
                .animate_value_with_time(egui::Id::new("progress"), self.fraction, 0.45);
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
        ui.painter().rect_filled(bar, 4, LINE);
        let filled = Rect::from_min_size(bar.min, vec2(bar.width() * shown, bar.height()));
        ui.painter().rect_filled(filled, 4, ACCENT);
        // A highlight travels along the filled part while work is going on.
        let time = ui.input(|i| i.time) as f32;
        let sweep = (time * 0.9).fract();
        let glow = Rect::from_min_size(
            pos2(filled.left() + filled.width() * sweep - 30.0, bar.top()),
            vec2(60.0, bar.height()),
        )
        .intersect(filled);
        if glow.is_positive() {
            ui.painter()
                .rect_filled(glow, 4, Color32::from_white_alpha(70));
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.action).size(14.0).color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{:.0}%", shown * 100.0))
                        .size(13.0)
                        .color(DIM),
                );
            });
        });
        ui.ctx().request_repaint();
    }

    fn finished(&mut self, ui: &mut Ui) {
        let since = (ui.input(|i| i.time) - self.page_since) as f32;
        let good = self.error.is_none();
        let color = if good { GOOD } else { BAD };

        // The mark draws itself in: the ring first, then the tick or the cross.
        let (area, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
        let ring = (since / 0.45).clamp(0.0, 1.0);
        let mark = ((since - 0.3) / 0.35).clamp(0.0, 1.0);
        let center = area.center();
        let points: Vec<egui::Pos2> = (0..=(48.0 * ring) as usize)
            .map(|n| {
                let angle = n as f32 / 48.0 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                center + vec2(angle.cos(), angle.sin()) * 28.0
            })
            .collect();
        ui.painter()
            .add(egui::Shape::line(points, Stroke::new(3.0, color)));
        let partial = |from: egui::Pos2, to: egui::Pos2, t: f32| {
            [from, from + (to - from) * t.clamp(0.0, 1.0)]
        };
        let stroke = Stroke::new(3.5, color);
        if good {
            let (a, b, c) = (
                center + vec2(-12.0, 1.0),
                center + vec2(-3.0, 10.0),
                center + vec2(13.0, -9.0),
            );
            ui.painter().line_segment(partial(a, b, mark * 2.0), stroke);
            if mark > 0.5 {
                ui.painter()
                    .line_segment(partial(b, c, mark * 2.0 - 1.0), stroke);
            }
        } else {
            let arm = 10.0;
            ui.painter().line_segment(
                partial(
                    center + vec2(-arm, -arm),
                    center + vec2(arm, arm),
                    mark * 2.0,
                ),
                stroke,
            );
            if mark > 0.5 {
                ui.painter().line_segment(
                    partial(
                        center + vec2(arm, -arm),
                        center + vec2(-arm, arm),
                        mark * 2.0 - 1.0,
                    ),
                    stroke,
                );
            }
        }
        if since < 1.0 {
            ui.ctx().request_repaint();
        }
        ui.add_space(16.0);

        match (&self.error, self.uninstalling) {
            (Some(error), _) => {
                let title = if self.uninstalling {
                    "rkit couldn't be removed"
                } else {
                    "rkit couldn't be installed"
                };
                Self::heading(
                    ui,
                    title,
                    "Nothing else was changed. You can go back and try again.",
                );
                ui.label(RichText::new(error).color(BAD));
            }
            (None, true) => Self::heading(ui, "rkit was removed", "Thanks for trying it."),
            (None, false) => {
                Self::heading(
                    ui,
                    "rkit is installed",
                    &format!("You'll find it in {}", self.options.dir.display()),
                );
                ui.checkbox(&mut self.launch, "Open rkit now");
            }
        }
    }

    /// Back and forward, at the bottom right.
    fn buttons(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let primary = |ui: &mut Ui, text: &str| {
            let button = egui::Button::new(RichText::new(text).size(14.5).color(BG))
                .fill(ACCENT)
                .corner_radius(8)
                .min_size(vec2(118.0, 34.0));
            ui.add(button).clicked()
        };
        let secondary = |ui: &mut Ui, text: &str| {
            let button = egui::Button::new(RichText::new(text).size(14.5))
                .corner_radius(8)
                .min_size(vec2(88.0, 34.0));
            ui.add(button).clicked()
        };
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match self.page {
            Page::Welcome if self.uninstalling => {
                if primary(ui, "Uninstall") {
                    self.start(&ctx);
                }
                if secondary(ui, "Cancel") {
                    self.close(&ctx);
                }
            }
            Page::Welcome => {
                if primary(ui, "Next") {
                    self.go(&ctx, Page::Options);
                }
                if secondary(ui, "Cancel") {
                    self.close(&ctx);
                }
            }
            Page::Options => {
                let ready = !self.dir_text.trim().is_empty();
                if ui.add_enabled_ui(ready, |ui| primary(ui, "Install")).inner {
                    self.start(&ctx);
                }
                if secondary(ui, "Back") {
                    self.go(&ctx, Page::Welcome);
                }
            }
            Page::Working => {}
            Page::Finished => {
                if primary(
                    ui,
                    if self.error.is_some() {
                        "Close"
                    } else {
                        "Finish"
                    },
                ) {
                    self.close(&ctx);
                }
                if self.error.is_some() && secondary(ui, "Back") {
                    let back = if self.uninstalling {
                        Page::Welcome
                    } else {
                        Page::Options
                    };
                    self.go(&ctx, back);
                }
            }
        });
    }
}

fn style(ctx: &Context) {
    // Segoe UI when it is there, as in ride itself; egui's own font otherwise.
    let mut fonts = egui::FontDefinitions::default();
    let font = PathBuf::from(std::env::var_os("WINDIR").unwrap_or("C:\\Windows".into()))
        .join("Fonts\\segoeui.ttf");
    if let Ok(bytes) = std::fs::read(font) {
        fonts
            .font_data
            .insert("ui".into(), Arc::new(egui::FontData::from_owned(bytes)));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "ui".into());
    }
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 7.0);
        style.spacing.button_padding = vec2(12.0, 6.0);
        style.spacing.icon_width = 18.0;
        let v = &mut style.visuals;
        v.override_text_color = Some(TEXT);
        v.panel_fill = BG;
        v.extreme_bg_color = Color32::from_rgb(0x10, 0x10, 0x12);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.4);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        for widget in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
        ] {
            widget.corner_radius = 7.into();
            widget.bg_stroke = Stroke::new(1.0, LINE);
        }
        v.widgets.inactive.weak_bg_fill = Color32::from_white_alpha(10);
        v.widgets.inactive.bg_fill = Color32::from_white_alpha(10);
        v.widgets.hovered.weak_bg_fill = Color32::from_white_alpha(22);
        v.widgets.hovered.bg_fill = Color32::from_white_alpha(22);
    });
}

impl eframe::App for Setup {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        ui.painter().rect_filled(ui.max_rect(), 0.0, BG);

        Panel::left("rail")
            .resizable(false)
            .exact_size(210.0)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(RAIL))
            .show(ui, |ui| self.rail(ui));

        // The top strip moves the window and closes it; there is no system title bar.
        Panel::top("title")
            .frame(Frame::NONE.inner_margin(Margin {
                left: 0,
                right: 8,
                top: 8,
                bottom: 0,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                let strip = ui.max_rect().expand2(vec2(0.0, 8.0));
                if ui
                    .interact(strip, ui.id().with("drag"), Sense::drag())
                    .drag_started()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.preview {
                        ui.painter().text(
                            pos2(strip.left() + 34.0, strip.center().y),
                            Align2::LEFT_CENTER,
                            "Preview: nothing is written to this computer",
                            FontId::proportional(12.5),
                            ACCENT,
                        );
                    }
                    // The cross is drawn, not typed: the interface font has no glyph for one.
                    let (rect, close) = ui.allocate_exact_size(vec2(30.0, 26.0), Sense::click());
                    // Closing is held back while files are being written.
                    let enabled = self.page != Page::Working;
                    if close.hovered() && enabled {
                        ui.painter()
                            .rect_filled(rect, 6, Color32::from_rgb(0xc4, 0x2b, 0x1c));
                    }
                    let color = match (enabled, close.hovered()) {
                        (false, _) => LINE,
                        (true, true) => Color32::WHITE,
                        (true, false) => DIM,
                    };
                    let (c, arm) = (rect.center(), 4.5);
                    for (from, to) in [
                        (vec2(-arm, -arm), vec2(arm, arm)),
                        (vec2(arm, -arm), vec2(-arm, arm)),
                    ] {
                        ui.painter()
                            .line_segment([c + from, c + to], Stroke::new(1.5, color));
                    }
                    if close.clicked() && enabled {
                        self.close(&ctx);
                    }
                });
            });
        Panel::bottom("buttons")
            .frame(Frame::NONE.inner_margin(Margin {
                left: 34,
                right: 26,
                top: 12,
                bottom: 22,
            }))
            .show_separator_line(false)
            .show(ui, |ui| self.buttons(ui));

        CentralPanel::default_margins()
            .frame(Frame::NONE.inner_margin(Margin {
                left: 34,
                right: 34,
                top: 18,
                bottom: 0,
            }))
            .show(ui, |ui| {
                // Each page fades in and settles a few pixels up.
                let since = (ui.input(|i| i.time) - self.page_since) as f32;
                let t = (since / 0.28).clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - t).powi(3);
                ui.set_opacity(eased);
                ui.add_space(14.0 * (1.0 - eased));
                if t < 1.0 {
                    ctx.request_repaint();
                }
                match self.page {
                    Page::Welcome => self.welcome(ui),
                    Page::Options => self.options(ui),
                    Page::Working => self.working(ui),
                    Page::Finished => self.finished(ui),
                }
            });
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }
}
