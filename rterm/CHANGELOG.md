# rterm changelog

rkit shows the entries newer than the rterm you have before you update it. Each `## <version>`
heading starts an entry; write for the people using rterm.

## 0.5.2

- **File paths open with Ctrl+click.** When a program prints a path to a file — like `C:\Users\you\pic.png` from Claude Code — it is now a link: hover to underline it, Ctrl+click to open it with whatever Windows uses for that kind of file (pictures in Photos, and so on). Before, only web addresses were clickable, and `file://` links wrongly opened in the browser. A trailing `:line:column` on a source path is ignored, so the file still opens.

## 0.5.1

- **Shells start with your own environment**, fresh from Windows, as Windows Terminal starts them: not with whatever started rterm. When rterm was opened from another tool (a Claude Code session sets NO_COLOR, for one), every tab inherited its settings, and Claude Code in them showed no colours. New variables (a PATH change, say) also reach new tabs without restarting.

## 0.5.0

- **rterm can be Windows' default terminal.** Turn on Settings → Default terminal for Windows and console programs started anywhere (cmd or PowerShell from the Start menu, scripts double-clicked in Explorer, tools that open a console) open as tabs in rterm, as they do in Windows Terminal; turn it off to give them back. This uses Windows Terminal's console host, so Windows Terminal needs to be installed (it is, on Windows 11).
- **Terminals draw text as Windows Terminal does.** Real bold from the font, italics, every kind of underline (double, curly, dotted, dashed) in its own colour, and strikethrough. Box-drawing lines and blocks are drawn as shapes, so frames and rounded corners join up from cell to cell and row to row, and pictures made of blocks have no gaps. Wide and unusual symbols are fitted into their cells instead of spilling over, and Chinese, Japanese and Korean show instead of empty boxes.
- **Claude Code and other tools look right**: programs on Windows check for Windows Terminal before they use colours and Unicode symbols, and now find it, so Claude Code shows its colours, ❯, ⏺ and frames instead of plain ASCII.
- Links in the output (and web addresses) open with Ctrl+click; programs that use the mouse get clicks and drags (Shift still selects text); programs that ask learn when the terminal gains or loses the keyboard; and synchronized output can't freeze the screen.

## 0.4.0

- rterm has its own version and changelog now, and updates on its own from rkit.
- `rterm --version` prints the version, for rkit to show.

## 0.3.0

- **Every pane has its own tabs.** Split the window and every pane gets a row of tabs above it, so you switch terminals right where they are; with one pane they stay up in the title bar.
- **Split by dragging.** Drag a tab to the left, right, top or bottom edge of any pane and the pane splits there; drop it among another pane's tabs to move it over. Panes split any way you like, and the lines between them drag to resize.

## 0.2.0

- **Split panes.** Split beside (Alt+Shift+=), below (Alt+Shift+-) or along the longer side (Alt+Shift+D). Alt and an arrow moves between panes, the lines between them drag to resize, and Ctrl+Shift+W closes the pane you're in.

## 0.1.0

- **rterm, a terminal for Windows**, made from ride's own terminal: tabs of PowerShell, PowerShell 7, Command Prompt, Git Bash, each WSL distribution and every host in your SSH config, in ride's look, with glass if you like.
- In the Start menu, as "Open in rterm" on folders, and by typing `rterm` in Explorer's address bar.
