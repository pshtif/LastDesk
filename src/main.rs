//! monodesk: per-monitor workspaces for Windows.
//!
//! Alt+1..4        switch the workspace of the monitor you're on
//! Alt+Shift+1..4  move the focused window to that workspace (same monitor)
//! Alt+Shift+P     pin/unpin the focused window (visible on every workspace)
//!
//! "The monitor you're on" = the monitor of the focused window, or of the mouse if no
//! window is focused. Windows are assigned to a workspace lazily: whatever is visible on a
//! monitor when you switch away belongs to the workspace you left.
//!
//! Crash safety: every hidden window is recorded in %LOCALAPPDATA%\monodesk\hidden.txt and
//! restored on exit, on panic, on display changes and on the next start.

#![windows_subsystem = "windows"]

mod cloak;
mod ui;
mod win;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, MonitorFromWindow, PAINTSTRUCT, MONITOR_DEFAULTTONEAREST, CreateRoundRectRgn, SetWindowRgn};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use cloak::{Cloaker, Method};
use win::{hwnd_key, key_hwnd, Monitor};

const WORKSPACES: usize = 4;
const WM_TRAY: u32 = WM_APP + 1;
const HK_FOCUS: i32 = 1; // ids 1..=WORKSPACES
const HK_MOVE: i32 = 101; // ids 101..
const HK_PIN: i32 = 200;
const OSD_TIMER: usize = 1;
const OSD_MS: u32 = 900;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APP_NAME: PCWSTR = w!("monodesk");

struct MonState {
    name: String,
    active: usize,
    /// Windows hidden on each (inactive) workspace, z-order topmost first.
    hidden: Vec<Vec<isize>>,
}

struct App {
    main: HWND,
    osd: HWND,
    cloaker: Cloaker,
    monitors: Vec<Monitor>,
    mons: Vec<MonState>,
    methods: HashMap<isize, Method>,
    pinned: HashSet<isize>,
    osd_model: ui::OsdModel,
    osd_scale: f32,
    tray_icon: HICON,
    taskbar_created: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn state_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("monodesk").join("hidden.txt")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

fn monitor_labels(n: usize) -> Vec<&'static str> {
    match n {
        1 => vec!["Monitor"],
        2 => vec!["Left monitor", "Right monitor"],
        3 => vec!["Left monitor", "Middle monitor", "Right monitor"],
        _ => (0..n).map(|_| "Monitor").collect(),
    }
}

/// Restore windows recorded by a previous run that didn't exit cleanly.
fn recover_from_file(cloaker: &Cloaker) {
    let path = state_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (Some(k), Some(m)) = (it.next(), it.next()) else { continue };
            let Ok(k) = k.parse::<isize>() else { continue };
            let h = key_hwnd(k);
            if unsafe { IsWindow(h).as_bool() } {
                cloaker.show(h, if m == "H" { Method::Hide } else { Method::Cloak });
            }
        }
    }
    let _ = std::fs::remove_file(path);
}

impl App {
    fn mon_index(&self, name: &str) -> Option<usize> {
        self.mons.iter().position(|m| m.name == name)
    }

    fn rebuild_monitors(&mut self) {
        self.monitors = win::monitors();
        self.mons = self
            .monitors
            .iter()
            .map(|m| MonState { name: m.name.clone(), active: 0, hidden: vec![Vec::new(); WORKSPACES] })
            .collect();
    }

    /// The focused window's monitor, else the mouse's monitor.
    fn current_mon(&self) -> usize {
        unsafe {
            let fg = GetForegroundWindow();
            if win::is_manageable(fg, false) {
                if let Some(i) = win::monitor_name_of_window(fg).and_then(|n| self.mon_index(&n)) {
                    return i;
                }
            }
        }
        win::monitor_name_at_cursor().and_then(|n| self.mon_index(&n)).unwrap_or(0)
    }

    fn visible_on(&self, m: usize) -> Vec<HWND> {
        let name = &self.mons[m].name;
        win::visible_windows()
            .into_iter()
            .filter(|h| !self.pinned.contains(&hwnd_key(*h)))
            .filter(|h| win::monitor_name_of_window(*h).as_deref() == Some(name.as_str()))
            .collect()
    }

    fn hide(&mut self, h: HWND) {
        let m = self.cloaker.hide(h);
        self.methods.insert(hwnd_key(h), m);
    }

