//! What the latest release offers for each app, and putting a newer copy of one in place.
//!
//! Every release publishes `apps.json` beside the setup program: for each app its version,
//! where its program is, the program's SHA-256 and size, and its changelog. An app is
//! updated on its own: its program is downloaded next to the installed one, checked, and
//! swapped in. A copy that is running keeps its old program until it is started again.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MANIFEST_URL: &str =
    "https://github.com/raisen1337/rideide/releases/latest/download/apps.json";
/// A program is some tens of megabytes; anything far bigger is not one.
const MAX_PROGRAM: u64 = 400 * 1024 * 1024;

#[derive(Deserialize, Clone)]
pub struct Entry {
    pub version: String,
    pub notes: String,
}

#[derive(Deserialize, Clone)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Newest first.
    #[serde(default)]
    pub changelog: Vec<Entry>,
}

#[derive(Deserialize)]
pub struct Manifest {
    pub apps: HashMap<String, Release>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into()
}

pub fn fetch_manifest() -> Result<Manifest, String> {
    let mut response = agent()
        .get(MANIFEST_URL)
        .call()
        .map_err(|e| format!("Couldn't reach GitHub: {e}"))?;
    let text = response
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("The release list isn't as expected: {e}"))
}

fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    Some((next()?, next()?, next().unwrap_or(0)))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The changelog entries newer than `installed`, or the newest one when nothing is newer.
pub fn whats_new<'a>(release: &'a Release, installed: Option<&str>) -> Vec<&'a Entry> {
    let newer: Vec<&Entry> = release
        .changelog
        .iter()
        .filter(|e| installed.is_none_or(|v| is_newer(&e.version, v)))
        .collect();
    if newer.is_empty() {
        release.changelog.iter().take(1).collect()
    } else {
        newer
    }
}

/// Downloads `release` and puts it at `target`, reporting progress from 0 to 1.
pub fn install(
    release: &Release,
    target: &Path,
    mut progress: impl FnMut(f32),
) -> Result<(), String> {
    if release.size > MAX_PROGRAM {
        return Err("The download is far bigger than expected".into());
    }
    let mut response = agent()
        .get(&release.url)
        .call()
        .map_err(|e| format!("Couldn't download: {e}"))?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_PROGRAM)
        .reader();
    let mut data = Vec::with_capacity(release.size as usize);
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..n]);
        progress((data.len() as f32 / release.size.max(1) as f32).min(1.0));
    }
    let hash: String = Sha256::digest(&data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if data.len() as u64 != release.size || hash != release.sha256.to_lowercase() {
        return Err("The download didn't match the release's checksum".into());
    }
    swap_in(target, &data)
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
    let mut moved = !target.exists();
    for _ in 0..20 {
        if moved || std::fs::rename(target, &old).is_ok() {
            moved = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    if !moved {
        let _ = std::fs::remove_file(&fresh);
        return Err(format!("{name} is in use; close it and try again"));
    }
    if let Err(error) = std::fs::rename(&fresh, target) {
        let _ = std::fs::rename(&old, target);
        let _ = std::fs::remove_file(&fresh);
        return Err(format!("Can't put the new {name} in place: {error}"));
    }
    Ok(())
}

/// The version an installed program reports with `--version`.
pub fn installed_version(exe: &Path) -> Option<String> {
    if !exe.exists() {
        return None;
    }
    let mut command = std::process::Command::new(exe);
    command.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let out = command.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse(&text).is_some().then_some(text)
}

/// Where the apps live: beside rkit.
pub fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(is_newer("v1.0", "0.9.9"));
    }

    #[test]
    fn whats_new_lists_what_is_newer_than_installed() {
        let release: Release = serde_json::from_str(
            r#"{"version":"0.5.0","url":"u","sha256":"s","size":1,"changelog":[
                {"version":"0.5.0","notes":"five"},{"version":"0.4.0","notes":"four"},
                {"version":"0.3.0","notes":"three"}]}"#,
        )
        .unwrap();
        let notes = |v| {
            whats_new(&release, v)
                .iter()
                .map(|e| e.notes.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(notes(Some("0.3.0")), ["five", "four"]);
        assert_eq!(notes(Some("0.5.0")), ["five"]);
        assert_eq!(notes(None), ["five", "four", "three"]);
    }

    /// Against the published release, by hand: cargo test -p rkit -- --ignored
    #[test]
    #[ignore]
    fn an_app_updates_from_the_release() {
        let manifest = fetch_manifest().unwrap();
        let release = &manifest.apps["rdrop"];
        let dir = std::env::temp_dir().join(format!("rkit-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("rdrop.exe");
        // An older copy in the way, as on a computer that is behind.
        std::fs::write(&target, b"old").unwrap();
        let mut seen = 0.0f32;
        install(release, &target, |p| seen = p).unwrap();
        assert_eq!(seen, 1.0);
        assert_eq!(
            installed_version(&target).as_deref(),
            Some(release.version.as_str())
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
