// Created by Peter @sHTiF Stefcek
//! Settings window: dark, applies every change immediately (like Windows Settings).
//!
//! Toggles, segmented pickers and buttons are drawn by LastDesk ("LastDeskCtl" class) so
//! they match the bar; text fields are native edits coloured dark. Toggle/picker changes
//! apply on click, text fields when they lose focus (and on close). Every apply goes through
//! the normal config parser: invalid input is reported in the status line and not saved.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Align, Config, Edge};
use crate::{ui, with_app};

pub const CLASS: PCWSTR = w!("LastDeskSettings");
pub const CTL_CLASS: PCWSTR = w!("LastDeskCtl");

const ID_OPEN: i32 = 100;
const ID_CLOSE: i32 = 2; // IDCANCEL: Esc closes
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
const ID_STATUS: i32 = 114;
const ID_NEXT: i32 = 115;
const ID_PREV: i32 = 116;
const ID_ANIM: i32 = 117;
const ID_TAILSCALE: i32 = 118;
const ID_WINKEY: i32 = 119;
const ID_MEMORY: i32 = 120;
/// Windows tweaks: ID_TWEAK + index into tweaks::ALL.
const ID_TWEAK: i32 = 130;
/// Section headers: ID_SECTION + section index.
const ID_SECTION: i32 = 150;

const EN_KILLFOCUS: u32 = 0x0200;
const WM_MOUSELEAVE: u32 = 0x02A3;

const BG: COLORREF = ui::rgb(0x1E, 0x1F, 0x22);
const FIELD: COLORREF = ui::rgb(0x2A, 0x2B, 0x30);
const FIELD_HOVER: COLORREF = ui::rgb(0x34, 0x35, 0x3B);
const LABEL: COLORREF = ui::rgb(0xD8, 0xDA, 0xDE);
const ERROR: COLORREF = ui::rgb(0xF0, 0x6A, 0x5A);
const OK: COLORREF = ui::rgb(0x6A, 0xD0, 0x8A);

/// Value shown for a choice, value written to the config.
const SWITCH_CHOICES: &[(&str, &str)] = &[("Alt", "alt"), ("Win+Alt", "win+alt"), ("Ctrl+Alt", "ctrl+alt")];
const MOVE_CHOICES: &[(&str, &str)] = &[("Alt+Shift", "alt+shift"), ("Win+Shift", "win+shift"), ("Ctrl+Alt+Shift", "ctrl+alt+shift")];

enum Kind {
    Toggle { label: String, on: bool },
    Seg { options: Vec<String>, values: Vec<String>, sel: usize },
    Button { label: String, accent: bool },
}

struct Ctl {
    kind: Kind,
    scale: f32,
    hover: Option<usize>,
    tracking: bool,
    /// The left button went down on this control (a click needs press and release here).
    pressed: bool,
}

struct State {
    hwnd: HWND,
    font: HFONT,
    bold: HFONT,
    bg: HBRUSH,
    field: HBRUSH,
    status: (String, bool),
    /// Not editable in the dialog; carried through so saving keeps it.
    launch_ignore: Vec<String>,
    /// Launchers when the dialog opened (fallback; normally read live from the app).
    launchers: Vec<crate::config::Launcher>,
}

thread_local! {
    static SETTINGS: RefCell<Option<State>> = const { RefCell::new(None) };
    static CTLS: RefCell<HashMap<isize, Ctl>> = RefCell::new(HashMap::new());
    /// (content height, current scroll position) when the window is taller than the screen.
    static SCROLL: std::cell::Cell<(i32, i32)> = const { std::cell::Cell::new((0, 0)) };
    /// Which sections are open (by index). Kept while LastDesk runs, so reopening Settings
    /// shows the same sections open.
    static OPEN: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    static LAYOUT: RefCell<Option<Layout>> = const { RefCell::new(None) };
}

/// Scroll the settings window's contents to `pos` (clamped), moving the controls.
unsafe fn scroll_to(hwnd: HWND, pos: i32) {
    let (content, cur) = SCROLL.with(|s| s.get());
    let mut cr = RECT::default();
    let _ = GetClientRect(hwnd, &mut cr);
    let page = cr.bottom - cr.top;
    let new = pos.clamp(0, (content - page).max(0));
    if new == cur {
        return;
    }
    SCROLL.with(|s| s.set((content, new)));
    let _ = ScrollWindowEx(hwnd, 0, cur - new, None, None, None, None, SW_SCROLLCHILDREN | SW_INVALIDATE | SW_ERASE);
    let si = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_POS, nPos: new, ..Default::default() };
    windows::Win32::UI::Controls::SetScrollInfo(hwnd, SB_VERT, &si, true);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn dlg() -> Option<HWND> {
    SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.hwnd))
}

