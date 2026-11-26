// Created by Peter @sHTiF Stefcek
//! Taking the picture: a monitor grabbed as it is, the overlay to pick a region or a window on
//! it (frozen, dimmed) and then draw on it (freehand, to point something out), and the result
//! saved as PNG and put on the clipboard.

use std::cell::RefCell;
use std::path::Path;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_DIB;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

/// A piece of the screen: its place (screen coordinates) and its pixels (BGRA, top row first).
pub struct Shot {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

/// The whole monitor that a point (a work area's middle) is on.
pub fn monitor_at(r: RECT) -> RECT {
    unsafe {
        let m = MonitorFromPoint(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(m, &mut mi).as_bool() {
            mi.rcMonitor
        } else {
            r
        }
    }
}

/// The screen inside `r`, as it is now.
pub fn grab(r: RECT) -> Option<Shot> {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) else {
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            return None;
        };
        let old = SelectObject(mem, bmp);
        let _ = BitBlt(mem, 0, 0, w, h, screen, r.left, r.top, SRCCOPY | CAPTUREBLT);
        let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        Some(Shot { x: r.left, y: r.top, w, h, px })
    }
}

/// Part of a shot (`r` in screen coordinates, clipped to it).
pub fn crop(s: &Shot, r: RECT) -> Option<Shot> {
    let l = r.left.max(s.x);
    let t = r.top.max(s.y);
    let rr = r.right.min(s.x + s.w);
    let b = r.bottom.min(s.y + s.h);
    if rr - l < 1 || b - t < 1 {
        return None;
    }
    let (w, h) = (rr - l, b - t);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let start = (((t - s.y + y) * s.w + (l - s.x)) * 4) as usize;
        px.extend_from_slice(&s.px[start..start + (w * 4) as usize]);
    }
    Some(Shot { x: l, y: t, w, h, px })
}

/// Saved as PNG (no transparency: the screen has none).
pub fn save_png(s: &Shot, path: &Path) -> windows::core::Result<()> {
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = factory.CreateStream()?;
        let p: Vec<u16> = path.as_os_str().encode_wide_z();
        stream.InitializeFromFilename(PCWSTR(p.as_ptr()), 0x4000_0000 /* GENERIC_WRITE */)?;
        let enc = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(windows::core::Error::empty)?;
        frame.Initialize(None)?;
        frame.SetSize(s.w as u32, s.h as u32)?;
        // 24-bit: the PNG encoder doesn't take 32bppBGR (it answers with 24bppBGR, and 4-byte
        // pixels written as 3-byte ones come out stretched and striped).
        let mut fmt = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut fmt)?;
        if fmt != GUID_WICPixelFormat24bppBGR {
            return Err(windows::core::Error::empty());
        }
        let bgr: Vec<u8> = s.px.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        frame.WritePixels(s.h as u32, (s.w * 3) as u32, &bgr)?;
        frame.Commit()?;
        enc.Commit()?;
    }
    Ok(())
}

trait WideZ {
    fn encode_wide_z(&self) -> Vec<u16>;
}
impl WideZ for std::ffi::OsStr {
    fn encode_wide_z(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(Some(0)).collect()
    }
}

/// On the clipboard as a bitmap (what pasting a picture takes).
pub fn to_clipboard(s: &Shot, owner: HWND) -> bool {
    unsafe {
        let header = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: s.w, biHeight: s.h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() };
        let hs = std::mem::size_of::<BITMAPINFOHEADER>();
        let size = hs + s.px.len();
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, size) else { return false };
        let p = GlobalLock(mem) as *mut u8;
        if p.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(&header as *const _ as *const u8, p, hs);
        // A DIB's rows go bottom first; alpha set so apps that read it don't see it transparent.
        let row = (s.w * 4) as usize;
        for y in 0..s.h as usize {
            let src = &s.px[y * row..(y + 1) * row];
            let dst = p.add(hs + (s.h as usize - 1 - y) * row);
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst, row);
            for x in 0..s.w as usize {
                *dst.add(x * 4 + 3) = 255;
            }
        }
        let _ = GlobalUnlock(mem);
        if OpenClipboard(owner).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = SetClipboardData(CF_DIB.0 as u32, HANDLE(mem.0)).is_ok();
        let _ = CloseClipboard();
        ok
    }
}

