//! LastDesk bar: a thin taskbar on top of every monitor.
//!
//!   [Start] [1][2][3][4] | window buttons of THIS monitor + workspace ...   battery  clock
//!
//! Registered as an app bar (SHAppBarMessage) so maximized windows stop below it. Window
//! buttons: click = focus (or minimize if already focused), middle-click = close.
//! Clicking the clock opens notifications/calendar (Win+N), battery opens quick settings (Win+A).

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

#[derive(Clone, Copy, PartialEq)]
pub enum Hit {
    Start,
    Workspace(usize),
    Window(isize),
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
}

pub struct Bar {
    pub hwnd: HWND,
    pub mon: usize,
    pub rect: RECT,
    pub scale: f32,
    pub start: RECT,
    pub cells: Vec<RECT>,
    pub buttons: Vec<Button>,
    pub battery: RECT,
    pub clock: RECT,
    /// Button order: first-seen, so buttons don't jump around with z-order.
    pub order: Vec<isize>,
    pub hover: Hit,
    pub tracking: bool,
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
    fn hit(&self, x: i32, y: i32) -> Hit {
        if contains(&self.start, x, y) {
            return Hit::Start;
        }
        if let Some(i) = self.cells.iter().position(|r| contains(r, x, y)) {
            return Hit::Workspace(i);
        }
        if let Some(b) = self.buttons.iter().find(|b| contains(&b.rect, x, y)) {
            return Hit::Window(b.hwnd);
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
        (BASE_HEIGHT * self.scale).round() as i32
    }

    /// Reserve the strip at the top of the monitor and move the bar there.
    fn place(&mut self, monitor: RECT) {
        let h = self.height();
        let mut abd = appbar_data(self.hwnd, RECT { left: monitor.left, top: monitor.top, right: monitor.right, bottom: monitor.top + h });
        unsafe {
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            abd.rc.bottom = abd.rc.top + h;
            SHAppBarMessage(ABM_SETPOS, &mut abd);
            self.rect = abd.rc;
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, abd.rc.left, abd.rc.top, abd.rc.right - abd.rc.left, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
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
                buttons: Vec::new(),
                battery: RECT::default(),
                clock: RECT::default(),
                order: Vec::new(),
                hover: Hit::None,
                tracking: false,
            };
            bar.place(r);
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
                SHAppBarMessage(ABM_REMOVE, &mut abd);
                let _ = DestroyWindow(b.hwnd);
            }
        }
        self.refresh_work_areas();
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
        let monitors = self.monitors.clone();
        if let Some(b) = self.bars.iter_mut().find(|b| b.hwnd == hwnd) {
            if let Some(mon) = monitors.get(b.mon) {
                b.place(mon.rect);
            }
        }
        self.refresh_work_areas();
        self.bars_refresh();
    }

    pub fn bar_fullscreen(&mut self, fullscreen: bool) {
        for b in &self.bars {
            unsafe {
                let _ = SetWindowPos(b.hwnd, if fullscreen { HWND_BOTTOM } else { HWND_TOPMOST }, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    /// Recompute buttons and layout for every bar, then repaint.
    pub fn bars_refresh(&mut self) {
        if self.bars.is_empty() {
            return;
        }
        let all = win::visible_windows();
        let n = workspaces();
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
            let b = &mut self.bars[bi];
            b.order.retain(|k| here.contains(k));
            for k in &here {
                if !b.order.contains(k) {
                    b.order.push(*k);
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
            let start_x = x + s(10.0);
            let avail = (b.battery.left - s(10.0) - start_x).max(0);
            let count = b.order.len() as i32;
            let bw = if count == 0 { 0 } else { s(220.0).min((avail - s(4.0) * (count - 1)) / count).max(s(34.0)) };
            let mut bx = start_x;
            b.buttons = b
                .order
                .iter()
                .filter_map(|k| {
                    if bx + bw > start_x + avail {
                        return None;
                    }
                    let hw = key_hwnd(*k);
                    let r = RECT { left: bx, top: s(3.0), right: bx + bw, bottom: h - s(3.0) };
                    bx += bw + s(4.0);
                    Some(Button {
                        hwnd: *k,
                        rect: r,
                        title: win::title(hw),
                        icon: crate::overview::window_icon(hw),
                        minimized: unsafe { IsIconic(hw).as_bool() },
                    })
                })
                .collect();
            unsafe {
                let _ = InvalidateRect(b.hwnd, None, false);
            }
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
                let color = if active || occupied { ui::TEXT } else { ui::rgb(0x60, 0x63, 0x6A) };
                let mut tr = *r;
                ui::draw_text(mem, &(i + 1).to_string(), &mut tr, color, f_num);
            }

            // Window buttons.
            for btn in &b.buttons {
                let is_fg = hwnd_key(fg) == btn.hwnd;
                let bg = if is_fg { ui::rgb(0x2C, 0x3D, 0x5E) } else if b.hover == Hit::Window(btn.hwnd) { hover_bg } else { ui::rgb(0x1F, 0x20, 0x24) };
                ui::fill_round(mem, btn.rect, s(6.0), bg);
                if is_fg {
                    let line = RECT { left: btn.rect.left + s(8.0), top: btn.rect.bottom - s(3.0), right: btn.rect.right - s(8.0), bottom: btn.rect.bottom - s(1.0) };
                    ui::fill_round(mem, line, s(2.0), ui::ACCENT);
                }
                let icon = s(16.0);
                let ix = btn.rect.left + s(8.0);
                if !btn.icon.is_invalid() {
                    let _ = DrawIconEx(mem, ix, (h - icon) / 2, btn.icon, icon, icon, 0, None, DI_NORMAL);
                }
                let mut tr = RECT { left: ix + icon + s(7.0), top: btn.rect.top, right: btn.rect.right - s(6.0), bottom: btn.rect.bottom };
                if tr.right - tr.left > s(12.0) {
                    ui::draw_text_left(mem, &btn.title, &mut tr, if btn.minimized { ui::MUTED } else { ui::TEXT }, f_text);
                }
            }

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

    fn bar_index(&self, hwnd: HWND) -> Option<usize> {
        self.bars.iter().position(|b| b.hwnd == hwnd)
    }

    pub fn bar_mouse_move(&mut self, hwnd: HWND, x: i32, y: i32) {
        let Some(i) = self.bar_index(hwnd) else { return };
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
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
        }
    }

    pub fn bar_mouse_leave(&mut self, hwnd: HWND) {
        let Some(i) = self.bar_index(hwnd) else { return };
        let b = &mut self.bars[i];
        b.tracking = false;
        b.hover = Hit::None;
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }

    /// Returns a key combo to press after the borrow ends (SendInput is fine either way,
    /// but keeping it outside avoids re-entrancy surprises).
    pub fn bar_click(&mut self, hwnd: HWND, x: i32, y: i32, middle: bool) -> Option<Vec<VIRTUAL_KEY>> {
        let i = self.bar_index(hwnd)?;
        let m = self.bars[i].mon;
        match self.bars[i].hit(x, y) {
            Hit::Start if !middle => return Some(vec![VK_LWIN]),
            Hit::Workspace(w) if !middle => self.switch_to(m, w, None),
            Hit::Window(k) => unsafe {
                let h = key_hwnd(k);
                if middle {
                    let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
                } else if GetForegroundWindow() == h && !IsIconic(h).as_bool() {
                    let _ = ShowWindow(h, SW_MINIMIZE);
                } else {
                    if IsIconic(h).as_bool() {
                        let _ = ShowWindow(h, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(h);
                }
            },
            Hit::Battery if !middle => return Some(vec![VK_LWIN, VIRTUAL_KEY('A' as u16)]),
            Hit::Clock if !middle => return Some(vec![VK_LWIN, VIRTUAL_KEY('N' as u16)]),
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
        WM_LBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = xy();
            let keys = with_app(|app| app.bar_click(hwnd, x, y, msg == WM_MBUTTONUP)).flatten();
            if let Some(k) = keys {
                press(&k);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            crate::show_menu(with_app(|a| a.main).unwrap_or(hwnd));
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        m if m == WM_APPBAR => {
            match wp.0 as u32 {
                ABN_POSCHANGED => {
                    with_app(|app| app.bar_reposition(hwnd));
                }
                ABN_FULLSCREENAPP => {
                    with_app(|app| app.bar_fullscreen(lp.0 != 0));
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
