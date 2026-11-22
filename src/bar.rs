//! LastDesk bar: a thin taskbar on top of every monitor.
//!
//!   [Start] [1][2][3][4] | window buttons of THIS monitor + workspace ...   net vol battery  clock
//!
//! Registered as an app bar (SHAppBarMessage) so maximized windows stop below it. Window
//! buttons: click = focus (or minimize if already focused), middle-click = close.
//! Clicking the clock opens notifications/calendar (Win+N); network, volume and battery open
//! quick settings (Win+A). Wheel over them changes the volume; middle-click on volume mutes.

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ui;
use crate::win::{self, hwnd_key, key_hwnd};
use crate::{with_app, workspaces, App};

pub const CLASS: windows::core::PCWSTR = w!("LastDeskBar");
pub const WM_APPBAR: u32 = WM_APP + 2;
const BASE_HEIGHT: f32 = 34.0;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIP_TIMER: usize = 1;
const PREVIEW_TIMER: usize = 2;
pub const TIP_CLASS: windows::core::PCWSTR = w!("LastDeskTip");
/// Button colour for a window asking for attention (FlashWindow), as on the Windows taskbar.
const ATTENTION: COLORREF = ui::rgb(0xC8, 0x5A, 0x14);
/// Top-edge line on pinned apps: quiet grey, so the blue focus line stays the strongest mark.
const PIN_EDGE: COLORREF = ui::rgb(0x8A, 0x90, 0x9C);

fn cursor() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

// ---------- tooltip for tray icons ----------

thread_local! {
    static TIP: std::cell::RefCell<(HWND, String, f32)> = const { std::cell::RefCell::new((HWND(std::ptr::null_mut()), String::new(), 1.0)) };
}

fn tip_font(scale: f32) -> HFONT {
    unsafe { ui::font((12.5 * scale) as i32, false) }
}

fn show_tip(text: &str, anchor: POINT, scale: f32, above: bool) {
    if text.trim().is_empty() {
        return;
    }
    let hwnd = TIP.with(|t| t.borrow().0);
    let hwnd = if hwnd.is_invalid() {
        let hinst: HINSTANCE = unsafe { GetModuleHandleW(None).unwrap_or_default().into() };
        let Ok(h) = (unsafe {
            CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT, TIP_CLASS, w!(""), WS_POPUP, 0, 0, 10, 10, None, None, hinst, None)
        }) else { return };
        h
    } else {
        hwnd
    };
    TIP.with(|t| *t.borrow_mut() = (hwnd, text.to_string(), scale));
    let pad = (8.0 * scale) as i32;
    unsafe {
        let dc = GetDC(hwnd);
        let f = tip_font(scale);
        let old = SelectObject(dc, f);
        let mut w: Vec<u16> = text.encode_utf16().collect();
        let mut r = RECT { left: 0, top: 0, right: (420.0 * scale) as i32, bottom: 0 };
        DrawTextW(dc, &mut w, &mut r, DT_CALCRECT | DT_NOPREFIX | DT_WORDBREAK);
        SelectObject(dc, old);
        let _ = DeleteObject(f);
        ReleaseDC(hwnd, dc);
        let (tw, th) = (r.right + pad * 2, r.bottom + pad * 2);
        let x = anchor.x - tw / 2;
        let gap = (4.0 * scale) as i32;
        let y = if above { anchor.y - th - gap } else { anchor.y + gap };
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, tw, th, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = InvalidateRect(hwnd, None, true);
    }
}

fn hide_tip() {
    let hwnd = TIP.with(|t| t.borrow().0);
    if !hwnd.is_invalid() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

pub unsafe extern "system" fn tip_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let (text, scale) = TIP.with(|t| { let t = t.borrow(); (t.1.clone(), t.2) });
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            let brush = CreateSolidBrush(ui::rgb(0x2A, 0x2B, 0x30));
            FillRect(hdc, &r, brush);
            let _ = DeleteObject(brush);
            let pad = (8.0 * scale) as i32;
            let f = tip_font(scale);
            let old = SelectObject(hdc, f);
            SetTextColor(hdc, ui::TEXT);
            SetBkMode(hdc, TRANSPARENT);
            let mut w: Vec<u16> = text.encode_utf16().collect();
            let mut tr = RECT { left: pad, top: pad, right: r.right - pad, bottom: r.bottom - pad };
            if !w.is_empty() {
                DrawTextW(hdc, &mut w, &mut tr, DT_NOPREFIX | DT_WORDBREAK);
            }
            SelectObject(hdc, old);
            let _ = DeleteObject(f);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Hit {
    Start,
    Workspace(usize),
    Launcher(usize),
    Tailscale,
    Window(isize),
    TrayIcon(usize),
    Tray,
    Privacy,
    Network,
    Volume,
    Battery,
    Clock,
    None,
}

pub struct Button {
    pub hwnd: isize,
    pub rect: RECT,
    pub title: String,
    pub icon: HICON,
    pub minimized: bool,
    /// The pinned app (index into config.launchers) this window belongs to.
    pub pin: Option<usize>,
}

/// Something laid out in the bar's app area: a pinned app's icon or a window button.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Item {
    Pin(usize),
    Win(isize),
}

pub struct Bar {
    pub hwnd: HWND,
    pub mon: usize,
    pub rect: RECT,
    pub scale: f32,
    pub start: RECT,
    pub cells: Vec<RECT>,
    /// Pinned apps without a window in this bar: (index into config.launchers, rect).
    pub pins: Vec<(usize, RECT)>,
    pub buttons: Vec<Button>,
    /// Tray icons shown in this bar: (owner hwnd, uid, rect).
    pub tray_icons: Vec<(u32, u32, RECT)>,
    pub tray: RECT,
    /// Microphone / camera in-use icons (zero width when nothing is in use).
    pub privacy: RECT,
    /// Tailscale icon (zero width when Tailscale isn't installed).
    pub ts: RECT,
    pub net: RECT,
    pub vol: RECT,
    pub battery: RECT,
    pub clock: RECT,
    /// Button order: first-seen, so buttons don't jump around with z-order.
    pub order: Vec<isize>,
    pub hover: Hit,
    pub tracking: bool,
    /// Registered as an app bar (normal mode) vs sitting in the hidden taskbar's strip.
    pub registered: bool,
    pub edge: crate::config::Edge,
    /// Hidden because a fullscreen window covers this monitor.
    pub fs: bool,
    /// A window button being pressed / dragged to reorder.
    pub drag: Option<BarDrag>,
    /// We set this monitor's work area ourselves (Windows ignored the app bar); undo on exit.
    pub forced_work: bool,
}

pub struct BarDrag {
    /// Window key, or launcher index when `pin`.
    pub key: isize,
    /// Dragging a pinned app (launcher) rather than a window button.
    pub pin: bool,
    pub down_x: i32,
    pub x: i32,
    /// Cursor offset inside the button when it was grabbed.
    pub grab: i32,
    pub active: bool,
}

/// Saved preferred order of apps on the bar (exe names), so reopened apps get their slot back.
fn order_path() -> std::path::PathBuf {
    crate::config::path().with_file_name("bar_order.txt")
}

pub fn load_rank() -> Vec<String> {
    std::fs::read_to_string(order_path())
        .map(|t| t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

fn save_rank(rank: &[String]) {
    let _ = std::fs::write(order_path(), rank.join("\n") + "\n");
}

/// For each monitor rect: is the top window there (ignoring small always-on-top ones)
/// a fullscreen window covering the whole monitor?
fn fullscreen_monitors(mons: &[RECT]) -> Vec<bool> {
    struct Scan<'a> {
        mons: &'a [RECT],
        result: Vec<Option<bool>>,
        own_pid: u32,
    }
    unsafe extern "system" fn cb(h: HWND, data: LPARAM) -> BOOL {
        let s = &mut *(data.0 as *mut Scan);
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() || crate::cloak::is_cloaked(h) {
            return TRUE;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == s.own_pid {
            return TRUE; // our bar, overview, slide panel…
        }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TRANSPARENT.0 != 0 || ex & WS_EX_NOACTIVATE.0 != 0 && ex & WS_EX_TOOLWINDOW.0 != 0 {
            return TRUE; // click-through overlays, notification toasts
        }
        let class = crate::win::class_name(h);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return TRUE;
        }
        let mut r = RECT::default();
        if GetWindowRect(h, &mut r).is_err() || r.right - r.left < 50 {
            return TRUE;
        }
        // An ordinary maximized window (title bar and all) isn't fullscreen, even when it covers
        // the monitor because the bar's strip wasn't reserved; counting it would keep the bar
        // hidden for good.
        let style = GetWindowLongW(h, GWL_STYLE) as u32;
        if IsZoomed(h).as_bool() && style & WS_CAPTION.0 == WS_CAPTION.0 {
            if let Some(i) = s.mons.iter().position(|m| contains(m, (r.left + r.right) / 2, (r.top + r.bottom) / 2)) {
                if s.result[i].is_none() {
                    s.result[i] = Some(false);
                }
            }
            return if s.result.iter().all(|r| r.is_some()) { FALSE } else { TRUE };
        }
        let topmost = ex & WS_EX_TOPMOST.0 != 0;
        // The monitor the window belongs to (not mere overlap: a maximized window's invisible
        // border reaches a few pixels into the neighbouring monitor).
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            return TRUE;
        }
        for (i, m) in s.mons.iter().enumerate() {
            if s.result[i].is_some() || mi.rcMonitor != *m {
                continue;
            }
            let covers = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
            if covers {
                s.result[i] = Some(true);
            } else if !topmost {
                s.result[i] = Some(false); // a normal window on top: not fullscreen
            }
        }
        if s.result.iter().all(|r| r.is_some()) { FALSE } else { TRUE }
    }
    let mut s = Scan { mons, result: vec![None; mons.len()], own_pid: unsafe { windows::Win32::System::Threading::GetCurrentProcessId() } };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut s as *mut _ as isize));
    }
    s.result.into_iter().map(|r| r.unwrap_or(false)).collect()
}

fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn press(keys: &[VIRTUAL_KEY]) {
    let mk = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 } },
    };
    let mut v: Vec<INPUT> = keys.iter().map(|k| mk(*k, false)).collect();
    v.extend(keys.iter().rev().map(|k| mk(*k, true)));
    unsafe {
        SendInput(&v, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Windows opens Start on the monitor whose taskbar was clicked last; with those taskbars
/// hidden it always picks the main monitor. Start's host window keeps wherever it was put,
/// so while it's closed (cloaked), move it onto `work` (that monitor's work area) and the
/// Win key then opens it there.
fn place_start_menu(work: RECT) {
    place_flyout(w!("Start"), work); // fills the work area
    place_flyout(w!("Search"), work); // what Start turns into once you type
}

/// Move a closed shell flyout (Start, Search, Notification Centre…) onto the monitor with
/// work area `work`, keeping how it sits in its current monitor's work area: full width or
/// height, centred, or anchored to an edge (with the gap scaled to the new monitor's DPI).
pub(crate) fn place_flyout(title: windows::core::PCWSTR, work: RECT) {
    place_window(w!("Windows.UI.Core.CoreWindow"), title, work);
}

/// Same, for any shell window by class + title. "Closed" = cloaked or hidden.
fn place_window(class: windows::core::PCWSTR, title: windows::core::PCWSTR, work: RECT) {
    place_window_ex(class, title, work, false);
}

/// `open_ok`: also move it while it is showing (for flyouts Windows re-places on open).
fn place_window_ex(class: windows::core::PCWSTR, title: windows::core::PCWSTR, work: RECT, open_ok: bool) {
    const FLAGS: SET_WINDOW_POS_FLAGS = SET_WINDOW_POS_FLAGS(SWP_NOZORDER.0 | SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0);
    unsafe {
        let Ok(h) = FindWindowW(class, title) else { return };
        if !open_ok && !crate::cloak::is_cloaked(h) && IsWindowVisible(h).as_bool() {
            return; // open right now: leave it alone
        }
        let mut cur = RECT::default();
        if GetWindowRect(h, &mut cur).is_err() {
            return;
        }
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            return;
        }
        let from = mi.rcWork;
        if from == work {
            return;
        }
        // Step 1: move it over and let it rescale itself for the new monitor's DPI.
        let _ = SetWindowPos(h, None, work.left, work.top, 0, 0, FLAGS | SWP_NOSIZE);
        let mut moved = RECT::default();
        let _ = GetWindowRect(h, &mut moved);
        let k = (moved.right - moved.left) as f32 / (cur.right - cur.left).max(1) as f32;
        // Step 2: place it in the same relative spot, per axis.
        let axis = |c0: i32, c1: i32, f0: i32, f1: i32, t0: i32, t1: i32, size: i32| -> (i32, i32) {
            let (g0, g1) = (c0 - f0, f1 - c1);
            if g0.abs() <= 2 && g1.abs() <= 2 {
                (t0, t1 - t0) // fills that axis
            } else if (g0 - g1).abs() <= 4 {
                (t0 + (t1 - t0 - size) / 2, size) // centred
            } else if g0 < g1 {
                (t0 + (g0 as f32 * k) as i32, size)
            } else {
                (t1 - (g1 as f32 * k) as i32 - size, size)
            }
        };
        let (x, w) = axis(cur.left, cur.right, from.left, from.right, work.left, work.right, moved.right - moved.left);
        let (y, hgt) = axis(cur.top, cur.bottom, from.top, from.bottom, work.top, work.bottom, moved.bottom - moved.top);
        let _ = SetWindowPos(h, None, x, y, w, hgt, FLAGS);
    }
}

fn appbar_data(hwnd: HWND, rc: RECT) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: WM_APPBAR,
        uEdge: ABE_TOP,
        rc,
        lParam: LPARAM(0),
    }
}

impl Bar {
    /// The app area left to right: (item, rect, pinned app it belongs to).
    fn items(&self) -> Vec<(Item, RECT, Option<usize>)> {
        let mut v: Vec<(Item, RECT, Option<usize>)> = self.pins.iter().map(|(li, r)| (Item::Pin(*li), *r, Some(*li))).collect();
        v.extend(self.buttons.iter().map(|bt| (Item::Win(bt.hwnd), bt.rect, bt.pin)));
        v.sort_by_key(|(_, r, _)| r.left);
        v
    }

    /// Put items (reordered / moved by a drag) back into pins, buttons and order.
    fn set_items(&mut self, items: &[(Item, RECT, Option<usize>)]) {
        let mut buttons = std::mem::take(&mut self.buttons);
        self.pins.clear();
        let mut shown: Vec<isize> = Vec::new();
        for (it, r, _) in items {
            match *it {
                Item::Pin(li) => self.pins.push((li, *r)),
                Item::Win(k) => {
                    if let Some(p) = buttons.iter().position(|bt| bt.hwnd == k) {
                        let mut bt = buttons.remove(p);
                        bt.rect = *r;
                        self.buttons.push(bt);
                        shown.push(k);
                    }
                }
            }
        }
        // Windows that didn't fit keep their place after the shown ones.
        let rest: Vec<isize> = self.order.iter().copied().filter(|k| !shown.contains(k)).collect();
        self.order = shown.into_iter().chain(rest).collect();
    }

    fn hit(&self, x: i32, y: i32) -> Hit {
        if contains(&self.start, x, y) {
            return Hit::Start;
        }
        if let Some(i) = self.cells.iter().position(|r| contains(r, x, y)) {
            return Hit::Workspace(i);
        }
        if let Some((li, _)) = self.pins.iter().find(|(_, r)| contains(r, x, y)) {
            return Hit::Launcher(*li);
        }
        if let Some(b) = self.buttons.iter().find(|b| contains(&b.rect, x, y)) {
            return Hit::Window(b.hwnd);
        }
        if let Some(i) = self.tray_icons.iter().position(|(_, _, r)| contains(r, x, y)) {
            return Hit::TrayIcon(i);
        }
        if contains(&self.tray, x, y) {
            return Hit::Tray;
        }
        if contains(&self.privacy, x, y) {
            return Hit::Privacy;
        }
        if contains(&self.ts, x, y) {
            return Hit::Tailscale;
        }
        if contains(&self.net, x, y) {
            return Hit::Network;
        }
        if contains(&self.vol, x, y) {
            return Hit::Volume;
        }
        if contains(&self.battery, x, y) {
            return Hit::Battery;
        }
        if contains(&self.clock, x, y) {
            return Hit::Clock;
        }
        Hit::None
    }

    fn height(&self) -> i32 {
        if self.registered || self.rect.bottom <= self.rect.top {
            (BASE_HEIGHT * self.scale).round() as i32
        } else {
            self.rect.bottom - self.rect.top
        }
    }

    /// Don't re-show a bar that's hidden for a fullscreen window.
    fn show_flags(&self) -> SET_WINDOW_POS_FLAGS {
        if self.fs { SWP_NOACTIVATE } else { SWP_NOACTIVATE | SWP_SHOWWINDOW }
    }

    /// Sit in strip (the hidden taskbar's reserved area) or, without one, reserve our own
    /// strip at the top of the monitor as an app bar.
    fn place(&mut self, monitor: RECT, strip: Option<RECT>) {
        if let Some(s) = strip {
            if self.registered {
                let mut abd = appbar_data(self.hwnd, RECT::default());
                unsafe {
                    SHAppBarMessage(ABM_REMOVE, &mut abd);
                }
                self.registered = false;
            }
            self.rect = s;
            unsafe {
                let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, s.left, s.top, s.right - s.left, s.bottom - s.top, self.show_flags());
            }
            return;
        }
        if !self.registered {
            let mut abd = appbar_data(self.hwnd, RECT::default());
            unsafe {
                SHAppBarMessage(ABM_NEW, &mut abd);
            }
            self.registered = true;
        }
        let h = (BASE_HEIGHT * self.scale).round() as i32;
        let bottom = self.edge == crate::config::Edge::Bottom;
        let rc = if bottom {
            RECT { left: monitor.left, top: monitor.bottom - h, right: monitor.right, bottom: monitor.bottom }
        } else {
            RECT { left: monitor.left, top: monitor.top, right: monitor.right, bottom: monitor.top + h }
        };
        let mut abd = appbar_data(self.hwnd, rc);
        abd.uEdge = if bottom { ABE_BOTTOM } else { ABE_TOP };
        unsafe {
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            if bottom {
                abd.rc.top = abd.rc.bottom - h;
            } else {
                abd.rc.bottom = abd.rc.top + h;
            }
            SHAppBarMessage(ABM_SETPOS, &mut abd);
            self.rect = abd.rc;
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, abd.rc.left, abd.rc.top, abd.rc.right - abd.rc.left, h, self.show_flags());
        }
        self.ensure_work_area(monitor);
    }

    /// Make sure the monitor's work area leaves out the bar. Windows sometimes ignores an app
    /// bar's reservation on a monitor (seen after the main display changed, with maximized
    /// windows covering it), and then maximized windows go over the bar. If so, set the work
    /// area directly.
    fn ensure_work_area(&mut self, monitor: RECT) {
        if !self.registered || self.rect.bottom <= self.rect.top {
            return; // in the hidden taskbar's strip: the taskbar reserves it
        }
        unsafe {
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(MonitorFromRect(&monitor, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() || mi.rcMonitor != monitor {
                return;
            }
            let mut work = mi.rcWork;
            let bottom = self.edge == crate::config::Edge::Bottom;
            if bottom {
                if work.bottom <= self.rect.top {
                    return;
                }
                work.bottom = self.rect.top;
            } else {
                if work.top >= self.rect.bottom {
                    return;
                }
                work.top = self.rect.bottom;
            }
            if SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut work as *mut RECT as _), SPIF_SENDCHANGE).is_ok() {
                self.forced_work = true;
                crate::trace(&format!("work area forced on mon{}", self.mon));
            }
        }
    }
}

