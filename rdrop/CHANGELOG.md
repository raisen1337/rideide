# rdrop changelog

rkit shows the entries newer than the rdrop you have before you update it. Each `## <version>`
heading starts an entry; write for the people using rdrop.

## 0.5.2

- **Dragging files in from Explorer or the desktop** lights up the pane under the mouse and says where they will go ("Copy 3 items into prod · …/var/www"), and they go into that pane. Before, with several panes, nothing showed while dragging, and the files could go into the pane last clicked instead.
- When both folders being compared have the same name, the sync buttons say which server they update.

## 0.5.1

- Chinese, Japanese and Korean file names show instead of empty boxes.

## 0.5.0

- **Only what changed goes.** A file of 4 MB or more that is already at the other end is compared block by block, as rsync does, and only the blocks that differ cross the network: two changed megabytes of a 67 MB file are two megabytes sent.
- **Large files over four connections at once.** From 64 MB up, a file goes in four parts side by side: 67 MB went up to a server in 2.4 seconds and came down in 2.4.
- **Transfers carry on where they stopped.** A stopped, failed or interrupted file is kept as a part beside its place and finished from there with Resume; the copy only takes the real name once it is whole.
- **Checked copies.** After a copy to or from a server, rdrop compares the SHA-256 of both ends (where the server has sha256sum) and fails the transfer if they differ. It can be turned off in the transfers.
- **Clear rules for files already there**: overwrite, newer only, skip, or keep both (the copy gets " (2)" in its name), chosen in the transfers and kept.
- **The queue survives a restart.** Transfers not finished when rdrop closes, or crashes, are there the next time, stopped, with Resume and Resume all.
- **Search** under any folder with Ctrl+F: by name (with * and ? if you like) or inside files with the Aa button. On a server the search runs there, with find and grep, so only the results cross the network; results can be opened, copied, dragged and deleted like any other rows. Esc goes back to the folder.
- **Compare folders**, here and on a server or on two servers, from a pane's ⋯ menu: what is only on one side, newer on one side, or differs, then a sync preview with every line to tick or untick, one way or both ways, with deletion of what the other side doesn't have if you want an exact copy. Nothing changes until you press Sync.
- **Watch and upload**: a folder here is watched, and every file saved in it goes up to the server folder in the other pane a moment later. Watches show on the left, survive restarts and stop with a click. Tools' own folders (.git, node_modules, target) are left out.
- **Edit in ride** (Ctrl+E): a file on a server opens in ride, connected to that server, and saving it writes it there directly; a folder opens as ride's folder.
- **Command palette** with Ctrl+Shift+P or Ctrl+K: every command, server, place and setting by typing a few letters of it, with its key beside it; a path typed in it is gone to.
- **rdrop in the terminal**: `rdrop ls`, `cp`, `get`, `put`, `sync` (with `--delete` and `--dry-run`), `rm` and `mkdir`, with places written as `host:/path`, using the same engine as the window. The setup puts rdrop on your PATH.
- **Owner and group** in Properties on a server, changed as with chown, and permissions and owners applied to everything inside a folder in one go. A link shows where it points.
- Folders here refresh on their own when something else changes them, as in Explorer.

## 0.4.0

- **Folders with many small files fly.** A folder now goes as one compressed stream through ssh, packed and unpacked by tar on the server, instead of file by file over SFTP: 400 small files went up in 0.7 seconds instead of 63, down in 1.6 and from one server to another in 1.7. Servers without a shell or tar still get the folder file by file.
- Ctrl+C, Ctrl+X and Ctrl+V work, also with files copied in Explorer, and the other keys work after clicking a row; before, a click on a row took the keys away from the list.

## 0.3.0

- **Works like Explorer.** Ctrl+C, Ctrl+X and Ctrl+V copy and move files, between panes, servers and Explorer itself: copy in Explorer, paste in rdrop, and the other way round. Delete sends files on this computer to the Recycle Bin; Shift+Delete, and anything on a server, asks first and deletes for good.
- **Rename in place** with F2, or by clicking a picked item and waiting a moment, as in Explorer. Ctrl+Shift+N makes a new folder ready to be named.
- **This PC**, with the drives and how full they are, and your Desktop, Downloads, Documents and the rest, in the pane and in the list on the left.
- **Previews.** Double-click or press Enter on a file to see it: pictures and drawings with zoom, sound with a player, code and text in colour with line numbers, logs that follow what is written to them (Live), and the bytes of anything else. The arrow keys go to the next file. Video, documents and archives open in their app.
- **Open with app** for files on a server: rdrop fetches the file, opens it, and sends it back each time you save it.
- **Large icons**, with pictures as thumbnails, beside the detailed list; each pane remembers its own.
- **Permissions**: a column on servers, and Properties (Alt+Enter) to change them as with chmod. Hidden files and dot files show or hide with Ctrl+H, symbolic links are marked.
- The arrow keys, Home, End and the page keys move through the list, with Shift to pick a range, and typing jumps to a name.
- New-looking dialogs, with clear titles, fields and buttons.

## 0.2.0

- **Sign in with a password.** When a server doesn't take your SSH keys, rdrop asks for the password, with a box to remember it. Remembered passwords are encrypted by Windows for your account.
- **Saved sites.** Save a connection with its host, port, user, password and the folder to start in, under Sites on the left; right-click one to edit it, forget its password or delete it. Without a remembered password, rdrop asks for it when it connects.
- A server seen for the first time is trusted from then on, and one whose key has changed is refused with a clear message.

## 0.1.0

- **rdrop, files between this computer and your servers.** It signs in the way `ssh` does, with your `~/.ssh/config`, keys, agent and jump hosts, and works with any server that has SFTP.
- **As many panes as you like**, each on this computer or on any server. Drag a pane by its header to the edge of another to put it there, drag a server from the list onto a pane to open it beside, and drag the lines between panes to resize.
- **Copy by dragging** rows from one pane onto another, or files from Explorer onto a pane. Server to server goes straight across, without a stop on this computer's disk. Folders copy with all they hold, and files keep their modification times.
- **Transfers run in the background**, three at a time, while you keep browsing; the bar at the bottom shows how far they are, and opens the full list.
- F5 copies to the pane you used before, F2 renames, F7 makes a folder, Del deletes, Backspace goes up, Alt+Left goes back, Tab moves to the next pane, and typing narrows the list.
