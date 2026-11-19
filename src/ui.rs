//! Drawing: the tray icon (current workspace numbers) and the on-screen indicator.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON};

pub const ACCENT: COLORREF = rgb(0x34, 0x78, 0xF6);
const BG: COLORREF = rgb(0x1E, 0x1F, 0x22);
const CELL: COLORREF = rgb(0x3A, 0x3B, 0x40);
const TEXT: COLORREF = rgb(0xFF, 0xFF, 0xFF);
const MUTED: COLORREF = rgb(0xA0, 0xA4, 0xAB);

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn font(height: i32, bold: bool) -> HFONT {
    let face = wide("Segoe UI");
    CreateFontW(
        -height, 0, 0, 0,
        if bold { 700 } else { 400 },
        0, 0, 0,
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        ANTIALIASED_QUALITY.0 as u32,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        PCWSTR(face.as_ptr()),
    )
}

unsafe fn draw_text(hdc: HDC, s: &str, r: &mut RECT, color: COLORREF, f: HFONT) {
    // DrawTextW reads through the (dangling) pointer of an empty slice: skip empty text.
    if s.is_empty() {
        return;
    }
    let old = SelectObject(hdc, f);
    SetTextColor(hdc, color);
    SetBkMode(hdc, TRANSPARENT);
    let mut w: Vec<u16> = s.encode_utf16().collect();
    DrawTextW(hdc, &mut w, r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    SelectObject(hdc, old);
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

/// Tray icon: a blue rounded square with the active workspace of each monitor, e.g. "12".
pub fn make_tray_icon(text: &str) -> HICON {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let screen = GetDC(HWND::default());
        let mem = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        let old = SelectObject(mem, color);

        let full = RECT { left: 0, top: 0, right: size, bottom: size };
        let brush = CreateSolidBrush(ACCENT);
        FillRect(mem, &full, brush);
        let _ = DeleteObject(brush);
        let scale = match text.chars().count() { 1 => 0.85, 2 => 0.68, _ => 0.5 };
        let f = font((size as f32 * scale) as i32, true);
        let mut r = full;
        draw_text(mem, text, &mut r, TEXT, f);
        let _ = DeleteObject(f);
        let _ = GdiFlush();

        // GDI leaves alpha at 0: make the square opaque with slightly rounded corners.
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        let rad = (size / 6).max(2);
        for y in 0..size {
            for x in 0..size {
                let dx = if x < rad { rad - x } else if x >= size - rad { x - (size - rad - 1) } else { 0 };
                let dy = if y < rad { rad - y } else if y >= size - rad { y - (size - rad - 1) } else { 0 };
                let i = (y * size + x) as usize;
                if dx * dx + dy * dy > rad * rad {
                    px[i] = 0;
                } else {
                    px[i] |= 0xFF00_0000;
                }
            }
        }
        SelectObject(mem, old);
        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(mask);
        let _ = DeleteObject(color);
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), screen);
        icon
    }
}

pub struct OsdModel {
    pub label: String,
    pub active: usize,
    pub counts: Vec<usize>,
}

pub struct OsdLayout {
    pub w: i32,
    pub h: i32,
    pub cell: i32,
    pub gap: i32,
    pub pad: i32,
    pub label_h: i32,
}

pub fn osd_layout(n: usize, scale: f32) -> OsdLayout {
    let s = |v: f32| (v * scale).round() as i32;
    let (cell, gap, pad, label_h) = (s(56.0), s(10.0), s(16.0), s(26.0));
    let w = pad * 2 + n as i32 * cell + (n as i32 - 1) * gap;
    let h = pad * 2 + label_h + cell;
    OsdLayout { w, h, cell, gap, pad, label_h }
}

pub unsafe fn paint_osd(hdc: HDC, model: &OsdModel, scale: f32) {
    let n = model.counts.len();
    let l = osd_layout(n, scale);
    let all = RECT { left: 0, top: 0, right: l.w, bottom: l.h };
    let brush = CreateSolidBrush(BG);
    FillRect(hdc, &all, brush);
    let _ = DeleteObject(brush);

    let small = font((13.0 * scale) as i32, false);
    let mut lr = RECT { left: l.pad, top: l.pad - (4.0 * scale) as i32, right: l.w - l.pad, bottom: l.pad + l.label_h - (6.0 * scale) as i32 };
    draw_text(hdc, &model.label, &mut lr, MUTED, small);
    let _ = DeleteObject(small);

    let big = font((22.0 * scale) as i32, true);
    let dot = (5.0 * scale).max(3.0) as i32;
    for i in 0..n {
        let x = l.pad + i as i32 * (l.cell + l.gap);
        let y = l.pad + l.label_h;
        let r = RECT { left: x, top: y, right: x + l.cell, bottom: y + l.cell };
        fill_round(hdc, r, (10.0 * scale) as i32, if i == model.active { ACCENT } else { CELL });
        let mut tr = RECT { bottom: r.bottom - (8.0 * scale) as i32, ..r };
        draw_text(hdc, &(i + 1).to_string(), &mut tr, TEXT, big);
        // One dot per window (max 5) along the bottom of the cell.
        let c = model.counts[i].min(5) as i32;
        if c > 0 {
            let span = c * dot + (c - 1) * dot;
            let mut dx = x + (l.cell - span) / 2;
            let dy = r.bottom - (10.0 * scale) as i32 - dot;
            for _ in 0..c {
                fill_round(hdc, RECT { left: dx, top: dy, right: dx + dot, bottom: dy + dot }, dot, if i == model.active { TEXT } else { MUTED });
                dx += dot * 2;
            }
        }
    }
    let _ = DeleteObject(big);
}
