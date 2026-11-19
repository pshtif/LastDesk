//! Overview: a full-screen panel on every monitor showing its workspaces as columns with
//! live window thumbnails. Click a window to jump to it, click a workspace header to switch,
//! drag a window onto another column (any monitor) to move it, type to filter, Esc to close.
//!
//! Thumbnails are DWM live thumbnails (like taskbar previews): the compositor draws the
//! window into our rectangle. Nothing is captured, copied or stored by monodesk.

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_BACK, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ui;
use crate::win::{self, hwnd_key, key_hwnd};
use crate::{monitor_labels, with_app, App, WORKSPACES};

pub const CLASS: windows::core::PCWSTR = w!("MonodeskOverview");

pub struct Card {
    pub hwnd: isize,
    pub rect: RECT,
    pub thumb_rect: RECT,
    pub thumb: isize,
    pub title: String,
    pub app: String,
    pub icon: HICON,
    pub pinned: bool,
}

pub struct Column {
    pub ws: usize,
    pub header: RECT,
    pub body: RECT,
    pub cards: Vec<Card>,
}

pub struct Overlay {
    pub hwnd: HWND,
    pub mon: usize,
    pub origin: POINT,
    pub w: i32,
    pub h: i32,
    pub scale: f32,
    pub columns: Vec<Column>,
}

pub struct Drag {
    pub hwnd: isize,
    pub from: (usize, usize),
    pub start: POINT,
    pub cur: POINT,
    pub active: bool,
    pub label: String,
}

pub struct Overview {
    pub overlays: Vec<Overlay>,
    pub filter: String,
    pub drag: Option<Drag>,
}

enum Hit {
    Card(usize, usize, isize),
    Header(usize, usize),
    Column(usize, usize),
    None,
}

fn contains(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

fn app_name(h: HWND) -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let Ok(proc_) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(proc_, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(proc_);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let file = path.rsplit('\\').next().unwrap_or(&path);
        let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
        let mut c = stem.chars();
        match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => String::new(),
        }
    }
}

fn window_icon(h: HWND) -> HICON {
    unsafe {
        let mut res: usize = 0;
        for kind in [ICON_SMALL2, ICON_SMALL, ICON_BIG] {
            let _ = SendMessageTimeoutW(h, WM_GETICON, WPARAM(kind as usize), LPARAM(0), SMTO_ABORTIFHUNG, 50, Some(&mut res));
            if res != 0 {
                return HICON(res as *mut _);
            }
        }
        let c = GetClassLongPtrW(h, GCLP_HICONSM);
        if c != 0 {
            return HICON(c as *mut _);
        }
        HICON(GetClassLongPtrW(h, GCLP_HICON) as *mut _)
    }
}

impl Overview {
    fn matches(&self, c: &Card) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let f = self.filter.to_lowercase();
        c.title.to_lowercase().contains(&f) || c.app.to_lowercase().contains(&f)
    }

    fn hit(&self, p: POINT) -> Hit {
        for ov in &self.overlays {
            let local = POINT { x: p.x - ov.origin.x, y: p.y - ov.origin.y };
            if local.x < 0 || local.y < 0 || local.x >= ov.w || local.y >= ov.h {
                continue;
            }
            for col in &ov.columns {
                if contains(&col.header, local) {
                    return Hit::Header(ov.mon, col.ws);
                }
                for card in &col.cards {
                    if contains(&card.rect, local) {
                        return Hit::Card(ov.mon, col.ws, card.hwnd);
                    }
                }
                if contains(&col.body, local) {
                    return Hit::Column(ov.mon, col.ws);
                }
            }
        }
        Hit::None
    }

    fn invalidate(&self) {
        for ov in &self.overlays {
            unsafe {
                let _ = InvalidateRect(ov.hwnd, None, false);
            }
        }
    }

    fn unregister_thumbs(&mut self) {
        for ov in &mut self.overlays {
            for col in &mut ov.columns {
                for c in &mut col.cards {
                    if c.thumb != 0 {
                        unsafe {
                            let _ = DwmUnregisterThumbnail(c.thumb);
                        }
                        c.thumb = 0;
                    }
                }
            }
        }
    }

    fn apply_opacity(&self) {
        for ov in &self.overlays {
            for col in &ov.columns {
                for c in &col.cards {
                    if c.thumb != 0 {
                        set_thumb(c.thumb, c.thumb_rect, if self.matches(c) { 255 } else { 45 });
                    }
                }
            }
        }
        self.invalidate();
    }
}