/// Let Tab/Esc work in the settings window (called from the message loop).
pub fn is_dialog_message(msg: &MSG) -> bool {
    match dlg() {
        Some(h) => unsafe { IsDialogMessageW(h, msg).as_bool() },
        None => false,
    }
}

fn item(id: i32) -> HWND {
    unsafe { GetDlgItem(dlg().unwrap_or_default(), id).unwrap_or_default() }
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

fn toggle_on(id: i32) -> bool {
    let k = item(id).0 as isize;
    CTLS.with(|c| matches!(c.borrow().get(&k), Some(Ctl { kind: Kind::Toggle { on: true, .. }, .. })))
}

/// Set a toggle's state from code (and repaint it).
fn set_toggle(id: i32, value: bool) {
    let h = item(id);
    let k = h.0 as isize;
    CTLS.with(|c| {
        if let Some(Ctl { kind: Kind::Toggle { on, .. }, .. }) = c.borrow_mut().get_mut(&k) {
            *on = value;
        }
    });
    unsafe {
        let _ = InvalidateRect(h, None, false);
    }
}

fn seg_value(id: i32) -> String {
    let k = item(id).0 as isize;
    CTLS.with(|c| match c.borrow().get(&k) {
        Some(Ctl { kind: Kind::Seg { values, sel, .. }, .. }) => values.get(*sel).cloned().unwrap_or_default(),
        _ => String::new(),
    })
}

/// Build a config from the controls via the normal parser (identical validation).
fn gather() -> Config {
    let lines = |id: i32| text_of(id).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    let text = format!(
        "workspaces = {}\nanimation = {}\nlaunch_ignore = {}\n[keys]\nswitch = {}\nmove = {}\npin = {}\noverview = {}\nnext = {}\nprev = {}\n[bar]\nenabled = {}\nposition = {}\nbuttons = {}\nalign = {}\nhide_taskbar = {}\ntray = {}\nmemory = {}\ntailscale = {}\nwin_key_start = {}\n[pinned_apps]\n{}\n[launchers]\n{}\n[rules]\n{}\n",
        seg_value(ID_WORKSPACES),
        seg_value(ID_ANIM),
        SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.launch_ignore.join(", ")).unwrap_or_default()),
        seg_value(ID_SWITCH),
        seg_value(ID_MOVE),
        text_of(ID_PIN).trim(),
        text_of(ID_OVERVIEW).trim(),
        text_of(ID_NEXT).trim(),
        text_of(ID_PREV).trim(),
        toggle_on(ID_BAR),
        seg_value(ID_EDGE),
        seg_value(ID_BUTTONS),
        seg_value(ID_ALIGN),
        toggle_on(ID_HIDE_TASKBAR),
        toggle_on(ID_TRAY),
        toggle_on(ID_MEMORY),
        toggle_on(ID_TAILSCALE),
        toggle_on(ID_WINKEY),
        lines(ID_PINNED),
        // Not edited here (pin/unpin from the bar): keep the app's current list.
        with_app(|a| a.config.launchers.clone())
            .or_else(|| SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.launchers.clone())))
            .unwrap_or_default()
            .iter()
            .map(|l| format!("{} = {}", l.name, l.target))
            .collect::<Vec<_>>()
            .join("\n"),
        lines(ID_RULES),
    );
    config::parse(&text)
}

fn set_status(text: String, ok: bool) {
    SETTINGS.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.status = (text.clone(), ok);
        }
    });
    let w = wide(&text);
    unsafe {
        let st = item(ID_STATUS);
        let _ = SetWindowTextW(st, PCWSTR(w.as_ptr()));
        let _ = InvalidateRect(st, None, true);
    }
}

/// Validate, save and apply what the controls say now.
fn apply() {
    let cfg = gather();
    if !cfg.errors.is_empty() {
        let list: Vec<String> = cfg.errors.iter().map(|e| e.split_once(": ").map(|(_, v)| v.to_string()).unwrap_or(e.clone())).collect();
        set_status(format!("Not saved, invalid: {}", list.join(", ")), false);
        return;
    }
    if let Err(e) = config::save(&cfg) {
        set_status(format!("Couldn't write the config file: {e}"), false);
        return;
    }
    // Only apply if something actually changed (avoids rebuilding the bar on every focus change).
    let changed = with_app(|app| config::render(&app.config) != config::render(&cfg)).unwrap_or(true);
    if changed {
        let problems = with_app(|app| app.apply_config(cfg)).unwrap_or_default();
        if !problems.is_empty() {
            set_status(format!("Saved, but: {}", problems.join(" ")), false);
            return;
        }
    }
    set_status("Saved ✓".into(), true);
}

