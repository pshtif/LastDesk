// Created by Peter @sHTiF Stefcek
//! Optional Windows tweaks offered in Settings (all off unless turned on). Each one shows the
//! system's actual state, read from the registry when Settings opens, and changes it directly.
//! Tweaks that live under HKLM or the policy keys need admin: LastDesk then starts a copy of
//! itself elevated (`lastdesk.exe --tweak <name> on|off`), so the permission prompt comes
//! right after the click that asked for it.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteValueW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, KEY_WOW64_64KEY,
    REG_DWORD, REG_OPTION_NON_VOLATILE, RRF_RT_REG_DWORD,
};
use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_HIDE, WM_APP};

/// Posted to the Settings window when an elevated change has finished (re-read the states).
pub const WM_TWEAK_DONE: u32 = WM_APP + 9;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tweak {
    StartSearch,
    SearchWebView2,
    BingResults,
    ExplorerPreload,
}

pub const ALL: [Tweak; 4] = [Tweak::StartSearch, Tweak::SearchWebView2, Tweak::BingResults, Tweak::ExplorerPreload];

const SEARCH_POLICY: PCWSTR = w!(r"SOFTWARE\Policies\Microsoft\Windows\Windows Search");
const WEBVIEW2_OVERRIDE: PCWSTR = w!(r"SYSTEM\CurrentControlSet\Control\FeatureManagement\Overrides\8\1694661260");
const EXPLORER_POLICY: PCWSTR = w!(r"Software\Policies\Microsoft\Windows\Explorer");
const EXPLORER_ADVANCED: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");

impl Tweak {
    pub fn label(self) -> &'static str {
        match self {
            Tweak::StartSearch => "Turn off Start search (frees ~100 MB; typing in Start finds nothing)",
            Tweak::SearchWebView2 => "Turn off Start search's web view (frees ~130 MB; after restart)",
            Tweak::BingResults => "No Bing web results in Start search",
            Tweak::ExplorerPreload => "Don't preload File Explorer in the background",
        }
    }

    pub fn needs_admin(self) -> bool {
        !matches!(self, Tweak::ExplorerPreload)
    }

    fn name(self) -> &'static str {
        match self {
            Tweak::StartSearch => "start-search",
            Tweak::SearchWebView2 => "search-webview2",
            Tweak::BingResults => "bing",
            Tweak::ExplorerPreload => "explorer-preload",
        }
    }

    fn from_name(s: &str) -> Option<Tweak> {
        ALL.into_iter().find(|t| t.name() == s)
    }

    /// Is the tweak applied right now?
    pub fn is_on(self) -> bool {
        match self {
            Tweak::StartSearch => get_dword(HKEY_LOCAL_MACHINE, SEARCH_POLICY, w!("DisableSearch")) == Some(1),
            Tweak::SearchWebView2 => get_dword(HKEY_LOCAL_MACHINE, WEBVIEW2_OVERRIDE, w!("EnabledState")) == Some(1),
            Tweak::BingResults => get_dword(HKEY_CURRENT_USER, EXPLORER_POLICY, w!("DisableSearchBoxSuggestions")) == Some(1),
            Tweak::ExplorerPreload => get_dword(HKEY_CURRENT_USER, EXPLORER_ADVANCED, w!("ShouldPrelaunchFileExplorer")) == Some(0),
        }
    }

    /// Write the registry values (the caller must have the rights).
    fn write(self, on: bool) -> bool {
        match (self, on) {
            (Tweak::StartSearch, true) => set_dword(HKEY_LOCAL_MACHINE, SEARCH_POLICY, w!("DisableSearch"), 1),
            (Tweak::StartSearch, false) => del_value(HKEY_LOCAL_MACHINE, SEARCH_POLICY, w!("DisableSearch")),
            (Tweak::SearchWebView2, true) => {
                set_dword(HKEY_LOCAL_MACHINE, WEBVIEW2_OVERRIDE, w!("EnabledState"), 1) && set_dword(HKEY_LOCAL_MACHINE, WEBVIEW2_OVERRIDE, w!("EnabledStateOptions"), 0)
            }
            (Tweak::SearchWebView2, false) => unsafe { RegDeleteKeyW(HKEY_LOCAL_MACHINE, WEBVIEW2_OVERRIDE) == ERROR_SUCCESS || !self.is_on() },
            (Tweak::BingResults, true) => set_dword(HKEY_CURRENT_USER, EXPLORER_POLICY, w!("DisableSearchBoxSuggestions"), 1),
            (Tweak::BingResults, false) => del_value(HKEY_CURRENT_USER, EXPLORER_POLICY, w!("DisableSearchBoxSuggestions")),
            (Tweak::ExplorerPreload, on) => set_dword(HKEY_CURRENT_USER, EXPLORER_ADVANCED, w!("ShouldPrelaunchFileExplorer"), if on { 0 } else { 1 }),
        }
    }
}

