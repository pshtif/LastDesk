// Created by Peter @sHTiF Stefcek
//! Hover previews for bar window buttons: a small popup with the window's title and a live
//! DWM thumbnail. It never takes focus or mouse input; the bar drives showing and hiding.

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ui;

pub const CLASS: PCWSTR = w!("LastDeskPreview");

struct State {
    hwnd: HWND,
    thumb: isize,
    target: isize,
    title: String,
    icon: HICON,
    scale: f32,
    thumb_rect: RECT,
    minimized: bool,
}

thread_local! {
    static PV: RefCell<State> = RefCell::new(State {
        hwnd: HWND::default(),
        thumb: 0,
        target: 0,
        title: String::new(),
        icon: HICON::default(),
        scale: 1.0,
        thumb_rect: RECT::default(),
        minimized: false,
    });
}

/// The window being previewed, or 0.
pub fn target() -> isize {
    PV.with(|p| p.borrow().target)
}

pub fn visible() -> bool {
    target() != 0
}

/// Show the preview for `target`, centred on `anchor_x`, just below `bar` (or above it).
pub fn show(target: HWND, title: &str, icon: HICON, anchor_x: i32, bar: RECT, above: bool, mon: RECT, scale: f32) {
    let s = |v: f32| (v * scale).round() as i32;
    unsafe {
        let mut hwnd = PV.with(|p| p.borrow().hwnd);
        if hwnd.is_invalid() {
            let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
            let Ok(h) = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                CLASS,
                w!(""),
                WS_POPUP,
                0, 0, 10, 10,
                None, None, hinst, None,
            ) else {
                return;
            };
            // Rounded corners (DWMWA_WINDOW_CORNER_PREFERENCE = 33, DWMWCP_ROUND = 2).
            let round: u32 = 2;
            let _ = DwmSetWindowAttribute(h, DWMWINDOWATTRIBUTE(33), &round as *const u32 as *const _, 4);
            hwnd = h;
        }
        let old = PV.with(|p| std::mem::take(&mut p.borrow_mut().thumb));
        if old != 0 {
            let _ = DwmUnregisterThumbnail(old);
        }

        let minimized = IsIconic(target).as_bool();
        let mut thumb = 0isize;
        let mut src = SIZE::default();
        if !minimized {
            if let Ok(t) = DwmRegisterThumbnail(hwnd, target) {
                thumb = t;
                src = DwmQueryThumbnailSourceSize(t).unwrap_or_default();
            }
        }
        let (max_w, max_h) = (s(260.0), s(160.0));
        let (tw, th) = if thumb != 0 && src.cx > 0 && src.cy > 0 {
            let k = (max_w as f32 / src.cx as f32).min(max_h as f32 / src.cy as f32).min(1.0);
            (((src.cx as f32) * k).round() as i32, ((src.cy as f32) * k).round() as i32)
        } else {
            (s(200.0), s(0.0))
        };
        let pad = s(8.0);
        let title_h = s(24.0);
        let w = tw.max(s(180.0)) + 2 * pad;
        let h = pad + title_h + if th > 0 { s(4.0) + th } else { 0 } + pad;
        let x = (anchor_x - w / 2).clamp(mon.left + s(4.0), (mon.right - w - s(4.0)).max(mon.left));
        let y = if above { bar.top - h - s(6.0) } else { bar.bottom + s(6.0) };
        let thumb_rect = RECT { left: (w - tw) / 2, top: pad + title_h + s(4.0), right: (w - tw) / 2 + tw, bottom: pad + title_h + s(4.0) + th };

        PV.with(|p| {
            let mut p = p.borrow_mut();
            p.hwnd = hwnd;
            p.thumb = thumb;
            p.target = target.0 as isize;
            p.title = title.to_string();
            p.icon = icon;
            p.scale = scale;
            p.thumb_rect = thumb_rect;
            p.minimized = minimized;
        });
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        if thumb != 0 {
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: thumb_rect,
                opacity: 255,
                fVisible: TRUE,
                fSourceClientAreaOnly: FALSE,
                ..Default::default()
            };
            let _ = DwmUpdateThumbnailProperties(thumb, &props);
        }
        let _ = InvalidateRect(hwnd, None, false);
    }
}

pub fn hide() {
    let (hwnd, thumb) = PV.with(|p| {
        let mut p = p.borrow_mut();
        p.target = 0;
        (p.hwnd, std::mem::take(&mut p.thumb))
    });
    unsafe {
        if thumb != 0 {
            let _ = DwmUnregisterThumbnail(thumb);
        }
        if !hwnd.is_invalid() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

pub unsafe extern "system" fn preview_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = CreateSolidBrush(ui::rgb(0x20, 0x21, 0x25));
            FillRect(hdc, &rc, brush);
            let _ = DeleteObject(brush);
            PV.with(|p| {
                let p = p.borrow();
                let s = |v: f32| (v * p.scale).round() as i32;
                let pad = s(8.0);
                let icon = s(16.0);
                let title_h = s(24.0);
                if !p.icon.is_invalid() {
                    let _ = DrawIconEx(hdc, pad, pad + (title_h - icon) / 2, p.icon, icon, icon, 0, None, DI_NORMAL);
                }
                let f = ui::font(s(13.0), false);
                let mut tr = RECT { left: pad + icon + s(8.0), top: pad, right: rc.right - pad, bottom: pad + title_h };
                // Minimized windows have no live image: title only, marked as minimized.
                let title = if p.minimized { format!("{} (minimized)", p.title) } else { p.title.clone() };
                ui::draw_text_left(hdc, &title, &mut tr, ui::TEXT, f);
                let _ = DeleteObject(f);
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
