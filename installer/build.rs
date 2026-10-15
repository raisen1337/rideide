//! Packs the release build of ride into the installer, compressed, so the setup
//! program carries everything it installs and never downloads anything.

use flate2::{write::GzEncoder, Compression};
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut total = 0;
    // ride, rterm, the terminal that comes with it, and rkit, which starts and updates both.
    for (exe, package) in [
        ("ride.exe", "ride"),
        ("rterm.exe", "rterm"),
        ("rkit.exe", "rkit"),
    ] {
        let app = root.join("target").join("release").join(exe);
        println!("cargo:rerun-if-changed={}", app.display());
        let bytes = std::fs::read(&app).unwrap_or_else(|_| {
            panic!(
                "{} is missing: build it first (cargo build --release -p {package})",
                app.display()
            )
        });
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join(format!("{exe}.gz"));
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&bytes).unwrap();
        std::fs::write(out, encoder.finish().unwrap()).unwrap();
        total += bytes.len();
    }
    println!("cargo:rustc-env=RIDE_PAYLOAD_SIZE={total}");

    // The program's icon, as Explorer, the taskbar and shortcuts show it.
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../app/assets")
        .join("ride.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon(&icon.to_string_lossy())
            .compile()
            .expect("embedding the icon failed");
    }
}
