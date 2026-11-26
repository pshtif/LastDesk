// Created by Peter @sHTiF Stefcek
//! LastShot: a LastDesk plugin for screenshots and screen recordings. A camera in the bar: a
//! click picks a region (or a window) to capture, the right click has the rest. Captures are
//! saved as PNG in Pictures\LastShot and copied to the clipboard; recordings (a region, through
//! ffmpeg) are saved there as MP4. Talks to LastDesk in JSON lines (see LastDesk's plugins.rs).
#![windows_subsystem = "windows"]

mod capture;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use serde_json::{json, Value};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_LINE: u32 = WM_APP + 1;
const WM_EOF: u32 = WM_APP + 2;
const TICK: usize = 1;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn out(v: Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

struct Recording {
    child: Child,
    path: PathBuf,
    started: Instant,
    frame: Vec<HWND>,
}

struct App {
    main: HWND,
    folder: PathBuf,
    recent: Vec<PathBuf>,
    recording: Option<Recording>,
}

fn folder() -> PathBuf {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    base.join("Pictures").join("LastShot")
}

fn stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{}-{:02}-{:02} {:02}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

fn ffmpeg() -> Option<PathBuf> {
    let mut places: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg.exe")).collect()).unwrap_or_default();
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        places.push(PathBuf::from(l).join("Microsoft").join("WinGet").join("Links").join("ffmpeg.exe"));
    }
    places.into_iter().find(|p| p.is_file())
}

fn rect_of(v: &Value) -> RECT {
    let n = |k: &str| v[k].as_i64().unwrap_or(0) as i32;
    RECT { left: n("left"), top: n("top"), right: n("right"), bottom: n("bottom") }
}

impl App {
    /// The bar item and menu as they are now (recording or not).
    fn publish(&self) {
        match &self.recording {
            Some(r) => {
                let s = r.started.elapsed().as_secs();
                out(json!({ "type": "item", "text": "REC", "color": "red", "tooltip": format!("Recording {}:{:02} \u{00B7} click to stop", s / 60, s % 60) }));
            }
            None => out(json!({ "type": "item", "glyph": "\u{E722}", "tooltip": "LastShot \u{00B7} click: capture a region or window (Ctrl+Shift+S)" })),
        }
        let mut items = vec![
            json!({ "id": "region", "label": "Capture region or window\tCtrl+Shift+S", "enabled": self.recording.is_none() }),
            json!({ "id": "screen", "label": "Capture this monitor", "enabled": self.recording.is_none() }),
            json!({ "separator": true }),
            json!({ "id": "record", "label": if self.recording.is_some() { "Stop recording\tCtrl+Shift+R" } else { "Record region\tCtrl+Shift+R" } }),
            json!({ "separator": true }),
            json!({ "id": "folder", "label": "Open captures folder" }),
        ];
        if !self.recent.is_empty() {
            items.push(json!({ "separator": true }));
            for (n, p) in self.recent.iter().enumerate() {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                items.push(json!({ "id": format!("open:{n}"), "label": name }));
            }
        }
        out(json!({ "type": "menu", "items": items }));
    }

    fn remember(&mut self, p: PathBuf) {
        self.recent.retain(|x| x != &p);
        self.recent.insert(0, p);
        self.recent.truncate(5);
    }

    fn handle(&mut self, v: &Value) {
        let monitor = rect_of(&v["monitor"]);
        match (v["type"].as_str().unwrap_or(""), v["id"].as_str().unwrap_or(""), v["button"].as_str().unwrap_or("")) {
            ("init", _, _) => {
                out(json!({ "type": "hotkeys", "keys": [{ "id": "region", "key": "ctrl+shift+s" }, { "id": "record", "key": "ctrl+shift+r" }] }));
                self.publish();
            }
            ("click", _, "left") | ("hotkey", "region", _) | ("menu", "region", _) => {
                if self.recording.is_some() {
                    self.stop_recording();
                } else {
                    self.capture(monitor, false);
                }
            }
            ("click", _, "middle") | ("menu", "screen", _) => self.capture(monitor, true),
            ("hotkey", "record", _) | ("menu", "record", _) => {
                if self.recording.is_some() {
                    self.stop_recording();
                } else {
                    self.start_recording(monitor);
                }
            }
            ("menu", "folder", _) => open(&self.folder),
            ("menu", id, _) if id.starts_with("open:") => {
                if let Some(p) = id[5..].parse::<usize>().ok().and_then(|n| self.recent.get(n)) {
                    open(p);
                }
            }
            _ => {}
        }
    }

