// Created by Peter @sHTiF Stefcek
//! LastShot: a LastDesk plugin for screenshots and screen recordings. A camera in the bar: a
//! click picks a region (or a window) to capture, the right click has the rest. Captures are
//! saved as PNG in Pictures\LastShot and copied to the clipboard; recordings (a region, through
//! ffmpeg) are saved there as MP4. Talks to LastDesk in JSON lines (see LastDesk's plugins.rs).
#![windows_subsystem = "windows"]

mod capture;
mod record;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_LINE: u32 = WM_APP + 1;
const WM_EOF: u32 = WM_APP + 2;
const TICK: usize = 1;

fn out(v: Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

struct App {
    main: HWND,
    folder: PathBuf,
    recent: Vec<PathBuf>,
    recording: Option<record::Recording>,
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
                let s = r.elapsed().as_secs();
                let (text, color, what) = if r.paused() { ("PAUSED", "attention", "Paused") } else { ("REC", "red", "Recording") };
                out(json!({ "type": "item", "text": text, "color": color, "tooltip": format!("{what} {}:{:02} \u{00B7} click to stop and save", s / 60, s % 60) }));
            }
            None => out(json!({ "type": "item", "glyph": "\u{E722}", "tooltip": "LastShot \u{00B7} click: capture a region or window (Ctrl+Shift+S)" })),
        }
        let mut items = vec![
            json!({ "id": "region", "label": "Capture region or window\tCtrl+Shift+S", "enabled": self.recording.is_none() }),
            json!({ "id": "screen", "label": "Capture this monitor", "enabled": self.recording.is_none() }),
            json!({ "separator": true }),
            json!({ "id": "record", "label": if self.recording.is_some() { "Stop and save recording\tCtrl+Shift+R" } else { "Record region\tCtrl+Shift+R" } }),
            json!({ "separator": true }),
            json!({ "id": "folder", "label": "Open captures folder" }),
        ];
        if let Some(r) = &self.recording {
            items.insert(4, json!({ "id": "pause", "label": if r.paused() { "Resume recording" } else { "Pause recording" } }));
            items.insert(5, json!({ "id": "cancel", "label": "Cancel recording" }));
        }
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
            ("menu", "pause", _) => self.toggle_pause(),
            ("menu", "cancel", _) => self.cancel_recording(),
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
        let _ = std::fs::create_dir_all(&self.folder);
        let path = self.folder.join(format!("LastShot {}.mp4", stamp()));
        match record::Recording::start(ff, r, path, self.main) {
            Ok(rec) => {
                self.recording = Some(rec);
                unsafe {
                    SetTimer(self.main, TICK, 1000, None);
                }
            }
            Err(e) => out(json!({ "type": "notify", "title": "LastShot", "text": e })),
        }
        self.publish();
    }

    fn toggle_pause(&mut self) {
        let Some(r) = self.recording.as_mut() else { return };
        if r.paused() {
            if let Err(e) = r.resume() {
                out(json!({ "type": "notify", "title": "LastShot", "text": e }));
            }
        } else {
            r.pause();
        }
        self.publish();
    }

    fn stop_recording(&mut self) {
        let Some(rec) = self.recording.take() else { return };
        unsafe {
            let _ = KillTimer(self.main, TICK);
        }
        let secs = rec.elapsed().as_secs();
        match rec.finish() {
            Some(p) => {
                out(json!({ "type": "notify", "title": format!("Recorded {}:{:02}", secs / 60, secs % 60), "text": p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() }));
                self.remember(p);
            }
            None => out(json!({ "type": "notify", "title": "LastShot", "text": "The recording couldn't be saved" })),
        }
        self.publish();
    }

    fn cancel_recording(&mut self) {
        let Some(rec) = self.recording.take() else { return };
        unsafe {
            let _ = KillTimer(self.main, TICK);
        }
        rec.cancel();
        self.publish();
    }
}

fn open(p: &std::path::Path) {
    let w: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(w.as_ptr()), None, None, SW_SHOWNORMAL);
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
                    if let Some(r) = &app.recording {
                        r.show();
                    }
                    app.publish();
                }
            });
            LRESULT(0)
        }
        record::WM_PANEL => {
            let app = APP.with(|a| a.borrow_mut().take());
            if let Some(mut app) = app {
                match wp.0 {
                    record::A_PAUSE => app.toggle_pause(),
                    record::A_STOP => app.stop_recording(),
                    record::A_CANCEL => app.cancel_recording(),
                    _ => {}
                }
                APP.with(|a| *a.borrow_mut() = Some(app));
            }
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