impl App {
    pub fn bars_create(&mut self) {
        if !self.config.bar || !self.bars.is_empty() {
            return;
        }
        let hinst: HINSTANCE = unsafe { GetModuleHandleW(None).unwrap_or_default().into() };
        for (m, mon) in self.monitors.iter().enumerate() {
            let r = mon.rect;
            let (mut dx, mut dy) = (96u32, 96u32);
            unsafe {
                let hm = MonitorFromPoint(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }, MONITOR_DEFAULTTONEAREST);
                let _ = GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            }
            let Ok(hwnd) = (unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                    CLASS,
                    w!("LastDesk bar"),
                    WS_POPUP,
                    r.left, r.top, r.right - r.left, 10,
                    None, None, hinst, None,
                )
            }) else { continue };
            let mut abd = appbar_data(hwnd, RECT::default());
            unsafe {
                SHAppBarMessage(ABM_NEW, &mut abd);
            }
            let mut bar = Bar {
                hwnd,
                mon: m,
                rect: RECT::default(),
                scale: dx as f32 / 96.0,
                start: RECT::default(),
                cells: Vec::new(),
                pins: Vec::new(),
                buttons: Vec::new(),
                tray_icons: Vec::new(),
                tray: RECT::default(),
                privacy: RECT::default(),
                ts: RECT::default(),
                net: RECT::default(),
                vol: RECT::default(),
                battery: RECT::default(),
                clock: RECT::default(),
                order: Vec::new(),
                hover: Hit::None,
                tracking: false,
                registered: true,
                edge: self.config.bar_edge,
                fs: false,
                drag: None,
                forced_work: false,
            };
            bar.place(r, None);
            self.bars.push(bar);
        }
        self.refresh_work_areas();
        self.bars_refresh();
        unsafe {
            SetTimer(self.main, crate::CLOCK_TIMER, 10_000, None);
        }
    }

    pub fn bars_destroy(&mut self) {
        for b in self.bars.drain(..) {
            let mut abd = appbar_data(b.hwnd, RECT::default());
            unsafe {
                if b.registered {
                    SHAppBarMessage(ABM_REMOVE, &mut abd);
                }
                let _ = DestroyWindow(b.hwnd);
                // We set the work area ourselves: give the bar's strip back.
                if b.forced_work {
                    if let Some(m) = self.monitors.get(b.mon) {
                        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                        if GetMonitorInfoW(MonitorFromRect(&m.rect, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
                            let mut work = mi.rcWork;
                            if b.edge == crate::config::Edge::Bottom {
                                work.bottom = work.bottom.max(b.rect.bottom).min(mi.rcMonitor.bottom);
                            } else {
                                work.top = work.top.min(b.rect.top).max(mi.rcMonitor.top);
                            }
                            let _ = SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut work as *mut RECT as _), SPIF_SENDCHANGE);
                        }
                    }
                }
            }
        }
        self.refresh_work_areas();
    }

    /// Safety net (clock timer): re-check that every bar's strip is still reserved.
    pub fn bars_ensure_work_areas(&mut self) {
        for i in 0..self.bars.len() {
            let Some(mon) = self.monitors.get(self.bars[i].mon).map(|m| m.rect) else { continue };
            let was = self.bars[i].forced_work;
            self.bars[i].ensure_work_area(mon);
            if self.bars[i].forced_work && !was {
                self.refresh_work_areas();
            }
        }
    }

    /// Our app bars shrink each monitor's work area: keep our copy current.
    pub fn refresh_work_areas(&mut self) {
        let fresh = win::monitors();
        for m in &mut self.monitors {
            if let Some(f) = fresh.iter().find(|f| f.name == m.name) {
                m.work = f.work;
            }
        }
    }

    pub fn bar_reposition(&mut self, hwnd: HWND) {
        let hidden = self.taskbar_hidden;
        let Some(i) = self.bars.iter().position(|b| b.hwnd == hwnd) else { return };
        let Some(mon) = self.monitors.get(self.bars[i].mon).map(|m| m.rect) else { return };
        // Work area without our own app bar, to see whether the taskbar reserves a strip here.
        if self.bars[i].registered && hidden {
            let mut abd = appbar_data(hwnd, RECT::default());
            unsafe {
                SHAppBarMessage(ABM_REMOVE, &mut abd);
            }
            self.bars[i].registered = false;
        }
        let work = win::monitors().into_iter().find(|m| m.rect == mon).map(|m| m.work).unwrap_or(mon);
        // Take the hidden taskbar's strip only if it's on the edge the bar should be on.
        let edge = self.bars[i].edge;
        let strip = if hidden {
            crate::taskbar::reserved_strip(mon, work).filter(|s| match edge {
                crate::config::Edge::Top => s.top == mon.top,
                crate::config::Edge::Bottom => s.bottom == mon.bottom,
            })
        } else {
            None
        };
        self.bars[i].place(mon, strip);
        self.refresh_work_areas();
        self.bars_refresh();
    }

    /// Hide each monitor's bar while a fullscreen window (browser F11, video player, game)
    /// is on top there; bring it back when that's no longer so.
    pub fn bars_fullscreen_check(&mut self) {
        let mons: Vec<RECT> = self.bars.iter().map(|b| self.monitors.get(b.mon).map(|m| m.rect).unwrap_or_default()).collect();
        let fs = fullscreen_monitors(&mons);
        for (b, on) in self.bars.iter_mut().zip(fs) {
            if b.fs == on {
                continue;
            }
            b.fs = on;
            unsafe {
                if on {
                    hide_tip();
                    let _ = ShowWindow(b.hwnd, SW_HIDE);
                } else {
                    let r = b.rect;
                    let _ = SetWindowPos(b.hwnd, HWND_TOPMOST, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                }
            }
        }
    }

    /// Recompute buttons and layout for every bar, then repaint.
    fn aumid(&self, h: HWND) -> String {
        let k = hwnd_key(h);
        if let Some(a) = self.aumid_cache.borrow().get(&k) {
            return a.clone();
        }
        let a = crate::launcher::window_aumid(h);
        self.aumid_cache.borrow_mut().insert(k, a.clone());
        a
    }

    /// Does window `k` belong to pinned app `li`? Store apps by AppUserModelID, others by exe.
    pub(crate) fn launcher_matches(&self, li: usize, k: isize) -> bool {
        let Some(l) = self.config.launchers.get(li) else { return false };
        let h = key_hwnd(k);
        match l.target.strip_prefix(crate::launcher::APPS_FOLDER) {
            Some(aumid) => self.aumid(h).eq_ignore_ascii_case(aumid),
            None => crate::launcher::stem(&l.target) == self.exe(h),
        }
    }

    /// Keep launcher_icons in step with config.launchers.
    fn sync_launcher_icons(&mut self) {
        let want: Vec<&String> = self.config.launchers.iter().map(|l| &l.target).collect();
        if self.launcher_icons.len() == want.len() && self.launcher_icons.iter().zip(&want).all(|((t, _), w)| t == *w) {
            return;
        }
        for (_, i) in self.launcher_icons.drain(..) {
            if !i.is_invalid() {
                unsafe {
                    let _ = DestroyIcon(i);
                }
            }
        }
        self.launcher_icons = self.config.launchers.iter().map(|l| (l.target.clone(), crate::launcher::icon(&l.target))).collect();
    }

    /// Pin or unpin window `k`'s app; saves the config.
    pub(crate) fn toggle_launcher(&mut self, k: isize) {
        let matching: Vec<usize> = (0..self.config.launchers.len()).filter(|li| self.launcher_matches(*li, k)).collect();
        if matching.is_empty() {
            let Some(l) = crate::launcher::for_window(key_hwnd(k)) else { return };
            self.config.launchers.push(l);
        } else {
            for li in matching.into_iter().rev() {
                self.config.launchers.remove(li);
            }
        }
        let _ = crate::config::save(&self.config);
        self.bars_refresh();
    }

    pub(crate) fn remove_launcher(&mut self, li: usize) {
        if li < self.config.launchers.len() {
            self.config.launchers.remove(li);
            let _ = crate::config::save(&self.config);
            self.bars_refresh();
        }
    }

    /// A pinned app was dragged: the pinned apps in `seq` (indices into config.launchers, in
    /// their new order) take that order in the config, in the places they held; others stay.
    fn apply_pin_order(&mut self, seq: Vec<usize>) {
        let mut places = seq.clone();
        places.sort_unstable();
        if places == seq || places.iter().any(|&p| p >= self.config.launchers.len()) {
            return;
        }
        let old = self.config.launchers.clone();
        let mut perm: Vec<usize> = (0..old.len()).collect();
        for (place, from) in places.iter().zip(&seq) {
            perm[*place] = *from;
        }
        self.config.launchers = perm.iter().map(|&f| old[f].clone()).collect();
        // Same move for the cached icons, so they needn't be loaded again.
        if self.launcher_icons.len() == old.len() {
            let icons = std::mem::take(&mut self.launcher_icons);
            self.launcher_icons = perm.iter().map(|&f| icons[f].clone()).collect();
        }
        let _ = crate::config::save(&self.config);
        self.bars_refresh();
    }

    /// Show the hover preview for window button `k` of bar `i`.
    fn show_preview(&self, i: usize, k: isize) {
        let b = &self.bars[i];
        let Some(btn) = b.buttons.iter().find(|bt| bt.hwnd == k) else { return };
        hide_tip();
        let above = b.edge == crate::config::Edge::Bottom;
        let mon = self.monitors.get(b.mon).map(|m| m.rect).unwrap_or(b.rect);
        let screen_bar = RECT { left: b.rect.left, top: b.rect.top, right: b.rect.right, bottom: b.rect.top + b.height() };
        crate::preview::show(key_hwnd(k), &btn.title, btn.icon, b.rect.left + (btn.rect.left + btn.rect.right) / 2, screen_bar, above, mon, b.scale);
    }

    pub fn bars_refresh(&mut self) {
        if self.bars.is_empty() {
            return;
        }
        self.sync_launcher_icons();
        let all = win::visible_windows();
        let n = workspaces();
        let icons_only = self.config.icons_only;
        let align = self.config.align;
        let tray_host = crate::tray::active();
        let tray_on = self.taskbar_hidden && !tray_host;
        let icons = if tray_host { crate::tray::icons() } else { Vec::new() };
        for bi in 0..self.bars.len() {
            let m = self.bars[bi].mon;
            if m >= self.mons.len() {
                continue;
            }
            let name = self.mons[m].name.clone();
            let here: Vec<isize> = all
                .iter()
                .filter(|h| win::monitor_name_of_window(**h).as_deref() == Some(name.as_str()))
                .map(|h| hwnd_key(*h))
                .collect();
            // Position of each window's app in the saved order (unknown apps go last).
            let ranks: std::collections::HashMap<isize, usize> = here
                .iter()
                .map(|k| {
                    let e = self.exe(key_hwnd(*k));
                    (*k, self.bar_rank.iter().position(|r| *r == e).unwrap_or(usize::MAX))
                })
                .collect();
            // Pinned apps with no window in this bar get a launcher button.
            let n_pins = self.config.launchers.len();
            let pins: Vec<usize> = (0..n_pins).filter(|li| !here.iter().any(|k| self.launcher_matches(*li, *k))).collect();
            // The pinned app each window belongs to (first match).
            let pin_of: std::collections::HashMap<isize, usize> =
                here.iter().filter_map(|k| (0..n_pins).find(|li| self.launcher_matches(*li, *k)).map(|li| (*k, li))).collect();
            let b = &mut self.bars[bi];
            b.order.retain(|k| here.contains(k));
            for k in &here {
                if !b.order.contains(k) {
                    // New window: before the first button whose app comes later in the saved order.
                    let r = ranks[k];
                    let pos = b.order.iter().position(|o| ranks.get(o).copied().unwrap_or(usize::MAX) > r).unwrap_or(b.order.len());
                    b.order.insert(pos, *k);
                }
            }
            let s = |v: f32| (v * b.scale).round() as i32;
            let w = b.rect.right - b.rect.left;
            let h = b.height();
            let pad = s(6.0);
            b.start = RECT { left: pad, top: s(3.0), right: pad + s(34.0), bottom: h - s(3.0) };
            let cell = s(24.0);
            let mut x = b.start.right + s(8.0);
            b.cells = (0..n)
                .map(|_| {
                    let r = RECT { left: x, top: (h - cell) / 2, right: x + cell, bottom: (h + cell) / 2 };
                    x += cell + s(4.0);
                    r
                })
                .collect();
            let right_w = s(150.0);
            let batt_w = s(64.0);
            b.clock = RECT { left: w - pad - right_w, top: s(3.0), right: w - pad, bottom: h - s(3.0) };
            b.battery = RECT { left: b.clock.left - batt_w, top: s(3.0), right: b.clock.left - s(4.0), bottom: h - s(3.0) };
            let iw = s(28.0);
            let icon_w = s(30.0);
            b.vol = RECT { left: b.battery.left - icon_w, top: s(3.0), right: b.battery.left, bottom: h - s(3.0) };
            b.net = RECT { left: b.vol.left - icon_w, top: s(3.0), right: b.vol.left, bottom: h - s(3.0) };
            let n_priv = (!self.status.mic.is_empty()) as i32 + (!self.status.cam.is_empty()) as i32;
            let ts_w = if crate::tailscale::active() { icon_w } else { 0 };
            b.ts = RECT { left: b.net.left - ts_w, top: s(3.0), right: b.net.left, bottom: h - s(3.0) };
            b.privacy = RECT { left: b.ts.left - icon_w * n_priv, top: s(3.0), right: b.ts.left, bottom: h - s(3.0) };
            let mut ix = b.privacy.left - s(4.0);
            let mut tray_icons: Vec<(u32, u32, RECT)> = icons
                .iter()
                .rev()
                .map(|(hw, uid, _, _)| {
                    ix -= iw;
                    (*hw, *uid, RECT { left: ix, top: s(3.0), right: ix + iw, bottom: h - s(3.0) })
                })
                .collect();
            tray_icons.reverse();
            b.tray_icons = tray_icons;
            b.tray = if tray_on { RECT { left: ix - s(32.0), top: s(3.0), right: ix - s(4.0), bottom: h - s(3.0) } } else { RECT { left: ix, top: 0, right: ix, bottom: 0 } };
            let start_x = x + s(10.0);
            let avail = (b.tray.left - s(10.0) - start_x).max(0);
            let count = b.order.len() as i32;
            let gap = s(4.0);
            // Launchers sit first, icon-sized, then the window buttons.
            let pw = s(36.0);
            let pins_w = pins.len() as i32 * (pw + gap);
            let avail_btn = (avail - pins_w).max(0);
            let bw = if count == 0 {
                0
            } else if icons_only {
                s(40.0)
            } else {
                s(220.0).min((avail_btn - gap * (count - 1)) / count).max(s(34.0))
            };
            let shown = if bw == 0 { 0 } else { ((avail_btn + gap) / (bw + gap)).min(count) };
            let total_btn = (shown * bw + (shown - 1).max(0) * gap).max(0);
            let total = pins_w + total_btn;
            let mut bx = match align {
                crate::config::Align::Left => start_x,
                // Centered on the whole bar (like Windows 11), kept inside the free area.
                crate::config::Align::Center => (w / 2 - total / 2).clamp(start_x, (start_x + avail - total).max(start_x)),
                crate::config::Align::Right => (start_x + avail - total).max(start_x),
            };
            // Pinned apps first, in their pinned order, each as its icon or (when running
            // here) its windows; then the other windows.
            let mut seq: Vec<Item> = Vec::new();
            for li in 0..n_pins {
                if pins.contains(&li) {
                    seq.push(Item::Pin(li));
                } else {
                    seq.extend(b.order.iter().filter(|k| pin_of.get(k) == Some(&li)).map(|k| Item::Win(*k)));
                }
            }
            seq.extend(b.order.iter().filter(|k| !pin_of.contains_key(k)).map(|k| Item::Win(*k)));
            // Mid-drag: keep the order being dragged into.
            if b.drag.as_ref().is_some_and(|d| d.active) {
                let old: Vec<Item> = b.items().into_iter().map(|(it, _, _)| it).collect();
                seq.sort_by_key(|it| old.iter().position(|o| o == it).unwrap_or(usize::MAX));
            }
            let mut placed = 0;
            b.pins.clear();
            b.buttons.clear();
            for it in seq {
                match it {
                    Item::Pin(li) => {
                        b.pins.push((li, RECT { left: bx, top: s(3.0), right: bx + pw, bottom: h - s(3.0) }));
                        bx += pw + gap;
                    }
                    Item::Win(k) => {
                        if placed >= shown {
                            continue; // no room
                        }
                        placed += 1;
                        let hw = key_hwnd(k);
                        b.buttons.push(Button {
                            hwnd: k,
                            rect: RECT { left: bx, top: s(3.0), right: bx + bw, bottom: h - s(3.0) },
                            title: win::title(hw),
                            icon: crate::overview::window_icon(hw),
                            minimized: unsafe { IsIconic(hw).as_bool() },
                            pin: pin_of.get(&k).copied(),
                        });
                        bx += bw + gap;
                    }
                }
            }
            if !b.pins.is_empty() {
                crate::trace(&format!("pins mon{} {:?}", b.mon, b.pins.iter().map(|(li, r)| (*li, r.left, r.right)).collect::<Vec<_>>()));
            }
            if n_pins > 0 {
                // Kinds only (no titles): P1 = icon of pinned app 1, W0 = window of pinned app 0, W = other window.
                let kinds: Vec<String> = b
                    .items()
                    .iter()
                    .map(|(it, r, g)| match (it, g) {
                        (Item::Pin(li), _) => format!("P{li}@{}", r.left),
                        (Item::Win(_), Some(li)) => format!("W{li}@{}", r.left),
                        (Item::Win(_), None) => format!("W@{}", r.left),
                    })
                    .collect();
                crate::trace(&format!("items mon{} {}", b.mon, kinds.join(" ")));
            }
            unsafe {
                let _ = InvalidateRect(b.hwnd, None, false);
            }
        }
        // The previewed window went away (closed, moved, other workspace): drop the preview.
        let t = crate::preview::target();
        if t != 0 && !self.bars.iter().any(|b| b.buttons.iter().any(|bt| bt.hwnd == t)) {
            crate::preview::hide();
        }
    }

    pub fn bar_paint(&self, hwnd: HWND, hdc: HDC) {
        let Some(b) = self.bars.iter().find(|b| b.hwnd == hwnd) else { return };
        let Some(ms) = self.mons.get(b.mon) else { return };
        let w = b.rect.right - b.rect.left;
        let h = b.height();
        let s = |v: f32| (v * b.scale).round() as i32;
        let fg = unsafe { GetForegroundWindow() };
        unsafe {
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, w, h);
            let old = SelectObject(mem, bmp);
            let brush = CreateSolidBrush(ui::rgb(0x17, 0x18, 0x1B));
            FillRect(mem, &RECT { left: 0, top: 0, right: w, bottom: h }, brush);
            let _ = DeleteObject(brush);

            let f_num = ui::font(s(13.0), true);
            let f_text = ui::font(s(13.0), false);
            let hover_bg = ui::rgb(0x2A, 0x2B, 0x30);

            // Start: four squares.
            if b.hover == Hit::Start {
                ui::fill_round(mem, b.start, s(6.0), hover_bg);
            }
            let q = s(7.0);
            let g = s(2.0);
            let cx = (b.start.left + b.start.right) / 2;
            let cy = (b.start.top + b.start.bottom) / 2;
            for (dx, dy) in [(-1, -1), (0, -1), (-1, 0), (0, 0)] {
                let l = cx + dx * (q + g) + if dx == 0 { g / 2 } else { -g / 2 };
                let t = cy + dy * (q + g) + if dy == 0 { g / 2 } else { -g / 2 };
                ui::fill_round(mem, RECT { left: l, top: t, right: l + q, bottom: t + q }, s(2.0), ui::ACCENT);
            }

            // Workspaces.
            for (i, r) in b.cells.iter().enumerate() {
                let active = i == ms.active;
                let occupied = if active { !b.order.is_empty() } else { !ms.hidden[i].is_empty() };
                let bg = if active { ui::ACCENT } else if b.hover == Hit::Workspace(i) { hover_bg } else { ui::rgb(0x22, 0x23, 0x27) };
                ui::fill_round(mem, *r, s(6.0), bg);
                // A window on another workspace wants attention: orange frame.
                if !active && ms.hidden[i].iter().any(|k| self.flashing.contains(k)) {
                    ui::frame_round(mem, *r, s(6.0), s(2.0).max(2), ATTENTION);
                }
                let color = if active || occupied { ui::TEXT } else { ui::rgb(0x60, 0x63, 0x6A) };
                let mut tr = *r;
                ui::draw_text(mem, &(i + 1).to_string(), &mut tr, color, f_num);
            }

            // Pinned apps that aren't running here: icon only. A dragged one leaves an empty
            // slot and floats under the cursor.
            let pin_drag = b.drag.as_ref().filter(|d| d.active && d.pin);
            // Pinned apps (icon or running) get a thin line along the top edge, mirroring the
            // focused window's accent line at the bottom.
            let pin_edge = |r: RECT| {
                let line = RECT { left: r.left + s(8.0), top: r.top + s(1.0), right: r.right - s(8.0), bottom: r.top + s(1.0) + s(2.0).max(2) };
                ui::fill_round(mem, line, s(1.0), PIN_EDGE);
            };
            let draw_pin = |li: usize, r: RECT, bg: Option<COLORREF>| {
                if let Some(bg) = bg {
                    ui::fill_round(mem, r, s(6.0), bg);
                }
                pin_edge(r);
                if let Some((_, icon)) = self.launcher_icons.get(li) {
                    if !icon.is_invalid() {
                        let sz = s(18.0);
                        let _ = DrawIconEx(mem, r.left + (r.right - r.left - sz) / 2, (h - sz) / 2, *icon, sz, sz, 0, None, DI_NORMAL);
                    }
                }
            };
            for (li, r) in &b.pins {
                if pin_drag.is_some_and(|d| d.key == *li as isize) {
                    ui::frame_round(mem, *r, s(6.0), s(1.0).max(1), ui::rgb(0x3A, 0x3C, 0x44));
                    continue;
                }
                draw_pin(*li, *r, (b.hover == Hit::Launcher(*li) && pin_drag.is_none()).then_some(hover_bg));
            }
            let draw_lifted_pin = || if let Some(d) = pin_drag {
                if let Some((li, r)) = b.pins.iter().find(|(l, _)| *l as isize == d.key) {
                    let pw = r.right - r.left;
                    let left = (d.x - d.grab).clamp(0, (w - pw).max(0));
                    let lifted = RECT { left, right: left + pw, ..*r };
                    draw_pin(*li, lifted, Some(ui::rgb(0x34, 0x36, 0x3D)));
                    ui::frame_round(mem, lifted, s(6.0), s(1.0).max(1), ui::ACCENT);
                }
            };

            // Window buttons. While dragging, the dragged one leaves an empty slot and is drawn
            // last, floating under the cursor.
            let dragging = b.drag.as_ref().filter(|d| d.active && !d.pin);
            let draw_button = |btn: &Button, rect: RECT, lifted: bool| {
                let is_fg = hwnd_key(fg) == btn.hwnd;
                let bg = if lifted {
                    ui::rgb(0x34, 0x36, 0x3D)
                } else if is_fg {
                    ui::rgb(0x2C, 0x3D, 0x5E)
                } else if self.flashing.contains(&btn.hwnd) {
                    ATTENTION
                } else if b.hover == Hit::Window(btn.hwnd) {
                    hover_bg
                } else {
                    ui::rgb(0x1F, 0x20, 0x24)
                };
                ui::fill_round(mem, rect, s(6.0), bg);
                if lifted {
                    ui::frame_round(mem, rect, s(6.0), s(1.0).max(1), ui::ACCENT);
                }
                if is_fg {
                    let line = RECT { left: rect.left + s(8.0), top: rect.bottom - s(3.0), right: rect.right - s(8.0), bottom: rect.bottom - s(1.0) };
                    ui::fill_round(mem, line, s(2.0), ui::ACCENT);
                }
                let icon = s(16.0);
                let icons_only = self.config.icons_only;
                let ix = if icons_only { rect.left + (rect.right - rect.left - icon) / 2 } else { rect.left + s(8.0) };
                if !btn.icon.is_invalid() {
                    let _ = DrawIconEx(mem, ix, (h - icon) / 2, btn.icon, icon, icon, 0, None, DI_NORMAL);
                }
                if btn.pin.is_some() {
                    pin_edge(rect);
                }
                let mut tr = RECT { left: ix + icon + s(7.0), top: rect.top, right: rect.right - s(6.0), bottom: rect.bottom };
                if !icons_only && tr.right - tr.left > s(12.0) {
                    ui::draw_text_left(mem, &btn.title, &mut tr, if btn.minimized { ui::MUTED } else { ui::TEXT }, f_text);
                }
            };
            for btn in &b.buttons {
                if dragging.is_some_and(|d| d.key == btn.hwnd) {
                    ui::frame_round(mem, btn.rect, s(6.0), s(1.0).max(1), ui::rgb(0x3A, 0x3C, 0x44));
                    continue;
                }
                draw_button(btn, btn.rect, false);
            }
            if let Some(d) = dragging {
                if let Some(btn) = b.buttons.iter().find(|bt| bt.hwnd == d.key) {
                    let bw = btn.rect.right - btn.rect.left;
                    let left = (d.x - d.grab).clamp(0, (w - bw).max(0));
                    draw_button(btn, RECT { left, right: left + bw, ..btn.rect }, true);
                }
            }
            draw_lifted_pin();

            // Tray icons (drawn from the live list: icons can change between refreshes).
            if !b.tray_icons.is_empty() {
                let cur = crate::tray::icons();
                for (i, (hw, uid, r)) in b.tray_icons.iter().enumerate() {
                    if b.hover == Hit::TrayIcon(i) {
                        ui::fill_round(mem, *r, s(6.0), hover_bg);
                    }
                    if let Some((_, _, icon, _)) = cur.iter().find(|c| c.0 == *hw && c.1 == *uid) {
                        let sz = s(16.0);
                        let _ = DrawIconEx(mem, r.left + (r.right - r.left - sz) / 2, (h - sz) / 2, *icon, sz, sz, 0, None, DI_NORMAL);
                    }
                }
            }

            // Tray: ^ brings up the Windows taskbar with its notification area.
            if b.tray.right > b.tray.left {
                if b.hover == Hit::Tray {
                    ui::fill_round(mem, b.tray, s(6.0), hover_bg);
                }
                let mut r = b.tray;
                ui::draw_text(mem, "^", &mut r, ui::TEXT, f_num);
            }

            // Network and volume icons. Wi-Fi bars are drawn over a dim full fan, like Windows.
            let f_icon = ui::icon_font(s(16.0));
            let dim = ui::rgb(0x5A, 0x5D, 0x64);
            let glyph = |c: u32| char::from_u32(c).map(String::from).unwrap_or_default();
            // Microphone / camera in use (only shown while something uses them).
            if b.hover == Hit::Privacy {
                ui::fill_round(mem, b.privacy, s(6.0), hover_bg);
            }
            {
                let mut x = b.privacy.left;
                let iw = if b.privacy.right > b.privacy.left { s(30.0) } else { 0 };
                for (on, g) in [(!self.status.mic.is_empty(), 0xE720), (!self.status.cam.is_empty(), 0xE714)] {
                    if on {
                        let mut r = RECT { left: x, right: x + iw, ..b.privacy };
                        ui::draw_text(mem, &glyph(g), &mut r, ui::TEXT, f_icon);
                        x += iw;
                    }
                }
            }
            // Tailscale: a 3×3 grid of dots with a bright "T" when connected (dim when not,
            // accent colour when using an exit node, orange when it needs a login).
            if b.ts.right > b.ts.left {
                if b.hover == Hit::Tailscale {
                    ui::fill_round(mem, b.ts, s(6.0), hover_bg);
                }
                use crate::tailscale::State;
                let st = crate::tailscale::status().unwrap_or_default();
                let lit = match st.state {
                    State::Running if !st.warnings().is_empty() => Some(ATTENTION),
                    State::Running if st.exit.is_some() => Some(ui::ACCENT),
                    State::Running => Some(ui::TEXT),
                    State::NeedsLogin => Some(ATTENTION),
                    State::Starting => Some(ui::MUTED),
                    State::Stopped | State::Unavailable => None,
                };
                let d = s(3.0).max(2);
                let step = s(5.0);
                let cx = (b.ts.left + b.ts.right) / 2;
                let cy = (b.ts.top + b.ts.bottom) / 2;
                for row in -1..=1 {
                    for col in -1..=1 {
                        let in_t = row == 0 || (row == 1 && col == 0);
                        let color = match lit {
                            Some(c) if in_t => c,
                            _ => dim,
                        };
                        let (x0, y0) = (cx + col * step - d / 2, cy + row * step - d / 2);
                        ui::fill_round(mem, RECT { left: x0, top: y0, right: x0 + d, bottom: y0 + d }, d / 2, color);
                    }
                }
            }
            if b.hover == Hit::Network {
                ui::fill_round(mem, b.net, s(6.0), hover_bg);
            }
            match self.status.net {
                crate::status::Net::Ethernet => ui::draw_text(mem, &glyph(0xE839), &mut b.net.clone(), ui::TEXT, f_icon),
                crate::status::Net::Wifi(q) => {
                    let bars = match q {
                        0..=24 => 0xE872,
                        25..=49 => 0xE873,
                        50..=74 => 0xE874,
                        _ => 0xE701,
                    };
                    if bars != 0xE701 {
                        ui::draw_text(mem, &glyph(0xE701), &mut b.net.clone(), dim, f_icon);
                    }
                    ui::draw_text(mem, &glyph(bars), &mut b.net.clone(), ui::TEXT, f_icon);
                }
                crate::status::Net::Offline => ui::draw_text(mem, &glyph(0xF384), &mut b.net.clone(), ui::MUTED, f_icon),
            }
            if b.hover == Hit::Volume {
                ui::fill_round(mem, b.vol, s(6.0), hover_bg);
            }
            let (vg, vc) = match self.status.volume() {
                None => (0xE74F, dim),
                Some((_, true)) => (0xE74F, ui::TEXT),
                Some((v, false)) if v <= 0.005 => (0xE992, ui::TEXT),
                Some((v, false)) if v < 0.34 => (0xE993, ui::TEXT),
                Some((v, false)) if v < 0.67 => (0xE994, ui::TEXT),
                Some(_) => (0xE995, ui::TEXT),
            };
            ui::draw_text(mem, &glyph(vg), &mut b.vol.clone(), vc, f_icon);
            let _ = DeleteObject(f_icon);

            // Battery.
            let mut ps = SYSTEM_POWER_STATUS::default();
            if GetSystemPowerStatus(&mut ps).is_ok() && ps.BatteryLifePercent <= 100 {
                if b.hover == Hit::Battery {
                    ui::fill_round(mem, b.battery, s(6.0), hover_bg);
                }
                let text = format!("{}%{}", ps.BatteryLifePercent, if ps.ACLineStatus == 1 { " ⚡" } else { "" });
                let color = if ps.BatteryLifePercent <= 15 && ps.ACLineStatus != 1 { ui::rgb(0xF0, 0x6A, 0x5A) } else { ui::TEXT };
                let mut r = b.battery;
                ui::draw_text(mem, &text, &mut r, color, f_text);
            }

            // Clock.
            if b.hover == Hit::Clock {
                ui::fill_round(mem, b.clock, s(6.0), hover_bg);
            }
            let t = GetLocalTime();
            let day = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][(t.wDayOfWeek as usize) % 7];
            let date = format!("{day} {}.{}.", t.wDay, t.wMonth);
            let time = format!("{:02}:{:02}", t.wHour, t.wMinute);
            let mut dr = RECT { right: b.clock.left + (b.clock.right - b.clock.left) * 55 / 100, ..b.clock };
            ui::draw_text(mem, &date, &mut dr, ui::MUTED, f_text);
            let mut tr = RECT { left: dr.right, ..b.clock };
            ui::draw_text(mem, &time, &mut tr, ui::TEXT, f_num);

            let _ = DeleteObject(f_num);
            let _ = DeleteObject(f_text);
            let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(mem);
        }
    }

    /// What's under (x, y) on this bar.
    pub fn bar_hit_at(&self, hwnd: HWND, x: i32, y: i32) -> Option<Hit> {
        Some(self.bars[self.bar_index(hwnd)?].hit(x, y))
    }

    /// The launcher button under (x, y): index into config.launchers.
    pub fn bar_launcher_at(&self, hwnd: HWND, x: i32, y: i32) -> Option<usize> {
        match self.bars[self.bar_index(hwnd)?].hit(x, y) {
            Hit::Launcher(li) => Some(li),
            _ => None,
        }
    }

    /// Right/double click on a tray icon. Returns true if (x, y) was a tray icon.
    /// The window button under (x, y): (window, monitor).
    pub fn bar_window_at(&self, hwnd: HWND, x: i32, y: i32) -> Option<(isize, usize)> {
        let b = &self.bars[self.bar_index(hwnd)?];
        match b.hit(x, y) {
            Hit::Window(k) => Some((k, b.mon)),
            _ => None,
        }
    }

    pub fn bar_tray_click(&mut self, hwnd: HWND, x: i32, y: i32, kind: crate::tray::Click) -> bool {
        let Some(i) = self.bar_index(hwnd) else { return false };
        if let Hit::TrayIcon(t) = self.bars[i].hit(x, y) {
            let (hw, uid, _) = self.bars[i].tray_icons[t];
            hide_tip();
            crate::tray::click(hw, uid, kind, cursor());
            return true;
        }
        false
    }

    fn status_tip(&self, hit: Hit) -> Option<String> {
        use crate::status::Net;
        match hit {
            Hit::Volume => Some(match self.status.volume() {
                None => "No audio output".into(),
                Some((v, m)) => format!("Volume {}%{}", (v * 100.0).round() as i32, if m { " (muted)" } else { "" }),
            }),
            Hit::Privacy => {
                let mut parts = Vec::new();
                if !self.status.mic.is_empty() {
                    parts.push(format!("Microphone: {}", self.status.mic.join(", ")));
                }
                if !self.status.cam.is_empty() {
                    parts.push(format!("Camera: {}", self.status.cam.join(", ")));
                }
                Some(parts.join("  ·  "))
            }
            Hit::Network => Some(match self.status.net {
                Net::Ethernet => "Ethernet".into(),
                Net::Wifi(_) if self.status.ssid.is_empty() => "Wi-Fi".into(),
                Net::Wifi(q) => format!("Wi-Fi: {} ({}%)", self.status.ssid, q),
                Net::Offline => "Not connected".into(),
            }),
            Hit::Tailscale => {
                use crate::tailscale::State;
                let st = crate::tailscale::status().unwrap_or_default();
                Some(match st.state {
                    State::Running => {
                        let mut parts = vec!["Tailscale: connected".to_string()];
                        if !st.tailnet.is_empty() {
                            parts.push(st.tailnet.clone());
                        }
                        if !st.ip.is_empty() {
                            parts.push(st.ip.clone());
                        }
                        if let Some(e) = st.exit_label() {
                            parts.push(format!("via {e}"));
                        }
                        let mut text = parts.join("  ·  ");
                        // Problems on their own lines.
                        for w in st.warnings() {
                            text.push_str("\n⚠ ");
                            text.push_str(&w);
                        }
                        text
                    }
                    State::Starting => "Tailscale: connecting…".into(),
                    State::NeedsLogin => "Tailscale: needs login (click)".into(),
                    State::Stopped => "Tailscale: disconnected".into(),
                    State::Unavailable => "Tailscale: service not running".into(),
                })
            }
            Hit::Battery => {
                let mut ps = SYSTEM_POWER_STATUS::default();
                let mut parts = Vec::new();
                if unsafe { GetSystemPowerStatus(&mut ps) }.is_ok() && ps.BatteryLifePercent <= 100 {
                    let state = if ps.ACLineStatus != 1 {
                        "on battery"
                    } else if ps.BatteryFlag & 8 != 0 {
                        "charging"
                    } else {
                        "plugged in"
                    };
                    parts.push(format!("Battery {}% ({state})", ps.BatteryLifePercent));
                }
                if let Some(rpm) = crate::asus::fan_rpm() {
                    parts.push(if rpm == 0 { "fan off".into() } else { format!("fan {rpm} rpm") });
                }
                (!parts.is_empty()).then(|| parts.join("  ·  "))
            }
            _ => None,
        }
    }

    fn show_status_tip(&self, i: usize, hit: Hit) {
        let b = &self.bars[i];
        let r = match hit {
            Hit::Volume => b.vol,
            Hit::Network => b.net,
            Hit::Privacy => b.privacy,
            Hit::Tailscale => b.ts,
            Hit::Battery => b.battery,
            _ => return,
        };
        if let Some(text) = self.status_tip(hit) {
            let above = b.edge == crate::config::Edge::Bottom;
            let y = if above { b.rect.top } else { b.rect.top + b.height() };
            show_tip(&text, POINT { x: b.rect.left + (r.left + r.right) / 2, y }, b.scale, above);
        }
    }

    /// Volume or network changed: repaint, and refresh an open status tooltip.
    pub fn bars_status_changed(&mut self) {
        for i in 0..self.bars.len() {
            unsafe {
                let _ = InvalidateRect(self.bars[i].hwnd, None, false);
            }
            if matches!(self.bars[i].hover, Hit::Volume | Hit::Network | Hit::Privacy | Hit::Tailscale | Hit::Battery) && TIP.with(|t| unsafe { IsWindowVisible(t.borrow().0).as_bool() }) {
                self.show_status_tip(i, self.bars[i].hover);
            }
        }
    }

    /// Mouse wheel over the volume, network or battery area changes the volume (2% per notch, like Windows).
    pub fn bar_wheel(&mut self, hwnd: HWND, x: i32, y: i32, delta: i16) -> bool {
        let Some(i) = self.bar_index(hwnd) else { return false };
        if !matches!(self.bars[i].hit(x, y), Hit::Volume | Hit::Network | Hit::Battery) {
            return false;
        }
        let Some((v, _)) = self.status.volume() else { return true };
        let steps = delta as f32 / 120.0;
        let nv = ((v * 50.0).round() + steps).clamp(0.0, 50.0) / 50.0;
        self.status.set_volume(nv);
        self.bars_status_changed();
        self.show_status_tip(i, Hit::Volume);
        true
    }

    pub fn bar_show_tip(&mut self, hwnd: HWND) {
        let Some(i) = self.bar_index(hwnd) else { return };
        if matches!(self.bars[i].hover, Hit::Volume | Hit::Network | Hit::Privacy | Hit::Tailscale | Hit::Battery) {
            self.show_status_tip(i, self.bars[i].hover);
            return;
        }
        if let Hit::Launcher(li) = self.bars[i].hover {
            let b = &self.bars[i];
            let (Some((_, r)), Some(l)) = (b.pins.iter().find(|(x, _)| *x == li), self.config.launchers.get(li)) else { return };
            let mut c = l.name.chars();
            let name = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
            let above = b.edge == crate::config::Edge::Bottom;
            let y = if above { b.rect.top } else { b.rect.top + b.height() };
            show_tip(&name, POINT { x: b.rect.left + (r.left + r.right) / 2, y }, b.scale, above);
            return;
        }
        let b = &self.bars[i];
        if let Hit::TrayIcon(t) = b.hover {
            let (hw, uid, r) = b.tray_icons[t];
            let text = crate::tray::tip(hw, uid);
            let above = b.edge == crate::config::Edge::Bottom;
            let y = if above { b.rect.top } else { b.rect.top + b.height() };
            show_tip(&text, POINT { x: b.rect.left + (r.left + r.right) / 2, y }, b.scale, above);
        }
    }

    fn bar_index(&self, hwnd: HWND) -> Option<usize> {
        self.bars.iter().position(|b| b.hwnd == hwnd)
    }

    /// Left button down: remember a window button as a possible drag.
    pub fn bar_drag_start(&mut self, hwnd: HWND, x: i32, y: i32) {
        crate::preview::hide();
        let Some(i) = self.bar_index(hwnd) else { return };
        let b = &mut self.bars[i];
        let (key, pin, left) = match b.hit(x, y) {
            Hit::Window(k) => (k, false, b.buttons.iter().find(|bt| bt.hwnd == k).map(|bt| bt.rect.left).unwrap_or(x)),
            Hit::Launcher(li) => (li as isize, true, b.pins.iter().find(|(l, _)| *l == li).map(|(_, r)| r.left).unwrap_or(x)),
            _ => return,
        };
        b.drag = Some(BarDrag { key, pin, down_x: x, x, grab: x - left, active: false });
        unsafe {
            SetCapture(hwnd);
        }
    }

    /// Mouse moved with a button pressed: once it moves a few pixels it's a drag; the
    /// dragged item follows the cursor and the others shift into the gap. Pinned apps
    /// (icons and their windows) are one group, other windows another; an item moves
    /// within its own group.
    fn bar_drag_move(&mut self, i: usize, x: i32) -> bool {
        let b = &mut self.bars[i];
        let Some(d) = b.drag.as_mut() else { return false };
        d.x = x;
        if !d.active {
            if (x - d.down_x).abs() < (6.0 * b.scale) as i32 {
                return true;
            }
            d.active = true;
            hide_tip();
        }
        let me = if d.pin { Item::Pin(d.key as usize) } else { Item::Win(d.key) };
        let grab = d.grab;
        let mut items = b.items();
        if let Some(cur) = items.iter().position(|(it, _, _)| *it == me) {
            let (_, r, group) = items.remove(cur);
            let w = r.right - r.left;
            let center = x - grab + w / 2;
            // Its new place: after every other item whose centre is left of its centre,
            // kept inside its group (pinned ones come first).
            let pinned = items.iter().filter(|(_, _, g)| g.is_some()).count();
            let (lo, hi) = if group.is_some() { (0, pinned) } else { (pinned, items.len()) };
            let at = items.iter().filter(|(_, r, _)| (r.left + r.right) / 2 < center).count().clamp(lo, hi);
            let start = items.iter().map(|(_, r, _)| r.left).chain(std::iter::once(r.left)).min().unwrap_or(r.left);
            items.insert(at, (me, r, group));
            let gap = (4.0 * b.scale).round() as i32;
            let mut bx = start;
            for (_, r, _) in items.iter_mut() {
                let w = r.right - r.left;
                r.left = bx;
                r.right = bx + w;
                bx += w + gap;
            }
            b.set_items(&items);
        }
        unsafe {
            let _ = InvalidateRect(b.hwnd, None, false);
        }
        true
    }

    /// Left button up. Returns true if it ended a drag (so it's not a click).
    pub fn bar_drag_end(&mut self, hwnd: HWND) -> bool {
        let Some(i) = self.bar_index(hwnd) else { return false };
        let Some(d) = self.bars[i].drag.take() else { return false };
        unsafe {
            let _ = ReleaseCapture();
            let _ = InvalidateRect(hwnd, None, false);
        }
        if !d.active {
            return false;
        }
        // A pinned app (icon or window) moved: save the pinned apps' new order.
        let items = self.bars[i].items();
        let me = if d.pin { Item::Pin(d.key as usize) } else { Item::Win(d.key) };
        if items.iter().any(|(it, _, g)| *it == me && g.is_some()) {
            let mut seq: Vec<usize> = Vec::new();
            for li in items.iter().filter_map(|(_, _, g)| *g) {
                if !seq.contains(&li) {
                    seq.push(li);
                }
            }
            self.apply_pin_order(seq);
            return true;
        }
        // Remember the new order of apps: the dragged bar's apps take the place of the
        // earliest of them in the saved order; other apps keep their relative order.
        let mut seq: Vec<String> = Vec::new();
        for k in self.bars[i].order.clone() {
            let e = self.exe(key_hwnd(k));
            if !e.is_empty() && !seq.contains(&e) {
                seq.push(e);
            }
        }
        let at = self.bar_rank.iter().position(|r| seq.contains(r)).unwrap_or(self.bar_rank.len());
        let before = self.bar_rank[..at].iter().filter(|r| !seq.contains(r)).count();
        self.bar_rank.retain(|r| !seq.contains(r));
        let at = before.min(self.bar_rank.len());
        for (n, e) in seq.into_iter().enumerate() {
            self.bar_rank.insert(at + n, e);
        }
        save_rank(&self.bar_rank);
        true
    }

    pub fn bar_mouse_move(&mut self, hwnd: HWND, x: i32, y: i32) {
        let Some(i) = self.bar_index(hwnd) else { return };
        if self.bar_drag_move(i, x) {
            return;
        }
        let b = &mut self.bars[i];
        if !b.tracking {
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            b.tracking = true;
        }
        let hit = b.hit(x, y);
        if hit != b.hover {
            b.hover = hit;
            hide_tip();
            if let Hit::TrayIcon(t) = hit {
                let (hw, uid, _) = b.tray_icons[t];
                crate::tray::click(hw, uid, crate::tray::Click::Hover, cursor());
                unsafe {
                    SetTimer(hwnd, TIP_TIMER, 450, None);
                }
            } else if matches!(hit, Hit::Volume | Hit::Network | Hit::Privacy | Hit::Tailscale | Hit::Battery | Hit::Launcher(_)) {
                unsafe {
                    SetTimer(hwnd, TIP_TIMER, 450, None);
                }
            }
            // Window preview: after a short hover, then follows the mouse between buttons at once.
            if let Hit::Window(k) = hit {
                if crate::preview::visible() {
                    self.show_preview(i, k);
                } else {
                    unsafe {
                        SetTimer(hwnd, PREVIEW_TIMER, 500, None);
                    }
                }
            } else {
                unsafe {
                    let _ = KillTimer(hwnd, PREVIEW_TIMER);
                }
                crate::preview::hide();
            }
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
        }
    }

    /// The preview delay elapsed: show it if the mouse is still on a window button.
    pub fn bar_preview_timer(&mut self, hwnd: HWND) {
        let Some(i) = self.bar_index(hwnd) else { return };
        if let Hit::Window(k) = self.bars[i].hover {
            if self.bars[i].drag.is_none() {
                self.show_preview(i, k);
            }
        }
    }

    /// The Win key went down: put the closed Start (and Search, Notification Centre) on the
    /// monitor you're on, so it opens there. Only needed while Windows' taskbars are hidden.
    pub fn start_follow(&mut self) {
        if !self.taskbar_hidden || self.bars.is_empty() {
            return;
        }
        let Some(mon) = self.monitors.get(self.current_mon()) else { return };
        let work = mon.work;
        place_start_menu(work);
        place_flyout(w!("Notification Centre"), work);
        place_flyout(w!("Notification Center"), work);
    }

    pub fn bar_mouse_leave(&mut self, hwnd: HWND) {
        let Some(i) = self.bar_index(hwnd) else { return };
        let b = &mut self.bars[i];
        b.tracking = false;
        b.hover = Hit::None;
        hide_tip();
        unsafe {
            let _ = KillTimer(hwnd, PREVIEW_TIMER);
            let _ = InvalidateRect(hwnd, None, false);
        }
        crate::preview::hide();
    }

    /// Returns a key combo to press after the borrow ends (SendInput is fine either way,
    /// but keeping it outside avoids re-entrancy surprises).
    pub fn bar_click(&mut self, hwnd: HWND, x: i32, y: i32, middle: bool) -> Option<Vec<VIRTUAL_KEY>> {
        let i = self.bar_index(hwnd)?;
        let m = self.bars[i].mon;
        crate::preview::hide();
        match self.bars[i].hit(x, y) {
            Hit::Launcher(li) if !middle => {
                hide_tip();
                if let Some(l) = self.config.launchers.get(li).cloned() {
                    // Open it on this bar's monitor, like Start does.
                    self.note_start_opened(m);
                    crate::launcher::launch(&l.target);
                }
            }
            Hit::Start if !middle => {
                place_start_menu(self.monitors[m].work);
                self.note_start_opened(m);
                return Some(vec![VK_LWIN]);
            }
            Hit::Workspace(w) if !middle => self.switch_to(m, w, None),
            Hit::Window(k) => unsafe {
                let h = key_hwnd(k);
                if middle {
                    let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
                } else if GetForegroundWindow() == h && !IsIconic(h).as_bool() {
                    let _ = ShowWindow(h, SW_MINIMIZE);
                } else {
                    // Works for elevated windows (Task Manager) too, and restores minimized ones.
                    SwitchToThisWindow(h, true);
                }
            },
            Hit::TrayIcon(t) => {
                let (hw, uid, _) = self.bars[i].tray_icons[t];
                hide_tip();
                crate::tray::click(hw, uid, if middle { crate::tray::Click::Middle } else { crate::tray::Click::Left }, cursor());
            }
            Hit::Tray if !middle => {
                crate::taskbar::peek();
                return Some(vec![VK_LWIN, VIRTUAL_KEY('B' as u16)]);
            }
            Hit::Privacy if !middle => unsafe {
                hide_tip();
                let page = if self.status.mic.is_empty() { w!("ms-settings:privacy-webcam") } else { w!("ms-settings:privacy-microphone") };
                ShellExecuteW(None, w!("open"), page, None, None, SW_SHOWNORMAL);
            },
            Hit::Volume if middle => {
                self.status.toggle_mute();
                self.bars_status_changed();
            }
            Hit::Network | Hit::Volume | Hit::Battery if !middle => {
                hide_tip();
                // Windows puts Quick settings back by the main taskbar every time it opens,
                // so wait for it to appear and move it then.
                let work = self.monitors[m].work;
                std::thread::spawn(move || unsafe {
                    for _ in 0..150 {
                        std::thread::sleep(std::time::Duration::from_millis(4));
                        if let Ok(h) = FindWindowW(w!("ControlCenterWindow"), w!("Quick settings")) {
                            if IsWindowVisible(h).as_bool() {
                                place_window_ex(w!("ControlCenterWindow"), w!("Quick settings"), work, true);
                                return;
                            }
                        }
                    }
                });
                return Some(vec![VK_LWIN, VIRTUAL_KEY(b'A' as u16)]);
            }
            Hit::Clock if !middle => {
                place_flyout(w!("Notification Centre"), self.monitors[m].work);
                place_flyout(w!("Notification Center"), self.monitors[m].work);
                return Some(vec![VK_LWIN, VIRTUAL_KEY('N' as u16)]);
            }
            _ => {}
        }
        None
    }
}

