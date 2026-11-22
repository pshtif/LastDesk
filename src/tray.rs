// Created by Peter @sHTiF Stefcek
//! System tray host: apps' notification-area icons shown in the LastDesk bar.
//!
//! Shell_NotifyIcon finds the tray with FindWindow("Shell_TrayWnd") and sends it WM_COPYDATA.
//! We register our own (hidden, topmost) window of that class so apps find ours first, keep
//! the icons, and forward every message to Explorer's real tray too. Explorer therefore keeps
//! a complete copy (and handles balloons/toasts): if LastDesk goes away nothing is lost.
//! Messages that aren't icon updates (app bar requests etc.) are passed to Explorer unchanged.

use std::cell::RefCell;

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::*;

const SIGNATURE: u32 = 0x3475_3423;
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_STATE: u32 = 0x08;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x01;
const NIN_SELECT: u32 = WM_USER;

pub struct Icon {
    pub hwnd: u32,
    pub uid: u32,
    guid: Option<[u8; 16]>,
    callback: u32,
    /// Our own copy (apps often destroy theirs right after the call).
    pub icon: HICON,
    pub tip: String,
    version: u32,
    hidden: bool,
}

impl Drop for Icon {
    fn drop(&mut self) {
        if !self.icon.is_invalid() {
            unsafe {
                let _ = DestroyIcon(self.icon);
            }
        }
    }
}

struct State {
    hwnd: HWND,
    main: HWND,
    icons: Vec<Icon>,
    taskbar_created: u32,
}

thread_local! {
    static TRAY: RefCell<Option<State>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy)]
pub enum Click {
    Left,
    Right,
    Middle,
    Double,
    Hover,
}

/// Fields of the 32-bit-compatible NOTIFYICONDATAW inside the tray message.
struct Nid<'a>(&'a [u8]);

impl Nid<'_> {
    fn u32(&self, off: usize) -> u32 {
        self.0.get(off..off + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0)
    }
    fn wstr(&self, off: usize, chars: usize) -> String {
        let Some(b) = self.0.get(off..(off + chars * 2).min(self.0.len())) else { return String::new() };
        let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0).collect();
        String::from_utf16_lossy(&w)
    }
    fn guid(&self) -> Option<[u8; 16]> {
        let b = self.0.get(936..952)?;
        let mut g = [0u8; 16];
        g.copy_from_slice(b);
        (g != [0u8; 16]).then_some(g)
    }
    fn hwnd(&self) -> u32 { self.u32(4) }
    fn uid(&self) -> u32 { self.u32(8) }
    fn flags(&self) -> u32 { self.u32(12) }
    fn callback(&self) -> u32 { self.u32(16) }
    fn hicon(&self) -> u32 { self.u32(20) }
    fn tip(&self) -> String { self.wstr(24, 128) }
    fn state(&self) -> u32 { self.u32(280) }
    fn state_mask(&self) -> u32 { self.u32(284) }
    fn version(&self) -> u32 { self.u32(800) }
}

fn find(icons: &[Icon], nid: &Nid) -> Option<usize> {
    let guid = if nid.flags() & NIF_GUID != 0 { nid.guid() } else { None };
    icons.iter().position(|i| match (guid, i.guid) {
        (Some(g), Some(ig)) => g == ig,
        _ => i.hwnd == nid.hwnd() && i.uid == nid.uid(),
    })
}

fn apply(icon: &mut Icon, nid: &Nid) {
    let f = nid.flags();
    if f & NIF_MESSAGE != 0 {
        icon.callback = nid.callback();
    }
    if f & NIF_ICON != 0 {
        let src = HICON(nid.hicon() as usize as *mut _);
        let copy = if src.is_invalid() { HICON::default() } else { unsafe { CopyIcon(src).unwrap_or_default() } };
        if !icon.icon.is_invalid() {
            unsafe {
                let _ = DestroyIcon(icon.icon);
            }
        }
        icon.icon = copy;
    }
    if f & NIF_TIP != 0 {
        icon.tip = nid.tip();
    }
    if f & NIF_STATE != 0 && nid.state_mask() & NIS_HIDDEN != 0 {
        icon.hidden = nid.state() & NIS_HIDDEN != 0;
    }
    if f & NIF_GUID != 0 {
        icon.guid = nid.guid();
    }
}

