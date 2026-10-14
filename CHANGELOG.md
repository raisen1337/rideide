# Changelog

ride shows the entries newer than the version you had, once, the first time it starts after an
update. Each `## <version>` heading starts an entry; write for the people using ride.

## 0.6.6

- **Ctrl+ and Ctrl- zoom the window again** (Ctrl+0 goes back to 100%). The zoom stays where you leave it and matches the slider in Settings → Appearance.
- A long message in the Claude chat wraps inside its bubble instead of pushing the whole conversation off the left edge of the panel. Short messages keep a bubble as wide as their text.

## 0.6.5

- **Write to Claude while it works.** In the Claude chat you can send more messages before it has finished: press Enter, or the arrow next to Stop. The message goes out at once and shows as queued under the answer. Claude takes it in as soon as it's done with what it is doing, and it joins the conversation there.
- Stopping Claude with messages still queued puts them back in the input box, so nothing you typed is lost.

## 0.6.4

- **Nobody can push your cursor any more.** Text someone else types right where your cursor is (their Enter at the end of your line, say) now goes after your cursor instead of carrying it along.
- **Cursors no longer step back while two people type.** A cursor sent on its own is now moved along by the edits its sender hadn't seen yet, just like the edits are.
- Edits from others land exactly where they were made in your editor too, instead of being worked out again from the text.
- Both people need this version again; ride on the server switches over by itself.

## 0.6.3

- **Live cursors move with the typing.** Someone's cursor no longer steps back and then forward while they type. Each edit now carries the typist's cursor, so it moves in the same step as their text.
- Typing next to the same character (spaces in an indent, a doubled letter, Enter above an indented line) is placed where it was typed, so everyone else's cursors stay where they should.
- Both people need this version: ride on the server switches to the new way of sharing edits.

## 0.6.2

- **Steadier cursors in live editing.** Someone else's cursor no longer jumps back and forth while they type or move around: a cursor is sent only once the text it points into has reached the others, the others' cursors move with what you type, and cursor moves go out at most 25 times a second. A file open in two tabs no longer makes your cursor flicker between them for the others.
- **ride doesn't close on an error any more.** If something goes wrong while drawing, ride carries on and writes what happened to `%LOCALAPPDATA%\ride\error.txt`. If ride does close, the next start says so and points to the file. Sending that file along helps fix the cause.

## 0.6.1

- **No GitHub needed.** Live editing and live sessions work without signing in: ride asks you for a nickname the first time, and you can change it in Settings → Account. Signing in with GitHub still gives you your picture and account name.
- A host sees which guests aren't signed in with GitHub, since a nickname can be anything.

## 0.6.0

- **Live sessions.** Share the folder you have open with people who can't get onto the server. Start one from the connection menu at the top ("Share this folder in a live session") or from Ctrl+Shift+P. You get a session ID to send them.
- To join, paste the ID where you'd type a host, or use "Join a live session…". The host sees your name and picture and lets you in or not.
- Guests see the shared folder and edit its files together with the host, cursors and all. They can't use the terminal, Git, Claude or anything outside that folder: their requests pass through the host's ride, which checks each one.
- The host can remove someone, or stop the session, which sends everyone out. A new session gets a new ID.
- The two copies of ride connect directly, encrypted. When no direct path can be found, the traffic goes through a public relay, still encrypted.

## 0.5.1

- **Chats from the terminal in History.** The chat's History now also lists the conversations you had with Claude Code in a terminal, in the open folder. Open one to read it, and send a message to carry on with it.
- The History, New chat and pop-out buttons at the top of the chat can be clicked anywhere, not only at their bottom edge.
- Your GitHub picture and name show at the right of the bottom bar while you're signed in. A click opens Settings → Account.
- The Marketplace is no longer searched for everything typed in Ctrl+Shift+P. Pick "Marketplace: Search colour themes and file icons…" to search it.

## 0.5.0

- **Live editing.** When someone else opens the same file on the same server through ride, you edit it together: their changes appear as they type, and you see their cursor and selection with their name. Their GitHub picture shows above the code while they're in the file.
- **Sign in with GitHub** in Settings → Account, to have your name and picture in live editing. ride shows a code to enter on github.com and reads only your public profile.
- Live editing works between people who log in to the server with the same account, and can be turned off in Settings → Account.

## 0.4.0

- **Database explorer.** A new side tab for MySQL, MariaDB and PostgreSQL databases on the server you're connected to. Add a connection, or fill it in from the folder's `.env`. Browse the tables and their columns, open the first 200 rows of a table with a click, and run any SQL in a query tab with Ctrl+Enter. Results show in a grid, and clicking a cell copies it.
- The queries run through the `mysql`/`mariadb` or `psql` client on the server, so nothing new has to be opened to the internet. Passwords are kept only for this session, unless you choose to remember them.

## 0.3.2

- **.env editor.** A `.env` file (also `.env.local`, `production.env` and the like) opens as a table of keys and values. The values stay hidden until you show one, or all of them. You can add and remove variables, and comments and order are kept. "Edit as text" switches to the plain file and back.
- Opening the same file twice in quick succession no longer gives it two tabs.

## 0.3.1

- **Branches.** The branch name at the top of the Git tab is now a menu: switch to another branch, check out a remote one, or create a new branch from where you are.
- **Stash.** A Stashes section in the Git tab puts all your changes aside (new files too) and brings them back with Apply or Pop; Drop asks first.
- **Blame on the current line.** At the end of the line the cursor is on: who changed it last, how long ago, and the commit's message. Turn it off in Settings → Editor.

## 0.3.0

- **Code folding.** Arrows beside the line numbers fold a block and open it again; Ctrl+Shift+[ and Ctrl+Shift+] do the same at the cursor, and the palette has Fold all and Unfold all. It works in every language, by indentation.
- **Minimap.** The whole file in miniature at the right of the editor; click or drag in it to scroll.
- **Breadcrumbs.** Above the code: the file's folders and the function or class the cursor is in. Click one for a list of everything in the file.
- **Outline.** A new side tab lists the file's functions, classes and types, nested as in the code, with a filter. Ctrl+Shift+O (or `@` in the command palette) jumps to one by name.
- **Format on save.** Each kind of file can have a formatter that runs on the host. Prettier and StyLua work out of the box through npx, and you can change or add others in Settings → Editor. Turn it on there or from the palette; Shift+Alt+F formats without saving.
- **Compare two files.** "File: Compare with another file…" in the palette shows the open file (unsaved changes included) against any other, side by side.

## 0.2.4

- While ride stays open it looks for updates every hour instead of once a day, so a new version published while you work installs the next time you close ride.

## 0.2.3

- **Marketplace in the command palette.** Type in Ctrl+Shift+P and colour themes and file icons from the VS Code Marketplace show under the commands. Rest on one and it previews: a colour theme on the whole window, file icons in the file tree. Enter installs it.

## 0.2.2

- **What's new.** After an update, ride shows this list once before anything else, so you know what changed.
- **Updates when ride opens.** ride checks for a new version every time it starts and, if there is one, restarts into it within a few seconds. Tabs and chats come back. An update found while you work still waits until you close ride.

## 0.2.1

- The GitHub link in Settings → Updates opens in your browser.

## 0.2.0

- **ride updates itself.** Once a day it looks for a new version on GitHub, downloads it in the background and installs it when you close ride. Your tabs and chats come back. You can also restart right away from the status bar, check by hand, or turn it off in Settings → Updates.
- Files in the tree can be selected again while two connections are open.
- The chat modes in Settings describe what they do now: Ask asks before any change, Edit asks only before commands.
