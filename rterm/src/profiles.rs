//! The shells a tab can run: the ones Windows comes with, the ones installed on top, each
//! WSL distribution, and the hosts in the SSH config.

use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Profile {
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub icon: char,
}

impl Profile {
    fn new(name: &str, program: impl Into<PathBuf>, args: &[&str]) -> Self {
        Self {
            name: name.into(),
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: Vec::new(),
            icon: crate::icons::TERMINAL,
        }
    }
}

/// A program on the PATH, as `where` would find it.
fn on_path(exe: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(exe))
        .find(|p| p.is_file())
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// The shells found without starting anything: quick enough for the first window.
pub fn local() -> Vec<Profile> {
    let mut found = Vec::new();
    let pwsh = on_path("pwsh.exe").or_else(|| {
        let p = env_dir("ProgramFiles")?.join("PowerShell\\7\\pwsh.exe");
        p.is_file().then_some(p)
    });
    if let Some(pwsh) = pwsh {
        found.push(Profile::new("PowerShell 7", pwsh, &["-NoLogo"]));
    }
    found.push(Profile::new(
        "Windows PowerShell",
        "powershell.exe",
        &["-NoLogo"],
    ));
    let cmd = env_dir("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    found.push(Profile::new("Command Prompt", cmd, &[]));
    if let Some(bash) = git_bash() {
        let mut profile = Profile::new("Git Bash", bash, &["--login", "-i"]);
        // Keeps bash in the folder it was started in rather than going home.
        profile.env.push(("CHERE_INVOKING".into(), "1".into()));
        found.push(profile);
    }
    found
}

fn git_bash() -> Option<PathBuf> {
    let beside_git = on_path("git.exe")
        .and_then(|git| Some(git.parent()?.parent()?.join("bin").join("bash.exe")));
    let standard = [
        env_dir("ProgramFiles"),
        env_dir("LOCALAPPDATA").map(|d| d.join("Programs")),
    ]
    .into_iter()
    .flatten()
    .map(|d| d.join("Git\\bin\\bash.exe"));
    beside_git.into_iter().chain(standard).find(|p| p.is_file())
}

/// The shells that take a moment to find: WSL distributions (asked of wsl.exe) and SSH
/// hosts (read from ~/.ssh/config).
pub fn slow() -> Vec<Profile> {
    let mut found = wsl();
    found.extend(ssh());
    found
}

fn wsl() -> Vec<Profile> {
    let Some(wsl) = env_dir("SystemRoot")
        .map(|d| d.join("System32\\wsl.exe"))
        .filter(|p| p.is_file())
    else {
        return Vec::new();
    };
    let mut command = std::process::Command::new(&wsl);
    command.args(["--list", "--quiet"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(out) = command.output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    // wsl.exe answers in UTF-16.
    let wide: Vec<u16> = out
        .stdout
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    String::from_utf16_lossy(&wide)
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
        .filter(|name| !name.is_empty() && !name.starts_with("docker-desktop"))
        .map(|name| {
            let mut profile = Profile::new(name, &wsl, &["-d", name]);
            profile.icon = crate::icons::REMOTE;
            profile
        })
        .collect()
}

fn ssh() -> Vec<Profile> {
    let Some(home) = env_dir("USERPROFILE") else {
        return Vec::new();
    };
    let Ok(config) = std::fs::read_to_string(home.join(".ssh").join("config")) else {
        return Vec::new();
    };
    let ssh = env_dir("SystemRoot")
        .map(|d| d.join("System32\\OpenSSH\\ssh.exe"))
        .filter(|p| p.is_file())
        .or_else(|| on_path("ssh.exe"));
    let Some(ssh) = ssh else {
        return Vec::new();
    };
    hosts(&config)
        .into_iter()
        .map(|host| {
            let mut profile = Profile::new(&format!("SSH: {host}"), &ssh, &[&host]);
            profile.icon = crate::icons::REMOTE;
            profile
        })
        .collect()
}

/// The hosts an SSH config names one by one; patterns are left out.
fn hosts(config: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for line in config.lines() {
        let mut words = line.split_whitespace();
        let Some(key) = words.next() else { continue };
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        for host in words {
            let pattern = host.contains(['*', '?', '!']);
            if !pattern && !hosts.iter().any(|h| h == host) {
                hosts.push(host.to_string());
            }
        }
    }
    hosts
}

/// The name a program's window title reduces to, when the shell puts its own path there.
pub fn tidy_title(title: &str) -> Option<&str> {
    let path = Path::new(title.trim());
    let is_exe = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    (!is_exe && !title.trim().is_empty()).then_some(title.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_hosts_skip_patterns() {
        let config = "Host vespucci\n  HostName 1.2.3.4\nHost vps2 *.lan !x\nhost *\nHost vps2\n";
        assert_eq!(hosts(config), ["vespucci", "vps2"]);
    }

    #[test]
    fn exe_titles_are_dropped() {
        assert_eq!(
            tidy_title("C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
            None
        );
        assert_eq!(tidy_title("~/src"), Some("~/src"));
    }
}
