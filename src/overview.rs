// Created by Peter @sHTiF Stefcek
//! Overview: a full-screen panel on every monitor showing its workspaces as columns with
//! live window thumbnails. Click a window to jump to it, click a workspace header to switch,
//! drag a window onto another column (any monitor) to move it, type to filter, Esc to close.
//!
//! Thumbnails are DWM live thumbnails (like taskbar previews): the compositor draws the
//! window into our rectangle. Nothing is captured, copied or stored by LastDesk.

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
use crate::{monitor_labels, with_app, workspaces, App};

pub const CLASS: windows::core::PCWSTR = w!("LastDeskOverview");

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
    /// The card's own thumbnail and where it sits (overlay-local), dimmed while dragging.
    pub orig_thumb: isize,
    pub orig_rect: RECT,
    /// Cursor offset inside the thumbnail, and its size.
    pub grab: POINT,
    pub size: SIZE,
    /// Live thumbnail following the cursor, registered on the overlay under it.
    pub ghost: isize,
    pub ghost_on: HWND,
    pub target: Option<(usize, usize)>,
}

impl Drag {
    fn drop_ghost(&mut self) {
        if self.ghost != 0 {
            unsafe {
                let _ = DwmUnregisterThumbnail(self.ghost);
            }
            self.ghost = 0;
            self.ghost_on = HWND::default();
        }
    }
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

fn drop_target(h: Hit) -> Option<(usize, usize)> {
    match h {
        Hit::Card(m, w, _) | Hit::Header(m, w) | Hit::Column(m, w) => Some((m, w)),
        Hit::None => None,
    }
}

fn contains(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

pub(crate) fn app_name(h: HWND) -> String {
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

pub(crate) fn window_icon(h: HWND) -> HICON {
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
        let c = GetClassLongPtrW(h, GCLP_HICON);
        if c != 0 {
            return HICON(c as *mut _);
        }
        store_app_icon(h)
    }
}

thread_local! {
    /// Store app icons by AppUserModelID, kept for good (one per app; callers borrow them).
    static STORE_ICONS: std::cell::RefCell<std::collections::HashMap<String, HICON>> = Default::default();
}

/// Store apps (Settings, Calculator…) live in a frame window that has no icon of its own;
/// their icon comes from the app's package, found by the window's AppUserModelID, the way
/// the taskbar does it.
fn store_app_icon(h: HWND) -> HICON {
    let aumid = crate::launcher::window_aumid(h);
    if aumid.is_empty() {
        return HICON::default();
    }
    STORE_ICONS.with(|cache| {
        if let Some(i) = cache.borrow().get(&aumid) {
            return *i;
        }
        let icon = crate::launcher::icon(&format!("shell:AppsFolder\\{aumid}"));
        // Remember misses too, so a window without one isn't looked up on every redraw.
        cache.borrow_mut().insert(aumid, icon);
        icon
    })
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
        (0..workspaces())
            .map(|w| {
                if w == self.mons[m].active {
                    win::visible_windows()
                        .into_iter()
                        .filter(|h| win::monitor_name_of_window(*h).as_deref() == Some(name.as_str()))
                        .map(|h| (hwnd_key(h), self.pinned.contains(&hwnd_key(h)) || self.is_pinned_app(h)))
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
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                    CLASS,
                    w!("LastDesk overview"),
                    WS_POPUP,
                    r.left, r.top, r.right - r.left, r.bottom - r.top,
                    None, None, hinst, None,
                ) else { continue };
                // Layered so the whole panel can fade in; fully opaque straight away if animation is off.
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), if self.config.animation > 0 { 0 } else { 255 }, LWA_ALPHA);
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
        self.overview_animate(true);
    }

    /// Zoom in (opening) or out (closing): windows of the shown workspace move between where
    /// they are on screen and their cards, the other cards grow/shrink and fade, and the panel
    /// fades. Blocking, synced to the display.
    fn overview_animate(&self, opening: bool) {
        let ms = self.config.animation;
        let Some(ov) = self.overview.as_ref() else { return };
        if ms == 0 {
            return;
        }
        // (thumb, from, to, fade)
        let mut items: Vec<(isize, RECT, RECT, bool)> = Vec::new();
        for o in &ov.overlays {
            let active = self.mons.get(o.mon).map(|m| m.active).unwrap_or(0);
            for col in &o.columns {
                for c in &col.cards {
                    if c.thumb == 0 {
                        continue;
                    }
                    let to = c.thumb_rect;
                    let h = key_hwnd(c.hwnd);
                    let mut wr = RECT::default();
                    let live = col.ws == active
                        && unsafe { !IsIconic(h).as_bool() && GetWindowRect(h, &mut wr).is_ok() };
                    if live {
                        let from = RECT {
                            left: wr.left - o.origin.x,
                            top: wr.top - o.origin.y,
                            right: wr.right - o.origin.x,
                            bottom: wr.bottom - o.origin.y,
                        };
                        items.push((c.thumb, from, to, false));
                    } else {
                        let (cx, cy) = ((to.left + to.right) / 2, (to.top + to.bottom) / 2);
                        let (hw, hh) = ((to.right - to.left) * 2 / 5, (to.bottom - to.top) * 2 / 5);
                        items.push((c.thumb, RECT { left: cx - hw, top: cy - hh, right: cx + hw, bottom: cy + hh }, to, true));
                    }
                }
            }
        }
        let lerp = |a: i32, b: i32, p: f32| a + ((b - a) as f32 * p).round() as i32;
        let frame = |p: f32| {
            for &(t, a, b, fade) in &items {
                let r = RECT {
                    left: lerp(a.left, b.left, p),
                    top: lerp(a.top, b.top, p),
                    right: lerp(a.right, b.right, p),
                    bottom: lerp(a.bottom, b.bottom, p),
                };
                set_thumb(t, r, if fade { (255.0 * p.sqrt()) as u8 } else { 255 });
            }
        };
        if opening {
            frame(0.0);
            // Paint the panels now: the normal WM_PAINT can't reach us until this returns.
            unsafe {
                for o in &ov.overlays {
                    let hdc = GetDC(o.hwnd);
                    self.overview_paint(o.hwnd, hdc);
                    ReleaseDC(o.hwnd, hdc);
                }
                let _ = DwmFlush();
            }
        }
        let fade_panels = |a: f32| {
            for o in &ov.overlays {
                unsafe {
                    let _ = SetLayeredWindowAttributes(o.hwnd, COLORREF(0), (255.0 * a) as u8, LWA_ALPHA);
                }
            }
        };
        let start = std::time::Instant::now();
        let total = (ms as f32 / 1000.0).max(0.001);
        loop {
            let t = (start.elapsed().as_secs_f32() / total).min(1.0);
            let e = 1.0 - (1.0 - t).powi(3); // ease-out
            if opening {
                frame(e);
                fade_panels((t * 1.6).min(1.0));
            } else {
                frame(1.0 - e);
                fade_panels(1.0 - t);
            }
            unsafe {
                let _ = DwmFlush();
            }
            if t >= 1.0 {
                break;
            }
        }
        if opening {
            ov.apply_opacity();
        }
    }

    pub fn overview_close(&mut self) {
        if let Some(ov) = self.overview.as_mut() {
            if let Some(mut d) = ov.drag.take() {
                d.drop_ghost();
            }
            unsafe {
                let _ = ReleaseCapture();
            }
            self.overview_animate(false);
        }
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
            let n = workspaces() as i32;
            let cw = (o.w - pad * (n + 1)) / n;
            let windows = &data[o.mon];
            o.columns = (0..workspaces())
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
            ui::draw_text_left(mem, labels.get(o.mon).map(|s| s.as_str()).unwrap_or("Monitor"), &mut r, ui::TEXT, f_label);
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
            if let Some(d) = ov.drag.as_ref().filter(|d| d.active && d.ghost == 0) {
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
        // Switch underneath first (no slide while the overview is open), so the zoom-out
        // lands the chosen workspace's windows where they really are.
        if m < self.mons.len() && w != self.mons[m].active {
            self.switch_to(m, w, None);
        }
        self.overview_close();
        if m >= self.mons.len() {
            return;
        }
        if let Some(k) = k {
            let h = key_hwnd(k);
            // Works for elevated windows (Task Manager) and restores minimized ones.
            unsafe { SwitchToThisWindow(h, true) };
        }
    }

    pub fn overview_mouse_down(&mut self, overlay: HWND, p: POINT) {
        let Some(ov) = self.overview.as_mut() else { return };
        match ov.hit(p) {
            Hit::Card(m, w, k) => {
                let found = ov
                    .overlays
                    .iter()
                    .flat_map(|o| o.columns.iter().flat_map(move |c| c.cards.iter().map(move |card| (o.origin, card))))
                    .find(|(_, c)| c.hwnd == k);
                let mut d = Drag {
                    hwnd: k,
                    from: (m, w),
                    start: p,
                    cur: p,
                    active: false,
                    label: String::new(),
                    orig_thumb: 0,
                    orig_rect: RECT::default(),
                    grab: POINT::default(),
                    size: SIZE::default(),
                    ghost: 0,
                    ghost_on: HWND::default(),
                    target: None,
                };
                if let Some((origin, c)) = found {
                    d.label = if c.app.is_empty() { c.title.clone() } else { c.app.clone() };
                    if c.thumb != 0 {
                        let r = c.thumb_rect;
                        d.orig_thumb = c.thumb;
                        d.orig_rect = r;
                        d.size = SIZE { cx: r.right - r.left, cy: r.bottom - r.top };
                        // Keep the grab point inside the thumbnail even if the click was on the title.
                        d.grab = POINT {
                            x: (p.x - origin.x - r.left).clamp(0, d.size.cx),
                            y: (p.y - origin.y - r.top).clamp(0, d.size.cy),
                        };
                    }
                }
                ov.drag = Some(d);
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
        if ov.drag.is_none() {
            return;
        }
        let target = drop_target(ov.hit(p));
        let at = ov
            .overlays
            .iter()
            .find(|o| p.x >= o.origin.x && p.y >= o.origin.y && p.x < o.origin.x + o.w && p.y < o.origin.y + o.h)
            .map(|o| (o.hwnd, o.origin));
        let d = ov.drag.as_mut().unwrap();
        d.cur = p;
        if !d.active && ((p.x - d.start.x).abs() > 8 || (p.y - d.start.y).abs() > 8) {
            d.active = true;
            if d.orig_thumb != 0 {
                set_thumb(d.orig_thumb, d.orig_rect, 50); // leave a faint placeholder
            }
        }
        if !d.active {
            return;
        }
        let mut repaint = d.target != target;
        d.target = target;
        if d.size.cx > 0 {
            if let Some((oh, origin)) = at {
                if d.ghost_on != oh {
                    d.drop_ghost();
                    if let Ok(t) = unsafe { DwmRegisterThumbnail(oh, key_hwnd(d.hwnd)) } {
                        d.ghost = t;
                        d.ghost_on = oh;
                    }
                }
                if d.ghost != 0 {
                    let (l, t) = (p.x - origin.x - d.grab.x, p.y - origin.y - d.grab.y);
                    set_thumb(d.ghost, RECT { left: l, top: t, right: l + d.size.cx, bottom: t + d.size.cy }, 225);
                }
            }
        } else {
            repaint = true; // text ghost is painted
        }
        if repaint {
            ov.invalidate();
        }
    }

    /// Paint every overlay right away (WM_PAINT can't run while we animate).
    fn overview_paint_now(&self) {
        let Some(ov) = self.overview.as_ref() else { return };
        unsafe {
            for o in &ov.overlays {
                let hdc = GetDC(o.hwnd);
                self.overview_paint(o.hwnd, hdc);
                ReleaseDC(o.hwnd, hdc);
                let _ = ValidateRect(o.hwnd, None);
            }
        }
    }

    pub fn overview_mouse_up(&mut self, p: POINT) {
        unsafe {
            let _ = ReleaseCapture();
        }
        let Some(ov) = self.overview.as_mut() else { return };
        let Some(mut d) = ov.drag.take() else { return };
        if !d.active {
            self.overview_jump(d.from.0, d.from.1, Some(d.hwnd));
            return;
        }
        if let Some(to) = drop_target(ov.hit(p)) {
            self.move_window(d.hwnd, d.from, to);
        }
        self.overview_layout();
        self.overview_land(&mut d);
    }

    /// Fly the dragged thumbnail into its (new or old) card, then hand over to the card.
    fn overview_land(&self, d: &mut Drag) {
        let Some(ov) = self.overview.as_ref() else { return d.drop_ghost() };
        let card = ov
            .overlays
            .iter()
            .filter(|o| o.hwnd == d.ghost_on)
            .flat_map(|o| o.columns.iter().flat_map(|c| c.cards.iter()))
            .find(|c| c.hwnd == d.hwnd && c.thumb != 0)
            .map(|c| (c.thumb, c.thumb_rect));
        let ms = self.config.animation.min(180);
        let (Some((thumb, to)), true, true) = (card, d.ghost != 0, ms > 0) else { return d.drop_ghost() };
        // Re-register the ghost so it draws above the freshly laid-out cards.
        let from = {
            let o = ov.overlays.iter().find(|o| o.hwnd == d.ghost_on).unwrap();
            let (l, t) = (d.cur.x - o.origin.x - d.grab.x, d.cur.y - o.origin.y - d.grab.y);
            RECT { left: l, top: t, right: l + d.size.cx, bottom: t + d.size.cy }
        };
        let on = d.ghost_on;
        d.drop_ghost();
        let Ok(g) = (unsafe { DwmRegisterThumbnail(on, key_hwnd(d.hwnd)) }) else { return };
        set_thumb(g, from, 225);
        set_thumb(thumb, to, 0);
        self.overview_paint_now();
        let lerp = |a: i32, b: i32, p: f32| a + ((b - a) as f32 * p).round() as i32;
        let start = std::time::Instant::now();
        let total = ms as f32 / 1000.0;
        loop {
            let t = (start.elapsed().as_secs_f32() / total).min(1.0);
            let e = 1.0 - (1.0 - t).powi(3);
            let r = RECT {
                left: lerp(from.left, to.left, e),
                top: lerp(from.top, to.top, e),
                right: lerp(from.right, to.right, e),
                bottom: lerp(from.bottom, to.bottom, e),
            };
            set_thumb(g, r, (225.0 + 30.0 * e) as u8);
            unsafe {
                let _ = DwmFlush();
            }
            if t >= 1.0 {
                break;
            }
        }
        unsafe {
            let _ = DwmUnregisterThumbnail(g);
        }
        ov.apply_opacity();
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
