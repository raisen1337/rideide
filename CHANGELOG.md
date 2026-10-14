# Changelog

ride shows the entries newer than the version you had, once, the first time it starts after an
update. Each `## <version>` heading starts an entry; write for the people using ride.

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
