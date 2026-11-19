# LastDesk

Per-monitor workspaces for Windows. Native Rust, no tiling, ~3 MB RAM.

Windows' own virtual desktops switch every monitor at once. LastDesk gives each monitor
its own workspaces 1–4.

| Keys | Action |
|---|---|
| `Alt+1..4` | Switch the workspace of the monitor you're on |
| `Alt+Shift+1..4` | Move the focused window to that workspace |
| `Alt+Shift+P` | Pin/unpin the focused window (shown on every workspace) |

"The monitor you're on" is the focused window's monitor, or the mouse's if nothing is focused.
Windows belong to whatever workspace was showing when you switched away. Clicking a hidden
window on the taskbar switches to its workspace. Always-on-top windows (picture-in-picture)
stay on every workspace.

Tray icon shows each monitor's current workspace (left to right). Click it for a list of
workspaces and windows, "Show all windows (reset)", "Start with Windows" and "Exit".

## Crash safety

Hidden windows are recorded in `%LOCALAPPDATA%\LastDesk\hidden.txt` and restored on exit,
on panic, on display changes and on the next start, so a crash never leaves windows invisible.

## Config

`%APPDATA%\LastDesk\config.ini` (tray > "Edit config…", then "Reload config"):

```ini
workspaces = 4            # per monitor, 1-9

[keys]
switch = alt              # alt+1..N
move = alt+shift          # alt+shift+1..N
pin = alt+shift+p
overview = alt+`

[pinned_apps]             # shown on every workspace
spotify

[rules]                   # new windows of an app open on monitor, workspace (1 = leftmost)
slack = 2, 3
```

Windows' own Win+Shift+Left/Right moves a window to another monitor and works with LastDesk.

## Remembered layout

The workspace of every hidden or pinned window and each monitor's current workspace are kept
in `%LOCALAPPDATA%\LastDesk\layout.txt`. When LastDesk restarts (update, crash) windows are
matched by window ID; after a reboot by exact app + window class + title. Anything not matched
stays visible on the current workspace. Tray > "Show all windows (reset)" clears the layout.

## Install

```
.\install.ps1              # build, install to %USERPROFILE%\tools\LastDesk, (re)start
.\install.ps1 -Autostart   # same, and start with Windows
```

Updating replaces a running copy gracefully: it saves the layout, exits, and the new copy puts
every window back.