// ---------- custom controls ----------

unsafe fn paint_ctl(hwnd: HWND, hdc: HDC, c: &Ctl) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    let s = |v: f32| (v * c.scale).round() as i32;
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old = SelectObject(mem, bmp);
    let brush = CreateSolidBrush(BG);
    FillRect(mem, &rc, brush);
    let _ = DeleteObject(brush);
    let f = ui::font(s(12.5), false);
    let fb = ui::font(s(12.5), true);
    match &c.kind {
        Kind::Toggle { label, on } => {
            let mut lr = RECT { left: 0, top: 0, right: w - s(56.0), bottom: h };
            ui::draw_text_left(mem, label, &mut lr, LABEL, f);
            let tw = s(40.0);
            let th = s(20.0);
            let track = RECT { left: w - tw - s(2.0), top: (h - th) / 2, right: w - s(2.0), bottom: (h + th) / 2 };
            let col = if *on { ui::ACCENT } else if c.hover.is_some() { FIELD_HOVER } else { ui::rgb(0x45, 0x47, 0x4E) };
            ui::fill_round(mem, track, th, col);
            let k = th - s(6.0);
            let kx = if *on { track.right - s(3.0) - k } else { track.left + s(3.0) };
            ui::fill_round(mem, RECT { left: kx, top: track.top + s(3.0), right: kx + k, bottom: track.top + s(3.0) + k }, k, ui::TEXT);
        }
        Kind::Seg { options, sel, .. } => {
            ui::fill_round(mem, rc, s(8.0), FIELD);
            let n = options.len().max(1) as i32;
            let pad = s(3.0);
            let cw = (w - pad * 2) / n;
            for (i, o) in options.iter().enumerate() {
                let r = RECT { left: pad + i as i32 * cw, top: pad, right: pad + (i as i32 + 1) * cw, bottom: h - pad };
                if i == *sel {
                    ui::fill_round(mem, r, s(6.0), ui::ACCENT);
                } else if c.hover == Some(i) {
                    ui::fill_round(mem, r, s(6.0), FIELD_HOVER);
                }
                let mut tr = r;
                ui::draw_text(mem, o, &mut tr, if i == *sel { ui::TEXT } else { LABEL }, if i == *sel { fb } else { f });
            }
        }
        Kind::Button { label, accent } => {
            let col = match (accent, c.hover.is_some()) {
                (true, false) => ui::ACCENT,
                (true, true) => ui::rgb(0x4A, 0x8A, 0xFF),
                (false, false) => FIELD,
                (false, true) => FIELD_HOVER,
            };
            ui::fill_round(mem, rc, s(8.0), col);
            let mut tr = rc;
            ui::draw_text(mem, label, &mut tr, ui::TEXT, f);
        }
    }
    let _ = DeleteObject(f);
    let _ = DeleteObject(fb);
    let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem);
}

fn seg_index_at(hwnd: HWND, n: usize, x: i32, scale: f32) -> usize {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let pad = (3.0 * scale).round() as i32;
    let cw = ((rc.right - pad * 2) / n.max(1) as i32).max(1);
    (((x - pad).max(0) / cw) as usize).min(n.saturating_sub(1))
}