    fn show(&mut self, k: isize) -> Option<HWND> {
        let h = key_hwnd(k);
        let m = self.methods.remove(&k).unwrap_or(Method::Cloak);
        if unsafe { !IsWindow(h).as_bool() } {
            return None;
        }
        self.cloaker.show(h, m);
        Some(h)
    }

    fn prune(&mut self) {
        for ms in &mut self.mons {
            for ws in &mut ms.hidden {
                ws.retain(|k| unsafe { IsWindow(key_hwnd(*k)).as_bool() });
            }
        }
        self.methods.retain(|k, _| unsafe { IsWindow(key_hwnd(*k)).as_bool() });
    }

    /// Give focus to something on monitor `m` after its windows went away.
    fn focus_empty(&self, m: usize) {
        unsafe {
            let _ = SetForegroundWindow(GetShellWindow());
            // Keep "the monitor you're on" pointing here: move the mouse if it's elsewhere.
            if win::monitor_name_at_cursor().as_deref() != Some(self.mons[m].name.as_str()) {
                let r = self.monitors[m].work;
                let _ = SetCursorPos((r.left + r.right) / 2, (r.top + r.bottom) / 2);
            }
        }
    }

    fn switch_to(&mut self, m: usize, target: usize, focus: Option<HWND>) {
        if target == self.mons[m].active {
            self.show_osd(m, None);
            return;
        }
        let leaving = self.visible_on(m);
        // Show the target first, then hide the old one: less desktop flash.
        let to_show = std::mem::take(&mut self.mons[m].hidden[target]);
        let mut first = None;
        for k in to_show {
            if let Some(h) = self.show(k) {
                if first.is_none() && unsafe { !IsIconic(h).as_bool() } {
                    first = Some(h);
                }
            }
        }
        for h in &leaving {
            if Some(*h) != focus {
                self.hide(*h);
            }
        }
        let old = self.mons[m].active;
        self.mons[m].hidden[old] = leaving.iter().filter(|h| Some(**h) != focus).map(|h| hwnd_key(*h)).collect();
        self.mons[m].active = target;
        match focus.or(first) {
            Some(h) => unsafe {
                let _ = SetForegroundWindow(h);
            },
            None => self.focus_empty(m),
        }
        self.save();
        self.update_tray();
        self.show_osd(m, None);
    }

    fn move_focused(&mut self, target: usize) {
        let fg = unsafe { GetForegroundWindow() };
        if !win::is_manageable(fg, false) {
            return;
        }
        let Some(m) = win::monitor_name_of_window(fg).and_then(|n| self.mon_index(&n)) else { return };
        if target == self.mons[m].active {
            self.show_osd(m, None);
            return;
        }
        let k = hwnd_key(fg);
        self.pinned.remove(&k);
        self.hide(fg);
        self.mons[m].hidden[target].insert(0, k);
        match self.visible_on(m).into_iter().find(|h| *h != fg) {
            Some(h) => unsafe {
                let _ = SetForegroundWindow(h);
            },
            None => self.focus_empty(m),
        }
        self.save();
        self.show_osd(m, Some(format!("Moved window to workspace {}", target + 1)));
    }

    fn toggle_pin(&mut self) {
        let fg = unsafe { GetForegroundWindow() };
        if !win::is_manageable(fg, false) {
            return;
        }
        let k = hwnd_key(fg);
        let pinned = if self.pinned.remove(&k) { false } else { self.pinned.insert(k) };
        let m = self.current_mon();
        self.show_osd(m, Some(if pinned { "Pinned: shown on every workspace".into() } else { "Unpinned".into() }));
    }

    /// Activation of a hidden window (taskbar click, app raising itself): go to its workspace.
    fn on_foreground(&mut self, h: HWND) {
        let k = hwnd_key(h);
        let found = self.mons.iter().enumerate().find_map(|(mi, ms)| {
            ms.hidden.iter().position(|ws| ws.contains(&k)).map(|wi| (mi, wi))
        });
        if let Some((m, w)) = found {
            self.switch_to(m, w, None);
            unsafe {
                let _ = SetForegroundWindow(h);
            }
        }
    }

    fn reveal_all(&mut self) {
        let keys: Vec<isize> = self.mons.iter_mut().flat_map(|ms| ms.hidden.iter_mut().flat_map(std::mem::take)).collect();
        for k in keys {
            self.show(k);
        }
        for ms in &mut self.mons {
            ms.active = 0;
        }
        self.methods.clear();
        self.save();
        self.update_tray();
    }

