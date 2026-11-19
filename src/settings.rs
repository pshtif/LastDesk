//! Settings window: a plain native dialog over config.ini. Save writes the file (with
//! comments) and applies it immediately; invalid input is reported and nothing is saved.

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::ApplicationInstallationAndServicing::{ActivateActCtx, CreateActCtxW, DeactivateActCtx, ACTCTXW};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Align, Config, Edge};
use crate::with_app;

pub const CLASS: PCWSTR = w!("LastDeskSettings");

const ID_SAVE: i32 = 1; // IDOK: Enter
const ID_CANCEL: i32 = 2; // IDCANCEL: Esc
const ID_OPEN: i32 = 100;
const ID_WORKSPACES: i32 = 101;
const ID_SWITCH: i32 = 102;
const ID_MOVE: i32 = 103;
const ID_PIN: i32 = 104;
const ID_OVERVIEW: i32 = 105;
const ID_BAR: i32 = 106;
const ID_EDGE: i32 = 107;
const ID_BUTTONS: i32 = 108;
const ID_ALIGN: i32 = 109;
const ID_HIDE_TASKBAR: i32 = 110;
const ID_TRAY: i32 = 111;
const ID_PINNED: i32 = 112;
const ID_RULES: i32 = 113;

const SWITCH_CHOICES: &[&str] = &["alt", "win+alt", "ctrl+alt", "ctrl+win", "win"];
const MOVE_CHOICES: &[&str] = &["alt+shift", "win+shift", "ctrl+alt+shift", "win+alt+shift", "ctrl+shift"];

struct State {
    hwnd: HWND,
    font: HFONT,
    bold: HFONT,
}

thread_local! {
    static SETTINGS: RefCell<Option<State>> = const { RefCell::new(None) };
    static ACTCTX: RefCell<Option<HANDLE>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Themed (common controls v6) look without embedding a manifest in the exe.
fn activation_context() -> Option<HANDLE> {
    ACTCTX.with(|a| {
        if let Some(h) = *a.borrow() {
            return Some(h);
        }
        let path = crate::state_path().with_file_name("controls.manifest");
        let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency><dependentAssembly>
    <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
  </dependentAssembly></dependency>
</assembly>"#;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, xml);
        let p = wide(&path.display().to_string());
        let ctx = ACTCTXW { cbSize: std::mem::size_of::<ACTCTXW>() as u32, lpSource: PCWSTR(p.as_ptr()), ..Default::default() };
        let h = unsafe { CreateActCtxW(&ctx) }.ok()?;
        *a.borrow_mut() = Some(h);
        Some(h)
    })
}

/// Let Tab/Enter/Esc work in the settings window (called from the message loop).
pub fn is_dialog_message(msg: &MSG) -> bool {
    let h = SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.hwnd));
    match h {
        Some(h) => unsafe { IsDialogMessageW(h, msg).as_bool() },
        None => false,
    }
}

fn item(id: i32) -> HWND {
    let h = SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.hwnd)).unwrap_or_default();
    unsafe { GetDlgItem(h, id).unwrap_or_default() }
}