pub unsafe extern "system" fn ctl_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let key = hwnd.0 as isize;
    let x = (lp.0 & 0xFFFF) as i16 as i32;
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            CTLS.with(|c| {
                if let Some(ctl) = c.borrow().get(&key) {
                    paint_ctl(hwnd, hdc, ctl);
                }
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let changed = CTLS.with(|c| {
                let mut c = c.borrow_mut();
                let Some(ctl) = c.get_mut(&key) else { return false };
                if !ctl.tracking {
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                    ctl.tracking = true;
                }
                let h = match &ctl.kind {
                    Kind::Seg { options, .. } => Some(seg_index_at(hwnd, options.len(), x, ctl.scale)),
                    _ => Some(0),
                };
                let ch = h != ctl.hover;
                ctl.hover = h;
                ch
            });
            if changed {
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            CTLS.with(|c| {
                if let Some(ctl) = c.borrow_mut().get_mut(&key) {
                    ctl.hover = None;
                    ctl.tracking = false;
                    ctl.pressed = false; // dragged off: releasing elsewhere isn't a click
                }
            });
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // A click counts only if it also started here (like a normal button): when a
            // section opens under the mouse, the button-up lands on a control that just
            // appeared there and must not flip it.
            CTLS.with(|c| {
                if let Some(ctl) = c.borrow_mut().get_mut(&key) {
                    ctl.pressed = true;
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let changed = CTLS.with(|c| {
                let mut c = c.borrow_mut();
                let Some(ctl) = c.get_mut(&key) else { return false };
                if !std::mem::replace(&mut ctl.pressed, false) {
                    return false; // the press began somewhere else
                }
                let scale = ctl.scale;
                match &mut ctl.kind {
                    Kind::Toggle { on, .. } => {
                        *on = !*on;
                        true
                    }
                    Kind::Seg { options, sel, .. } => {
                        let i = seg_index_at(hwnd, options.len(), x, scale);
                        let ch = i != *sel;
                        *sel = i;
                        ch
                    }
                    Kind::Button { .. } => true,
                }
            });
            if changed {
                let _ = InvalidateRect(hwnd, None, false);
                // Tell the dialog (outside the CTLS borrow).
                let id = GetDlgCtrlID(hwnd);
                if let Ok(parent) = GetParent(hwnd) {
                    SendMessageW(parent, WM_COMMAND, WPARAM(id as usize), LPARAM(hwnd.0 as isize));
                }
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            CTLS.with(|c| c.borrow_mut().remove(&key));
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ---------- window ----------

pub fn open() {
    if let Some(h) = dlg() {
        unsafe {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    let Some(cfg) = with_app(|app| app.config.clone()) else { return };
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let hm = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let sc = dx as f32 / 96.0;
        let s = |v: i32| (v as f32 * sc).round() as i32;
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(hm, &mut mi);

        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        // Created at a provisional size; resized to the content at the end.
        let Ok(hwnd) = CreateWindowExW(WS_EX_CONTROLPARENT, CLASS, w!("LastDesk settings"), style, 0, 0, s(560), s(700), None, None, hinst, None) else { return };
        // Dark title bar (DWMWA_USE_IMMERSIVE_DARK_MODE = 20).
        let dark: BOOL = TRUE;
        let _ = DwmSetWindowAttribute(hwnd, windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(20), &dark as *const BOOL as *const _, 4);

        let face = wide("Segoe UI");
        let mk_font = |px: f32, weight: i32| {
            CreateFontW(-(px * sc).round() as i32, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET.0 as u32, 0, 0, CLEARTYPE_QUALITY.0 as u32, 0, PCWSTR(face.as_ptr()))
        };
        let font = mk_font(13.0, 400);
        let bold = mk_font(14.5, 600);
        SETTINGS.with(|st| {
            *st.borrow_mut() = Some(State { hwnd, font, bold, bg: CreateSolidBrush(BG), field: CreateSolidBrush(FIELD), status: (String::new(), true), launch_ignore: cfg.launch_ignore.clone(), launchers: cfg.launchers.clone() })
        });

        let native = |class: PCWSTR, text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: i32, f: HFONT| -> HWND {
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
        let custom = |kind: Kind, x: i32, y: i32, w: i32, h: i32, id: i32| {
            let c = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CTL_CLASS,
                w!(""),
                WS_CHILD | WS_VISIBLE,
                s(x), s(y), s(w), s(h),
                hwnd,
                HMENU(id as isize as *mut _),
                hinst,
                None,
            )
            .unwrap_or_default();
            CTLS.with(|m| m.borrow_mut().insert(c.0 as isize, Ctl { kind, scale: sc, hover: None, tracking: false, pressed: false }));
            let _ = InvalidateRect(c, None, false);
        };
        let label = |text: &str, y: i32| {
            native(w!("STATIC"), text, 0, 28, y + 7, 200, 20, -1, font);
        };
        // Section headers are clickable (SS_NOTIFY) and fold their section open / closed.
        let heads: RefCell<Vec<(HWND, i32, String)>> = RefCell::new(Vec::new());
        let header = |text: &str, y: i32| {
            let i = heads.borrow().len();
            let open = OPEN.with(|o| o.borrow().get(i).copied().unwrap_or(false));
            // SS_NOTIFY (clickable) | SS_CENTERIMAGE (text centred vertically in the band).
            let h = native(w!("STATIC"), &section_title(text, open), 0x0100 | 0x0200, 12, y, 536, 30, ID_SECTION + i as i32, bold);
            heads.borrow_mut().push((h, s(y), text.to_string()));
        };
        let seg = |id: i32, choices: &[(&str, &str)], current: &str, y: i32| {
            let mut options: Vec<String> = choices.iter().map(|c| c.0.to_string()).collect();
            let mut values: Vec<String> = choices.iter().map(|c| c.1.to_string()).collect();
            let sel = match values.iter().position(|v| v == current) {
                Some(i) => i,
                None => {
                    options.push(current.to_string());
                    values.push(current.to_string());
                    values.len() - 1
                }
            };
            custom(Kind::Seg { options, values, sel }, 232, y, 300, 32, id);
        };
        let toggle = |id: i32, text: &str, on: bool, y: i32| {
            custom(Kind::Toggle { label: text.to_string(), on }, 28, y, 504, 30, id);
        };
        let edit = |id: i32, text: &str, x: i32, y: i32, w: i32, h: i32, multi: bool| {
            let mut st = WS_TABSTOP.0 | (ES_AUTOHSCROLL as u32) | WS_BORDER.0;
            if multi {
                st |= (ES_MULTILINE as u32) | (ES_AUTOVSCROLL as u32) | (ES_WANTRETURN as u32) | WS_VSCROLL.0;
            }
            let e = native(w!("EDIT"), text, st, x, y, w, h, id, font);
            let _ = SetWindowTheme(e, w!("DarkMode_Explorer"), None);
        };

        let mut y = 18;
        header("Workspaces", y);
        y += 32;
        label("Workspaces per monitor", y);
        let nums: Vec<(String, String)> = (1..=9).map(|n| (n.to_string(), n.to_string())).collect();
        let nums_ref: Vec<(&str, &str)> = nums.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        seg(ID_WORKSPACES, &nums_ref, &cfg.workspaces.to_string(), y);
        y += 40;
        label("Switch animation", y);
        let anim_sel = match config::anim_name(cfg.animation).as_str() {
            n @ ("off" | "fast" | "normal" | "slow") => n.to_string(),
            _ => "normal".to_string(),
        };
        seg(ID_ANIM, &[("Off", "off"), ("Fast", "fast"), ("Normal", "normal"), ("Slow", "slow")], &anim_sel, y);
        y += 50;

        header("Shortcuts", y);
        y += 32;
        label("Switch workspace  (+ 1…N)", y);
        seg(ID_SWITCH, SWITCH_CHOICES, &cfg.switch_text, y);
        y += 40;
        label("Move window  (+ 1…N)", y);
        seg(ID_MOVE, MOVE_CHOICES, &cfg.move_text, y);
        y += 40;
        label("Pin window", y);
        edit(ID_PIN, &cfg.pin.text, 232, y + 3, 300, 26, false);
        y += 38;
        label("Overview", y);
        edit(ID_OVERVIEW, &cfg.overview.text, 232, y + 3, 300, 26, false);
        y += 38;
        label("Next workspace", y);
        edit(ID_NEXT, &cfg.next.text, 232, y + 3, 300, 26, false);
        y += 38;
        label("Previous workspace", y);
        edit(ID_PREV, &cfg.prev.text, 232, y + 3, 300, 26, false);
        y += 50;

        header("Bar", y);
        y += 34;
        toggle(ID_BAR, "Show the LastDesk bar", cfg.bar, y);
        y += 38;
        label("Position", y);
        seg(ID_EDGE, &[("Top", "top"), ("Bottom", "bottom")], if cfg.bar_edge == Edge::Bottom { "bottom" } else { "top" }, y);
        y += 40;
        label("Window buttons", y);
        seg(ID_BUTTONS, &[("Icon + title", "full"), ("Icon only", "icons")], if cfg.icons_only { "icons" } else { "full" }, y);
        y += 40;
        label("Buttons alignment", y);
        let al = match cfg.align {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        };
        seg(ID_ALIGN, &[("Left", "left"), ("Center", "center"), ("Right", "right")], al, y);
        y += 44;
        toggle(ID_HIDE_TASKBAR, "Hide the Windows taskbar (the bar takes its place)", cfg.hide_taskbar, y);
        y += 36;
        toggle(ID_TRAY, "Show tray icons in the bar", cfg.tray, y);
        y += 36;
        toggle(ID_MEMORY, "Show memory use in the bar", cfg.memory, y);
        y += 48;

        header("Integrations", y);
        y += 34;
        toggle(ID_WINKEY, "Win key opens Start on the monitor you're on", cfg.win_key_start, y);
        y += 36;
        // Only offered where Tailscale is installed.
        if crate::tailscale::installed() {
            toggle(ID_TAILSCALE, "Tailscale in the bar (status, connect, exit nodes)", cfg.tailscale, y);
            y += 36;
        }
        y += 12;

        // Windows tweaks: show and change the system's own settings (not stored in config).
        header("Windows tweaks", y);
        y += 26;
        native(w!("STATIC"), "Applied to Windows right away. Most ask for admin permission.", 0, 28, y, 504, 20, -1, font);
        y += 26;
        for (i, t) in crate::tweaks::ALL.iter().enumerate() {
            toggle(ID_TWEAK + i as i32, t.label(), t.is_on(), y);
            y += 36;
        }
        y += 12;

        header("Pinned apps", y);
        y += 26;
        native(w!("STATIC"), "Shown on every workspace. One exe name per line, e.g. spotify", 0, 28, y, 504, 20, -1, font);
        y += 24;
        edit(ID_PINNED, &cfg.pinned_apps.join("\r\n"), 28, y, 504, 64, true);
        y += 80;
        header("App rules", y);
        y += 26;
        native(w!("STATIC"), "app = monitor, workspace    e.g.  slack = 2, 3   (monitor 1 = leftmost)", 0, 28, y, 504, 20, -1, font);
        y += 24;
        let rules: Vec<String> = cfg.rules.iter().map(|r| format!("{} = {}, {}", r.app, r.monitor + 1, r.workspace + 1)).collect();
        edit(ID_RULES, &rules.join("\r\n"), 28, y, 504, 64, true);
        y += 80;

        let footer_y = y;
        native(w!("STATIC"), "Changes are saved and applied as you make them.", 0, 28, y, 504, 20, ID_STATUS, font);
        y += 30;
        custom(Kind::Button { label: "Open config file".into(), accent: false }, 28, y, 150, 34, ID_OPEN);
        custom(Kind::Button { label: "Close".into(), accent: true }, 432, y, 100, 34, ID_CLOSE);
        y += 34 + 20;

        // Which controls belong to which section (by where they were placed), so sections
        // can fold: every child below a header and above the next one is that section's.
        let heads = heads.into_inner();
        let mut children: Vec<HWND> = Vec::new();
        unsafe extern "system" fn collect(h: HWND, l: LPARAM) -> BOOL {
            (*(l.0 as *mut Vec<HWND>)).push(h);
            TRUE
        }
        let _ = EnumChildWindows(hwnd, Some(collect), LPARAM(&mut children as *mut _ as isize));
        let top_of = |c: HWND| {
            let mut r = RECT::default();
            let _ = GetWindowRect(c, &mut r);
            let mut p = POINT { x: r.left, y: r.top };
            let _ = ScreenToClient(hwnd, &mut p);
            p.y
        };
        let fy = s(footer_y);
        let mut layout = Layout { sections: Vec::new(), footer: Vec::new(), footer_h: s(y) - fy, top: heads.first().map(|h| h.1).unwrap_or(0), scale: sc, style };
        for (i, (h, hy, title)) in heads.iter().enumerate() {
            let next = heads.get(i + 1).map(|n| n.1).unwrap_or(fy);
            layout.sections.push(Section { header: *h, title: title.clone(), items: Vec::new(), full: next - hy });
        }
        for c in children {
            if heads.iter().any(|h| h.0 == c) || GetParent(c).unwrap_or_default() != hwnd {
                continue;
            }
            let cy = top_of(c);
            if cy >= fy {
                layout.footer.push((c, cy - fy));
            } else if let Some(i) = heads.iter().rposition(|h| h.1 <= cy) {
                layout.sections[i].items.push((c, cy - heads[i].1));
            }
        }
        LAYOUT.with(|l| *l.borrow_mut() = Some(layout));
        relayout(hwnd, Some(mi.rcWork));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// "▾ Bar" (open) / "▸ Bar" (closed), indented inside the header band.
fn section_title(title: &str, open: bool) -> String {
    format!("   {}   {title}", if open { "\u{25BE}" } else { "\u{25B8}" })
}

fn section_is_open(i: usize) -> bool {
    OPEN.with(|o| o.borrow().get(i).copied().unwrap_or(false))
}

struct Section {
    header: HWND,
    title: String,
    /// Controls and their offset below the header (pixels).
    items: Vec<(HWND, i32)>,
    /// Height of the section when open.
    full: i32,
}

struct Layout {
    sections: Vec<Section>,
    footer: Vec<(HWND, i32)>,
    footer_h: i32,
    top: i32,
    scale: f32,
    style: WINDOW_STYLE,
}

/// Place the sections (open ones with their controls, closed ones as just the header), then
/// size the window to the content: a scroll bar when it's taller than the screen. `center`:
/// first placement, centred in this work area; otherwise the window keeps its position.
unsafe fn relayout(hwnd: HWND, center: Option<RECT>) {
    // Positions below are absolute: undo any scrolling first.
    let (_, pos) = SCROLL.with(|s| s.get());
    if pos != 0 {
        let _ = ScrollWindowEx(hwnd, 0, pos, None, None, None, None, SW_SCROLLCHILDREN);
    }
    let Some((content, sc, style)) = LAYOUT.with(|l| {
        let l = l.borrow();
        let l = l.as_ref()?;
        let collapsed = (36.0 * l.scale).round() as i32;
        let mut y = l.top;
        let place = |h: HWND, y: i32, show: bool| {
            let mut r = RECT::default();
            let _ = GetWindowRect(h, &mut r);
            let mut p = POINT { x: r.left, y: r.top };
            let _ = ScreenToClient(hwnd, &mut p);
            let _ = SetWindowPos(h, None, p.x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | if show { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW });
        };
        for (i, s) in l.sections.iter().enumerate() {
            let open = OPEN.with(|o| o.borrow().get(i).copied().unwrap_or(false));
            place(s.header, y, true);
            for (c, off) in &s.items {
                place(*c, y + off, open);
            }
            y += if open { s.full } else { collapsed };
        }
        y += (8.0 * l.scale).round() as i32;
        for (c, off) in &l.footer {
            place(*c, y + off, true);
        }
        Some((y + l.footer_h, l.scale, l.style))
    }) else { return };

    // Size the window to the content.
    let hm = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(hm, &mut mi);
    let wa = center.unwrap_or(mi.rcWork);
    let mut r = RECT { left: 0, top: 0, right: (560.0 * sc).round() as i32, bottom: content };
    let _ = AdjustWindowRectEx(&mut r, style, false, WS_EX_CONTROLPARENT);
    let (mut ww, wh) = (r.right - r.left, r.bottom - r.top);
    let avail = wa.bottom - wa.top;
    let fits = wh <= avail;
    // Only flip the scroll bar bit: the other bits (WS_VISIBLE…) must stay as they are.
    let cur_style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
    let new_style = if fits { cur_style & !WS_VSCROLL.0 } else { cur_style | WS_VSCROLL.0 };
    let _ = SetWindowLongPtrW(hwnd, GWL_STYLE, new_style as isize);
    if !fits {
        ww += windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(SM_CXVSCROLL, (96.0 * sc) as u32);
    }
    let h = wh.min(avail);
    let (x, y) = match center {
        Some(_) => (wa.left + ((wa.right - wa.left) - ww) / 2, (wa.top + (avail - h) / 2).max(wa.top)),
        None => {
            let mut cur = RECT::default();
            let _ = GetWindowRect(hwnd, &mut cur);
            // Keep the top where it is, unless the window would run off the bottom.
            (cur.left, cur.top.min(wa.bottom - h).max(wa.top))
        }
    };
    let _ = SetWindowPos(hwnd, None, x, y, ww, h, SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE);
    SCROLL.with(|s| s.set((content, 0)));
    let mut cr = RECT::default();
    let _ = GetClientRect(hwnd, &mut cr);
    let si = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_RANGE | SIF_PAGE | SIF_POS, nMin: 0, nMax: content - 1, nPage: (cr.bottom - cr.top) as u32, nPos: 0, nTrackPos: 0 };
    windows::Win32::UI::Controls::SetScrollInfo(hwnd, SB_VERT, &si, true);
    let _ = InvalidateRect(hwnd, None, true);
}

/// A section header was clicked. Accordion: opening a section closes the one that was open;
/// clicking the open one closes it.
unsafe fn toggle_section(hwnd: HWND, i: usize) {
    let headers: Vec<(HWND, String)> = LAYOUT.with(|l| l.borrow().as_ref().map(|l| l.sections.iter().map(|s| (s.header, s.title.clone())).collect()).unwrap_or_default());
    let opening = !section_is_open(i);
    OPEN.with(|o| {
        let mut o = o.borrow_mut();
        o.clear();
        o.resize(headers.len().max(i + 1), false);
        o[i] = opening;
    });
    for (n, (h, title)) in headers.iter().enumerate() {
        let t = wide(&section_title(title, section_is_open(n)));
        let _ = SetWindowTextW(*h, PCWSTR(t.as_ptr()));
        let _ = InvalidateRect(*h, None, true); // colour depends on open / closed
    }
    relayout(hwnd, None);
}

fn close(hwnd: HWND) {
    // Text fields apply on focus loss; make sure the last edit is applied too.
    apply();
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

pub unsafe extern "system" fn settings_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as i32;
            let code = ((wp.0 >> 16) & 0xFFFF) as u32;
            match id {
                ID_CLOSE => close(hwnd),
                ID_OPEN => {
                    let p = wide(&config::path().display().to_string());
                    ShellExecuteW(None, w!("open"), w!("notepad.exe"), PCWSTR(p.as_ptr()), None, SW_SHOWNORMAL);
                }
                ID_WORKSPACES | ID_ANIM | ID_SWITCH | ID_MOVE | ID_BAR | ID_EDGE | ID_BUTTONS | ID_ALIGN | ID_HIDE_TASKBAR | ID_TRAY | ID_MEMORY | ID_TAILSCALE | ID_WINKEY => apply(),
                ID_PIN | ID_OVERVIEW | ID_NEXT | ID_PREV | ID_PINNED | ID_RULES if code == EN_KILLFOCUS => apply(),
                // STN_CLICKED only (a double-click also sends STN_DBLCLK: don't fold twice).
                i if (ID_SECTION..ID_SECTION + 20).contains(&i) && code == 0 => toggle_section(hwnd, (i - ID_SECTION) as usize),
                i if (ID_TWEAK..ID_TWEAK + crate::tweaks::ALL.len() as i32).contains(&i) => {
                    let t = crate::tweaks::ALL[(i - ID_TWEAK) as usize];
                    let want = toggle_on(i);
                    if !crate::tweaks::set(t, want, hwnd) {
                        // Prompt cancelled or not allowed: show the real state again.
                        set_toggle(i, t.is_on());
                    } else if !t.needs_admin() {
                        set_toggle(i, t.is_on());
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
            let (_, cur) = SCROLL.with(|s| s.get());
            scroll_to(hwnd, cur - delta * 60 / 120 * windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) as i32 / 96);
            LRESULT(0)
        }
        WM_VSCROLL => {
            let (_, cur) = SCROLL.with(|s| s.get());
            let mut cr = RECT::default();
            let _ = GetClientRect(hwnd, &mut cr);
            let page = cr.bottom - cr.top;
            let line = 40 * windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) as i32 / 96;
            let target = match (wp.0 & 0xFFFF) as i32 {
                x if x == SB_LINEUP.0 => cur - line,
                x if x == SB_LINEDOWN.0 => cur + line,
                x if x == SB_PAGEUP.0 => cur - page,
                x if x == SB_PAGEDOWN.0 => cur + page,
                x if x == SB_THUMBTRACK.0 || x == SB_THUMBPOSITION.0 => {
                    let mut si = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_TRACKPOS, ..Default::default() };
                    let _ = GetScrollInfo(hwnd, SB_VERT, &mut si);
                    si.nTrackPos
                }
                x if x == SB_TOP.0 => 0,
                x if x == SB_BOTTOM.0 => i32::MAX,
                _ => cur,
            };
            scroll_to(hwnd, target);
            LRESULT(0)
        }
        // An elevated tweak finished: re-read every tweak's real state.
        crate::tweaks::WM_TWEAK_DONE => {
            for (i, t) in crate::tweaks::ALL.iter().enumerate() {
                set_toggle(ID_TWEAK + i as i32, t.is_on());
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC if (ID_SECTION..ID_SECTION + 20).contains(&GetDlgCtrlID(HWND(lp.0 as *mut _))) => {
            // Section header: a lighter band; the open section's title in the accent colour.
            let hdc = HDC(wp.0 as *mut _);
            let i = (GetDlgCtrlID(HWND(lp.0 as *mut _)) - ID_SECTION) as usize;
            SetTextColor(hdc, if section_is_open(i) { ui::ACCENT } else { ui::TEXT });
            SetBkMode(hdc, OPAQUE);
            SetBkColor(hdc, FIELD);
            LRESULT(SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.field).unwrap_or_default()).0 as isize)
        }
        WM_SETCURSOR if (ID_SECTION..ID_SECTION + 20).contains(&GetDlgCtrlID(HWND(wp.0 as *mut _))) => {
            if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                SetCursor(c);
            }
            LRESULT(1)
        }
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(wp.0 as *mut _);
            let is_status = HWND(lp.0 as *mut _) == item(ID_STATUS);
            let (bg, color) = SETTINGS.with(|s| {
                let s = s.borrow();
                let st = s.as_ref();
                let color = match (is_status, st.map(|s| s.status.clone())) {
                    (true, Some((t, ok))) if !t.is_empty() => if ok { OK } else { ERROR },
                    (true, _) => ui::MUTED,
                    _ => LABEL,
                };
                (st.map(|s| s.bg).unwrap_or_default(), color)
            });
            SetTextColor(hdc, color);
            SetBkMode(hdc, TRANSPARENT);
            SetBkColor(hdc, BG);
            LRESULT(bg.0 as isize)
        }
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wp.0 as *mut _);
            SetTextColor(hdc, ui::TEXT);
            SetBkColor(hdc, FIELD);
            let br = SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.field).unwrap_or_default());
            LRESULT(br.0 as isize)
        }
        WM_ERASEBKGND => {
            let hdc = HDC(wp.0 as *mut _);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let br = SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.bg).unwrap_or_default());
            FillRect(hdc, &rc, br);
            LRESULT(1)
        }
        WM_CLOSE => {
            close(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(st) = SETTINGS.with(|s| s.borrow_mut().take()) {
                let _ = DeleteObject(st.font);
                let _ = DeleteObject(st.bold);
                let _ = DeleteObject(st.bg);
                let _ = DeleteObject(st.field);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