    fn save(&mut self) {
        self.prune();
        let mut out = String::new();
        let mut list = Vec::new();
        for ms in &self.mons {
            for ws in &ms.hidden {
                for k in ws {
                    let m = *self.methods.get(k).unwrap_or(&Method::Cloak);
                    out.push_str(&format!("{} {}\n", k, if m == Method::Hide { "H" } else { "C" }));
                    list.push((*k, m));
                }
            }
        }
        let path = state_path();
        if list.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, out);
        }
        *PANIC_LIST.lock().unwrap_or_else(|e| e.into_inner()) = list;
    }

    fn counts(&self, m: usize) -> Vec<usize> {
        (0..WORKSPACES)
            .map(|w| if w == self.mons[m].active { self.visible_on(m).len() } else { self.mons[m].hidden[w].len() })
            .collect()
    }

    fn show_osd(&mut self, m: usize, note: Option<String>) {
        let labels = monitor_labels(self.mons.len());
        self.osd_model = ui::OsdModel {
            label: note.unwrap_or_else(|| labels[m].to_string()),
            active: self.mons[m].active,
            counts: self.counts(m),
        };
        let mon = &self.monitors[m];
        unsafe {
            let mut dpi_x = 96;
            let mut dpi_y = 96;
            let hm = MonitorFromWindow(self.main, MONITOR_DEFAULTTONEAREST);
            let _ = hm; // fall back to 96 if the monitor lookup below fails
            let pt = POINT { x: (mon.rect.left + mon.rect.right) / 2, y: (mon.rect.top + mon.rect.bottom) / 2 };
            let hm = windows::Win32::Graphics::Gdi::MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let _ = GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            self.osd_scale = dpi_x as f32 / 96.0;
            let l = ui::osd_layout(WORKSPACES, self.osd_scale);
            let x = (mon.work.left + mon.work.right - l.w) / 2;
            let y = mon.work.top + (mon.work.bottom - mon.work.top) * 2 / 3 - l.h / 2;
            let rgn = CreateRoundRectRgn(0, 0, l.w + 1, l.h + 1, (14.0 * self.osd_scale) as i32, (14.0 * self.osd_scale) as i32);
            let _ = SetWindowPos(self.osd, HWND_TOPMOST, x, y, l.w, l.h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            SetWindowRgn(self.osd, rgn, true);
            let _ = InvalidateRect(self.osd, None, true);
            SetTimer(self.osd, OSD_TIMER, OSD_MS, None);
        }
    }

    fn tray_text(&self) -> (String, String) {
        let labels = monitor_labels(self.mons.len());
        let icon: String = self.mons.iter().take(3).map(|m| (m.active + 1).to_string()).collect();
        let tip = self
            .mons
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{}: {}", labels[i].replace(" monitor", ""), m.active + 1))
            .collect::<Vec<_>>()
            .join(" · ");
        (icon, format!("monodesk — {tip}"))
    }

    fn update_tray(&mut self) {
        let (text, tip) = self.tray_text();
        let icon = ui::make_tray_icon(&text);
        let mut nid = tray_data(self.main);
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = icon;
        copy_wide(&mut nid.szTip, &tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            if !self.tray_icon.is_invalid() {
                let _ = DestroyIcon(self.tray_icon);
            }
        }
        self.tray_icon = icon;
    }

    fn add_tray(&mut self) {
        let mut nid = tray_data(self.main);
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        let (text, tip) = self.tray_text();
        self.tray_icon = ui::make_tray_icon(&text);
        nid.hIcon = self.tray_icon;
        copy_wide(&mut nid.szTip, &tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        }
    }
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() }
}

static PANIC_LIST: std::sync::Mutex<Vec<(isize, Method)>> = std::sync::Mutex::new(Vec::new());

// ---------- autostart ----------

fn autostart_enabled() -> bool {
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, APP_NAME, RRF_RT_REG_SZ, None, None, None).is_ok()
    }
}

fn set_autostart(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
            let v = wide(&format!("\"{exe}\""));
            let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, APP_NAME, REG_SZ.0, Some(bytes.as_ptr() as *const _), bytes.len() as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, APP_NAME);
        }
    }
}

// ---------- tray menu ----------

