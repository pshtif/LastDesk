// Created by Peter @sHTiF Stefcek
//! Watchdog. LastDesk now carries the bar, tray icons and (optionally) the Tailscale
//! connection, so if it ever disappears unexpectedly it should come back by itself.
//!
//! After start-up LastDesk launches a copy of itself as `lastdesk.exe --guard <pid>`. The guard
//! only waits for that process to end. If it ended without writing the clean-exit marker (Exit
//! from the menu, a close request from the installer, signing out or shutting down all write
//! it), the guard starts LastDesk again; the new instance starts its own guard. A crash loop is
//! stopped: after 3 restarts within 2 minutes the guard gives up and says so.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE};

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const MAX_RESTARTS: usize = 3;
const WINDOW_SECS: u64 = 120;

fn marker() -> PathBuf {
    crate::state_path().with_file_name("clean-exit")
}

fn history() -> PathBuf {
    crate::state_path().with_file_name("guard-restarts.txt")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// LastDesk is exiting on purpose: the guard must not bring it back.
pub fn mark_clean_exit() {
    let _ = std::fs::write(marker(), b"1");
}

/// Called once LastDesk is up: clear the marker and start the guard for this process.
pub fn spawn() {
    let _ = std::fs::remove_file(marker());
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::process::Command::new(exe)
        .args(["--guard", &std::process::id().to_string()])
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn();
}

/// Guard mode: wait for `pid`, restart LastDesk if it ended unexpectedly.
pub fn run(pid: u32) {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return };
        WaitForSingleObject(h, INFINITE);
        let _ = CloseHandle(h);
    }
    // Give a clean exit a moment to finish writing its marker.
    std::thread::sleep(Duration::from_millis(500));
    if marker().exists() {
        return;
    }
    // Crash-loop protection.
    let t = now();
    let mut recent: Vec<u64> = std::fs::read_to_string(history())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .filter(|&s: &u64| t.saturating_sub(s) < WINDOW_SECS)
        .collect();
    if recent.len() >= MAX_RESTARTS {
        crate::trace("guard: too many unexpected exits, not restarting");
        crate::message(
            "LastDesk closed unexpectedly several times in a row, so it wasn't restarted again.\n\nStart it from the Start menu or tools\\LastDesk when ready. Details (if any) are in %LOCALAPPDATA%\\LastDesk\\crash.log.",
            true,
        );
        return;
    }
    recent.push(t);
    let _ = std::fs::write(history(), recent.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n"));
    crate::trace("guard: LastDesk ended unexpectedly, restarting it");
    // The old instance's single-instance mutex is released when it dies; start the new one.
    std::thread::sleep(Duration::from_millis(1000));
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).spawn();
    }
}
