// Created by Peter @sHTiF Stefcek
//! Recording a region through ffmpeg (gdigrab), with a red frame around it and a small panel by
//! it: the time, pause / resume, stop & save, cancel. The frame and the panel are kept out of the
//! recording (Windows' exclude-from-capture), so they can sit inside it too.
//!
//! ffmpeg's screen grabber can't pause: a pause ends the piece being recorded, resuming starts
//! the next, and stopping joins the pieces into one file (no re-encoding).

use std::cell::Cell;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Posted to the main window when a panel button is pressed (wparam: one of the A_ values).
pub const WM_PANEL: u32 = WM_APP + 3;
pub const A_PAUSE: usize = 1;
pub const A_STOP: usize = 2;
pub const A_CANCEL: usize = 3;

pub struct Recording {
    ffmpeg: PathBuf,
    region: RECT,
    /// The finished file, and the pieces recorded so far (each pause ends one).
    pub path: PathBuf,
    parts: Vec<PathBuf>,
    child: Option<Child>,
    /// Recorded time before the current piece, and when the current piece started.
    before: Duration,
    since: Option<Instant>,
    frame: Vec<HWND>,
    panel: HWND,
}

fn spawn(ffmpeg: &Path, r: RECT, out: &Path) -> std::io::Result<Child> {
    Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-f", "gdigrab", "-framerate", "30", "-draw_mouse", "1"])
        .args(["-offset_x", &r.left.to_string(), "-offset_y", &r.top.to_string(), "-video_size", &format!("{}x{}", r.right - r.left, r.bottom - r.top), "-i", "desktop"])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-y"])
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
}

