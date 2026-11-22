// Created by Peter @sHTiF Stefcek
//! Pinned app buttons ("launchers"): what to store when a window is pinned, matching running
//! windows to a launcher, the launcher's icon, and starting it.
//!
//! Desktop apps are stored as their exe path and matched by exe name. Store apps run inside
//! WindowsApps (not launchable directly) or ApplicationFrameHost, so they are stored as
//! "shell:AppsFolder\<AUMID>" and matched by the window's AppUserModelID.

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow, PROPERTYKEY};
use windows::Win32::UI::Shell::{ILFree, SHGetFileInfoW, SHParseDisplayName, ShellExecuteW, SHFILEINFOW, SHGFI_DISPLAYNAME, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_PIDL};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, HICON, SW_SHOWNORMAL};

use crate::config::Launcher;

pub const APPS_FOLDER: &str = "shell:AppsFolder\\";

// PKEY_AppUserModel_ID {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5
const PKEY_AUMID: PROPERTYKEY = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3), pid: 5 };

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The window's explicit AppUserModelID ("" if it has none).
pub fn window_aumid(h: HWND) -> String {
    unsafe {
        let Ok(store) = SHGetPropertyStoreForWindow::<_, IPropertyStore>(h) else { return String::new() };
        let Ok(v) = store.GetValue(&PKEY_AUMID) else { return String::new() };
        match PropVariantToStringAlloc(&v) {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            }
            Err(_) => String::new(),
        }
    }
}

fn exe_path(h: HWND) -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(p);
        if ok {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }
}

pub fn stem(path: &str) -> String {
    let file = path.rsplit('\\').next().unwrap_or(path);
    file.rsplit_once('.').map(|(a, _)| a).unwrap_or(file).to_lowercase()
}

/// Shell display name of a target ("Calculator" for a Store app), if the shell knows one.
fn display_name(target: &str) -> Option<String> {
    unsafe {
        let t = wide(target);
        let mut pidl = std::ptr::null_mut();
        SHParseDisplayName(PCWSTR(t.as_ptr()), None, &mut pidl, 0, None).ok()?;
        let mut sfi = SHFILEINFOW::default();
        let r = SHGetFileInfoW(PCWSTR(pidl as *const u16), Default::default(), Some(&mut sfi), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_PIDL | SHGFI_DISPLAYNAME);
        ILFree(Some(pidl));
        if r == 0 {
            return None;
        }
        let end = sfi.szDisplayName.iter().position(|&c| c == 0).unwrap_or(sfi.szDisplayName.len());
        let s = String::from_utf16_lossy(&sfi.szDisplayName[..end]);
        (!s.is_empty()).then_some(s)
    }
}

/// What to pin for this window, or None if it can't be relaunched.
pub fn for_window(h: HWND) -> Option<Launcher> {
    let path = exe_path(h);
    if path.is_empty() {
        return None;
    }
    let packaged = path.to_lowercase().contains("\\windowsapps\\") || stem(&path) == "applicationframehost";
    let target = if packaged {
        let aumid = window_aumid(h);
        if aumid.is_empty() {
            return None;
        }
        format!("{APPS_FOLDER}{aumid}")
    } else {
        path.clone()
    };
    let name = display_name(&target).unwrap_or_else(|| stem(&path));
    // Keep the config line parseable.
    let name: String = name.chars().filter(|c| !matches!(c, '=' | '#' | '[' | ']')).collect::<String>().trim().to_lowercase();
    Some(Launcher { name: if name.is_empty() { stem(&path) } else { name }, target })
}

/// The launcher's icon (32 px; drawn scaled). Caller owns it (DestroyIcon).
pub fn icon(target: &str) -> HICON {
    unsafe {
        let t = wide(target);
        let mut pidl = std::ptr::null_mut();
        if SHParseDisplayName(PCWSTR(t.as_ptr()), None, &mut pidl, 0, None).is_err() {
            return HICON::default();
        }
        let mut sfi = SHFILEINFOW::default();
        SHGetFileInfoW(PCWSTR(pidl as *const u16), Default::default(), Some(&mut sfi), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_PIDL | SHGFI_ICON | SHGFI_LARGEICON);
        ILFree(Some(pidl));
        sfi.hIcon
    }
}

pub fn launch(target: &str) {
    let t = wide(target);
    // Desktop apps start in their own folder, like a shortcut would.
    let dir = if target.starts_with(APPS_FOLDER) { None } else { target.rsplit_once('\\').map(|(d, _)| wide(d)) };
    unsafe {
        ShellExecuteW(
            None,
            windows::core::w!("open"),
            PCWSTR(t.as_ptr()),
            None,
            dir.as_ref().map(|d| PCWSTR(d.as_ptr())).unwrap_or(PCWSTR::null()),
            SW_SHOWNORMAL,
        );
    }
}
