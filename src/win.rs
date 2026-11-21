//! Window and monitor queries.

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::cloak::is_cloaked;

#[derive(Clone, Debug)]
pub struct Monitor {
    pub name: String,
    pub rect: RECT,
    pub work: RECT,
}

pub fn hwnd_key(h: HWND) -> isize {
    h.0 as isize
}

pub fn key_hwnd(k: isize) -> HWND {
    HWND(k as *mut _)
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn monitor_info(hm: HMONITOR) -> Option<Monitor> {
    unsafe {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(hm, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            return None;
        }
        Some(Monitor {
            name: wide_to_string(&mi.szDevice),
            rect: mi.monitorInfo.rcMonitor,
            work: mi.monitorInfo.rcWork,
        })
    }
}

/// All monitors, ordered left-to-right (then top-to-bottom).
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(hm: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let v = &mut *(data.0 as *mut Vec<Monitor>);
        if let Some(m) = monitor_info(hm) {
            v.push(m);
        }
        TRUE
    }
    let mut v: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v.sort_by_key(|m| (m.rect.left, m.rect.top));
    v
}

pub fn monitor_name_of_window(h: HWND) -> Option<String> {
    unsafe {
        // Minimized windows report the taskbar-ish position; use their restore rect.
        let hm = if IsIconic(h).as_bool() {
            let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            if GetWindowPlacement(h, &mut wp).is_ok() {
                let r = wp.rcNormalPosition;
                MonitorFromPoint(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }, MONITOR_DEFAULTTONEAREST)
            } else {
                MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST)
            }
        } else {
            MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST)
        };
        monitor_info(hm).map(|m| m.name)
    }
}

pub fn monitor_name_at_cursor() -> Option<String> {
    unsafe {
        let mut p = POINT::default();
        if GetCursorPos(&mut p).is_err() {
            return None;
        }
        monitor_info(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST)).map(|m| m.name)
    }
}

pub fn title(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

const IGNORED_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "NotifyIconOverflowWindow",
    "ForegroundStaging",
    "MultitaskingViewFrame",
];

/// A normal application window that belongs on a workspace.
/// `allow_cloaked` is used when checking windows we hid ourselves.
pub fn is_manageable(h: HWND, allow_cloaked: bool) -> bool {
    unsafe {
        if !IsWindow(h).as_bool() || !IsWindowVisible(h).as_bool() {
            return false;
        }
        if GetAncestor(h, GA_ROOT) != h {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return false;
        }
        let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if style & WS_CHILD.0 != 0 {
            return false;
        }
        if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
            return false;
        }
        if ex & WS_EX_NOACTIVATE.0 != 0 {
            return false;
        }
        // Always-on-top windows (picture-in-picture, overlays) stay on every workspace.
        if ex & WS_EX_TOPMOST.0 != 0 {
            return false;
        }
        if GetWindowTextLengthW(h) == 0 {
            return false;
        }
        let class = class_name(h);
        if IGNORED_CLASSES.iter().any(|c| *c == class) {
            return false;
        }
        if !allow_cloaked && is_cloaked(h) {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(h, &mut r).is_ok() && !IsIconic(h).as_bool() && (r.right - r.left <= 1 || r.bottom - r.top <= 1) {
            return false;
        }
        true
    }
}

/// Every top-level window, visible or not.
pub fn all_top_level() -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, data: LPARAM) -> BOOL {
        (*(data.0 as *mut Vec<HWND>)).push(h);
        TRUE
    }
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// Visible, manageable top-level windows in z-order (topmost first).
pub fn visible_windows() -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, data: LPARAM) -> BOOL {
        let v = &mut *(data.0 as *mut Vec<HWND>);
        if is_manageable(h, false) {
            v.push(h);
        }
        TRUE
    }
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// Put text on the clipboard.
pub fn set_clipboard_text(text: &str) {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(h);
                if SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h.0)).is_err() {
                    let _ = GlobalFree(h);
                }
            }
        }
        let _ = CloseClipboard();
    }
}