pub unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let xy = || ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            with_app(|app| app.bar_paint(hwnd, hdc));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y) = xy();
            with_app(|app| app.bar_mouse_move(hwnd, x, y));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|app| app.bar_mouse_leave(hwnd));
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy();
            with_app(|app| app.bar_drag_start(hwnd, x, y));
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // Capture lost mid-drag (Alt+Tab, a popup…): drop where it is.
            with_app(|app| app.bar_drag_end(hwnd));
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = xy();
            if msg == WM_LBUTTONUP && with_app(|app| app.bar_drag_end(hwnd)) == Some(true) {
                return LRESULT(0);
            }
            if msg == WM_LBUTTONUP && with_app(|app| app.bar_hit_at(hwnd, x, y)).flatten() == Some(Hit::Tailscale) {
                hide_tip();
                tailscale_menu();
                return LRESULT(0);
            }
            let keys = with_app(|app| app.bar_click(hwnd, x, y, msg == WM_MBUTTONUP)).flatten();
            if let Some(k) = keys {
                press(&k);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = xy();
            if with_app(|app| app.bar_tray_click(hwnd, x, y, crate::tray::Click::Right)) == Some(true) {
                return LRESULT(0);
            }
            crate::preview::hide();
            hide_tip();
            match with_app(|app| app.bar_hit_at(hwnd, x, y)).flatten() {
                Some(Hit::Tailscale) => {
                    tailscale_menu();
                    return LRESULT(0);
                }
                _ => {}
            }
            if let Some(li) = with_app(|app| app.bar_launcher_at(hwnd, x, y)).flatten() {
                launcher_menu(li);
                return LRESULT(0);
            }
            match with_app(|app| app.bar_window_at(hwnd, x, y)).flatten() {
                Some((k, m)) => window_menu(k, m),
                None => crate::show_menu(with_app(|a| a.main).unwrap_or(hwnd)),
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = xy();
            with_app(|app| app.bar_tray_click(hwnd, x, y, crate::tray::Click::Double));
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Wheel coordinates are screen-relative.
            let mut p = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut p);
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16;
            if with_app(|app| app.bar_wheel(hwnd, p.x, p.y, delta)) == Some(true) {
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER if wp.0 == TIP_TIMER => {
            let _ = KillTimer(hwnd, TIP_TIMER);
            with_app(|app| app.bar_show_tip(hwnd));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == PREVIEW_TIMER => {
            let _ = KillTimer(hwnd, PREVIEW_TIMER);
            with_app(|app| app.bar_preview_timer(hwnd));
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        m if m == WM_APPBAR => {
            match wp.0 as u32 {
                ABN_POSCHANGED => {
                    with_app(|app| app.bar_reposition(hwnd));
                }
                ABN_FULLSCREENAPP => {
                    with_app(|app| app.bars_fullscreen_check());
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Right-click menu for a window button. Runs the (message-pumping) menu outside any borrow.
fn window_menu(k: isize, m: usize) {
    const CMD_MINRESTORE: usize = 1;
    const CMD_PIN: usize = 2;
    const CMD_CLOSE: usize = 3;
    const CMD_LAUNCHER: usize = 4;
    const CMD_WS: usize = 100; // 100 + workspace index
    let h = key_hwnd(k);
    let Some((main, active, pinned, on_bar)) = with_app(|app| {
        let active = app.mons.get(m).map(|ms| ms.active).unwrap_or(0);
        let on_bar = (0..app.config.launchers.len()).any(|li| app.launcher_matches(li, k));
        (app.main, active, app.pinned.contains(&k), on_bar)
    }) else { return };
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let mut title = win::title(h);
        if title.chars().count() > 50 {
            title = title.chars().take(47).collect::<String>() + "…";
        }
        let t = wide(&title.replace('&', "&&"));
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, windows::core::PCWSTR(t.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
        let minimized = IsIconic(h).as_bool();
        let t = wide(if minimized { "Restore" } else { "Minimize" });
        let _ = AppendMenuW(menu, MF_STRING, CMD_MINRESTORE, windows::core::PCWSTR(t.as_ptr()));
        let t = wide("Pin to all workspaces");
        let _ = AppendMenuW(menu, MF_STRING | if pinned { MF_CHECKED } else { MF_UNCHECKED }, CMD_PIN, windows::core::PCWSTR(t.as_ptr()));
        let t = wide(if on_bar { "Unpin from bar" } else { "Pin to bar" });
        let _ = AppendMenuW(menu, MF_STRING, CMD_LAUNCHER, windows::core::PCWSTR(t.as_ptr()));
        if let Ok(sub) = CreatePopupMenu() {
            for w in 0..workspaces() {
                let t = wide(&format!("Workspace {}", w + 1));
                let flags = if w == active { MF_STRING | MF_CHECKED | MF_GRAYED } else { MF_STRING };
                let _ = AppendMenuW(sub, flags, CMD_WS + w, windows::core::PCWSTR(t.as_ptr()));
            }
            let t = wide("Move to workspace");
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, windows::core::PCWSTR(t.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
        let t = wide("Close");
        let _ = AppendMenuW(menu, MF_STRING, CMD_CLOSE, windows::core::PCWSTR(t.as_ptr()));

        let pt = cursor();
        let _ = SetForegroundWindow(main);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, main, None).0 as usize;
        let _ = PostMessageW(main, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd {
            CMD_MINRESTORE => {
                if minimized {
                    SwitchToThisWindow(h, true);
                } else {
                    let _ = ShowWindow(h, SW_MINIMIZE);
                }
            }
            CMD_PIN => {
                with_app(|app| {
                    if !app.pinned.remove(&k) {
                        app.pinned.insert(k);
                    }
                    app.save();
                    app.bars_refresh();
                });
            }
            CMD_CLOSE => {
                let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            CMD_LAUNCHER => {
                with_app(|app| app.toggle_launcher(k));
            }
            c if (CMD_WS..CMD_WS + workspaces()).contains(&c) => {
                with_app(|app| {
                    app.move_window(k, (m, active), (m, c - CMD_WS));
                    app.bars_refresh();
                });
            }
            _ => {}
        }
    }
}

/// Show a popup menu at the cursor and return the chosen command (0 = none).
unsafe fn run_menu(menu: HMENU) -> usize {
    let Some(main) = with_app(|app| app.main) else { return 0 };
    let pt = cursor();
    let _ = SetForegroundWindow(main);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, main, None).0 as usize;
    let _ = PostMessageW(main, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    cmd
}

fn wide_z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// What an exit node menu item does.
enum ExitPick {
    None,
    Suggested,
    Node(String),
    Best(String, Option<String>),
    LanAccess(bool),
}

/// Tailscale's menu (left or right click on its icon): connect/disconnect, exit node, IP.
fn tailscale_menu() {
    use crate::tailscale::State;
    const CMD_TOGGLE: usize = 1;
    const CMD_COPY_IP: usize = 2;
    const CMD_APP: usize = 3;
    const CMD_ADMIN: usize = 4;
    const CMD_UNATTENDED: usize = 5;
    const CMD_PICK: usize = 1000; // 1000 + index into picks
    let st = crate::tailscale::status().unwrap_or_default();
    // Refresh the exit node list in the background for next time (online states change).
    std::thread::spawn(crate::tailscale::load_exit_nodes);
    let nodes = crate::tailscale::exit_nodes();
    let active = st.exit.as_ref().map(|(id, _)| id.clone());
    let active_node = active.as_ref().and_then(|id| nodes.iter().find(|n| &n.id == id).cloned());
    let mut picks: Vec<ExitPick> = Vec::new();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let add = |m: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
            let t = wide_z(&text.replace('&', "&&"));
            let _ = AppendMenuW(m, flags, id, windows::core::PCWSTR(t.as_ptr()));
        };
        let add_sub = |m: HMENU, sub: HMENU, flags: MENU_ITEM_FLAGS, text: &str| {
            let t = wide_z(&text.replace('&', "&&"));
            let _ = AppendMenuW(m, MF_POPUP | flags, sub.0 as usize, windows::core::PCWSTR(t.as_ptr()));
        };
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let head = match st.state {
            State::Running if !st.tailnet.is_empty() => format!("Connected to {}", st.tailnet),
            State::Running => "Connected".into(),
            State::Starting => "Connecting…".into(),
            State::NeedsLogin => "Needs login".into(),
            State::Stopped => "Disconnected".into(),
            State::Unavailable => "Tailscale service isn't running".into(),
        };
        add(menu, MF_STRING | MF_GRAYED, 0, &head);
        if let Some(l) = st.exit_label() {
            add(menu, MF_STRING | MF_GRAYED, 0, &format!("Exit node: {l}"));
        }
        for w in st.warnings() {
            let w: String = if w.chars().count() > 90 { w.chars().take(87).collect::<String>() + "…" } else { w };
            add(menu, MF_STRING | MF_GRAYED, 0, &format!("⚠ {w}"));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
        match st.state {
            State::Running | State::Starting => add(menu, MF_STRING, CMD_TOGGLE, "Disconnect"),
            State::Stopped => add(menu, MF_STRING, CMD_TOGGLE, "Connect"),
            State::NeedsLogin => add(menu, MF_STRING, CMD_APP, "Log in…"),
            State::Unavailable => {}
        }
        if st.state == State::Running {
            if !st.ip.is_empty() {
                add(menu, MF_STRING, CMD_COPY_IP, &format!("Copy this device's IP ({})", st.ip));
            }
            if !nodes.is_empty() || active.is_some() {
                if let Ok(sub) = CreatePopupMenu() {
                    let mut pick = |p: ExitPick| {
                        picks.push(p);
                        CMD_PICK + picks.len() - 1
                    };
                    add(sub, MF_STRING | check(active.is_none()), pick(ExitPick::None), "None");
                    add(sub, MF_STRING, pick(ExitPick::Suggested), "Fastest (Tailscale's suggestion)");
                    // Your own exit nodes, by name.
                    let own: Vec<&crate::tailscale::ExitNode> = nodes.iter().filter(|n| !n.is_located()).collect();
                    if !own.is_empty() {
                        let _ = AppendMenuW(sub, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
                        for n in own {
                            let on = active.as_deref() == Some(n.id.as_str());
                            let label = if n.online { n.name.clone() } else { format!("{} (offline)", n.name) };
                            let flags = MF_STRING | check(on) | if n.online || on { MF_ENABLED } else { MF_GRAYED };
                            add(sub, flags, pick(ExitPick::Node(n.id.clone())), &label);
                        }
                    }
                    // Located ones (Mullvad): Country → City; picking one uses its best server.
                    let mut countries: Vec<&str> = nodes.iter().filter(|n| n.is_located()).map(|n| n.country.as_str()).collect();
                    countries.dedup();
                    if !countries.is_empty() {
                        let _ = AppendMenuW(sub, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
                    }
                    for country in countries {
                        let in_country: Vec<&crate::tailscale::ExitNode> = nodes.iter().filter(|n| n.country == country).collect();
                        let any_online = in_country.iter().any(|n| n.online);
                        let here = active_node.as_ref().is_some_and(|a| a.country == country);
                        let mut cities: Vec<&str> = in_country.iter().map(|n| n.city.as_str()).collect();
                        cities.dedup();
                        let dim = if any_online { MF_ENABLED } else { MF_GRAYED };
                        if cities.len() <= 1 {
                            add(sub, MF_STRING | check(here) | dim, pick(ExitPick::Best(country.to_string(), None)), country);
                            continue;
                        }
                        let Ok(csub) = CreatePopupMenu() else { continue };
                        add(csub, MF_STRING | dim, pick(ExitPick::Best(country.to_string(), None)), &format!("Any city in {country}"));
                        let _ = AppendMenuW(csub, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
                        for city in cities {
                            let online = in_country.iter().any(|n| n.city == city && n.online);
                            let on = here && active_node.as_ref().is_some_and(|a| a.city == city);
                            let flags = MF_STRING | check(on) | if online { MF_ENABLED } else { MF_GRAYED };
                            add(csub, flags, pick(ExitPick::Best(country.to_string(), Some(city.to_string()))), city);
                        }
                        add_sub(sub, csub, check(here) | dim, country);
                    }
                    let _ = AppendMenuW(sub, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
                    add(sub, MF_STRING | check(st.lan_access), pick(ExitPick::LanAccess(!st.lan_access)), "Allow local network access");
                    add_sub(menu, sub, check(active.is_some()), "Exit node");
                }
            }
        }
        if st.state != State::Unavailable {
            add(menu, MF_STRING | check(st.unattended), CMD_UNATTENDED, "Stay connected without LastDesk (unattended)");
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
        add(menu, MF_STRING, CMD_APP, "Open Tailscale app");
        add(menu, MF_STRING, CMD_ADMIN, "Admin console");
        match run_menu(menu) {
            CMD_TOGGLE => crate::tailscale::set_running(st.state == State::Stopped),
            CMD_COPY_IP => crate::win::set_clipboard_text(&st.ip),
            CMD_UNATTENDED => crate::tailscale::set_unattended(!st.unattended),
            CMD_APP => {
                if let Some(p) = crate::tailscale::app_path() {
                    let t = wide_z(&p);
                    ShellExecuteW(None, w!("open"), windows::core::PCWSTR(t.as_ptr()), None, None, SW_SHOWNORMAL);
                }
            }
            CMD_ADMIN => {
                ShellExecuteW(None, w!("open"), w!("https://login.tailscale.com/admin/machines"), None, None, SW_SHOWNORMAL);
            }
            c if c >= CMD_PICK && c < CMD_PICK + picks.len() => match &picks[c - CMD_PICK] {
                ExitPick::None => crate::tailscale::set_exit_node(None),
                ExitPick::Suggested => crate::tailscale::use_suggested(),
                ExitPick::Node(id) => crate::tailscale::set_exit_node(Some(id)),
                ExitPick::Best(country, city) => match crate::tailscale::best_node(country, city.as_deref()) {
                    Some(id) => crate::tailscale::set_exit_node(Some(&id)),
                    None => crate::message("No server there is online right now.", true),
                },
                ExitPick::LanAccess(on) => crate::tailscale::set_lan_access(*on),
            },
            _ => {}
        }
    }
}

/// Right-click menu for a pinned app's launcher button.
fn launcher_menu(li: usize) {
    const CMD_OPEN: usize = 1;
    const CMD_UNPIN: usize = 2;
    let Some(main) = with_app(|app| app.main) else { return };
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let t = wide("Open");
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, windows::core::PCWSTR(t.as_ptr()));
        let t = wide("Unpin from bar");
        let _ = AppendMenuW(menu, MF_STRING, CMD_UNPIN, windows::core::PCWSTR(t.as_ptr()));
        let pt = cursor();
        let _ = SetForegroundWindow(main);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, main, None).0 as usize;
        let _ = PostMessageW(main, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd {
            CMD_OPEN => {
                with_app(|app| {
                    let m = crate::win::monitor_name_at_cursor().and_then(|n| app.mon_index(&n)).unwrap_or(0);
                    if let Some(l) = app.config.launchers.get(li).cloned() {
                        app.note_start_opened(m);
                        crate::launcher::launch(&l.target);
                    }
                });
            }
            CMD_UNPIN => {
                with_app(|app| app.remove_launcher(li));
            }
            _ => {}
        }
    }
}