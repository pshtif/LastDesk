# Created by Peter @sHTiF Stefcek
# Build LastDesk and install it to %USERPROFILE%\tools\LastDesk, replacing a running copy.
# The running copy is closed gracefully: it saves the workspace layout and shows all windows,
# and the new copy puts every window back on its workspace when it starts.
#
#   .\install.ps1              build + install + start
#   .\install.ps1 -Autostart   also start LastDesk with Windows
param([switch]$Autostart)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$dest = Join-Path $env:USERPROFILE 'tools\LastDesk'
$exe = Join-Path $dest 'lastdesk.exe'
New-Item -ItemType Directory -Force $dest | Out-Null

# Ask a running LastDesk to exit (WM_CLOSE to its hidden main window).
Add-Type @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class LastDeskCtl {
  delegate bool EP(IntPtr h, IntPtr l);
  [DllImport("user32")] static extern bool EnumWindows(EP f, IntPtr l);
  [DllImport("user32", CharSet=CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32")] static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  public static bool Close() { bool ok = false; EnumWindows((h, l) => { var sb = new StringBuilder(64); GetClassNameW(h, sb, 64);
    if (sb.ToString() == "LastDeskMain") ok = PostMessageW(h, 0x10, IntPtr.Zero, IntPtr.Zero); return true; }, IntPtr.Zero); return ok; }
}
'@
function Stop-LastDesk {
    if ([LastDeskCtl]::Close()) {
        for ($i = 0; $i -lt 50 -and (Get-Process lastdesk -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 100 }
    }
    if (Get-Process lastdesk -ErrorAction SilentlyContinue) { throw "LastDesk is still running; close it from the tray and retry" }
}

# A copy running straight from target\ would lock the build output: stop it first.
$target = (Resolve-Path 'target\release' -ErrorAction SilentlyContinue).Path
if ($target -and (Get-Process lastdesk -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$target*" })) { Stop-LastDesk }

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "build failed" }

Stop-LastDesk

Copy-Item 'target\release\lastdesk.exe' $exe -Force

if ($Autostart) {
    Set-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'LastDesk' -Value "`"$exe`""
}

# Start detached so it outlives this shell.
$si = New-CimInstance -ClassName Win32_ProcessStartup -ClientOnly -Property @{ ShowWindow = [uint16]1 }
$null = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = "`"$exe`""; CurrentDirectory = $dest; ProcessStartupInformation = $si }
"LastDesk installed to $exe"