/// Handle one Shell_NotifyIcon call. Returns TRUE/FALSE like Explorer does.
fn handle_notify(state: &mut State, msg: u32, nid: &Nid) -> bool {
    match msg {
        NIM_ADD => {
            if find(&state.icons, nid).is_some() {
                return false;
            }
            let mut icon = Icon {
                hwnd: nid.hwnd(),
                uid: nid.uid(),
                guid: None,
                callback: 0,
                icon: HICON::default(),
                tip: String::new(),
                version: 0,
                hidden: false,
            };
            apply(&mut icon, nid);
            state.icons.push(icon);
            true
        }
        NIM_MODIFY => match find(&state.icons, nid) {
            Some(i) => {
                apply(&mut state.icons[i], nid);
                true
            }
            None => false,
        },
        NIM_DELETE => match find(&state.icons, nid) {
            Some(i) => {
                state.icons.remove(i);
                true
            }
            None => false,
        },
        NIM_SETVERSION => match find(&state.icons, nid) {
            Some(i) => {
                state.icons[i].version = nid.version();
                true
            }
            None => false,
        },
        _ => true,
    }
}

/// Explorer's real tray window (not ours).
fn explorer_tray() -> Option<HWND> {
    unsafe extern "system" fn cb(h: HWND, data: LPARAM) -> BOOL {
        let out = &mut *(data.0 as *mut Option<HWND>);
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid != GetCurrentProcessId() && crate::win::class_name(h) == "Shell_TrayWnd" {
            *out = Some(h);
            return FALSE;
        }
        TRUE
    }
    let mut out = None;
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

fn forward(msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(ex) = explorer_tray() else { return LRESULT(0) };
    unsafe {
        if InSendMessage().as_bool() || msg == WM_COPYDATA {
            let mut res: usize = 0;
            let _ = SendMessageTimeoutW(ex, msg, wp, lp, SMTO_ABORTIFHUNG, 3000, Some(&mut res));
            LRESULT(res as isize)
        } else {
            let _ = PostMessageW(ex, msg, wp, lp);
            LRESULT(0)
        }
    }
}

fn changed(main: HWND) {
    unsafe {
        SetTimer(main, crate::BAR_TIMER, 60, None);
    }
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            let cds = &*(lp.0 as *const COPYDATASTRUCT);
            if cds.dwData == 1 && !cds.lpData.is_null() && cds.cbData >= 8 {
                let data = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
                let sig = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let nim = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                // Keep Explorer's copy complete, then answer from ours.
                let _ = forward(msg, wp, lp);
                if sig == SIGNATURE {
                    let nid = Nid(&data[8..]);
                    let result = TRAY.with(|t| {
                        t.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(|s| {
                            let r = handle_notify(s, nim, &nid);
                            changed(s.main);
                            r
                        }))
                    });
                    return LRESULT(result.unwrap_or(true) as isize);
                }
                return LRESULT(1);
            }
            forward(msg, wp, lp)
        }
        // Someone else asking our tray window to close (Explorer may, when it starts up and
        // finds a Shell_TrayWnd): refuse. LastDesk removes it itself with DestroyWindow.
        WM_CLOSE => {
            crate::trace("tray window: refused a WM_CLOSE from outside");
            LRESULT(0)
        }
        // Commands and private shell messages meant for the taskbar go to Explorer.
        WM_COMMAND => forward(msg, wp, lp),
        m if (WM_USER..0xC000).contains(&m) => forward(msg, wp, lp),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Become the tray apps talk to. Apps are asked to re-register their icons.
pub fn create(main: HWND) {
    if TRAY.with(|t| t.borrow().is_some()) {
        return;
    }
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let class = WNDCLASSW { lpfnWndProc: Some(tray_proc), hInstance: hinst, lpszClassName: w!("Shell_TrayWnd"), ..Default::default() };
        RegisterClassW(&class);
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("Shell_TrayWnd"), w!(""), WS_POPUP, 0, 0, 0, 0, None, None, hinst, None) else {
            return;
        };
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        TRAY.with(|t| *t.borrow_mut() = Some(State { hwnd, main, icons: Vec::new(), taskbar_created }));
        raise(hwnd);
        let _ = PostMessageW(HWND_BROADCAST, taskbar_created, WPARAM(0), LPARAM(0));
    }
}

