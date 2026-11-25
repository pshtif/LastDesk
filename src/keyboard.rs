// Created by Peter @sHTiF Stefcek
//! Keyboard layouts for the bar: which one the focused app types with ("ENG" over "UK", as
//! Windows shows it), the installed ones, and switching. Shown only when there's more than one to switch between.
//! Read on a short timer (Windows sends no event to other apps when the layout
//! changes); cheap: one call for the focused window's thread.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Globalization::{GetLocaleInfoW, LOCALE_SABBREVLANGNAME, LOCALE_SISO3166CTRYNAME, LOCALE_SLOCALIZEDDISPLAYNAME};
use windows::Win32::System::Registry::{RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, GetKeyboardLayoutList, HKL};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, PostMessageW, WM_INPUTLANGCHANGEREQUEST};

/// The layout last seen in the focused app (kept while focus is on LastDesk's own menus).
static CURRENT: AtomicIsize = AtomicIsize::new(0);

/// The installed layouts, in Windows' order.
pub fn layouts() -> Vec<isize> {
    unsafe {
        let n = GetKeyboardLayoutList(None);
        if n <= 0 {
            return Vec::new();
        }
        let mut v = vec![HKL::default(); n as usize];
        let got = GetKeyboardLayoutList(Some(&mut v));
        v.truncate(got.max(0) as usize);
        v.into_iter().map(|h| h.0 as isize).collect()
    }
}

/// Worth showing: more than one layout to switch between.
pub fn shown() -> bool {
    unsafe { GetKeyboardLayoutList(None) > 1 }
}

/// Layouts added or removed since last asked: the bars make room for the indicator, or drop it.
pub fn shown_changed() -> bool {
    static SHOWN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    let now = shown() as u8;
    let was = SHOWN.swap(now, Ordering::Relaxed);
    was != 2 && was != now
}

/// The focused app's layout, read again; true when it changed since last time.
pub fn refresh() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        // LastDesk's own windows (its menus, Settings) aren't what the user types into.
        let mut pid = 0u32;
        let thread = GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == std::process::id() {
            return false;
        }
        let hkl = GetKeyboardLayout(thread).0 as isize;
        if hkl == 0 {
            return false;
        }
        CURRENT.swap(hkl, Ordering::Relaxed) != hkl
    }
}

pub fn current() -> isize {
    let c = CURRENT.load(Ordering::Relaxed);
    if c == 0 {
        refresh();
    }
    CURRENT.load(Ordering::Relaxed)
}

fn locale_info(lcid: u32, what: u32) -> String {
    unsafe {
        let mut buf = [0u16; 128];
        let n = GetLocaleInfoW(lcid, what, Some(&mut buf));
        if n <= 1 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..(n as usize - 1)])
    }
}

/// The language (low word) and the layout's own locale (from its registry id: 0x0809 → GB).
fn parts(hkl: isize) -> (u32, u32) {
    let lang = (hkl as usize & 0xFFFF) as u32;
    let device = ((hkl as usize >> 16) & 0xFFFF) as u32;
    // A variant ("F002"): its locale is in the registry entry's id; else the device id is one.
    let layout_lcid = if device & 0xF000 == 0xF000 { klid(hkl).and_then(|k| u32::from_str_radix(&k[4..], 16).ok()).unwrap_or(lang) } else { device };
    (lang, layout_lcid)
}

/// "ENG" and "UK": the language, and the country of the layout.
pub fn short(hkl: isize) -> (String, String) {
    let (lang, layout) = parts(hkl);
    let top = locale_info(lang, LOCALE_SABBREVLANGNAME).to_uppercase();
    let mut bottom = locale_info(layout, LOCALE_SISO3166CTRYNAME);
    if bottom == "GB" {
        bottom = "UK".into();
    }
    (top, bottom)
}

/// The layout's registry id ("00000809", "00010409"), found by its "Layout Id" for variants.
fn klid(hkl: isize) -> Option<String> {
    let device = ((hkl as usize >> 16) & 0xFFFF) as u32;
    if device & 0xF000 != 0xF000 {
        return Some(format!("{device:08X}"));
    }
    let want = format!("{:04x}", device & 0x0FFF);
    unsafe {
        let mut key = HKEY::default();
        let path: Vec<u16> = "SYSTEM\\CurrentControlSet\\Control\\Keyboard Layouts\0".encode_utf16().collect();
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), 0, KEY_READ, &mut key).is_err() {
            return None;
        }
        let mut found = None;
        for i in 0.. {
            let mut name = [0u16; 32];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(key, i, PWSTR(name.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None).is_err() {
                break;
            }
            let sub = String::from_utf16_lossy(&name[..len as usize]);
            if reg_string(key, &sub, "Layout Id").is_some_and(|id| id.eq_ignore_ascii_case(&want)) {
                found = Some(sub);
                break;
            }
        }
        let _ = RegCloseKey(key);
        found
    }
}

fn reg_string(key: HKEY, sub: &str, value: &str) -> Option<String> {
    unsafe {
        let sub_w: Vec<u16> = sub.encode_utf16().chain(Some(0)).collect();
        let val_w: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let mut buf = [0u16; 256];
        let mut size = (buf.len() * 2) as u32;
        RegGetValueW(key, PCWSTR(sub_w.as_ptr()), PCWSTR(val_w.as_ptr()), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).ok().ok()?;
        let n = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

/// "English (United Kingdom) · US": the language, and the layout's own name.
pub fn name(hkl: isize) -> String {
    let (lang, _) = parts(hkl);
    let language = locale_info(lang, LOCALE_SLOCALIZEDDISPLAYNAME);
    let layout = klid(hkl).and_then(|k| reg_string(HKEY_LOCAL_MACHINE, &format!("SYSTEM\\CurrentControlSet\\Control\\Keyboard Layouts\\{k}"), "Layout Text"));
    match layout {
        Some(l) if !l.is_empty() => format!("{language} · {l}"),
        _ => language,
    }
}

/// Switches the app `to` (the one that was focused) to the layout; Windows applies it to every
/// app unless "a different input method for each app window" is set.
pub fn switch(to: HWND, hkl: isize) {
    unsafe {
        let _ = PostMessageW(to, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(hkl));
    }
    CURRENT.store(hkl, Ordering::Relaxed);
}
