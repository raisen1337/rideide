//! Installed as rdrop.com beside rdrop.exe. Typed in a terminal, `rdrop` finds this one
//! first (Windows tries .com before .exe), and it runs rdrop.exe with the terminal as its
//! input and output and waits for it, so commands print where they were typed, scripts
//! wait for them and get their exit code. rdrop.exe is a window program, which a terminal
//! wouldn't wait for. Without a command, the window opens and the terminal is free again.

use std::process::Command;

const COMMANDS: [&str; 10] = [
    "ls",
    "cp",
    "get",
    "put",
    "sync",
    "rm",
    "mkdir",
    "help",
    "--help",
    "--version",
];

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let exe = std::env::current_exe()
        .map(|p| p.with_file_name("rdrop.exe"))
        .unwrap_or_else(|_| "rdrop.exe".into());
    let command = args
        .first()
        .and_then(|a| a.to_str())
        .is_some_and(|a| COMMANDS.contains(&a));
    let mut child = Command::new(&exe);
    child.args(&args);
    if !command {
        let code = match child.spawn() {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("rdrop: {}: {e}", exe.display());
                1
            }
        };
        std::process::exit(code);
    }
    match child.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("rdrop: {}: {e}", exe.display());
            std::process::exit(1);
        }
    }
}
