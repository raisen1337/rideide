# rdrop changelog

rkit shows the entries newer than the rdrop you have before you update it. Each `## <version>`
heading starts an entry; write for the people using rdrop.

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
