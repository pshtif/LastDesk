// Created by Peter @sHTiF Stefcek
//! Win key opens Start on the monitor you're on. Windows opens Start where its host window
//! last was, and with the taskbars hidden that's always the main monitor. So when the Win key
//! goes down, ask the main thread to move the (still closed) Start window to the current
//! monitor; Start opens on key-up, by which time it's there.
//!
//! Optional (Settings > Integrations), off by default. The hook runs on its own thread so
//! typing never waits on LastDesk, and it only looks at whether the key is a Win key:
//! nothing else is read, kept or passed on. Turning the option off removes the hook.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LWIN, VK_RWIN};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_APP,
    WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// Posted to the main window when a Win key goes down.
pub const WM_WINKEY: u32 = WM_APP + 8;

static MAIN: AtomicIsize = AtomicIsize::new(0);
static DOWN: AtomicBool = AtomicBool::new(false);
/// Thread running the hook (0 = off).
static THREAD: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let vk = (*(lp.0 as *const KBDLLHOOKSTRUCT)).vkCode;
        if vk == VK_LWIN.0 as u32 || vk == VK_RWIN.0 as u32 {
            match wp.0 as u32 {
                // Held keys repeat key-down: only the first one counts.
                WM_KEYDOWN | WM_SYSKEYDOWN if !DOWN.swap(true, Ordering::Relaxed) => {
                    let _ = PostMessageW(HWND(MAIN.load(Ordering::Relaxed) as *mut _), WM_WINKEY, WPARAM(0), LPARAM(0));
                }
                WM_KEYUP | WM_SYSKEYUP => DOWN.store(false, Ordering::Relaxed),
                _ => {}
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Install or remove the hook.
pub fn set_enabled(on: bool, main: HWND) {
    MAIN.store(main.0 as isize, Ordering::Relaxed);
    let running = THREAD.load(Ordering::SeqCst);
    if on && running == 0 {
        std::thread::spawn(|| unsafe {
            let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0) else { return };
            THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
            let mut msg = MSG::default();
            // Until WM_QUIT (set_enabled(false)).
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
            let _ = UnhookWindowsHookEx(h);
            THREAD.store(0, Ordering::SeqCst);
        });
    } else if !on && running != 0 {
        unsafe {
            let _ = PostThreadMessageW(running, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}