    /// A region or window picked on the monitor the user is on (or all of it), saved and copied.
    fn capture(&mut self, work: RECT, whole: bool) {
        let mon = capture::monitor_at(work);
        let Some(shot) = capture::grab(mon) else { return };
        let (picked, shot, strokes) = if whole { (Some(mon), shot, Vec::new()) } else { capture::pick(shot, false) };
        let Some(r) = picked else { return };
        let Some(mut part) = capture::crop(&shot, r) else { return };
        // What was drawn on it, into the picture (strokes are relative to the whole monitor).
        let (dx, dy) = (shot.x - part.x, shot.y - part.y);
        capture::burn(&mut part, &strokes, dx, dy);
        let _ = std::fs::create_dir_all(&self.folder);
        let path = self.folder.join(format!("LastShot {}.png", stamp()));
        if let Err(e) = capture::save_png(&part, &path) {
            out(json!({ "type": "notify", "title": "LastShot", "text": format!("Couldn't save it: {e}") }));
            return;
        }
        let copied = capture::to_clipboard(&part, self.main);
        out(json!({ "type": "notify", "title": format!("Captured {} \u{00D7} {}", part.w, part.h), "text": format!("{}{}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), if copied { " \u{00B7} copied" } else { "" }) }));
        self.remember(path);
        self.publish();
    }

    fn start_recording(&mut self, work: RECT) {
        let Some(ff) = ffmpeg() else {
            out(json!({ "type": "notify", "title": "LastShot", "text": "Recording needs ffmpeg (winget install ffmpeg)" }));
            return;
        };
        let mon = capture::monitor_at(work);
        let Some(shot) = capture::grab(mon) else { return };
        let (picked, _, _) = capture::pick(shot, true);
        let Some(r) = picked else { return };
        // H.264 wants even sizes.
        let (w, h) = ((r.right - r.left) & !1, (r.bottom - r.top) & !1);
        if w < 16 || h < 16 {
            return;
        }
        let _ = std::fs::create_dir_all(&self.folder);
        let path = self.folder.join(format!("LastShot {}.mp4", stamp()));
        use std::os::windows::process::CommandExt;
        let child = Command::new(ff)
            .args(["-hide_banner", "-loglevel", "error", "-f", "gdigrab", "-framerate", "30", "-draw_mouse", "1"])
            .args(["-offset_x", &r.left.to_string(), "-offset_y", &r.top.to_string(), "-video_size", &format!("{w}x{h}"), "-i", "desktop"])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-y"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        let child = match child {
            Ok(c) => c,
            Err(e) => {
                out(json!({ "type": "notify", "title": "LastShot", "text": format!("Couldn't start ffmpeg: {e}") }));
                return;
            }
        };
        let frame = frame_around(RECT { left: r.left, top: r.top, right: r.left + w, bottom: r.top + h });
        self.recording = Some(Recording { child, path, started: Instant::now(), frame });
        unsafe {
            SetTimer(self.main, TICK, 1000, None);
        }
        self.publish();
    }

    fn stop_recording(&mut self) {
        let Some(mut rec) = self.recording.take() else { return };
        unsafe {
            let _ = KillTimer(self.main, TICK);
            for f in &rec.frame {
                let _ = DestroyWindow(*f);
            }
        }
        // "q" asks ffmpeg to finish the file properly.
        if let Some(mut stdin) = rec.child.stdin.take() {
            let _ = stdin.write_all(b"q\n");
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = rec.child.try_wait() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = rec.child.kill();
        let secs = rec.started.elapsed().as_secs();
        if rec.path.is_file() {
            out(json!({ "type": "notify", "title": format!("Recorded {}:{:02}", secs / 60, secs % 60), "text": rec.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() }));
            self.remember(rec.path);
        } else {
            out(json!({ "type": "notify", "title": "LastShot", "text": "The recording couldn't be saved" }));
        }
        self.publish();
    }
}

fn open(p: &std::path::Path) {
    let w: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(w.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

unsafe extern "system" fn plain_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// A thin red frame just outside the recorded region (not in the recording), click-through.
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
                CreateWindowExW(
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
                .ok()
                .inspect(|h| {
                    let _ = SetLayeredWindowAttributes(*h, COLORREF(0), 220, LWA_ALPHA);
                })
            })
            .collect()
    }
}

thread_local! {
    static APP: std::cell::RefCell<Option<App>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LINE => {
            let line = *Box::from_raw(lp.0 as *mut String);
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                // Taken out while handling: the capture overlay runs its own message loop.
                let app = APP.with(|a| a.borrow_mut().take());
                if let Some(mut app) = app {
                    app.handle(&v);
                    APP.with(|a| *a.borrow_mut() = Some(app));
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TICK => {
            APP.with(|a| {
                if let Some(app) = a.borrow().as_ref() {
                    app.publish();
                }
            });
            LRESULT(0)
        }
        WM_EOF => {
            // LastDesk stopped this plugin (or exited): a recording is finished first.
            let app = APP.with(|a| a.borrow_mut().take());
            if let Some(mut app) = app {
                app.stop_recording();
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("LastShotMain");
        let wc = WNDCLASSW { lpfnWndProc: Some(main_proc), hInstance: inst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let main = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("LastShot"), WINDOW_STYLE(0), 0, 0, 0, 0, HWND_MESSAGE, None, inst, None).unwrap_or_default();
        APP.with(|a| *a.borrow_mut() = Some(App { main, folder: folder(), recent: Vec::new(), recording: None }));
        // LastDesk's lines, handed to the window thread one by one.
        let target = main.0 as isize;
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                let p = Box::into_raw(Box::new(line));
                let _ = PostMessageW(HWND(target as *mut _), WM_LINE, WPARAM(0), LPARAM(p as isize));
            }
            let _ = PostMessageW(HWND(target as *mut _), WM_EOF, WPARAM(0), LPARAM(0));
        });
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}