fn set_thumb(thumb: isize, r: RECT, opacity: u8) {
    let props = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
        rcDestination: r,
        opacity,
        fVisible: true.into(),
        fSourceClientAreaOnly: false.into(),
        ..Default::default()
    };
    unsafe {
        let _ = DwmUpdateThumbnailProperties(thumb, &props);
    }
}

impl App {
    /// Windows per workspace on monitor `m`: (hwnd, pinned).
    fn windows_by_ws(&self, m: usize) -> Vec<Vec<(isize, bool)>> {
        let name = &self.mons[m].name;
        (0..WORKSPACES)
            .map(|w| {
                if w == self.mons[m].active {
                    win::visible_windows()
                        .into_iter()
                        .filter(|h| win::monitor_name_of_window(*h).as_deref() == Some(name.as_str()))
                        .map(|h| (hwnd_key(h), self.pinned.contains(&hwnd_key(h))))
                        .collect()
                } else {
                    self.mons[m].hidden[w].iter().map(|k| (*k, false)).collect()
                }
            })
            .collect()
    }

    pub fn overview_toggle(&mut self) {
        if self.overview.is_some() {
            self.overview_close();
        } else {
            self.overview_open();
        }
    }

    pub fn overview_open(&mut self) {
        self.prune();
        let cur = self.current_mon();
        let mut overlays = Vec::new();
        unsafe {
            let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
            for (m, mon) in self.monitors.iter().enumerate() {
                let r = mon.rect;
                let Ok(hwnd) = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                    CLASS,
                    w!("monodesk overview"),
                    WS_POPUP,
                    r.left, r.top, r.right - r.left, r.bottom - r.top,
                    None, None, hinst, None,
                ) else { continue };
                let (mut dx, mut dy) = (96u32, 96u32);
                let hm = MonitorFromPoint(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }, MONITOR_DEFAULTTONEAREST);
                let _ = GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
                overlays.push(Overlay {
                    hwnd,
                    mon: m,
                    origin: POINT { x: r.left, y: r.top },
                    w: r.right - r.left,
                    h: r.bottom - r.top,
                    scale: dx as f32 / 96.0,
                    columns: Vec::new(),
                });
            }
        }
        if overlays.is_empty() {
            return;
        }
        self.overview = Some(Overview { overlays, filter: String::new(), drag: None });
        self.overview_layout();
        unsafe {
            let ov = self.overview.as_ref().unwrap();
            let mut focus = ov.overlays[0].hwnd;
            for o in &ov.overlays {
                let _ = ShowWindow(o.hwnd, SW_SHOWNA);
                if o.mon == cur {
                    focus = o.hwnd;
                }
            }
            let _ = SetForegroundWindow(focus);
            let _ = SetFocus(focus);
        }
    }

    pub fn overview_close(&mut self) {
        if let Some(mut ov) = self.overview.take() {
            ov.unregister_thumbs();
            unsafe {
                let _ = ReleaseCapture();
                for o in &ov.overlays {
                    let _ = DestroyWindow(o.hwnd);
                }
            }
        }
    }

    /// (Re)compute columns and cards for every overlay and (re)register thumbnails.
    pub fn overview_layout(&mut self) {
        let data: Vec<Vec<Vec<(isize, bool)>>> = (0..self.mons.len()).map(|m| self.windows_by_ws(m)).collect();
        let Some(ov) = self.overview.as_mut() else { return };
        ov.unregister_thumbs();
        for o in &mut ov.overlays {
            let s = |v: f32| (v * o.scale).round() as i32;
            let (pad, top_h, header_h, gap, title_h, inner) = (s(28.0), s(56.0), s(44.0), s(12.0), s(28.0), s(10.0));
            let n = WORKSPACES as i32;
            let cw = (o.w - pad * (n + 1)) / n;
            let windows = &data[o.mon];
            o.columns = (0..WORKSPACES)
                .map(|w| {
                    let x = pad + w as i32 * (cw + pad);
                    let header = RECT { left: x, top: top_h, right: x + cw, bottom: top_h + header_h };
                    let body = RECT { left: x, top: header.bottom + gap, right: x + cw, bottom: o.h - pad };
                    let list = &windows[w];
                    let k = list.len() as i32;
                    let avail = body.bottom - body.top - inner * 2;
                    let ideal = (cw as f32 * 0.62) as i32 + title_h + inner * 2;
                    let card_h = if k == 0 { 0 } else { ideal.min((avail - gap * (k - 1)) / k).max(title_h + inner * 2) };
                    let cards = list
                        .iter()
                        .enumerate()
                        .map(|(i, (key, pinned))| {
                            let h = key_hwnd(*key);
                            let top = body.top + inner + i as i32 * (card_h + gap);
                            let rect = RECT { left: x + inner, top, right: x + cw - inner, bottom: top + card_h };
                            let area = RECT {
                                left: rect.left + inner,
                                top: rect.top + inner,
                                right: rect.right - inner,
                                bottom: rect.bottom - inner - title_h,
                            };
                            let mut thumb = 0isize;
                            let mut thumb_rect = RECT::default();
                            if area.bottom - area.top > s(24.0) {
                                if let Ok(t) = unsafe { DwmRegisterThumbnail(o.hwnd, h) } {
                                    let src = unsafe { DwmQueryThumbnailSourceSize(t) }.unwrap_or(SIZE { cx: 16, cy: 9 });
                                    let (aw, ah) = ((area.right - area.left) as f32, (area.bottom - area.top) as f32);
                                    let (sw, sh) = (src.cx.max(1) as f32, src.cy.max(1) as f32);
                                    let k = (aw / sw).min(ah / sh);
                                    let (tw, th) = ((sw * k) as i32, (sh * k) as i32);
                                    let tl = area.left + ((area.right - area.left) - tw) / 2;
                                    let tt = area.top + ((area.bottom - area.top) - th) / 2;
                                    thumb_rect = RECT { left: tl, top: tt, right: tl + tw, bottom: tt + th };
                                    thumb = t;
                                }
                            }
                            Card {
                                hwnd: *key,
                                rect,
                                thumb_rect,
                                thumb,
                                title: win::title(h),
                                app: app_name(h),
                                icon: window_icon(h),
                                pinned: *pinned,
                            }
                        })
                        .collect();
                    Column { ws: w, header, body, cards }
                })
                .collect();
        }
        ov.apply_opacity();
    }

    pub fn overview_paint(&self, hwnd: HWND, hdc: HDC) {
        let Some(ov) = self.overview.as_ref() else { return };
        let Some(o) = ov.overlays.iter().find(|o| o.hwnd == hwnd) else { return };
        let labels = monitor_labels(self.mons.len());
        let active = self.mons.get(o.mon).map(|m| m.active).unwrap_or(0);
        let s = |v: f32| (v * o.scale).round() as i32;
        unsafe {
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, o.w, o.h);
            let old = SelectObject(mem, bmp);
            let brush = CreateSolidBrush(ui::rgb(0x12, 0x13, 0x16));
            FillRect(mem, &RECT { left: 0, top: 0, right: o.w, bottom: o.h }, brush);
            let _ = DeleteObject(brush);

            let f_label = ui::font(s(15.0), true);
            let f_hint = ui::font(s(13.0), false);
            let f_head = ui::font(s(18.0), true);
            let f_app = ui::font(s(13.0), true);
            let f_title = ui::font(s(12.0), false);

            // Top row: monitor name left, filter / hint centered.
            let mut r = RECT { left: s(28.0), top: s(14.0), right: o.w / 3, bottom: s(46.0) };
            ui::draw_text_left(mem, labels.get(o.mon).copied().unwrap_or("Monitor"), &mut r, ui::TEXT, f_label);
            let mut r = RECT { left: o.w / 4, top: s(14.0), right: o.w * 3 / 4, bottom: s(46.0) };
            if ov.filter.is_empty() {
                ui::draw_text(mem, "Click to jump · drag to move · type to filter · Esc to close", &mut r, ui::MUTED, f_hint);
            } else {
                ui::draw_text(mem, &format!("Filter: {}", ov.filter), &mut r, ui::ACCENT, f_label);
            }

            // Drop target under the cursor while dragging.
            let target = ov.drag.as_ref().filter(|d| d.active).and_then(|d| match ov.hit(d.cur) {
                Hit::Card(m, w, _) | Hit::Header(m, w) | Hit::Column(m, w) => Some((m, w)),
                Hit::None => None,
            });

            for col in &o.columns {
                let is_active = col.ws == active;
                let is_target = target == Some((o.mon, col.ws));
                ui::fill_round(mem, col.body, s(14.0), ui::rgb(0x1B, 0x1C, 0x20));
                if is_target {
                    ui::frame_round(mem, col.body, s(14.0), s(3.0).max(2), ui::ACCENT);
                }
                ui::fill_round(mem, col.header, s(12.0), if is_active { ui::ACCENT } else { ui::CELL });
                let mut hr = col.header;
                let label = if col.cards.is_empty() {
                    format!("{}", col.ws + 1)
                } else {
                    format!("{}   ·   {} window{}", col.ws + 1, col.cards.len(), if col.cards.len() == 1 { "" } else { "s" })
                };
                ui::draw_text(mem, &label, &mut hr, ui::TEXT, f_head);
                if col.cards.is_empty() {
                    let mut er = col.body;
                    ui::draw_text(mem, "empty", &mut er, ui::rgb(0x55, 0x58, 0x5E), f_hint);
                }
                for c in &col.cards {
                    let dim = !ov.matches(c);
                    ui::fill_round(mem, c.rect, s(10.0), ui::rgb(0x26, 0x27, 0x2C));
                    let inner = s(10.0);
                    let title_h = s(28.0);
                    let ty = c.rect.bottom - inner - title_h;
                    let icon = s(16.0);
                    if !c.icon.is_invalid() {
                        let _ = DrawIconEx(mem, c.rect.left + inner, ty + (title_h - icon) / 2, c.icon, icon, icon, 0, None, DI_NORMAL);
                    }
                    let app = if c.pinned { format!("{} (pinned)", c.app) } else { c.app.clone() };
                    let text_left = c.rect.left + inner + icon + s(8.0);
                    let app_w = (c.rect.right - text_left) / 3;
                    let mut ar = RECT { left: text_left, top: ty, right: text_left + app_w, bottom: ty + title_h };
                    ui::draw_text_left(mem, &app, &mut ar, if dim { ui::rgb(0x55, 0x58, 0x5E) } else { ui::TEXT }, f_app);
                    let mut tr = RECT { left: text_left + app_w + s(6.0), top: ty, right: c.rect.right - inner, bottom: ty + title_h };
                    ui::draw_text_left(mem, &c.title, &mut tr, if dim { ui::rgb(0x45, 0x48, 0x4E) } else { ui::MUTED }, f_title);
                    if c.thumb == 0 && c.rect.bottom - c.rect.top > title_h + inner * 3 {
                        let mut nr = RECT { bottom: ty, ..c.rect };
                        ui::draw_text(mem, "no preview", &mut nr, ui::rgb(0x55, 0x58, 0x5E), f_hint);
                    }
                }
            }

            // Drag ghost follows the cursor on whichever monitor it is.
            if let Some(d) = ov.drag.as_ref().filter(|d| d.active) {
                let lx = d.cur.x - o.origin.x;
                let ly = d.cur.y - o.origin.y;
                if lx >= 0 && ly >= 0 && lx < o.w && ly < o.h {
                    let g = RECT { left: lx + s(12.0), top: ly + s(12.0), right: lx + s(232.0), bottom: ly + s(52.0) };
                    ui::fill_round(mem, g, s(10.0), ui::ACCENT);
                    let mut gr = RECT { left: g.left + s(12.0), right: g.right - s(12.0), ..g };
                    ui::draw_text_left(mem, &d.label, &mut gr, ui::TEXT, f_app);
                }
            }

            for f in [f_label, f_hint, f_head, f_app, f_title] {
                let _ = DeleteObject(f);
            }
            let _ = BitBlt(hdc, 0, 0, o.w, o.h, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(mem);
        }
    }

    fn overview_jump(&mut self, m: usize, w: usize, k: Option<isize>) {
        self.overview_close();
        if m >= self.mons.len() {
            return;
        }
        if w != self.mons[m].active {
            self.switch_to(m, w, None);
        }
        if let Some(k) = k {
            let h = key_hwnd(k);
            unsafe {
                if IsIconic(h).as_bool() {
                    let _ = ShowWindow(h, SW_RESTORE);
                }
                let _ = SetForegroundWindow(h);
            }
        }
    }

    pub fn overview_mouse_down(&mut self, overlay: HWND, p: POINT) {
        let Some(ov) = self.overview.as_mut() else { return };
        match ov.hit(p) {
            Hit::Card(m, w, k) => {
                let label = ov
                    .overlays
                    .iter()
                    .flat_map(|o| o.columns.iter().flat_map(|c| c.cards.iter()))
                    .find(|c| c.hwnd == k)
                    .map(|c| if c.app.is_empty() { c.title.clone() } else { c.app.clone() })
                    .unwrap_or_default();
                ov.drag = Some(Drag { hwnd: k, from: (m, w), start: p, cur: p, active: false, label });
                unsafe {
                    SetCapture(overlay);
                }
            }
            Hit::Header(m, w) => self.overview_jump(m, w, None),
            Hit::Column(_, _) => {}
            Hit::None => self.overview_close(),
        }
    }

    pub fn overview_mouse_move(&mut self, p: POINT) {
        let Some(ov) = self.overview.as_mut() else { return };
        let Some(d) = ov.drag.as_mut() else { return };
        d.cur = p;
        if !d.active && ((p.x - d.start.x).abs() > 8 || (p.y - d.start.y).abs() > 8) {
            d.active = true;
        }
        if d.active {
            ov.invalidate();
        }
    }

    pub fn overview_mouse_up(&mut self, p: POINT) {
        unsafe {
            let _ = ReleaseCapture();
        }
        let Some(ov) = self.overview.as_mut() else { return };
        let Some(d) = ov.drag.take() else { return };
        if !d.active {
            self.overview_jump(d.from.0, d.from.1, Some(d.hwnd));
            return;
        }
        let to = match ov.hit(p) {
            Hit::Card(m, w, _) | Hit::Header(m, w) | Hit::Column(m, w) => Some((m, w)),
            Hit::None => None,
        };
        if let Some(to) = to {
            self.move_window(d.hwnd, d.from, to);
        }
        self.overview_layout();
    }

    pub fn overview_key(&mut self, vk: u32) {
        let Some(ov) = self.overview.as_mut() else { return };
        if vk == VK_ESCAPE.0 as u32 {
            if ov.filter.is_empty() {
                self.overview_close();
            } else {
                ov.filter.clear();
                ov.apply_opacity();
            }
        } else if vk == VK_BACK.0 as u32 {
            ov.filter.pop();
            ov.apply_opacity();
        } else if vk == VK_RETURN.0 as u32 {
            let first = ov.overlays.iter().find_map(|o| {
                o.columns.iter().find_map(|c| c.cards.iter().find(|card| ov.matches(card)).map(|card| (o.mon, c.ws, card.hwnd)))
            });
            if let Some((m, w, k)) = first {
                self.overview_jump(m, w, Some(k));
            }
        }
    }

    pub fn overview_char(&mut self, ch: u32) {
        let Some(ov) = self.overview.as_mut() else { return };
        if let Some(c) = char::from_u32(ch).filter(|c| !c.is_control()) {
            ov.filter.push(c);
            ov.apply_opacity();
        }
    }
}

pub unsafe extern "system" fn overview_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let cursor = || {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        p
    };
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            with_app(|app| app.overview_paint(hwnd, hdc));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            let p = cursor();
            with_app(|app| app.overview_mouse_down(hwnd, p));
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let p = cursor();
            with_app(|app| app.overview_mouse_move(p));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let p = cursor();
            with_app(|app| app.overview_mouse_up(p));
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            with_app(|app| app.overview_close());
            LRESULT(0)
        }
        WM_KEYDOWN => {
            with_app(|app| app.overview_key(wp.0 as u32));
            LRESULT(0)
        }
        WM_CHAR => {
            with_app(|app| app.overview_char(wp.0 as u32));
            LRESULT(0)
        }
        WM_ACTIVATEAPP => {
            if wp.0 == 0 {
                with_app(|app| app.overview_close());
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
