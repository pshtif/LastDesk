# LastDesk

Per-monitor workspaces and a lean taskbar for Windows 11. Native Rust, a single ~600 KB exe,
about 6–10 MB of RAM, plus optional plugins.

Created by Peter @sHTiF Stefcek.

Windows' own virtual desktops switch every monitor at once. LastDesk gives each monitor its
own workspaces, and replaces the Windows taskbar with its own bar on every monitor.

## Features

**Workspaces**
- Each monitor has its own workspaces. It starts with one; **+** in the bar adds one (up to
  nine). Right-click a workspace to remove it: its windows move to the one before.
- Optionally, an empty workspace is removed when you switch away from it (Settings, off by
  default).
- Overview of all workspaces (`Alt+``).
- Pin a window to every workspace.
- Rules: open an app on a given monitor and workspace.
- Apps started from Start or the bar open on the monitor you started them from.

**Bar** (one per monitor, top or bottom)
- Start button, workspace cells and **+**. An orange frame marks a workspace with a window
  asking for attention.
- Window buttons:
  - reorder by dragging
  - hover for a live preview
  - turn orange when a window wants attention
  - right-click for minimize, pin, move to workspace (or a new one) or close
- Pinned apps ("Pin to bar"):
  - they keep their place and show a thin line along the top edge
  - while not running, they show as an icon that starts the app on that monitor
  - reorder by dragging
- App tray icons, hosted in the bar.
- Status icons:
  - microphone / camera in use (hover shows which app)
  - keyboard layout ("ENG" over "UK"), when more than one is installed; click to switch
  - Wi-Fi signal or Ethernet
  - volume (scroll to change, middle-click to mute)
  - battery, plus fan speed on ASUS laptops
  - clock
- Quick Settings, Start and the notification centre open on the monitor you clicked.
- Can hide the Windows taskbar. The bar keeps its strip of the screen reserved.

**Optional** (Settings, all off by default)
- Memory meter: RAM in use. Hover lists the apps using the most.
- Win key opens Start on the monitor you're on.
- Network drives that switch between a home path and an away path, e.g. a NAS by local
  address at home and through Tailscale or a VPN elsewhere. See below.
- Plugins (see below): Tailscale, LastTerminal, LastShot.

**Windows tweaks** (Settings; each shows and changes Windows' own setting; most ask for admin)
- Turn off Start search (~100 MB; typing in Start finds nothing).
- Turn off Start search's web view (~130 MB, after a restart).
- No Bing web results in Start search.
- Don't preload File Explorer in the background.

## Plugins

Optional parts that run as their own small programs, in `plugins\<id>\` next to
`lastdesk.exe`. Turn them on in **Settings → Plugins**; a plugin that's off isn't started, and
one that crashes takes nothing else down. `install.ps1` builds and installs them.

- **Tailscale**: status in the bar, connect / disconnect, and exit nodes. Mullvad nodes are
  grouped by country and city. Health warnings and an alert when the exit node goes offline.
  It talks to the Tailscale service directly, so Tailscale's tray app isn't needed.
- **LastTerminal** (if installed): its icon with the number of running sessions, the list on
  hover; click for a menu to open LastTerminal on one of them, or start it when it isn't
  running.
- **LastShot**: screenshots and screen recordings.
  - Click the camera or `Ctrl+Shift+S`: drag a region or click a window (`Enter`: the whole
    monitor), draw on it freehand if you like, then save. It's saved as PNG in
    `Pictures\LastShot` and copied to the clipboard.
  - `Ctrl+Shift+R`: record a region to MP4 (needs ffmpeg), with a panel to pause, stop or
    cancel. The panel and the frame around the region aren't recorded.

A plugin talks to LastDesk in JSON lines over its stdin / stdout. It can add a bar item (an
icon, a glyph or short text, with a tooltip), a menu, hotkeys and notifications. The protocol
is described at the top of `src/plugins.rs`.

## Keys

| Keys | Action |
|---|---|
| `Alt+1..9` | Switch to that workspace of the monitor you're on (if it has one) |
| `Alt+Shift+1..9` | Move the focused window there (one past the last makes a new workspace) |
| `Ctrl+Alt+Left/Right` | Previous / next workspace |
| `Alt+Shift+P` | Pin / unpin the focused window (shown on every workspace) |
| ``Alt+` `` | Overview |

"The monitor you're on" is the focused window's monitor, or the mouse's if you moved there
since. Windows' own `Win+Shift+Left/Right` moves a window to another monitor and works with
LastDesk.

## Requirements

- Windows 11. Developed and tested on ARM64 (Snapdragon), recent builds. It also builds for
  x64 (Intel / AMD); that build is untested. Windows 10 is untested.
- To build: [Rust](https://rustup.rs) (stable) and the Visual Studio C++ build tools.
- LastShot's recording: [ffmpeg](https://ffmpeg.org) (`winget install ffmpeg`).

## Install

```
.\install.ps1              # build, install to %USERPROFILE%\tools\LastDesk, (re)start
.\install.ps1 -Autostart   # same, and start with Windows
```

`install.ps1` builds for the machine it runs on (ARM64 or x64). To build for the other one:

```
rustup target add x86_64-pc-windows-msvc      # or aarch64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
```

Updating replaces a running copy gracefully. The old copy saves the layout and exits, and the
new copy puts every window back.

## Config

Right-click the bar (or the tray icon) > **Settings…** to change the common options. They
apply as you change them. Everything lives in `%APPDATA%\LastDesk\config.ini` (> "Edit config
file…", then "Reload config"):

```ini
remove_empty_workspaces = false   # remove an empty workspace when you leave it
animation = normal          # off | fast | normal | slow
launch_ignore = steam       # apps that keep their own position when started from Start

[keys]
switch = alt                # alt+1..9
move = alt+shift            # alt+shift+1..9
pin = alt+shift+p
overview = alt+`
next = ctrl+alt+right
prev = ctrl+alt+left

[bar]
enabled = true
position = top              # top | bottom
buttons = full              # full (icon + title) | icons
align = left                # left | center | right
hide_taskbar = false        # hide the Windows taskbar; the bar takes its place
tray = true                 # app tray icons in the bar
memory = false              # memory meter
win_key_start = false       # Win key opens Start on the monitor you're on

[plugins]
enabled = tailscale, lastterminal, lastshot

[pinned_apps]               # shown on every workspace
spotify

[launchers]                 # pinned to the bar (right-click a window > Pin to bar)
calculator = shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App

[rules]                     # new windows of an app open on monitor, workspace (1 = leftmost)
slack = 2, 3
```

### Network drives

Create them from the tray menu > "Set up network drives…". This writes
`%APPDATA%\LastDesk\drives.ini`, pre-filled with the drives mapped now:

```ini
home = aa-bb-cc-dd-ee-ff    # your router's MAC address: menu > "This network is home"
V: = \\192.168.1.10\Media | \\my-nas\Media | NAS Media
#    path at home         | path elsewhere  | name shown (optional)
```

- **How home is recognised:** by the router's hardware address, not by local IP. A café that
  happens to use the same addresses is never sent your login.
- **When drives switch:** only when you change networks, and only once the new path answers.
  Short Wi-Fi drops change nothing.
- **Failed switches:** the previous path is put back.
- **Without this file** the feature is off.

## Safety and privacy

- **Crash safety:** hidden windows are recorded in `%LOCALAPPDATA%\LastDesk\hidden.txt` and
  restored on exit, on panic, on display changes and on the next start. A crash never leaves
  windows invisible, and a hidden Windows taskbar is shown again.
- **Watchdog:** a tiny guard process restarts LastDesk if it ever disappears without you
  choosing Exit. It stops after 3 restarts within 2 minutes, so a crash can't loop.
- **Remembered layout:** each window's workspace, and each monitor's workspaces, are kept in
  `%LOCALAPPDATA%\LastDesk\layout.txt` and restored after restarts and reboots. Tray > "Show
  all windows (reset)" clears it.
- **Window previews:** live DWM thumbnails. Nothing is captured or stored.
- **Network:** no telemetry and no network access of its own. The Tailscale plugin only talks
  to the local Tailscale service, the LastTerminal plugin only to LastTerminal on this PC.
- **Screenshots and recordings** (LastShot) stay on this PC, in `Pictures\LastShot`.
- **Keyboard hook:** the optional Win-key feature uses one that only checks whether a key is
  the Win key.

## License

[MIT](LICENSE) © 2026 Peter @sHTiF Stefcek
