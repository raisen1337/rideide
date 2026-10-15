# Changelog

ride shows the entries newer than the version you had, once, the first time it starts after an
update. Each `## <version>` heading starts an entry; write for the people using ride.

## 0.9.6

- **rterm, a terminal for Windows.** A separate app that comes with ride, made from ride's own terminal: tabs of PowerShell, PowerShell 7, Command Prompt, Git Bash, each WSL distribution and every host in your SSH config, in ride's look, with glass if you like. Find it in the Start menu as rterm, right-click a folder for "Open in rterm", or type `rterm` in Explorer's address bar. Updating ride installs it; setup has a box to leave it out.
- With glass on, Windows no longer draws a faint second set of minimize and maximize buttons under ride's own.

## 0.9.5

- Pick several files and folders in the file tree: Ctrl+click adds or removes one, Shift+click takes a range. Right-click the picked rows to add them to chat, copy their paths or delete them together. Escape or a plain click clears the pick.

## 0.9.4

- Notifications come from ride, with its name and icon, instead of from Windows PowerShell. ride registers both for your user the first time it shows one; uninstalling removes them.

## 0.9.3

- Chat and Agents find Claude Code on Linux and macOS hosts where it works in the terminal but ride said it wasn't installed. ride starts it through a login shell, which skips the part of `~/.bashrc` where nvm and Claude Code's installer put themselves on the PATH; it now also looks in `~/.local/bin`, nvm's and fnm's Node versions, and npm's, bun's and volta's folders.

## 0.9.2

- Clicking the box in the middle of the title bar opens the file finder again. The click that opened it was also taken as a click outside it, so it closed in the same moment and only flickered.

## 0.9.1

- **Glass looks like glass.** The window used Windows' own acrylic, a pale, thin material that turned milky over anything bright and went flat whenever ride wasn't the active window, so lowering the opacity only made it whiter. It now has a frosted blur of its own, tinted with the theme's background: what is behind shows through as soft colour, the panes stay dark on a dark theme, and it stays blurred when another window has the focus.

## 0.9.0

- **Inlay hints.** The types and parameter names the language server works out show faintly in the code, as in VS Code: `greet(name: "x")`, `const total: number`. They aren't part of the text, and clicks beside them land where you'd expect. TypeScript gets them set up as VS Code users have them. On and off in Settings → Editor, the Command Palette, or a shortcut of your choosing.
- **Semantic highlighting.** Where a language server can tell them apart, parameters, variables, fields, constants and types each get their own colour. VS Code themes bring their own colours for these.
- **Sticky scroll.** The first lines of the function, class and blocks the top of the view is inside stay pinned above the code; click one to go to it. Up to 5 lines, which you can change.
- **Keyboard shortcuts in Settings.** Every shortcut is listed with its keys: click one and press the new keys, remove it, or put it back to the default. Shortcuts with the same keys are pointed out. The Command Palette shows the keys you set.
- **Settings has a search box**, and the Editor section has more in it: cursor style (line, block, underline), indent guides, matching brackets, show whitespace (never, in the selection, everywhere), rulers at the columns you list, and whether the last line can be scrolled to the top.
- **Brackets and quotes close themselves.** Typing `(` puts `)` after the cursor, typing `)` steps over it, Backspace between the two takes both, and a selection gets wrapped. Enter between `{}` opens an indented line. Off in Settings if you'd rather not.
- New in the editor: Alt+Up and Alt+Down move lines, Shift+Alt+Down copies them, Ctrl+Shift+K deletes them, Ctrl+G goes to a line (`:` in the palette), Ctrl+B shows or hides the side bar.
- Find in file is a small box at the top right again; it had grown to cover the file.

## 0.8.5

- **Full access mode for Claude.** A fourth mode next to Ask, Edit and Auto: Claude does whatever it asks to do without stopping for you, any command or tool included (pushes and deploys too). Questions it puts to you still come to you, and anything your own Claude Code settings deny stays denied. It is never the default; pick it in the chat, or in Settings for new chats.

## 0.8.4

