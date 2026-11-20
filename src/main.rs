//! LastDesk: per-monitor workspaces for Windows.
//!
//! Alt+1..4        switch the workspace of the monitor you're on
//! Alt+Shift+1..4  move the focused window to that workspace (same monitor)
//! Alt+Shift+P     pin/unpin the focused window (visible on every workspace)
//!
//! "The monitor you're on" = the monitor of the focused window, or of the mouse if no
//! window is focused. Windows are assigned to a workspace lazily: whatever is visible on a
//! monitor when you switch away belongs to the workspace you left.
//!
//! Crash safety: every hidden window is recorded in %LOCALAPPDATA%\LastDesk\hidden.txt and
//! restored on exit, on panic, on display changes and on the next start.

#![windows_subsystem = "windows"]

mod anim;
mod bar;
mod cloak;
mod config;
mod layout;
mod overview;
mod settings;
mod taskbar;
mod tray;
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

/// Workspaces per monitor (from config, 1..=9).
static WS_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(4);

fn workspaces() -> usize {
    WS_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

const WM_TRAY: u32 = WM_APP + 1;
/// Sent by `lastdesk.exe --settings` to the running instance.
const WM_OPEN_SETTINGS: u32 = WM_APP + 3;
const HK_FOCUS: i32 = 1; // ids 1..=9
const HK_MOVE: i32 = 101; // ids 101..=109
const HK_PIN: i32 = 200;
const HK_OVERVIEW: i32 = 300;
const HK_NEXT: i32 = 301;
const HK_PREV: i32 = 302;
const OSD_TIMER: usize = 1;
const RULE_TIMER: usize = 2;
const RULE_DELAY_MS: u32 = 250;
const CLOCK_TIMER: usize = 3;
const BAR_TIMER: usize = 4;
const PLACE_TIMER: usize = 5;
const TRAY_TIMER: usize = 6;
const OSD_MS: u32 = 900;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APP_NAME: PCWSTR = w!("LastDesk");

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
    overview: Option<overview::Overview>,
    /// Monitor the mouse was on at the last focus change (see `current_mon`).
    focus_mouse_mon: Option<String>,
    /// While set, `save()` doesn't touch layout.txt (exit/reveal that should be undone later).
    layout_frozen: bool,
    config: config::Config,
    /// Lower-case exe name per window (process lookups are not free).
    exe_cache: RefCell<HashMap<isize, String>>,
    /// Windows that existed already / were checked against app rules.
    seen: HashSet<isize>,
    /// New windows waiting for the rule timer (the shell needs a moment before it can cloak them).
    pending: Vec<isize>,
    /// Start was opened from this monitor's bar: (monitor, when, windows that already existed).
    /// New windows appearing soon after go to that monitor.
    launch: Option<(usize, std::time::Instant, HashSet<isize>)>,
    bars: Vec<bar::Bar>,
    /// LastDesk is keeping the Windows taskbar hidden (its bar sits in the taskbar's strip).
    taskbar_hidden: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn state_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("LastDesk").join("hidden.txt")
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
            .map(|m| MonState { name: m.name.clone(), active: 0, hidden: vec![Vec::new(); workspaces()] })
            .collect();
    }

    /// "The monitor you're on": whichever moved last, focus or mouse.
    /// If the mouse is on a different monitor than it was at the last focus change, the user
    /// has moved to it since, so the mouse wins. Otherwise the focused window's monitor wins.
    fn current_mon(&self) -> usize {
        let mouse = win::monitor_name_at_cursor();
        if let (Some(now), Some(then)) = (&mouse, &self.focus_mouse_mon) {
            if now != then {
                if let Some(i) = self.mon_index(now) {
                    return i;
                }
            }
        }
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

    fn exe(&self, h: HWND) -> String {
        let k = hwnd_key(h);
        if let Some(e) = self.exe_cache.borrow().get(&k) {
            return e.clone();
        }
        let e = overview::app_name(h).to_lowercase();
        self.exe_cache.borrow_mut().insert(k, e.clone());
        e
    }

    fn is_pinned_app(&self, h: HWND) -> bool {
        !self.config.pinned_apps.is_empty() && {
            let e = self.exe(h);
            self.config.pinned_apps.iter().any(|a| *a == e)
        }
    }

    fn visible_on(&self, m: usize) -> Vec<HWND> {
        let name = &self.mons[m].name;
        win::visible_windows()
            .into_iter()
            .filter(|h| !self.pinned.contains(&hwnd_key(*h)) && !self.is_pinned_app(*h))
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
        self.seen.retain(|k| unsafe { IsWindow(key_hwnd(*k)).as_bool() });
        self.exe_cache.borrow_mut().retain(|k, _| unsafe { IsWindow(key_hwnd(*k)).as_bool() });
    }

    /// A window was shown: queue it for the rule check if it's new.
    fn on_show(&mut self, h: HWND) {
        let k = hwnd_key(h);
        if (self.config.rules.is_empty() && self.launch_target(k).is_none())
            || self.seen.contains(&k)
            || !win::is_manageable(h, false)
        {
            return;
        }
        self.seen.insert(k);
        self.pending.push(k);
        unsafe {
            SetTimer(self.main, RULE_TIMER, RULE_DELAY_MS, None);
        }
    }

    fn run_pending_rules(&mut self) {
        for k in std::mem::take(&mut self.pending) {
            let h = key_hwnd(k);
            if win::is_manageable(h, false) && !self.apply_rule(h) {
                self.apply_launch(h);
            }
        }
    }

    /// Monitor a new window should open on because Start was just opened from that
    /// monitor's bar (Windows itself always opens them on the main monitor).
    fn launch_target(&self, k: isize) -> Option<usize> {
        const WINDOW: std::time::Duration = std::time::Duration::from_secs(20);
        match &self.launch {
            Some((m, at, existing)) if at.elapsed() < WINDOW && !existing.contains(&k) => Some(*m),
            _ => None,
        }
    }

    fn apply_launch(&mut self, h: HWND) {
        let k = hwnd_key(h);
        let Some(to) = self.launch_target(k) else { return };
        let Some(cur) = win::monitor_name_of_window(h).and_then(|n| self.mon_index(&n)) else { return };
        if cur != to && to < self.mons.len() {
            self.move_window(k, (cur, self.mons[cur].active), (to, self.mons[to].active));
        }
    }

    /// Remember that Start was opened from monitor `m`'s bar.
    pub(crate) fn note_start_opened(&mut self, m: usize) {
        self.launch = Some((m, std::time::Instant::now(), win::all_top_level().into_iter().map(hwnd_key).collect()));
    }

    /// Send window h to the workspace its app rule names. Returns whether a rule matched.
    fn apply_rule(&mut self, h: HWND) -> bool {
        let k = hwnd_key(h);
        let exe = self.exe(h);
        let Some(rule) = self.config.rules.iter().find(|r| r.app == exe) else { return false };
        let to = (rule.monitor.min(self.mons.len().saturating_sub(1)), rule.workspace.min(workspaces() - 1));
        let Some(cur) = win::monitor_name_of_window(h).and_then(|n| self.mon_index(&n)) else { return true };
        let from = (cur, self.mons[cur].active);
        if from == to {
            return true;
        }
        self.move_window(k, from, to);
        if to.1 != self.mons[to.0].active {
            let labels = monitor_labels(self.mons.len());
            self.show_osd(to.0, Some(format!("{} → {}, workspace {}", overview::app_name(h), labels[to.0], to.1 + 1)));
        }
        true
    }

    /// Hide or restore the Windows taskbar to match the config.
    fn taskbar_apply(&mut self) {
        let want = self.config.bar && self.config.hide_taskbar && !self.bars.is_empty();
        if want && !self.taskbar_hidden {
            taskbar::hide();
            self.taskbar_hidden = true;
        } else if !want && self.taskbar_hidden {
            taskbar::restore();
            self.taskbar_hidden = false;
        }
        // The freed (or re-taken) strip changes where the bars belong: re-place now and
        // once more shortly, in case Explorer applies the change late.
        self.bars_place_all();
        unsafe {
            SetTimer(self.main, PLACE_TIMER, 400, None);
        }
    }

    /// Start/stop hosting tray icons to match the config.
    fn tray_apply(&mut self) {
        let want = self.config.bar && self.config.tray && !self.bars.is_empty();
        if want && !tray::active() {
            tray::create(self.main);
            unsafe {
                SetTimer(self.main, TRAY_TIMER, 2000, None);
            }
        } else if !want && tray::active() {
            tray::destroy();
            unsafe {
                let _ = KillTimer(self.main, TRAY_TIMER);
            }
        }
        self.bars_refresh();
    }

    fn bars_place_all(&mut self) {
        let hwnds: Vec<HWND> = self.bars.iter().map(|b| b.hwnd).collect();
        for h in hwnds {
            self.bar_reposition(h);
        }
        self.taskbar_enforce();
    }

    fn taskbar_restore(&mut self) {
        if self.taskbar_hidden {
            taskbar::restore();
            self.taskbar_hidden = false;
        }
    }

    /// Explorer shows its taskbar again now and then: hide it unless the shell UI is in use.
    fn taskbar_enforce(&self) {
        if self.taskbar_hidden {
            let fg = unsafe { GetForegroundWindow() };
            if !taskbar::is_shell_ui(fg) {
                taskbar::hide_all();
            }
        }
    }

    /// Coalesce bursts of window events into one bar refresh.
    fn schedule_bar_refresh(&self) {
        if !self.bars.is_empty() {
            unsafe {
                SetTimer(self.main, BAR_TIMER, 60, None);
            }
        }
    }

    fn unregister_hotkeys(&self) {
        unsafe {
            for i in 0..9 {
                let _ = UnregisterHotKey(self.main, HK_FOCUS + i);
                let _ = UnregisterHotKey(self.main, HK_MOVE + i);
            }
            let _ = UnregisterHotKey(self.main, HK_PIN);
            let _ = UnregisterHotKey(self.main, HK_OVERVIEW);
            let _ = UnregisterHotKey(self.main, HK_NEXT);
            let _ = UnregisterHotKey(self.main, HK_PREV);
        }
    }

    /// Register shortcuts from the config; returns the ones another app already owns.
    fn register_hotkeys(&self) -> Vec<String> {
        let c = &self.config;
        let mut failed = Vec::new();
        unsafe {
            for i in 0..workspaces() as u32 {
                let vk = 0x31 + i; // '1'..
                if RegisterHotKey(self.main, HK_FOCUS + i as i32, c.switch | MOD_NOREPEAT, vk).is_err() {
                    failed.push(format!("{}+{}", c.switch_text, i + 1));
                }
                if RegisterHotKey(self.main, HK_MOVE + i as i32, c.move_ | MOD_NOREPEAT, vk).is_err() {
                    failed.push(format!("{}+{}", c.move_text, i + 1));
                }
            }
            if RegisterHotKey(self.main, HK_PIN, c.pin.mods | MOD_NOREPEAT, c.pin.vk).is_err() {
                failed.push(format!("{} (pin)", c.pin.text));
            }
            if RegisterHotKey(self.main, HK_OVERVIEW, c.overview.mods | MOD_NOREPEAT, c.overview.vk).is_err() {
                failed.push(format!("{} (overview)", c.overview.text));
            }
            if RegisterHotKey(self.main, HK_NEXT, c.next.mods | MOD_NOREPEAT, c.next.vk).is_err() {
                failed.push(format!("{} (next)", c.next.text));
            }
            if RegisterHotKey(self.main, HK_PREV, c.prev.mods | MOD_NOREPEAT, c.prev.vk).is_err() {
                failed.push(format!("{} (previous)", c.prev.text));
            }
        }
        failed
    }

    /// Re-read config.ini and apply it. Returns problems to report.
    fn reload_config(&mut self) -> Vec<String> {
        let new = config::load();
        self.apply_config(new)
    }

    /// Apply a config (from file or the Settings window). Returns problems to report.
    fn apply_config(&mut self, new: config::Config) -> Vec<String> {
        let old_edge = self.config.bar_edge;
        let mut problems: Vec<String> = new.errors.iter().map(|e| format!("Not understood, ignored: {e}")).collect();
        self.unregister_hotkeys();
        if new.workspaces != workspaces() {
            // Changing the count: show everything, then rebuild and re-apply the layout.
            let saved = layout::load();
            self.layout_frozen = true;
            self.reveal_all();
            WS_COUNT.store(new.workspaces, std::sync::atomic::Ordering::Relaxed);
            self.rebuild_monitors();
            self.layout_frozen = false;
            self.config = new;
            self.apply_layout(saved);
        } else {
            self.config = new;
        }
        // Bar moved to the other edge: rebuild it there.
        if !self.bars.is_empty() && self.config.bar_edge != old_edge {
            self.taskbar_restore();
            self.bars_destroy();
        }
        if self.config.bar && self.bars.is_empty() {
            self.bars_create();
        } else if !self.config.bar && !self.bars.is_empty() {
            self.taskbar_restore();
            self.bars_destroy();
        }
        self.taskbar_apply();
        self.tray_apply();
        let failed = self.register_hotkeys();
        if !failed.is_empty() {
            problems.push(format!("These shortcuts are used by another app and won't work: {}", failed.join(", ")));
        }
        problems
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
        let slide = if self.config.animation > 0 && self.overview.is_none() {
            let dir = if target > self.mons[m].active { 1 } else { -1 };
            let inn: Vec<HWND> = self.mons[m].hidden[target].iter().map(|k| key_hwnd(*k)).collect();
            let name = self.mons[m].name.clone();
            let still: Vec<HWND> = win::visible_windows()
                .into_iter()
                .filter(|h| self.pinned.contains(&hwnd_key(*h)) || self.is_pinned_app(*h))
                .filter(|h| win::monitor_name_of_window(*h).as_deref() == Some(name.as_str()))
                .collect();
            anim::Slide::begin(self.monitors[m].work, dir, self.config.animation, &leaving, &inn, &still)
        } else {
            None
        };
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
        if let Some(s) = slide {
            s.run();
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
        self.save();
        let m = self.current_mon();
        self.show_osd(m, Some(if pinned { "Pinned: shown on every workspace".into() } else { "Unpinned".into() }));
    }

    /// Move window `k` from (monitor, workspace) to another, possibly on another monitor.
    fn move_window(&mut self, k: isize, from: (usize, usize), to: (usize, usize)) {
        if from == to || from.0 >= self.mons.len() || to.0 >= self.mons.len() {
            return;
        }
        let h = key_hwnd(k);
        if unsafe { !IsWindow(h).as_bool() } {
            return;
        }
        self.pinned.remove(&k);
        let was_visible = from.1 == self.mons[from.0].active;
        if !was_visible {
            self.mons[from.0].hidden[from.1].retain(|x| *x != k);
        }
        if from.0 != to.0 {
            self.reposition(h, from.0, to.0, was_visible);
        }
        let target_visible = to.1 == self.mons[to.0].active;
        match (was_visible, target_visible) {
            (true, false) => {
                self.hide(h);
                self.mons[to.0].hidden[to.1].insert(0, k);
            }
            (false, true) => {
                self.show(k);
            }
            (false, false) => self.mons[to.0].hidden[to.1].insert(0, k),
            (true, true) => {}
        }
        self.save();
        self.update_tray();
    }

    /// Place a window on another monitor at the same relative spot, keeping its size
    /// (clamped) and maximized/minimized state.
    fn reposition(&mut self, h: HWND, from: usize, to: usize, visible: bool) {
        // Windows hidden with SW_HIDE would reappear if we changed their placement.
        if !visible && self.methods.get(&hwnd_key(h)) == Some(&Method::Hide) {
            return;
        }
        let a = self.monitors[from].work;
        let b = self.monitors[to].work;
        // rcNormalPosition is in "workspace" coordinates: screen minus the primary work-area offset.
        let off = self
            .monitors
            .iter()
            .find(|m| m.rect.left == 0 && m.rect.top == 0)
            .map(|m| (m.work.left - m.rect.left, m.work.top - m.rect.top))
            .unwrap_or((0, 0));
        unsafe {
            let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            if GetWindowPlacement(h, &mut wp).is_err() {
                return;
            }
            let r = wp.rcNormalPosition;
            let (sl, st) = (r.left + off.0, r.top + off.1);
            let w = (r.right - r.left).min(b.right - b.left);
            let ht = (r.bottom - r.top).min(b.bottom - b.top);
            let nl = b.left + (sl - a.left).clamp(0, (b.right - b.left - w).max(0));
            let nt = b.top + (st - a.top).clamp(0, (b.bottom - b.top - ht).max(0));
            wp.rcNormalPosition = RECT { left: nl - off.0, top: nt - off.1, right: nl - off.0 + w, bottom: nt - off.1 + ht };
            wp.showCmd = match SHOW_WINDOW_CMD(wp.showCmd as i32) {
                SW_SHOWMAXIMIZED => SW_SHOWMAXIMIZED.0 as u32,
                SW_SHOWMINIMIZED => SW_SHOWMINNOACTIVE.0 as u32,
                _ => SW_SHOWNOACTIVATE.0 as u32,
            };
            let _ = SetWindowPlacement(h, &wp);
        }
    }

    /// Activation of a hidden window (taskbar click, app raising itself): go to its workspace.
    fn on_foreground(&mut self, h: HWND) {
        self.taskbar_enforce();
        self.focus_mouse_mon = win::monitor_name_at_cursor();
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

    fn save_layout(&self) {
        if self.layout_frozen {
            return;
        }
        let mut l = layout::Layout::default();
        let entry = |mon: &str, ws: Option<usize>, k: isize| {
            let h = key_hwnd(k);
            layout::Entry {
                mon: mon.to_string(),
                ws,
                hwnd: k,
                exe: overview::app_name(h),
                class: win::class_name(h),
                title: win::title(h),
            }
        };
        for ms in &self.mons {
            l.active.push((ms.name.clone(), ms.active));
            for (w, list) in ms.hidden.iter().enumerate() {
                for k in list {
                    l.entries.push(entry(&ms.name, Some(w), *k));
                }
            }
        }
        for k in &self.pinned {
            let h = key_hwnd(*k);
            if unsafe { IsWindow(h).as_bool() } {
                if let Some(mon) = win::monitor_name_of_window(h) {
                    l.entries.push(entry(&mon, None, *k));
                }
            }
        }
        layout::save(&l);
    }

    /// Put windows back on the workspaces recorded in `l` (all windows are visible now).
    fn apply_layout(&mut self, l: layout::Layout) {
        for (name, a) in &l.active {
            if let Some(m) = self.mon_index(name) {
                if *a < workspaces() {
                    self.mons[m].active = *a;
                }
            }
        }
        let candidates = win::visible_windows();
        let mut used: HashSet<isize> = HashSet::new();
        for e in l.entries {
            let Some(m) = self.mon_index(&e.mon) else { continue };
            let same_app = |h: HWND| overview::app_name(h) == e.exe && win::class_name(h) == e.class;
            let by_hwnd = Some(key_hwnd(e.hwnd)).filter(|h| e.hwnd != 0 && win::is_manageable(*h, false) && same_app(*h));
            let found = by_hwnd.or_else(|| {
                candidates.iter().copied().find(|h| !used.contains(&hwnd_key(*h)) && same_app(*h) && win::title(*h) == e.title)
            });
            let Some(h) = found else { continue };
            let k = hwnd_key(h);
            if !used.insert(k) {
                continue;
            }
            if let Some(cur) = win::monitor_name_of_window(h).and_then(|n| self.mon_index(&n)) {
                if cur != m {
                    self.reposition(h, cur, m, true);
                }
            }
            match e.ws {
                None => {
                    self.pinned.insert(k);
                }
                Some(w) if w < workspaces() && w != self.mons[m].active => {
                    self.hide(h);
                    self.mons[m].hidden[w].push(k);
                }
                _ => {}
            }
        }
        self.save();
        self.update_tray();
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
        self.save_layout();
    }

    fn counts(&self, m: usize) -> Vec<usize> {
        (0..workspaces())
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
            let l = ui::osd_layout(workspaces(), self.osd_scale);
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
        (icon, format!("LastDesk — {tip}"))
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
    Overview,
    RevealAll,
    Settings,
    EditConfig,
    ReloadConfig,
    Autostart(bool),
    Exit,
}

fn message(text: &str, warn: bool) {
    let w = wide(text);
    unsafe {
        MessageBoxW(None, PCWSTR(w.as_ptr()), w!("LastDesk"), MB_OK | if warn { MB_ICONWARNING } else { MB_ICONINFORMATION });
    }
}

pub(crate) fn show_menu(hwnd: HWND) {
    // Build the model while borrowed, then run the (message-pumping) menu without a borrow.
    let Some(entries) = with_app(|app| {
        app.prune();
        let labels = monitor_labels(app.mons.len());
        let mut e: Vec<(String, Option<MenuAction>, bool)> = Vec::new(); // label, action, checked
        for m in 0..app.mons.len() {
            e.push((labels[m].to_string(), None, false));
            let visible = app.visible_on(m);
            for w in 0..workspaces() {
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
        e.push(("Overview  (Alt+`)".into(), Some(MenuAction::Overview), false));
        e.push(("Show all windows (reset)".into(), Some(MenuAction::RevealAll), false));
        e.push(("Settings…".into(), Some(MenuAction::Settings), false));
        e.push(("Edit config file…".into(), Some(MenuAction::EditConfig), false));
        e.push(("Reload config".into(), Some(MenuAction::ReloadConfig), false));
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
            Some(MenuAction::Overview) => {
                with_app(|app| app.overview_open());
            }
            Some(MenuAction::RevealAll) => {
                with_app(|app| app.reveal_all());
            }
            Some(MenuAction::Settings) => settings::open(),
            Some(MenuAction::EditConfig) => {
                let p = wide(&config::path().display().to_string());
                ShellExecuteW(None, w!("open"), w!("notepad.exe"), PCWSTR(p.as_ptr()), None, SW_SHOWNORMAL);
            }
            Some(MenuAction::ReloadConfig) => {
                // Report outside the borrow: MessageBox pumps messages.
                if let Some(problems) = with_app(|app| app.reload_config()) {
                    if problems.is_empty() {
                        with_app(|app| {
                            let m = app.current_mon();
                            app.show_osd(m, Some("Config reloaded".into()));
                        });
                    } else {
                        message(&problems.join("\n"), true);
                    }
                }
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
                if id == HK_OVERVIEW {
                    app.overview_toggle();
                    return;
                }
                app.overview_close();
                if (HK_FOCUS..HK_FOCUS + workspaces() as i32).contains(&id) {
                    let m = app.current_mon();
                    app.switch_to(m, (id - HK_FOCUS) as usize, None);
                } else if (HK_MOVE..HK_MOVE + workspaces() as i32).contains(&id) {
                    app.move_focused((id - HK_MOVE) as usize);
                } else if id == HK_PIN {
                    app.toggle_pin();
                } else if id == HK_NEXT || id == HK_PREV {
                    // Step one workspace, stopping at the ends (like Windows' desktops).
                    let m = app.current_mon();
                    let a = app.mons[m].active;
                    let t = if id == HK_NEXT { (a + 1).min(workspaces() - 1) } else { a.saturating_sub(1) };
                    app.switch_to(m, t, None);
                }
            });
            LRESULT(0)
        }
        WM_OPEN_SETTINGS => {
            settings::open();
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
            // Monitors changed: show everything, then re-apply the remembered layout to
            // whatever monitors exist now (a reconnected monitor gets its workspaces back).
            with_app(|app| {
                app.overview_close();
                app.bars_destroy();
                let saved = layout::load();
                app.layout_frozen = true;
                app.reveal_all();
                app.rebuild_monitors();
                app.layout_frozen = false;
                app.apply_layout(saved);
                app.bars_create();
                app.taskbar_enforce();
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == BAR_TIMER => {
            let _ = KillTimer(hwnd, BAR_TIMER);
            with_app(|app| {
                app.bars_refresh();
                app.taskbar_enforce();
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TRAY_TIMER => {
            tray::maintain();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == PLACE_TIMER => {
            let _ = KillTimer(hwnd, PLACE_TIMER);
            with_app(|app| {
                app.bars_place_all();
                app.taskbar_enforce();
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CLOCK_TIMER => {
            with_app(|app| {
                app.taskbar_enforce();
                for b in &app.bars {
                    let _ = InvalidateRect(b.hwnd, None, false);
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == RULE_TIMER => {
            let _ = KillTimer(hwnd, RULE_TIMER);
            with_app(|app| app.run_pending_rules());
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            // Logging off / shutting down: keep the layout for next time.
            with_app(|app| {
                app.taskbar_restore();
                app.layout_frozen = true;
                app.reveal_all();
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            tray::destroy();
            with_app(|app| {
                app.taskbar_restore();
                app.bars_destroy();
                app.layout_frozen = true;
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
                with_app(|app| {
                    app.add_tray();
                    // Explorer restarted: its taskbar is back, hide it again.
                    if app.taskbar_hidden {
                        taskbar::hide_all();
                        app.bars_place_all();
                    }
                });
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

unsafe extern "system" fn win_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, id_child: i32, _: u32, _: u32) {
    if id_object != OBJID_WINDOW.0 || hwnd.is_invalid() {
        return;
    }
    if event == EVENT_SYSTEM_FOREGROUND {
        let root = GetAncestor(hwnd, GA_ROOT);
        with_app(|app| app.on_foreground(if root.is_invalid() { hwnd } else { root }));
        with_app(|app| app.schedule_bar_refresh());
        return;
    }
    if id_child != CHILDID_SELF as i32 {
        return;
    }
    if event == EVENT_OBJECT_SHOW {
        with_app(|app| app.on_show(hwnd));
    }
    with_app(|app| app.schedule_bar_refresh());
}

fn trace(s: &str) {
    if std::env::var_os("LASTDESK_TRACE").is_some() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("lastdesk-trace.txt")) {
            let _ = writeln!(f, "{s}");
        }
    }
}

/// Dark popup menus: uxtheme's SetPreferredAppMode (ordinal 135, ForceDark = 2) and
/// FlushMenuThemes (136). Undocumented but stable since Windows 10 1903; skipped if missing.
fn enable_dark_menus() {
    use windows::core::PCSTR;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(ux, PCSTR(135 as *const u8)) {
            let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(2);
        }
        if let Some(f) = GetProcAddress(ux, PCSTR(136 as *const u8)) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}

fn main() {
    enable_dark_menus();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _mutex = CreateMutexW(None, true, w!("Local\\lastdesk-single-instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if std::env::args().any(|a| a == "--settings") {
                if let Ok(h) = FindWindowW(w!("LastDeskMain"), None) {
                    let _ = PostMessageW(h, WM_OPEN_SETTINGS, WPARAM(0), LPARAM(0));
                }
                return;
            }
            MessageBoxW(None, w!("LastDesk is already running (see the tray)."), w!("LastDesk"), MB_OK | MB_ICONINFORMATION);
            return;
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        trace("com"); let cloaker = Cloaker::new(); trace("cloaker");
        recover_from_file(&cloaker);
        taskbar::recover();
        trace("recovered");

        // On panic: restore every hidden window before the process dies.
        std::panic::set_hook(Box::new(|info| {
            let log = state_path().with_file_name("crash.log");
            if let Some(dir) = log.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&log, format!("{info}\n{}", std::backtrace::Backtrace::force_capture()));
            tray::destroy();
            taskbar::restore_if_hidden();
            let c = Cloaker::new();
            for (k, m) in PANIC_LIST.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                c.show(key_hwnd(*k), *m);
            }
            let _ = std::fs::remove_file(state_path());
        }));

        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let main_class = WNDCLASSW { lpfnWndProc: Some(main_proc), hInstance: hinst, lpszClassName: w!("LastDeskMain"), ..Default::default() };
        RegisterClassW(&main_class);
        let osd_class = WNDCLASSW {
            lpfnWndProc: Some(osd_proc),
            hInstance: hinst,
            lpszClassName: w!("LastDeskOSD"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&osd_class);
        let overview_class = WNDCLASSW {
            lpfnWndProc: Some(overview::overview_proc),
            hInstance: hinst,
            lpszClassName: overview::CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&overview_class);
        let bar_class = WNDCLASSW {
            lpfnWndProc: Some(bar::bar_proc),
            hInstance: hinst,
            lpszClassName: bar::CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            style: CS_DBLCLKS,
            ..Default::default()
        };
        RegisterClassW(&bar_class);
        let settings_class = WNDCLASSW {
            lpfnWndProc: Some(settings::settings_proc),
            hInstance: hinst,
            lpszClassName: settings::CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&settings_class);
        let ctl_class = WNDCLASSW {
            lpfnWndProc: Some(settings::ctl_proc),
            hInstance: hinst,
            lpszClassName: settings::CTL_CLASS,
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&ctl_class);
        let tip_class = WNDCLASSW { lpfnWndProc: Some(bar::tip_proc), hInstance: hinst, lpszClassName: bar::TIP_CLASS, ..Default::default() };
        RegisterClassW(&tip_class);
        trace("classes");

        let main = CreateWindowExW(WINDOW_EX_STYLE(0), w!("LastDeskMain"), w!("LastDesk"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, hinst, None).unwrap();
        let osd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            w!("LastDeskOSD"),
            w!("LastDesk osd"),
            WS_POPUP,
            0, 0, 10, 10,
            None, None, hinst, None,
        )
        .unwrap();
        trace("windows"); let _ = SetLayeredWindowAttributes(osd, COLORREF(0), 240, LWA_ALPHA);

        let cfg = config::load();
        WS_COUNT.store(cfg.workspaces, std::sync::atomic::Ordering::Relaxed);
        let mut app = App {
            main,
            osd,
            cloaker,
            monitors: Vec::new(),
            mons: Vec::new(),
            methods: HashMap::new(),
            pinned: HashSet::new(),
            osd_model: ui::OsdModel { label: String::new(), active: 0, counts: vec![0; workspaces()] },
            osd_scale: 1.0,
            tray_icon: HICON::default(),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            overview: None,
            focus_mouse_mon: None,
            layout_frozen: false,
            config: cfg,
            exe_cache: RefCell::new(HashMap::new()),
            seen: HashSet::new(),
            pending: Vec::new(),
            launch: None,
            bars: Vec::new(),
            taskbar_hidden: false,
        };
        trace("app"); app.rebuild_monitors(); trace("monitors");
        app.add_tray(); trace("tray");
        app.apply_layout(layout::load());
        trace("layout");
        app.bars_create();
        app.taskbar_apply();
        app.tray_apply();
        // Rules only apply to windows opened from now on.
        app.seen = win::visible_windows().into_iter().map(hwnd_key).collect();
        app.seen.extend(app.mons.iter().flat_map(|m| m.hidden.iter().flatten().copied()));
        let mut problems: Vec<String> = app.config.errors.iter().map(|e| format!("Config not understood, ignored: {e}")).collect();
        let failed = app.register_hotkeys();
        if !failed.is_empty() {
            problems.push(format!("These shortcuts are used by another app and won't work:\n{}", failed.join(", ")));
        }
        APP.with(|a| *a.borrow_mut() = Some(app));
        if !problems.is_empty() {
            message(&problems.join("\n"), true);
        }

        trace("hotkeys");
        let _hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        // Window appear/disappear/rename/cloak/minimize/move: keep bars current, apply rules.
        let _hook_obj = SetWinEventHook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        let _hook_name = SetWinEventHook(EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        let _hook_cloak = SetWinEventHook(EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        let _hook_min = SetWinEventHook(EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        let _hook_move = SetWinEventHook(EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT);

        trace("loop"); let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if settings::is_dialog_message(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // Windows were restored and the state file removed in WM_DESTROY. Skip the normal
        // runtime teardown: releasing the shell COM object during process exit can hang.
        let _ = windows::Win32::System::Threading::TerminateProcess(windows::Win32::System::Threading::GetCurrentProcess(), 0);
    }
}