/// Asks ffmpeg to finish its file ("q"), and waits for it (killed after 10 s).
fn end(mut child: Child) {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"q\n");
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl Recording {
    /// Starts recording `r` (even sizes: H.264 wants them) into `path`.
    pub fn start(ffmpeg: PathBuf, r: RECT, path: PathBuf, main: HWND) -> Result<Recording, String> {
        let r = RECT { right: r.left + ((r.right - r.left) & !1), bottom: r.top + ((r.bottom - r.top) & !1), ..r };
        if r.right - r.left < 16 || r.bottom - r.top < 16 {
            return Err("Too small to record".into());
        }
        let first = part_path(&path, 0);
        let child = spawn(&ffmpeg, r, &first).map_err(|e| format!("Couldn't start ffmpeg: {e}"))?;
        let frame = frame_around(r);
        let panel = panel(r, main);
        Ok(Recording { ffmpeg, region: r, path, parts: vec![first], child: Some(child), before: Duration::ZERO, since: Some(Instant::now()), frame, panel })
    }

    pub fn paused(&self) -> bool {
        self.child.is_none()
    }

    /// Recorded time (pauses not counted).
    pub fn elapsed(&self) -> Duration {
        self.before + self.since.map(|s| s.elapsed()).unwrap_or_default()
    }

    pub fn pause(&mut self) {
        if let Some(c) = self.child.take() {
            self.before += self.since.take().map(|s| s.elapsed()).unwrap_or_default();
            end(c);
            self.show();
        }
    }

    pub fn resume(&mut self) -> Result<(), String> {
        if self.child.is_some() {
            return Ok(());
        }
        let next = part_path(&self.path, self.parts.len());
        let child = spawn(&self.ffmpeg, self.region, &next).map_err(|e| format!("Couldn't start ffmpeg: {e}"))?;
        self.parts.push(next);
        self.child = Some(child);
        self.since = Some(Instant::now());
        self.show();
        Ok(())
    }

    /// The panel shows the time and state as they are now.
    pub fn show(&self) {
        PANEL.with(|p| p.set((self.elapsed().as_secs(), self.paused())));
        unsafe {
            let _ = InvalidateRect(self.panel, None, false);
        }
    }

    fn close_windows(&self) {
        unsafe {
            for f in &self.frame {
                let _ = DestroyWindow(*f);
            }
            let _ = DestroyWindow(self.panel);
        }
    }

    /// Stopped and saved: the pieces joined into the file. The file, if it was made.
    pub fn finish(mut self) -> Option<PathBuf> {
        self.close_windows();
        if let Some(c) = self.child.take() {
            end(c);
        }
        let parts: Vec<PathBuf> = self.parts.iter().filter(|p| p.is_file()).cloned().collect();
        match parts.len() {
            0 => None,
            1 => std::fs::rename(&parts[0], &self.path).ok().map(|_| self.path.clone()),
            _ => {
                // ffmpeg's concat: the pieces listed in a file, copied into one (no re-encoding).
                let list = self.path.with_extension("txt");
                let text: String = parts.iter().map(|p| format!("file '{}'\n", p.display().to_string().replace('\'', "'\\''"))).collect();
                let _ = std::fs::write(&list, text);
                let ok = Command::new(&self.ffmpeg)
                    .args(["-hide_banner", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"])
                    .arg(&list)
                    .args(["-c", "copy", "-movflags", "+faststart", "-y"])
                    .arg(&self.path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                let _ = std::fs::remove_file(&list);
                if ok {
                    for p in &parts {
                        let _ = std::fs::remove_file(p);
                    }
                    Some(self.path.clone())
                } else {
                    // Not joined: the pieces are kept as they are.
                    parts.first().cloned()
                }
            }
        }
    }

    /// Stopped and thrown away.
    pub fn cancel(mut self) {
        self.close_windows();
        if let Some(c) = self.child.take() {
            end(c);
        }
        for p in &self.parts {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// "LastShot … .part1.mp4" next to the finished file.
fn part_path(path: &Path, n: usize) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    path.with_file_name(format!("{stem}.part{}.mp4", n + 1))
}

/// Kept out of screenshots and recordings (Windows 10 2004 and later).
unsafe fn exclude_from_capture(h: HWND) {
    let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
}

unsafe extern "system" fn plain_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// A thin red frame just outside the recorded region, click-through.
fn frame_around(r: RECT) -> Vec<HWND> {
    let t = 3;
    let bars = [
        RECT { left: r.left - t, top: r.top - t, right: r.right + t, bottom: r.top },
        RECT { left: r.left - t, top: r.bottom, right: r.right + t, bottom: r.bottom + t },
        RECT { left: r.left - t, top: r.top, right: r.left, bottom: r.bottom },
        RECT { left: r.right, top: r.top, right: r.right + t, bottom: r.bottom },
    ];
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("LastShotFrame");
        let wc = WNDCLASSW { lpfnWndProc: Some(plain_proc), hInstance: inst.into(), lpszClassName: class, hbrBackground: CreateSolidBrush(COLORREF(0x004A4AF0)), ..Default::default() };
        RegisterClassW(&wc);
        bars.iter()
            .filter_map(|b| {
                let h = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED,
                    class,
                    w!(""),
                    WS_POPUP | WS_VISIBLE,
                    b.left,
                    b.top,
                    b.right - b.left,
                    b.bottom - b.top,
                    None,
                    None,
                    inst,
                    None,
                )
                .ok()?;
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 220, LWA_ALPHA);
                exclude_from_capture(h);
                Some(h)
            })
            .collect()
    }
}

// ---- the panel ----

thread_local! {
    /// What the panel shows: recorded seconds, paused.
    static PANEL: Cell<(u64, bool)> = const { Cell::new((0, false)) };
    /// The main window (panel buttons are posted to it), the button size, the button hovered.
    static PANEL_MAIN: Cell<isize> = const { Cell::new(0) };
    static PANEL_SIZE: Cell<i32> = const { Cell::new(32) };
    static PANEL_HOVER: Cell<Option<usize>> = const { Cell::new(None) };
}

/// The panel's layout (client coordinates): the time's place, and the buttons pause, stop, cancel.
fn layout(b: i32) -> (RECT, [RECT; 3]) {
    let pad = b / 5;
    let time = RECT { left: pad, top: pad, right: pad + b * 2 + b / 2, bottom: pad + b };
    let mut x = time.right + pad;
    let buttons = [0, 1, 2].map(|_| {
        let r = RECT { left: x, top: pad, right: x + b, bottom: pad + b };
        x += b + pad / 2;
        r
    });
    (time, buttons)
}

fn panel_size(b: i32) -> (i32, i32) {
    let (_, bs) = layout(b);
    (bs[2].right + b / 5, b + 2 * (b / 5))
}

unsafe fn round(hdc: HDC, r: RECT, radius: i32, color: COLORREF) {
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

unsafe fn font(height: i32, face: &str, weight: i32) -> HFONT {
    let f: Vec<u16> = face.encode_utf16().chain(Some(0)).collect();
    CreateFontW(-height, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32, (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32, PCWSTR(f.as_ptr()))
}

unsafe fn text(hdc: HDC, s: &str, r: RECT, f: HFONT, color: COLORREF, flags: DRAW_TEXT_FORMAT) {
    let old = SelectObject(hdc, f);
    SetTextColor(hdc, color);
    SetBkMode(hdc, TRANSPARENT);
    let mut w: Vec<u16> = s.encode_utf16().collect();
    let mut rr = r;
    let _ = DrawTextW(hdc, &mut w, &mut rr, flags | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    SelectObject(hdc, old);
}

unsafe extern "system" fn panel_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let b = PANEL_SIZE.with(|s| s.get());
    let at = || POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
    let hit = |p: POINT| layout(b).1.iter().position(|r| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom);
    match msg {
        WM_MOUSEMOVE => {
            let h = hit(at());
            if PANEL_HOVER.with(|c| c.replace(h)) != h {
                let _ = InvalidateRect(hwnd, None, false);
            }
            let mut tme = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT>() as u32,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        0x02A3 /* WM_MOUSELEAVE */ => {
            PANEL_HOVER.with(|c| c.set(None));
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(i) = hit(at()) {
                let main = PANEL_MAIN.with(|m| m.get());
                let action = [A_PAUSE, A_STOP, A_CANCEL][i];
                let _ = PostMessageW(HWND(main as *mut _), WM_PANEL, WPARAM(action), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default());
            LRESULT(1)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
            let old = SelectObject(mem, bmp);
            let bg = CreateSolidBrush(COLORREF(0x00221F1C));
            FillRect(mem, &rc, bg);
            let _ = DeleteObject(bg);
            let (secs, paused) = PANEL.with(|p| p.get());
            let (time, buttons) = layout(b);
            // A dot (red recording, amber paused) and the time.
            let d = b / 3;
            let dot = RECT { left: time.left + b / 6, top: time.top + (b - d) / 2, right: time.left + b / 6 + d, bottom: time.top + (b - d) / 2 + d };
            round(mem, dot, d, if paused { COLORREF(0x0000B4F5) } else { COLORREF(0x003C3CEB) });
            let f_time = font(b * 9 / 20, "Segoe UI", 600);
            let label = if paused { format!("{}:{:02} paused", secs / 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) };
            text(mem, &label, RECT { left: dot.right + b / 5, ..time }, f_time, COLORREF(0x00FFFFFF), DT_LEFT);
            let _ = DeleteObject(f_time);
            // Buttons: pause / resume, stop (save), cancel.
            let f_icon = font(b * 9 / 20, "Segoe Fluent Icons", 400);
            let glyphs = [if paused { '\u{E768}' } else { '\u{E769}' }, '\u{E71A}', '\u{E711}'];
            let hover = PANEL_HOVER.with(|c| c.get());
            for (i, r) in buttons.iter().enumerate() {
                if hover == Some(i) {
                    round(mem, *r, b / 4, if i == 1 { COLORREF(0x003C3CEB) } else { COLORREF(0x00403B37) });
                }
                text(mem, &glyphs[i].to_string(), *r, f_icon, COLORREF(0x00FFFFFF), DT_CENTER);
            }
            let _ = DeleteObject(f_icon);
            let _ = BitBlt(hdc, 0, 0, rc.right, rc.bottom, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(mem);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The panel, under the region's bottom-right corner (above it when there's no room, inside it
/// when neither fits), with rounded corners.
fn panel(r: RECT, main: HWND) -> HWND {
    unsafe {
        let m = MonitorFromPoint(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let b = (32 * dx as i32) / 96;
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut mi);
        let work = mi.rcWork;
        PANEL_SIZE.with(|s| s.set(b));
        PANEL_MAIN.with(|s| s.set(main.0 as isize));
        PANEL.with(|p| p.set((0, false)));
        let (w, h) = panel_size(b);
        let gap = 8;
        let x = (r.right - w).clamp(work.left, (work.right - w).max(work.left));
        let y = if r.bottom + gap + h <= work.bottom {
            r.bottom + gap
        } else if r.top - gap - h >= work.top {
            r.top - gap - h
        } else {
            r.bottom - gap - h
        };
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("LastShotPanel");
        let wc = WNDCLASSW { lpfnWndProc: Some(panel_proc), hInstance: inst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, class, w!("LastShot recording"), WS_POPUP | WS_VISIBLE, x, y, w, h, None, None, inst, None).unwrap_or_default();
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, b / 2, b / 2);
        let _ = SetWindowRgn(hwnd, rgn, true);
        exclude_from_capture(hwnd);
        hwnd
    }
}