fn text_of(id: i32) -> String {
    let h = item(id);
    unsafe {
        let n = GetWindowTextLengthW(h);
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

fn checked(id: i32) -> bool {
    unsafe { SendMessageW(item(id), BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1 }
}

fn combo_index(id: i32) -> usize {
    unsafe { SendMessageW(item(id), CB_GETCURSEL, WPARAM(0), LPARAM(0)).0.max(0) as usize }
}

fn combo_text(id: i32) -> String {
    let h = item(id);
    unsafe {
        let i = SendMessageW(h, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        if i < 0 {
            return String::new();
        }
        let n = SendMessageW(h, CB_GETLBTEXTLEN, WPARAM(i as usize), LPARAM(0)).0.max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        SendMessageW(h, CB_GETLBTEXT, WPARAM(i as usize), LPARAM(buf.as_mut_ptr() as isize));
        String::from_utf16_lossy(&buf[..n])
    }
}

/// Build a config from the controls (via the normal parser, so validation is identical).
fn gather() -> Config {
    let lines = |id: i32| text_of(id).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    let edge = if combo_index(ID_EDGE) == 1 { "bottom" } else { "top" };
    let buttons = if combo_index(ID_BUTTONS) == 1 { "icons" } else { "full" };
    let align = ["left", "center", "right"][combo_index(ID_ALIGN).min(2)];
    let text = format!(
        "workspaces = {}\n[keys]\nswitch = {}\nmove = {}\npin = {}\noverview = {}\n[bar]\nenabled = {}\nposition = {}\nbuttons = {}\nalign = {}\nhide_taskbar = {}\ntray = {}\n[pinned_apps]\n{}\n[rules]\n{}\n",
        combo_index(ID_WORKSPACES) + 1,
        combo_text(ID_SWITCH),
        combo_text(ID_MOVE),
        text_of(ID_PIN).trim(),
        text_of(ID_OVERVIEW).trim(),
        checked(ID_BAR),
        edge,
        buttons,
        align,
        checked(ID_HIDE_TASKBAR),
        checked(ID_TRAY),
        lines(ID_PINNED),
        lines(ID_RULES),
    );
    config::parse(&text)
}

fn message(owner: HWND, text: &str, warn: bool) {
    let w = wide(text);
    unsafe {
        MessageBoxW(owner, PCWSTR(w.as_ptr()), w!("LastDesk settings"), MB_OK | if warn { MB_ICONWARNING } else { MB_ICONINFORMATION });
    }
}

fn save(hwnd: HWND) {
    let cfg = gather();
    if !cfg.errors.is_empty() {
        let list: Vec<String> = cfg.errors.iter().map(|e| e.split_once(": ").map(|(_, v)| v.to_string()).unwrap_or(e.clone())).collect();
        message(hwnd, &format!("These entries aren't valid, nothing was saved:\n\n{}", list.join("\n")), true);
        return;
    }
    if let Err(e) = config::save(&cfg) {
        message(hwnd, &format!("Couldn't write the config file:\n{e}"), true);
        return;
    }
    let problems = with_app(|app| app.apply_config(cfg)).unwrap_or_default();
    if !problems.is_empty() {
        message(hwnd, &format!("Saved, with problems:\n\n{}", problems.join("\n")), true);
    }
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

pub fn open() {
    if let Some(h) = SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.hwnd)) {
        unsafe {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    let Some(cfg) = with_app(|app| app.config.clone()) else { return };
    unsafe {
        // Size for the monitor under the mouse.
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let hm = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let sc = dx as f32 / 96.0;
        let s = |v: i32| (v as f32 * sc).round() as i32;
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(hm, &mut mi);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let (cw, ch) = (s(520), s(640));
        let mut r = RECT { left: 0, top: 0, right: cw, bottom: ch };
        let _ = AdjustWindowRectEx(&mut r, style, false, WS_EX_DLGMODALFRAME);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let wa = mi.rcWork;
        let x = wa.left + ((wa.right - wa.left) - ww) / 2;
        let y = wa.top + ((wa.bottom - wa.top) - wh) / 2;

        let ctx = activation_context();
        let mut cookie = 0usize;
        if let Some(c) = ctx {
            let _ = ActivateActCtx(c, &mut cookie);
        }

        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let Ok(hwnd) = CreateWindowExW(WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT, CLASS, w!("LastDesk settings"), style, x, y, ww, wh, None, None, hinst, None) else {
            if ctx.is_some() {
                let _ = DeactivateActCtx(0, cookie);
            }
            return;
        };
        let face = wide("Segoe UI");
        let mk_font = |pt: i32, weight: i32| {
            CreateFontW(-(pt as f32 * dx as f32 / 72.0).round() as i32, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET.0 as u32, 0, 0, CLEARTYPE_QUALITY.0 as u32, 0, PCWSTR(face.as_ptr()))
        };
        let font = mk_font(9, 400);
        let bold = mk_font(10, 600);
        SETTINGS.with(|st| *st.borrow_mut() = Some(State { hwnd, font, bold }));

        let ctl = |class: PCWSTR, text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: i32, f: HFONT| -> HWND {
            let t = wide(text);
            let c = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(t.as_ptr()),
                WINDOW_STYLE(style) | WS_CHILD | WS_VISIBLE,
                s(x), s(y), s(w), s(h),
                hwnd,
                HMENU(id as isize as *mut _),
                hinst,
                None,
            )
            .unwrap_or_default();
            SendMessageW(c, WM_SETFONT, WPARAM(f.0 as usize), LPARAM(1));
            c
        };
        let label = |text: &str, y: i32| {
            ctl(w!("STATIC"), text, 0, 24, y + 4, 200, 20, -1, font);
        };
        let header = |text: &str, y: i32| {
            ctl(w!("STATIC"), text, 0, 16, y, 480, 22, -1, bold);
        };
        let combo = |id: i32, items: &[&str], sel: usize, y: i32, w: i32| {
            let c = ctl(w!("COMBOBOX"), "", (CBS_DROPDOWNLIST as u32) | WS_TABSTOP.0 | WS_VSCROLL.0, 230, y, w, 200, id, font);
            for it in items {
                let t = wide(it);
                SendMessageW(c, CB_ADDSTRING, WPARAM(0), LPARAM(t.as_ptr() as isize));
            }
            SendMessageW(c, CB_SETCURSEL, WPARAM(sel), LPARAM(0));
        };
        let check = |id: i32, text: &str, on: bool, y: i32| {
            let c = ctl(w!("BUTTON"), text, (BS_AUTOCHECKBOX as u32) | WS_TABSTOP.0, 24, y, 470, 22, id, font);
            SendMessageW(c, BM_SETCHECK, WPARAM(on as usize), LPARAM(0));
        };
        let edit = |id: i32, text: &str, y: i32, w: i32, h: i32, multi: bool| {
            let mut st = WS_BORDER.0 | WS_TABSTOP.0 | (ES_AUTOHSCROLL as u32);
            if multi {
                st |= (ES_MULTILINE as u32) | (ES_AUTOVSCROLL as u32) | (ES_WANTRETURN as u32) | WS_VSCROLL.0;
            }
            let x = if multi { 24 } else { 230 };
            ctl(w!("EDIT"), text, st, x, y, w, h, id, font);
        };
        let with_current = |choices: &[&str], current: &str| -> (Vec<String>, usize) {
            let mut v: Vec<String> = choices.iter().map(|s| s.to_string()).collect();
            let i = match v.iter().position(|c| c == current) {
                Some(i) => i,
                None => {
                    v.push(current.to_string());
                    v.len() - 1
                }
            };
            (v, i)
        };

        let mut y = 14;
        header("Workspaces", y);
        y += 28;
        label("Workspaces per monitor", y);
        let nums: Vec<String> = (1..=9).map(|n| n.to_string()).collect();
        combo(ID_WORKSPACES, &nums.iter().map(|s| s.as_str()).collect::<Vec<_>>(), cfg.workspaces - 1, y, 80);
        y += 40;

        header("Shortcuts", y);
        y += 28;
        label("Switch workspace  (+ 1…N)", y);
        let (sw, si) = with_current(SWITCH_CHOICES, &cfg.switch_text);
        combo(ID_SWITCH, &sw.iter().map(|s| s.as_str()).collect::<Vec<_>>(), si, y, 160);
        y += 32;
        label("Move window to workspace  (+ 1…N)", y);
        let (mv, mi_) = with_current(MOVE_CHOICES, &cfg.move_text);
        combo(ID_MOVE, &mv.iter().map(|s| s.as_str()).collect::<Vec<_>>(), mi_, y, 160);
        y += 32;
        label("Pin window", y);
        edit(ID_PIN, &cfg.pin.text, y, 160, 24, false);
        y += 32;
        label("Overview", y);
        edit(ID_OVERVIEW, &cfg.overview.text, y, 160, 24, false);
        y += 40;

        header("Bar", y);
        y += 28;
        check(ID_BAR, "Show the LastDesk bar", cfg.bar, y);
        y += 28;
        label("Position", y);
        combo(ID_EDGE, &["Top", "Bottom"], if cfg.bar_edge == Edge::Bottom { 1 } else { 0 }, y, 160);
        y += 32;
        label("Window buttons", y);
        combo(ID_BUTTONS, &["Icon and title", "Icon only"], cfg.icons_only as usize, y, 160);
        y += 32;
        label("Window buttons alignment", y);
        let al = match cfg.align {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
        };
        combo(ID_ALIGN, &["Left", "Center", "Right"], al, y, 160);
        y += 34;
        check(ID_HIDE_TASKBAR, "Hide the Windows taskbar (the bar takes its place)", cfg.hide_taskbar, y);
        y += 26;
        check(ID_TRAY, "Show tray icons in the bar", cfg.tray, y);
        y += 38;

        header("Pinned apps  (shown on every workspace, one exe name per line)", y);
        y += 26;
        edit(ID_PINNED, &cfg.pinned_apps.join("\r\n"), y, 470, 54, true);
        y += 66;
        header("App rules  (app = monitor, workspace   e.g.  slack = 2, 3)", y);
        y += 26;
        let rules: Vec<String> = cfg.rules.iter().map(|r| format!("{} = {}, {}", r.app, r.monitor + 1, r.workspace + 1)).collect();
        edit(ID_RULES, &rules.join("\r\n"), y, 470, 54, true);

        let by = 640 - 46;
        ctl(w!("BUTTON"), "Open config file", WS_TABSTOP.0, 24, by, 130, 30, ID_OPEN, font);
        ctl(w!("BUTTON"), "Cancel", WS_TABSTOP.0, 300, by, 90, 30, ID_CANCEL, font);
        ctl(w!("BUTTON"), "Save", (BS_DEFPUSHBUTTON as u32) | WS_TABSTOP.0, 404, by, 90, 30, ID_SAVE, font);

        if let Some(c) = ctx {
            let _ = DeactivateActCtx(0, cookie);
            let _ = c;
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

pub unsafe extern "system" fn settings_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            match (wp.0 & 0xFFFF) as i32 {
                ID_SAVE => save(hwnd),
                ID_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                }
                ID_OPEN => {
                    let p = wide(&config::path().display().to_string());
                    ShellExecuteW(None, w!("open"), w!("notepad.exe"), PCWSTR(p.as_ptr()), None, SW_SHOWNORMAL);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(st) = SETTINGS.with(|s| s.borrow_mut().take()) {
                let _ = DeleteObject(st.font);
                let _ = DeleteObject(st.bold);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
