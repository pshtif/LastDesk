// Created by Peter @sHTiF Stefcek
//! Workspace switch slide. A temporary panel covers the monitor's work area, shows the
//! wallpaper, and slides DWM live thumbnails of the old workspace's windows out and the new
//! ones in. The real swap happens underneath; the panel is removed when the slide ends.
//! Like the overview, nothing is captured or stored: the compositor draws the windows.

use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("LastDeskSlide");

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Out,
    In,
    Still,
}

pub struct Slide {
    hwnd: HWND,
    thumbs: Vec<(isize, RECT, Role)>,
    width: i32,
    dir: i32,
    dur: Duration,
}

unsafe extern "system" fn slide_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !PaintDesktop(hdc).as_bool() {
                FillRect(hdc, &ps.rcPaint, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn ensure_class() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(slide_proc),
            hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
    });
}

fn offset(r: RECT, dx: i32) -> RECT {
    RECT { left: r.left + dx, top: r.top, right: r.right + dx, bottom: r.bottom }
}

fn place(thumb: isize, r: RECT) {
    let props = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_SOURCECLIENTAREAONLY,
        rcDestination: r,
        fVisible: true.into(),
        fSourceClientAreaOnly: false.into(),
        ..Default::default()
    };
    unsafe {
        let _ = DwmUpdateThumbnailProperties(thumb, &props);
    }
}

impl Slide {
    /// Cover `work` and show the current state. Window lists are top-most first.
    /// `dir` = +1 when going to a higher workspace (content moves left), -1 otherwise.
    pub fn begin(work: RECT, dir: i32, ms: u32, out: &[HWND], inn: &[HWND], still: &[HWND]) -> Option<Slide> {
        ensure_class();
        let (w, h) = (work.right - work.left, work.bottom - work.top);
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("LastDesk slide"),
                WS_POPUP,
                work.left, work.top, w, h,
                None, None, GetModuleHandleW(None).unwrap_or_default(), None,
            )
            .ok()?
        };
        let mut s = Slide { hwnd, thumbs: Vec::new(), width: w, dir, dur: Duration::from_millis(ms as u64) };
        // Register bottom-most first so later (higher) windows draw on top.
        for (list, role) in [(out, Role::Out), (inn, Role::In), (still, Role::Still)] {
            for &win in list.iter().rev() {
                unsafe {
                    if IsIconic(win).as_bool() || !IsWindow(win).as_bool() {
                        continue;
                    }
                    let mut r = RECT::default();
                    if GetWindowRect(win, &mut r).is_err() {
                        continue;
                    }
                    let Ok(t) = DwmRegisterThumbnail(hwnd, win) else { continue };
                    s.thumbs.push((t, offset(RECT { top: r.top - work.top, bottom: r.bottom - work.top, ..r }, -work.left), role));
                }
            }
        }
        s.frame(0.0);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            let _ = UpdateWindow(hwnd);
            let _ = DwmFlush();
        }
        Some(s)
    }

    fn frame(&self, p: f32) {
        let w = self.width as f32;
        for &(t, r, role) in &self.thumbs {
            let dx = match role {
                Role::Out => -(self.dir as f32) * w * p,
                Role::In => (self.dir as f32) * w * (1.0 - p),
                Role::Still => 0.0,
            };
            place(t, offset(r, dx.round() as i32));
        }
    }

    /// Play the slide (blocking, synced to the display) and remove the panel.
    pub fn run(self) {
        let start = Instant::now();
        let total = self.dur.as_secs_f32().max(0.001);
        loop {
            let t = (start.elapsed().as_secs_f32() / total).min(1.0);
            let p = 1.0 - (1.0 - t).powi(3); // ease-out
            self.frame(p);
            unsafe {
                let _ = DwmFlush();
            }
            if t >= 1.0 {
                break;
            }
        }
    }
}

impl Drop for Slide {
    fn drop(&mut self) {
        unsafe {
            for &(t, _, _) in &self.thumbs {
                let _ = DwmUnregisterThumbnail(t);
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