enum MenuAction {
    Go(usize, usize, Option<isize>),
    RevealAll,
    Autostart(bool),
    Exit,
}

fn show_menu(hwnd: HWND) {
    // Build the model while borrowed, then run the (message-pumping) menu without a borrow.
    let Some(entries) = with_app(|app| {
        app.prune();
        let labels = monitor_labels(app.mons.len());
        let mut e: Vec<(String, Option<MenuAction>, bool)> = Vec::new(); // label, action, checked
        for m in 0..app.mons.len() {
            e.push((labels[m].to_string(), None, false));
            let visible = app.visible_on(m);
            for w in 0..WORKSPACES {
                let wins: Vec<isize> = if w == app.mons[m].active {
                    visible.iter().map(|h| hwnd_key(*h)).collect()
                } else {
                    app.mons[m].hidden[w].clone()
                };
                let state = if w == app.mons[m].active { "  (shown)" } else if wins.is_empty() { "  (empty)" } else { "" };
                e.push((format!("    Workspace {}{}", w + 1, state), Some(MenuAction::Go(m, w, None)), w == app.mons[m].active));
                for k in wins {
                    let mut t = win::title(key_hwnd(k));
                    if t.chars().count() > 60 {
                        t = t.chars().take(57).collect::<String>() + "…";
                    }
                    e.push((format!("            {}", t.replace('&', "&&")), Some(MenuAction::Go(m, w, Some(k))), false));
                }
            }
            e.push((String::new(), None, false)); // separator
        }
        e.push(("Show all windows (reset)".into(), Some(MenuAction::RevealAll), false));
        let auto = autostart_enabled();
        e.push(("Start with Windows".into(), Some(MenuAction::Autostart(!auto)), auto));
        e.push(("Exit (shows all windows)".into(), Some(MenuAction::Exit), false));
        e
    }) else { return };

    unsafe {
        let menu = CreatePopupMenu().unwrap();
        for (i, (label, action, checked)) in entries.iter().enumerate() {
            if label.is_empty() {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                continue;
            }
            let mut flags = MF_STRING;
            if action.is_none() {
                flags |= MF_GRAYED;
            }
            if *checked {
                flags |= MF_CHECKED;
            }
            let w = wide(label);
            let _ = AppendMenuW(menu, flags, i + 1, PCWSTR(w.as_ptr()));
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        let idx = cmd.0 as usize;
        if idx == 0 || idx > entries.len() {
            return;
        }
        match &entries[idx - 1].1 {
            Some(MenuAction::Go(m, w, k)) => {
                with_app(|app| {
                    let target = k.map(key_hwnd);
                    if *w == app.mons[*m].active {
                        if let Some(h) = target {
                            let _ = SetForegroundWindow(h);
                        }
                        app.show_osd(*m, None);
                    } else {
                        app.switch_to(*m, *w, None);
                        if let Some(h) = target {
                            let _ = SetForegroundWindow(h);
                        }
                    }
                });
            }
            Some(MenuAction::RevealAll) => {
                with_app(|app| app.reveal_all());
            }
            Some(MenuAction::Autostart(on)) => set_autostart(*on),
            Some(MenuAction::Exit) => {
                let _ = DestroyWindow(hwnd);
            }
            None => {}
        }
    }
}

// ---------- window procedures ----------

/// Send an unassigned key while Alt is held so releasing Alt doesn't open app menus.
fn mask_alt() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0xE8), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    unsafe {
        SendInput(&[key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)], std::mem::size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            mask_alt();
            let id = wp.0 as i32;
            with_app(|app| {
                if (HK_FOCUS..HK_FOCUS + WORKSPACES as i32).contains(&id) {
                    let m = app.current_mon();
                    app.switch_to(m, (id - HK_FOCUS) as usize, None);
                } else if (HK_MOVE..HK_MOVE + WORKSPACES as i32).contains(&id) {
                    app.move_focused((id - HK_MOVE) as usize);
                } else if id == HK_PIN {
                    app.toggle_pin();
                }
            });
            LRESULT(0)
        }
        WM_TRAY => {
            let ev = (lp.0 & 0xFFFF) as u32;
            if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            with_app(|app| {
                app.reveal_all();
                trace("app"); app.rebuild_monitors(); trace("monitors");
                app.update_tray();
            });
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            with_app(|app| app.reveal_all());
            LRESULT(0)
        }
        WM_DESTROY => {
            with_app(|app| {
                app.reveal_all();
                let nid = tray_data(app.main);
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            });
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            let tc = with_app(|a| a.taskbar_created).unwrap_or(0);
            if tc != 0 && msg == tc {
                with_app(|app| app.add_tray());
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    }
}

