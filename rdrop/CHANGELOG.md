# rdrop changelog

rkit shows the entries newer than the rdrop you have before you update it. Each `## <version>`
heading starts an entry; write for the people using rdrop.

## 0.1.0

- **rdrop, files between this computer and your servers.** It signs in the way `ssh` does, with your `~/.ssh/config`, keys, agent and jump hosts, and works with any server that has SFTP.
- **As many panes as you like**, each on this computer or on any server. Drag a pane by its header to the edge of another to put it there, drag a server from the list onto a pane to open it beside, and drag the lines between panes to resize.
- **Copy by dragging** rows from one pane onto another, or files from Explorer onto a pane. Server to server goes straight across, without a stop on this computer's disk. Folders copy with all they hold, and files keep their modification times.
- **Transfers run in the background**, three at a time, while you keep browsing; the bar at the bottom shows how far they are, and opens the full list.
- F5 copies to the pane you used before, F2 renames, F7 makes a folder, Del deletes, Backspace goes up, Alt+Left goes back, Tab moves to the next pane, and typing narrows the list.
