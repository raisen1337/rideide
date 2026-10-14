# ride

A code editor for working on remote servers over SSH, written in Rust (egui). Claude Code runs on the server as the built-in assistant.

- `app/` — the editor
- `agent/` — the small helper ride uploads to the server
- `proto/` — the protocol between them
- `installer/` — the Windows setup program

Build the setup program on Windows with `.\build-installer.ps1`; it lands in `dist\ride-setup.exe`. The Linux agent is built for `x86_64-unknown-linux-musl` (`rustup target add x86_64-unknown-linux-musl`).