unsafe extern "system" fn osd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            with_app(|app| ui::paint_osd(hdc, &app.osd_model, app.osd_scale));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER => {
            let _ = KillTimer(hwnd, OSD_TIMER);
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn win_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, _: i32, _: u32, _: u32) {
    if event == EVENT_SYSTEM_FOREGROUND && id_object == OBJID_WINDOW.0 && !hwnd.is_invalid() {
        let root = GetAncestor(hwnd, GA_ROOT);
        with_app(|app| app.on_foreground(if root.is_invalid() { hwnd } else { root }));
    }
}

fn trace(s: &str) {
    if std::env::var_os("MONODESK_TRACE").is_some() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("monodesk-trace.txt")) {
            let _ = writeln!(f, "{s}");
        }
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _mutex = CreateMutexW(None, true, w!("Local\\monodesk-single-instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            MessageBoxW(None, w!("monodesk is already running (see the tray)."), w!("monodesk"), MB_OK | MB_ICONINFORMATION);
            return;
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        trace("com"); let cloaker = Cloaker::new(); trace("cloaker");
        recover_from_file(&cloaker); trace("recovered");

        // On panic: restore every hidden window before the process dies.
        std::panic::set_hook(Box::new(|info| {
            let log = state_path().with_file_name("crash.log");
            if let Some(dir) = log.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&log, format!("{info}\n{}", std::backtrace::Backtrace::force_capture()));
            let c = Cloaker::new();
            for (k, m) in PANIC_LIST.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                c.show(key_hwnd(*k), *m);
            }
            let _ = std::fs::remove_file(state_path());
        }));

        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let main_class = WNDCLASSW { lpfnWndProc: Some(main_proc), hInstance: hinst, lpszClassName: w!("MonodeskMain"), ..Default::default() };
        RegisterClassW(&main_class);
        let osd_class = WNDCLASSW {
            lpfnWndProc: Some(osd_proc),
            hInstance: hinst,
            lpszClassName: w!("MonodeskOSD"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&osd_class); trace("classes");

        let main = CreateWindowExW(WINDOW_EX_STYLE(0), w!("MonodeskMain"), w!("monodesk"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, hinst, None).unwrap();
        let osd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            w!("MonodeskOSD"),
            w!("monodesk osd"),
            WS_POPUP,
            0, 0, 10, 10,
            None, None, hinst, None,
        )
        .unwrap();
        trace("windows"); let _ = SetLayeredWindowAttributes(osd, COLORREF(0), 240, LWA_ALPHA);

        let mut app = App {
            main,
            osd,
            cloaker,
            monitors: Vec::new(),
            mons: Vec::new(),
            methods: HashMap::new(),
            pinned: HashSet::new(),
            osd_model: ui::OsdModel { label: String::new(), active: 0, counts: vec![0; WORKSPACES] },
            osd_scale: 1.0,
            tray_icon: HICON::default(),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
        };
        trace("app"); app.rebuild_monitors(); trace("monitors");
        app.add_tray(); trace("tray");
        APP.with(|a| *a.borrow_mut() = Some(app));

        // Hotkeys.
        let mut failed = Vec::new();
        for i in 0..WORKSPACES as u32 {
            let vk = 0x31 + i; // '1'..
            if RegisterHotKey(main, HK_FOCUS + i as i32, MOD_ALT | MOD_NOREPEAT, vk).is_err() {
                failed.push(format!("Alt+{}", i + 1));
            }
            if RegisterHotKey(main, HK_MOVE + i as i32, MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, vk).is_err() {
                failed.push(format!("Alt+Shift+{}", i + 1));
            }
        }
        if RegisterHotKey(main, HK_PIN, MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, 'P' as u32).is_err() {
            failed.push("Alt+Shift+P".into());
        }
        if !failed.is_empty() {
            let msg = wide(&format!("These shortcuts are used by another app and won't work:\n{}", failed.join(", ")));
            MessageBoxW(None, PCWSTR(msg.as_ptr()), w!("monodesk"), MB_OK | MB_ICONWARNING);
        }

        trace("hotkeys"); let _hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT);

        trace("loop"); let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