pub fn destroy() {
    let st = TRAY.with(|t| t.borrow_mut().take());
    if let Some(s) = st {
        unsafe {
            let _ = DestroyWindow(s.hwnd);
        }
        // Icons stay valid in Explorer (everything was forwarded).
    }
}

/// Stay the first Shell_TrayWnd that FindWindow returns, mirroring Explorer's taskbar rect
/// (some apps read the taskbar position from that window).
fn raise(hwnd: HWND) {
    unsafe {
        let mut r = RECT::default();
        if let Some(ex) = explorer_tray() {
            let _ = GetWindowRect(ex, &mut r);
        }
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE);
    }
}

/// Periodic check: drop icons of dead apps; if Explorer's tray got ahead of ours (Explorer
/// restarted, z-order change) take the lead back and ask apps to re-send their icons.
pub fn maintain() {
    let info = TRAY.with(|t| {
        t.try_borrow_mut().ok().and_then(|mut s| {
            s.as_mut().map(|s| {
                let before = s.icons.len();
                s.icons.retain(|i| unsafe { IsWindow(HWND(i.hwnd as usize as *mut _)).as_bool() });
                if s.icons.len() != before {
                    changed(s.main);
                }
                crate::trace(&format!("tray icons: {} ({} visible)", s.icons.len(), s.icons.iter().filter(|i| !i.hidden && !i.icon.is_invalid()).count()));
                (s.hwnd, s.taskbar_created)
            })
        })
    });
    let Some((hwnd, tc)) = info else { return };
    unsafe {
        let first = FindWindowW(w!("Shell_TrayWnd"), None).unwrap_or_default();
        if first != hwnd {
            raise(hwnd);
            let _ = PostMessageW(HWND_BROADCAST, tc, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn active() -> bool {
    TRAY.with(|t| t.borrow().is_some())
}

/// Visible icons: (owner hwnd, uid, icon, tip).
pub fn icons() -> Vec<(u32, u32, HICON, String)> {
    TRAY.with(|t| {
        t.borrow()
            .as_ref()
            .map(|s| s.icons.iter().filter(|i| !i.hidden && !i.icon.is_invalid()).map(|i| (i.hwnd, i.uid, i.icon, i.tip.clone())).collect())
            .unwrap_or_default()
    })
}

pub fn tip(hwnd: u32, uid: u32) -> String {
    TRAY.with(|t| {
        t.borrow().as_ref().and_then(|s| s.icons.iter().find(|i| i.hwnd == hwnd && i.uid == uid).map(|i| i.tip.clone())).unwrap_or_default()
    })
}

/// Deliver a mouse action to the app the way Explorer does.
pub fn click(hwnd: u32, uid: u32, kind: Click, at: POINT) {
    let Some((callback, version)) = TRAY.with(|t| {
        t.borrow().as_ref().and_then(|s| s.icons.iter().find(|i| i.hwnd == hwnd && i.uid == uid).map(|i| (i.callback, i.version)))
    }) else { return };
    if callback == 0 {
        return;
    }
    let owner = HWND(hwnd as usize as *mut _);
    let send = |m: u32| {
        let (wp, lp) = if version >= 4 {
            (((at.x as u16 as usize) | ((at.y as u16 as usize) << 16)), ((m & 0xFFFF) as isize) | ((uid as isize & 0xFFFF) << 16))
        } else {
            (uid as usize, m as isize)
        };
        unsafe {
            let _ = PostMessageW(owner, callback, WPARAM(wp), LPARAM(lp));
        }
    };
    unsafe {
        // Let the app bring its menu/window to the front.
        let mut pid = 0u32;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
    }
    match kind {
        Click::Left => {
            send(WM_LBUTTONDOWN);
            send(WM_LBUTTONUP);
            if version >= 3 {
                send(NIN_SELECT);
            }
        }
        Click::Right => {
            send(WM_RBUTTONDOWN);
            send(WM_RBUTTONUP);
            if version >= 3 {
                send(WM_CONTEXTMENU);
            }
        }
        Click::Middle => {
            send(WM_MBUTTONDOWN);
            send(WM_MBUTTONUP);
        }
        Click::Double => {
            send(WM_LBUTTONDBLCLK);
            send(WM_LBUTTONUP);
        }
        Click::Hover => send(WM_MOUSEMOVE),
    }
}
