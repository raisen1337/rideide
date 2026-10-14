//! Cross-compiles the Linux agent so the app can embed it and upload it to remote hosts.

use std::path::PathBuf;
use std::process::Command;

const TARGET: &str = "x86_64-unknown-linux-musl";

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    for dir in [
        "agent/src",
        "agent/Cargo.toml",
        "proto/src",
        "proto/Cargo.toml",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
    }

    // A separate target dir, because the outer cargo holds the lock on the main one.
    let target_dir = root.join("target").join("agent");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(&root)
        .args([
            "build",
            "--release",
            "-p",
            "ride-agent",
            "--target",
            TARGET,
            "--target-dir",
        ])
        .arg(&target_dir)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .status()
        .expect("failed to run cargo for the agent");
    assert!(
        status.success(),
        "building ride-agent failed; is the target installed? (rustup target add {TARGET})"
    );

    let agent = target_dir.join(TARGET).join("release").join("ride-agent");
    println!("cargo:rustc-env=RIDE_AGENT_PATH={}", agent.display());

    // The program's icon, as Explorer, the taskbar and shortcuts show it.
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("ride.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon(&icon.to_string_lossy())
            .compile()
            .expect("embedding the icon failed");
    }
}
