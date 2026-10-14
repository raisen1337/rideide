<p align="center">
  <img src="app/assets/ride.svg" width="96" alt="ride">
</p>

<h1 align="center">ride</h1>

<p align="center">
  A code editor for working on servers over SSH, with Claude Code built in.
</p>

<p align="center">
  <a href="https://github.com/raisen1337/rideide/releases/latest"><img src="https://img.shields.io/github/v/release/raisen1337/rideide?style=flat-square&color=6c9af4&label=release" alt="Latest release"></a>
  <a href="https://github.com/raisen1337/rideide/releases"><img src="https://img.shields.io/github/downloads/raisen1337/rideide/total?style=flat-square&color=6c9af4" alt="Downloads"></a>
  <a href="https://github.com/raisen1337/rideide/commits/main"><img src="https://img.shields.io/github/last-commit/raisen1337/rideide?style=flat-square&color=2b2c33" alt="Last commit"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-2b2c33?style=flat-square" alt="Platform">
  <img src="https://img.shields.io/badge/built%20with-Rust-2b2c33?style=flat-square&logo=rust" alt="Rust">
</p>

<p align="center">
  <img src="docs/split.png" alt="ride with three panes open" width="900">
</p>

ride opens a folder on a server the way you'd open one on your own disk. It connects with the
hosts already in your `~/.ssh/config`, uploads a small agent to the server, and from then on the
file tree, editor, terminal and search all work on the server directly — no syncing, no mounting.
It runs on your own computer as well.

## Download

Get **`ride-setup.exe`** from the [latest release](https://github.com/raisen1337/rideide/releases/latest)
and run it. The installer puts ride in your user folder, so it doesn't ask for admin rights.

After that ride keeps itself current. Each time it opens it checks the latest release here; a
newer version is downloaded, checked against its SHA-256 and installed, and ride restarts into it
with your tabs and chats. One that turns up while you work installs when you close ride. A short
"What's new" from [CHANGELOG.md](CHANGELOG.md) shows once after each update. Settings → Updates
turns this off or checks by hand.

You need:

- Windows 10 or 11 (the OpenSSH client that comes with Windows is enough)
- A Linux x86_64 server you can reach with `ssh <host>`
- [Claude Code](https://docs.claude.com/en/docs/claude-code) installed and logged in on that server, for the chat

## Claude

<img src="docs/claude.png" alt="Claude answering about the open file" width="900">

The chat runs the Claude Code that's installed on the server, with the account logged in there.
There is no API key to paste into ride.

- **Ask** asks before Claude changes a file or runs a command, and shows you the diff or command
  first. **Edit** lets it edit freely and asks only for commands.
- Every message you send is a checkpoint: **restore files** puts back what Claude edited after it.
- Paste screenshots with Ctrl+V or attach images.
- **Agents** runs several Claude sessions side by side, each with its own task.
- When Claude builds something with a web page, it can open a preview for you: ride forwards the
  port from the server and opens it in your browser.

## Editor

- Split the editor into columns and rows by dragging a tab to the edge of another pane.
- Code folding, a minimap, breadcrumbs, and an outline of the file's functions and types
  (Ctrl+Shift+O to jump to one).
- Format on save, with the formatter of your choice per file type, run on the server. Prettier
  and StyLua work out of the box through npx.
- Errors on open and save, for Lua, JavaScript, Python, PHP, shell and JSON, using the checkers already
  on the server (`luac`, `node`, `python3`, `php`, `bash`).
- Compare any two files side by side.
- `.env` files open as a table, with the values hidden until you show them.
- The Command Palette (Ctrl+Shift+P) also searches the VS Code Marketplace for themes and file icons,
  and previews them before you install one.

## Git, server and databases

- A Git panel: stage, unstage, discard, commit, pull and push, with changed lines marked in the
  gutter. Switch, check out and create branches, stash changes, and see who last changed the
  line you're on. Claude can write the commit message.
- A database explorer for MySQL, MariaDB and PostgreSQL on the server: tables, columns, and a
  query tab with a result grid. It can take the connection from your `.env`.
- A server panel with CPU, memory, disk, PM2 processes and listening ports; forward any port to
  `localhost` in one click.
- Open several connections at once and drag files and folders from one server's tree to another's.
- The folder and open tabs, unsaved changes included, come back the next time you open ride.
- Themes and VS Code icon themes, and ride updates itself from this repository's releases.

## Building from source

```powershell
rustup target add x86_64-unknown-linux-musl   # for the server agent
.\build-installer.ps1                          # writes dist\ride-setup.exe
```

`cargo run -p ride` starts the editor without installing it.

To release, add an entry to `CHANGELOG.md`, raise `version` in the root `Cargo.toml` and push to `main`. The
[Release workflow](.github/workflows/release.yml) tests and builds on GitHub's Windows runners,
then publishes `ride-setup.exe` with a `latest.json` that installed copies read to update.

| Folder | What it is |
| --- | --- |
| `app/` | The editor (Rust, [egui](https://github.com/emilk/egui)) |
| `agent/` | The helper ride uploads to the server; also an MCP server Claude uses for previews |
| `proto/` | The protocol between the two |
| `installer/` | The Windows setup program |
