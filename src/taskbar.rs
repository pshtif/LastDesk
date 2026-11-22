//! Hiding the Windows taskbar while the LastDesk bar is on.
//!
//! The Explorer taskbar windows are hidden (ShowWindow) but keep their reserved strip of the
//! screen; the LastDesk bar then sits exactly in that strip. No Windows setting is changed.
//! A marker file (%LOCALAPPDATA%\LastDesk\taskbar.txt) records that the taskbar is hidden so
//! the next start shows it again if LastDesk was killed; the panic hook restores it too.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub static HIDDEN: AtomicBool = AtomicBool::new(false);

fn record_path() -> std::path::PathBuf {
    crate::state_path().with_file_name("taskbar.txt")
}

pub fn tray_windows() -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, data: LPARAM) -> BOOL {
        let v = &mut *(data.0 as *mut Vec<HWND>);
        if crate::win::class_name(h) == "Shell_SecondaryTrayWnd" {
            v.push(h);
        }
        TRUE
    }
    let mut v = Vec::new();
    unsafe {
        if let Ok(main) = FindWindowW(w!("Shell_TrayWnd"), None) {
            if !main.is_invalid() {
                v.push(main);
            }
        }
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

fn primary_tray() -> Option<HWND> {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), None).ok().filter(|h| !h.is_invalid()) }
}

/// The strip a (hidden) taskbar reserves on this monitor, if it reserves one: i.e. the
/// taskbar is on this monitor and the work area doesn't overlap it (not auto-hide).
pub fn reserved_strip(monitor: RECT, work: RECT) -> Option<RECT> {
    for h in tray_windows() {
        let mut r = RECT::default();
        if unsafe { GetWindowRect(h, &mut r) }.is_err() {
            continue;
        }
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let on_monitor = cx >= monitor.left && cx < monitor.right && cy >= monitor.top && cy < monitor.bottom;
        let overlaps_work = r.left < work.right && r.right > work.left && r.top < work.bottom && r.bottom > work.top;
        if on_monitor && !overlaps_work && r.bottom > r.top && r.right > r.left {
            // Clamp to the monitor (the window can extend a few pixels past it).
            return Some(RECT {
                left: r.left.max(monitor.left),
                top: r.top.max(monitor.top),
                right: r.right.min(monitor.right),
                bottom: r.bottom.min(monitor.bottom),
            });
        }
    }
    None
}

pub fn hide_all() {
    for h in tray_windows() {
        unsafe {
            if IsWindowVisible(h).as_bool() {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }
}

pub fn show_all() {
    for h in tray_windows() {
        unsafe {
            let _ = ShowWindow(h, SW_SHOWNA);
        }
    }
}

/// Hide the taskbars on secondary monitors. LastDesk never shows those on purpose, so unlike
/// the main one (peeked at for tray icons) they're hidden whatever has focus. Explorer shows
/// them again on some settings changes (e.g. a policy refresh).
pub fn hide_secondary() {
    for h in tray_windows() {
        unsafe {
            if crate::win::class_name(h) == "Shell_SecondaryTrayWnd" && IsWindowVisible(h).as_bool() {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }
}

/// Show just the primary taskbar (to reach tray icons); it's hidden again when focus leaves.
pub fn peek() {
    if let Some(t) = primary_tray() {
        unsafe {
            let _ = ShowWindow(t, SW_SHOWNA);
        }
    }
}

/// Start menu, search, tray overflow, quick settings: don't fight the shell while these are up.
pub fn is_shell_ui(h: HWND) -> bool {
    matches!(
        crate::win::class_name(h).as_str(),
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "NotifyIconOverflowWindow"
            | "TopLevelWindowForOverflowXamlIsland"
            | "Windows.UI.Core.CoreWindow"
            | "XamlExplorerHostIslandWindow"
            | "ControlCenterWindow"
    )
}

pub fn hide() {
    let _ = std::fs::write(record_path(), "hidden");
    HIDDEN.store(true, Ordering::SeqCst);
    hide_all();
}

pub fn restore() {
    show_all();
    HIDDEN.store(false, Ordering::SeqCst);
    let _ = std::fs::remove_file(record_path());
}

/// A previous run was killed while the taskbar was hidden: show it again.
pub fn recover() {
    if record_path().exists() {
        restore();
    }
}

/// For the panic hook.
pub fn restore_if_hidden() {
    if HIDDEN.load(Ordering::SeqCst) {
        restore();
    }
}