fn get_dword(root: HKEY, key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut n = 4u32;
    unsafe { RegGetValueW(root, key, value, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as _), Some(&mut n)) == ERROR_SUCCESS }.then_some(v)
}

fn set_dword(root: HKEY, key: PCWSTR, value: PCWSTR, v: u32) -> bool {
    unsafe {
        let mut h = HKEY::default();
        if RegCreateKeyExW(root, key, 0, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE | KEY_WOW64_64KEY, None, &mut h, None) != ERROR_SUCCESS {
            return false;
        }
        let ok = RegSetValueExW(h, value, 0, REG_DWORD, Some(&v.to_le_bytes())) == ERROR_SUCCESS;
        let _ = RegCloseKey(h);
        ok
    }
}

fn del_value(root: HKEY, key: PCWSTR, value: PCWSTR) -> bool {
    unsafe {
        let mut h = HKEY::default();
        if RegCreateKeyExW(root, key, 0, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE | KEY_WOW64_64KEY, None, &mut h, None) != ERROR_SUCCESS {
            return false;
        }
        let r = RegDeleteValueW(h, value);
        let _ = RegCloseKey(h);
        r == ERROR_SUCCESS || r.0 == 2 // already gone
    }
}

/// Turn a tweak on or off. Without admin rights needed it's done here; otherwise an elevated
/// copy does it and `notify` gets WM_TWEAK_DONE when it's finished. Returns false if it
/// couldn't start (e.g. the permission prompt was cancelled).
pub fn set(t: Tweak, on: bool, notify: HWND) -> bool {
    if !t.needs_admin() {
        return t.write(on);
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    let file: Vec<u16> = exe.as_os_str().encode_wide_z();
    let params: Vec<u16> = format!("--tweak {} {}", t.name(), if on { "on" } else { "off" }).encode_utf16().chain(std::iter::once(0)).collect();
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: notify,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        if ShellExecuteExW(&mut sei).is_err() || sei.hProcess.is_invalid() {
            return false;
        }
        let proc = sei.hProcess.0 as isize;
        let n = notify.0 as isize;
        std::thread::spawn(move || {
            let h = windows::Win32::Foundation::HANDLE(proc as *mut _);
            WaitForSingleObject(h, INFINITE);
            let _ = CloseHandle(h);
            let _ = PostMessageW(HWND(n as *mut _), WM_TWEAK_DONE, WPARAM(0), LPARAM(0));
        });
    }
    true
}

/// `--tweak <name> on|off` mode (already elevated): apply and exit.
pub fn run_elevated(args: &[String]) {
    let (Some(name), Some(state)) = (args.first(), args.get(1)) else { return };
    if let Some(t) = Tweak::from_name(name) {
        t.write(state == "on");
    }
}

trait WideZ {
    fn encode_wide_z(&self) -> Vec<u16>;
}

impl WideZ for std::ffi::OsStr {
    fn encode_wide_z(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}