// ---- drawing ----

/// Freehand strokes: red, round-ended, a few pixels wide.
const PEN: COLORREF = COLORREF(0x003C3CEB);
const PEN_WIDTH: u32 = 5;

/// Draws strokes (points relative to `origin`, which is where the DC's (0, 0) is on screen).
unsafe fn draw_strokes(hdc: HDC, strokes: &[Vec<POINT>], dx: i32, dy: i32) {
    let brush = LOGBRUSH { lbStyle: BS_SOLID, lbColor: PEN, lbHatch: 0 };
    let pen = ExtCreatePen(PEN_STYLE(PS_GEOMETRIC.0 | PS_SOLID.0 | PS_ENDCAP_ROUND.0 | PS_JOIN_ROUND.0), PEN_WIDTH, &brush, None);
    let old = SelectObject(hdc, pen);
    for st in strokes {
        let pts: Vec<POINT> = st.iter().map(|p| POINT { x: p.x + dx, y: p.y + dy }).collect();
        if pts.len() == 1 {
            // A dot: a stroke to itself.
            let _ = MoveToEx(hdc, pts[0].x, pts[0].y, None);
            let _ = LineTo(hdc, pts[0].x + 1, pts[0].y);
        } else if pts.len() > 1 {
            let _ = Polyline(hdc, &pts);
        }
    }
    SelectObject(hdc, old);
    let _ = DeleteObject(pen);
}

/// Strokes drawn into a picture. `dx`/`dy`: from stroke coordinates to the picture's.
pub fn burn(s: &mut Shot, strokes: &[Vec<POINT>], dx: i32, dy: i32) {
    if strokes.is_empty() {
        return;
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: s.w, biHeight: -s.h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        if let Ok(bmp) = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
            let old = SelectObject(mem, bmp);
            let n = s.px.len();
            std::ptr::copy_nonoverlapping(s.px.as_ptr(), bits as *mut u8, n);
            draw_strokes(mem, strokes, dx, dy);
            let _ = GdiFlush();
            std::ptr::copy_nonoverlapping(bits as *const u8, s.px.as_mut_ptr(), n);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp);
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}

// ---- the overlay ----

/// Visible top-level windows (screen rects, frame without the shadow), topmost first.
fn windows_now() -> Vec<RECT> {
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        let list = &mut *(lp.0 as *mut Vec<RECT>);
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
        if cloaked != 0 {
            return true.into();
        }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
            return true.into();
        }
        let mut r = RECT::default();
        if DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32).is_err() {
            let _ = GetWindowRect(h, &mut r);
        }
        if r.right - r.left > 40 && r.bottom - r.top > 40 {
            list.push(r);
        }
        true.into()
    }
    let mut list: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