- **See the pictures Claude looks at.** When Claude reads an image file, or a tool hands it a screenshot (Tribib's canvas, say), the picture shows in the chat right under that step: in the side chat, the Agents view, the Designer and a subagent's own conversation. Click it to see it whole; a click or Escape closes it.

## 0.8.3

- **Tribib is part of ride now.** ride starts Tribib itself and keeps its window inside the Designer: it has no window or taskbar button of its own and shows only in the Designer view. A Tribib ride started closes with ride. One you had open already is taken in too, and gets its own window back when ride closes.
- **Setup installs Tribib**, ticked by default and optional. It is downloaded from Tribib's own releases and checked against the checksum Tribib publishes before it runs; Tribib keeps itself up to date after that. If it can't be had during setup, ride is installed anyway, and the Designer offers it again.
- Sign in to Tribib once, in the Designer, and it remembers you.
- The Designer takes Tribib's real window, never one of its overlays: clicks on Tribib's home screen, with your documents, go through again.

## 0.8.2

- Tribib fills its place in the Designer to the edge: the strip it left at its sides and bottom is gone.
- The document list above Tribib, and any other list or menu, opens in front of it: Tribib steps aside while one is open.

## 0.8.1

- **Tribib inside ride.** The Designer now holds the Tribib app itself in its middle: your projects, the canvas, every tool, used right there as in Tribib. The chat with Claude is at the right, the design sessions at the left. Open Tribib (or have it open) and it moves in; the button at the top right of it gives it its own window back, and "Bring Tribib in" takes it in again.
- Tribib hides while you're in another view, and while a dialog or the command palette is open over it.
- When ride closes, Tribib gets its own window back. Ending ride from Task Manager, though, takes Tribib's window with it, as Windows closes a window held inside another one; close ride normally while Tribib is in it.

## 0.8.0

- **MCP servers for Claude.** Settings → Claude → MCP servers: a name and a URL, and every Claude ride starts gets them (the chat, the agents and the Designer). A server on this computer, at 127.0.0.1, is reached from the server Claude runs on through an SSH tunnel ride opens by itself, so a desktop tool works with Claude on your VPS.
- **The Designer view** (the new icon above Agents). Claude designs user interfaces in Tribib: each design session is a chat of its own with a Tribib document of its own, and the canvas shows live beside it as Claude builds, every moment while it works. Pick the document above the canvas; Fit or 100%, pause, look again, or open Tribib. Tribib's tools don't ask for approval every step. Ask for the design as code and Claude writes it into the open project. "Add Tribib" in the view or in Settings sets its server up.
- **Drag files from Explorer into the file tree.** While you carry them over the tree, the folder under the cursor lights up with "Upload into …", and they go into that folder (a file's row means its folder; empty space, the open folder).

## 0.7.5

- **A full table editor in the database explorer.** Click a table to open it in a tab of its own:
  - its rows a page at a time (50 to 1000 per page), with the total count and Next/Previous;
  - click a column's heading to sort by it, again to reverse, a third time for no order;
  - search every column at once, or filter with a WHERE condition;
  - double-click a cell to edit it (Enter keeps it, Escape drops it, Tab goes on to the next cell); long values open in a larger box; right-click sets a cell to NULL;
  - add a row (it starts with the columns' defaults), duplicate one, or delete the selected rows with Delete;
  - changed cells are marked yellow, new rows green, deleted rows red, until you save; Save (Ctrl+S) makes all of it as one transaction, so nothing goes in unless everything does, and Discard drops it;
  - rows are found by their primary key; a table without one is matched by all of its values;
  - click row numbers to select rows (Shift and Ctrl as usual, Ctrl+A for all), move with the arrow keys, and Ctrl+C copies the rows or the cell;
  - copy or save the page, or the selected rows, as CSV, JSON, SQL INSERTs or tab-separated text;
  - drag a heading's edge to make a column wider; the heading shows the primary key, and its tooltip the type, default and whether it may be NULL;
  - right-click a value to filter by it.
- A table's right-click menu in the explorer can still query its first 200 rows, or copy its name.
- Query results use the same grid: sort them by a column, select rows, and copy or save them.

## 0.7.4

- **Terminals side by side.** Drag a terminal's tab onto the edge of a terminal to put it beside or under it, as with the editor's tabs, or use the split button next to +. The line between two columns can be dragged. With several, the one you're typing in is marked with its colour.
- **A modern look for dialogs and menus.** Every question ride asks now comes the same way: the window dims, the box sits in the middle with its title and the main button at the bottom right (red where something can't be undone); Enter confirms, Escape cancels. Right-click menus have their icons in a column of their own and light up the row under the mouse.
- The Problems tab no longer grows taller as problems are found; it keeps the size you gave the panel and scrolls.
- Terminal tabs show the program's name instead of its whole path, and the panel's own buttons stay visible however many terminals there are.

## 0.7.3

- **A cleaner search.** The search and replace boxes are the same width and line up, with their buttons inside at the right; the box you're typing in is outlined and shows its cursor. The side bar no longer grows wider by itself while the search is open.
- **Name and colour your terminals.** Double-click a terminal's tab, or right-click it, to rename it. Without a name of yours, a terminal is named by what runs in it (Claude Code names it after what it's working on). Right-click → Colour marks it with a colour.
- **Copy path** and **Copy relative path** in the right-click menu of the file tree and of the editor's tabs.

## 0.7.2

- A click on the Terminal or Problems tab no longer moves the panel: it moves only once its bar is dragged some way.
- The panel keeps the height you gave it when you switch between Terminal and Problems, and the side bar keeps its width when you switch views.

## 0.7.1

- The Terminal and Problems tabs at the top of the panel can be clicked again; before, a click there started moving the panel.
- Side bars can't be made narrower than their contents any more (the branch menu ran into the pull and push buttons). A very long branch name is cut short.

## 0.7.0

- **Language servers.** ride now understands your code the way VS Code does, with the same programs, run on the server next to it:
  - completions as you type (Tab or Enter takes one; Ctrl+Space asks for them), with each one's type and documentation;
  - problems underlined as you type, with the message at the end of the line and in the new Problems tab;
  - rest the mouse on a name to see what it is;
  - the arguments of the call you're typing, above the cursor;
  - F12 or Ctrl+click goes to a definition, Shift+F12 lists every reference, F2 renames across files, Ctrl+. offers quick fixes;
  - Shift+Alt+F formats with the language server when the file has no formatter set.
- TypeScript and JavaScript, Python, Lua, Rust, Go, C and C++, PHP, JSON, CSS, HTML, Bash and YAML are set up. The first time a file needs a server that isn't on the server, ride offers to install it in a terminal. The list can be changed in Settings → Editor.
- **Replace in the folder.** The arrow next to the folder search (or Ctrl+Shift+H) opens a Replace box. Each result shows what the line will become; replace in one file or in all of them, and leave a file out with its ×. Open files change in their tab, so Ctrl+Z takes it back.
- **A layout like VS Code's.** The title bar is slim, with the open folder in the middle (click it to go to a file) and buttons for the side bar, the panel and the chat. The views moved to a column of icons on the left: Explorer, Search, Source control, Claude's changes, Outline, Databases and the server monitor, with Agents, Themes, your account and Settings at the bottom. The connection, the branch and the problem count are at the left of the status bar.
- The panel at the bottom has two tabs, Terminal and Problems.

## 0.6.8

- **Full Markdown in the Claude chat and the Agents view.** Tables, numbered and nested lists, checklists, italic and struck-through text, links (a click opens them in the browser), quotes, horizontal rules and headings of every level now show formatted, instead of as raw symbols.

## 0.6.7

- **Subagents in the Claude chat.** When Claude starts subagents, each one shows as a card in the conversation, with its kind, what it was asked to do, and whether it's still working or done.
- Click a card to open that subagent's own conversation: what it wrote and every tool it used, as it happens. The arrow at the top goes back. A subagent's own subagents open the same way.
- Files a subagent edits now count in the Changes panel too.

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