struct Overlay {
    shot: Shot,
    /// The shot darkened (shown outside the selection).
    dim: Vec<u8>,
    windows: Vec<RECT>,
    start: Option<POINT>,
    now: POINT,
    result: Option<Option<RECT>>,
    record: bool,
    /// Picked: drawing on it now (screen coordinates).
    paint: Option<RECT>,
    /// Freehand strokes, in overlay coordinates.
    strokes: Vec<Vec<POINT>>,
    drawing: bool,
    /// The toolbar's button size (pixels, for the monitor's scale) and the one under the mouse.
    button: i32,
    hover: Option<usize>,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

unsafe fn fill_round(hdc: HDC, r: RECT, radius: i32, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let pen = CreatePen(PS_SOLID, 1, color);
    let ob = SelectObject(hdc, brush);
    let op = SelectObject(hdc, pen);
    let _ = RoundRect(hdc, r.left, r.top, r.right, r.bottom, radius, radius);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(brush);
    let _ = DeleteObject(pen);
}

fn norm(a: POINT, b: POINT) -> RECT {
    RECT { left: a.x.min(b.x), top: a.y.min(b.y), right: a.x.max(b.x) + 1, bottom: a.y.max(b.y) + 1 }
}

/// The toolbar while drawing: pen (the one tool for now), undo | cancel, save & copy.
const TOOLS: [u32; 4] = [0xE70F, 0xE7A7, 0xE711, 0xE73E];
const T_PEN: usize = 0;
const T_UNDO: usize = 1;
const T_CANCEL: usize = 2;
const T_SAVE: usize = 3;

/// Where the toolbar is (overlay coordinates): below the picture, right-aligned; above it when
/// there's no room; inside its bottom edge when neither fits. The strip and its buttons.
fn toolbar(o: &Overlay) -> Option<(RECT, Vec<RECT>)> {
    let p = o.paint?;
    let b = o.button;
    let (gap, pad, sep) = (b / 6, b / 6, b / 2);
    let w = pad * 2 + 4 * b + 3 * gap + sep;
    let h = b + pad * 2;
    let (t, r, btm) = (p.top - o.shot.y, p.right - o.shot.x, p.bottom - o.shot.y);
    let x = (r - w).clamp(0, (o.shot.w - w).max(0));
    let y = if btm + 8 + h <= o.shot.h {
        btm + 8
    } else if t - 8 - h >= 0 {
        t - 8 - h
    } else {
        (btm - h - 8).max(0)
    };
    let mut buttons = Vec::new();
    let mut bx = x + pad;
    for i in 0..TOOLS.len() {
        buttons.push(RECT { left: bx, top: y + pad, right: bx + b, bottom: y + pad + b });
        bx += b + gap + if i == T_UNDO { sep } else { 0 };
    }
    Some((RECT { left: x, top: y, right: x + w, bottom: y + h }, buttons))
}

fn inside(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// What's selected now (screen coordinates): the dragged rectangle, or the window under the mouse.
fn selection(o: &Overlay) -> Option<RECT> {
    if o.paint.is_some() {
        return o.paint;
    }
    let p = POINT { x: o.now.x + o.shot.x, y: o.now.y + o.shot.y };
    match o.start {
        Some(s) => {
            let s = POINT { x: s.x + o.shot.x, y: s.y + o.shot.y };
            Some(norm(s, p))
        }
        None => o.windows.iter().find(|r| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom).copied(),
    }
}

fn dib(hdc: HDC, w: i32, h: i32, px: &[u8], x: i32, y: i32, sx: i32, sy: i32, sw: i32, sh: i32, total_h: i32) {
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -total_h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        // SetDIBitsToDevice with a top-down DIB: rows counted from the top.
        let _ = SetDIBitsToDevice(hdc, x, y, sw as u32, sh as u32, sx, total_h - sy - sh, 0, total_h as u32, px.as_ptr() as *const _, &bmi, DIB_RGB_COLORS);
        let _ = h;
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let xy = || POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
    let finish = |r: Option<RECT>| {
        OVERLAY.with(|o| {
            if let Some(o) = o.borrow_mut().as_mut() {
                o.result = Some(r);
            }
        });
    };
    match msg {
        WM_SETCURSOR => {
            let over = OVERLAY.with(|o| o.borrow().as_ref().and_then(|o| toolbar(o).map(|(bar, _)| inside(&bar, o.now))).unwrap_or(false));
            SetCursor(LoadCursorW(None, if over { IDC_ARROW } else { IDC_CROSS }).unwrap_or_default());
            LRESULT(1)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            OVERLAY.with(|o| {
                if let Some(o) = o.borrow_mut().as_mut() {
                    o.now = xy();
                    if o.drawing {
                        let p = o.now;
                        if let Some(st) = o.strokes.last_mut() {
                            st.push(p);
                        }
                    } else {
                        o.hover = toolbar(o).and_then(|(_, bs)| bs.iter().position(|b| inside(b, o.now)));
                    }
                }
            });
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let pressed = OVERLAY.with(|o| {
                let b = o.borrow();
                let o = b.as_ref()?;
                let (bar, bs) = toolbar(o)?;
                if !inside(&bar, xy()) {
                    return None;
                }
                Some((bs.iter().position(|b| inside(b, xy())), o.paint))
            });
            if let Some((button, paint)) = pressed {
                match button {
                    Some(T_UNDO) => OVERLAY.with(|o| {
                        if let Some(o) = o.borrow_mut().as_mut() {
                            o.strokes.pop();
                        }
                    }),
                    Some(T_CANCEL) => finish(None),
                    Some(T_SAVE) => finish(paint),
                    _ => {}
                }
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            SetCapture(hwnd);
            OVERLAY.with(|o| {
                if let Some(o) = o.borrow_mut().as_mut() {
                    o.now = xy();
                    if o.paint.is_some() {
                        o.drawing = true;
                        o.strokes.push(vec![xy()]);
                    } else {
                        o.start = Some(xy());
                    }
                }
            });
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            // Drawing: the stroke ends.
            let painting = OVERLAY.with(|o| {
                let mut b = o.borrow_mut();
                let Some(o) = b.as_mut() else { return false };
                o.drawing = false;
                o.paint.is_some()
            });
            if painting {
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            let r = OVERLAY.with(|o| {
                let mut b = o.borrow_mut();
                let o = b.as_mut()?;
                o.now = xy();
                // Hardly moved: a click, which takes the window under the mouse.
                if let Some(s) = o.start {
                    if (s.x - o.now.x).abs() < 5 && (s.y - o.now.y).abs() < 5 {
                        o.start = None;
                    }
                }
                let r = selection(o)?;
                // Kept on the monitor shown.
                Some(RECT { left: r.left.max(o.shot.x), top: r.top.max(o.shot.y), right: r.right.min(o.shot.x + o.shot.w), bottom: r.bottom.min(o.shot.y + o.shot.h) })
            });
            let r = r.filter(|r| r.right - r.left >= 4 && r.bottom - r.top >= 4);
            // Picked: drawn on next (a recording starts at once).
            let record = OVERLAY.with(|o| o.borrow().as_ref().map(|o| o.record).unwrap_or(true));
            if record || r.is_none() {
                finish(r);
            } else {
                OVERLAY.with(|o| {
                    if let Some(o) = o.borrow_mut().as_mut() {
                        o.paint = r;
                        o.start = None;
                    }
                });
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // Picking: cancelled. Drawing: nothing (done with Enter or the save button).
            if OVERLAY.with(|o| o.borrow().as_ref().map(|o| o.paint.is_none()).unwrap_or(true)) {
                finish(None);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wp.0 == VK_ESCAPE.0 as usize {
                finish(None);
            } else if wp.0 == VK_RETURN.0 as usize {
                // Drawing: done. Picking: the whole monitor, drawn on next (recorded at once).
                let (paint, record, whole) = OVERLAY.with(|o| {
                    o.borrow().as_ref().map(|o| (o.paint, o.record, RECT { left: o.shot.x, top: o.shot.y, right: o.shot.x + o.shot.w, bottom: o.shot.y + o.shot.h })).unwrap_or((None, true, RECT::default()))
                });
                if paint.is_some() {
                    finish(paint);
                } else if record {
                    finish(Some(whole));
                } else {
                    OVERLAY.with(|o| {
                        if let Some(o) = o.borrow_mut().as_mut() {
                            o.paint = Some(whole);
                        }
                    });
                    let _ = InvalidateRect(hwnd, None, false);
                }
            } else if wp.0 == 'Z' as usize && GetKeyState(VK_CONTROL.0 as i32) < 0 {
                // The last stroke undone.
                OVERLAY.with(|o| {
                    if let Some(o) = o.borrow_mut().as_mut() {
                        o.strokes.pop();
                    }
                });
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &ps as *const _ as *mut _);
            OVERLAY.with(|o| {
                let b = o.borrow();
                let Some(o) = b.as_ref() else { return };
                let (w, h) = (o.shot.w, o.shot.h);
                let mem = CreateCompatibleDC(hdc);
                let bmp = CreateCompatibleBitmap(hdc, w, h);
                let old = SelectObject(mem, bmp);
                dib(mem, w, h, &o.dim, 0, 0, 0, 0, w, h, h);
                if let Some(sel) = selection(o) {
                    let l = (sel.left - o.shot.x).clamp(0, w);
                    let t = (sel.top - o.shot.y).clamp(0, h);
                    let r = (sel.right - o.shot.x).clamp(0, w);
                    let btm = (sel.bottom - o.shot.y).clamp(0, h);
                    if r > l && btm > t {
                        dib(mem, w, h, &o.shot.px, l, t, l, t, r - l, btm - t, h);
                        let color = if o.record { COLORREF(0x004A4AF0) } else { COLORREF(0x00F67834) };
                        let brush = CreateSolidBrush(color);
                        let _ = FrameRect(mem, &RECT { left: l, top: t, right: r, bottom: btm }, brush);
                        let _ = FrameRect(mem, &RECT { left: l - 1, top: t - 1, right: r + 1, bottom: btm + 1 }, brush);
                        let _ = DeleteObject(brush);
                        // Its size, just above (or inside at the top edge).
                        let label: Vec<u16> = format!(" {} \u{00D7} {} ", r - l, btm - t).encode_utf16().collect();
                        SetBkColor(mem, COLORREF(0x00222222));
                        SetTextColor(mem, COLORREF(0x00FFFFFF));
                        SetBkMode(mem, OPAQUE);
                        let ly = if t > 22 { t - 22 } else { t + 4 };
                        let _ = TextOutW(mem, l, ly, &label);
                        // The strokes, kept inside the picture.
                        if !o.strokes.is_empty() {
                            let _ = IntersectClipRect(mem, l, t, r, btm);
                            draw_strokes(mem, &o.strokes, 0, 0);
                            let _ = SelectClipRgn(mem, None);
                        }
                    }
                }
                if let Some((bar, bs)) = toolbar(o) {
                    fill_round(mem, bar, o.button / 3, COLORREF(0x00262320));
                    let face: Vec<u16> = "Segoe Fluent Icons".encode_utf16().chain(Some(0)).collect();
                    let font = CreateFontW(-(o.button * 9 / 20), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32, ANTIALIASED_QUALITY.0 as u32, (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32, PCWSTR(face.as_ptr()));
                    let oldf = SelectObject(mem, font);
                    SetBkMode(mem, TRANSPARENT);
                    for (i, b) in bs.iter().enumerate() {
                        let bg = if i == T_PEN {
                            Some(COLORREF(0x00F67834))
                        } else if o.hover == Some(i) {
                            Some(if i == T_SAVE { COLORREF(0x00F67834) } else { COLORREF(0x00403B37) })
                        } else {
                            None
                        };
                        if let Some(c) = bg {
                            fill_round(mem, *b, o.button / 4, c);
                        }
                        SetTextColor(mem, COLORREF(0x00FFFFFF));
                        let mut g: Vec<u16> = char::from_u32(TOOLS[i]).map(|c| c.to_string()).unwrap_or_default().encode_utf16().collect();
                        let mut rr = *b;
                        let _ = DrawTextW(mem, &mut g, &mut rr, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
                    }
                    SelectObject(mem, oldf);
                    let _ = DeleteObject(font);
                }
                let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
                SelectObject(mem, old);
                let _ = DeleteObject(bmp);
                let _ = DeleteDC(mem);
            });
            ps.hdc = hdc;
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The monitor shown frozen and dimmed: drag a region, click a window, Enter for all of it,
/// Esc (or a right click) to cancel. Then (not when recording) drawn on freehand, with a toolbar
/// by it: Enter or its save button is done, Ctrl+Z or undo takes the last stroke back, Esc or
/// cancel drops it. The rectangle chosen
/// (screen coordinates), the shot, and the strokes (relative to the shot).
pub fn pick(shot: Shot, record: bool) -> (Option<RECT>, Shot, Vec<Vec<POINT>>) {
    let dim: Vec<u8> = shot.px.iter().enumerate().map(|(i, v)| if i % 4 == 3 { *v } else { (*v as u32 * 45 / 100) as u8 }).collect();
    let windows = windows_now();
    let (x, y, w, h) = (shot.x, shot.y, shot.w, shot.h);
    let mut now = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut now);
    }
    now = POINT { x: now.x - x, y: now.y - y };
    let button = unsafe {
        let m = MonitorFromPoint(POINT { x: x + w / 2, y: y + h / 2 }, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (34 * dx as i32) / 96
    };
    OVERLAY.with(|o| *o.borrow_mut() = Some(Overlay { shot, dim, windows, start: None, now, result: None, record, paint: None, strokes: Vec::new(), drawing: false, button, hover: None }));
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("LastShotOverlay");
        let wc = WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: inst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, class, w!("LastShot"), WS_POPUP | WS_VISIBLE, x, y, w, h, None, None, inst, None).unwrap_or_default();
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
        let mut m = MSG::default();
        loop {
            if OVERLAY.with(|o| o.borrow().as_ref().map(|o| o.result.is_some()).unwrap_or(true)) {
                break;
            }
            if !GetMessageW(&mut m, None, 0, 0).as_bool() {
                break;
            }
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        let _ = DestroyWindow(hwnd);
    }
    let o = OVERLAY.with(|o| o.borrow_mut().take()).expect("overlay state");
    (o.result.flatten(), o.shot, o.strokes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_land_in_the_picture() {
        // A white 100×60 picture at (200, 300) on a monitor whose shot started at (0, 0).
        let mut s = Shot { x: 200, y: 300, w: 100, h: 60, px: vec![255; 100 * 60 * 4] };
        // A stroke across the middle, in monitor coordinates (as the overlay keeps them).
        let stroke = vec![POINT { x: 210, y: 330 }, POINT { x: 290, y: 330 }];
        burn(&mut s, &[stroke], -200, -300);
        let at = |x: usize, y: usize| { let i = (y * 100 + x) * 4; (s.px[i], s.px[i + 1], s.px[i + 2]) };
        // On the line: the pen's red (BGR: 0x3C, 0x3C, 0xEB); far from it: still white.
        assert_eq!(at(50, 30), (0x3C, 0x3C, 0xEB));
        assert_eq!(at(50, 5), (255, 255, 255));
        assert_eq!(at(5, 30), (255, 255, 255));
    }

    #[test]
    fn png_keeps_every_pixel() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        // Every pixel different (odd width, so rows don't line up by chance).
        let (w, h) = (37, 11);
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7) as u8, (i * 13) as u8, (i * 29) as u8, 0]).collect();
        let s = Shot { x: 0, y: 0, w, h, px };
        let path = std::env::temp_dir().join("lastshot-test.png");
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        save_png(&s, &path).expect("saved");
        let back = unsafe {
            let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let p: Vec<u16> = path.as_os_str().encode_wide_z();
            let dec = factory.CreateDecoderFromFilename(PCWSTR(p.as_ptr()), None, windows::Win32::Foundation::GENERIC_READ, WICDecodeMetadataCacheOnDemand).unwrap();
            let frame = dec.GetFrame(0).unwrap();
            let (mut fw, mut fh) = (0, 0);
            frame.GetSize(&mut fw, &mut fh).unwrap();
            assert_eq!((fw, fh), (w as u32, h as u32));
            let conv = factory.CreateFormatConverter().unwrap();
            conv.Initialize(&frame, &GUID_WICPixelFormat32bppBGR, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).unwrap();
            let mut buf = vec![0u8; (w * h * 4) as usize];
            conv.CopyPixels(std::ptr::null(), (w * 4) as u32, &mut buf).unwrap();
            buf
        };
        let _ = std::fs::remove_file(&path);
        for i in 0..(w * h) as usize {
            assert_eq!(&back[i * 4..i * 4 + 3], &s.px[i * 4..i * 4 + 3], "pixel {i}");
        }
    }
}
