// Created by Peter @sHTiF Stefcek
//! Hiding windows without minimizing them.
//!
//! Uses the shell's (undocumented but long-stable) IApplicationView::SetCloak, the same
//! mechanism Windows virtual desktops, GlazeWM and komorebi use. A cloaked window keeps
//! its size, position and z-order and is skipped by Alt+Tab. If cloaking isn't
//! available for a window we fall back to ShowWindow(SW_HIDE).

use std::ffi::c_void;
use std::ptr::null_mut;

use windows::core::{GUID, HRESULT};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Com::{CoCreateInstance, IServiceProvider, CLSCTX_ALL};
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOWNA};
use windows::core::Interface;

const CLSID_IMMERSIVE_SHELL: GUID = GUID::from_u128(0xC2F03A33_21F5_47FA_B4BB_156362A2F239);
const IID_APP_VIEW_COLLECTION: GUID = GUID::from_u128(0x1841C6D7_4F9D_42C0_AF41_8747538F10E5);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Cloak,
    Hide,
}

pub struct Cloaker {
    // Kept alive for the process lifetime; owns the collection pointer below.
    _shell: Option<IServiceProvider>,
    collection: *mut c_void,
}

type QueryServiceFn =
    unsafe extern "system" fn(*mut c_void, *const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
type GetViewForHwndFn = unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> HRESULT;
type SetCloakFn = unsafe extern "system" fn(*mut c_void, i32, i32) -> HRESULT;
type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;

unsafe fn vfn(obj: *mut c_void, index: usize) -> *const c_void {
    let vtbl = *(obj as *const *const *const c_void);
    *vtbl.add(index)
}

impl Cloaker {
    /// Requires COM to be initialised on the calling thread.
    pub fn new() -> Self {
        unsafe {
            let shell: Option<IServiceProvider> =
                CoCreateInstance(&CLSID_IMMERSIVE_SHELL, None, CLSCTX_ALL).ok();
            let mut collection = null_mut();
            if let Some(sp) = &shell {
                let qs: QueryServiceFn = std::mem::transmute(vfn(sp.as_raw(), 3));
                let hr = qs(sp.as_raw(), &IID_APP_VIEW_COLLECTION, &IID_APP_VIEW_COLLECTION, &mut collection);
                if hr.is_err() {
                    collection = null_mut();
                }
            }
            Cloaker { _shell: shell, collection }
        }
    }

    /// SetCloak(type=1, flag): flag 2 cloaks, 0 uncloaks.
    unsafe fn set_cloak(&self, hwnd: HWND, cloak: bool) -> bool {
        if self.collection.is_null() {
            return false;
        }
        // IApplicationViewCollection::GetViewForHwnd is vtable slot 6.
        let get_view: GetViewForHwndFn = std::mem::transmute(vfn(self.collection, 6));
        let mut view = null_mut();
        if get_view(self.collection, hwnd, &mut view).is_err() || view.is_null() {
            return false;
        }
        // IApplicationView (IInspectable-based) ::SetCloak is vtable slot 12.
        let set: SetCloakFn = std::mem::transmute(vfn(view, 12));
        let ok = set(view, 1, if cloak { 2 } else { 0 }).is_ok();
        let release: ReleaseFn = std::mem::transmute(vfn(view, 2));
        release(view);
        ok
    }

    pub fn hide(&self, hwnd: HWND) -> Method {
        unsafe {
            if self.set_cloak(hwnd, true) {
                Method::Cloak
            } else {
                let _ = ShowWindow(hwnd, SW_HIDE);
                Method::Hide
            }
        }
    }

    pub fn show(&self, hwnd: HWND, method: Method) {
        unsafe {
            match method {
                Method::Cloak => {
                    if !self.set_cloak(hwnd, false) {
                        let _ = ShowWindow(hwnd, SW_SHOWNA);
                    }
                }
                Method::Hide => {
                    let _ = ShowWindow(hwnd, SW_SHOWNA);
                }
            }
        }
    }
}

pub fn is_cloaked(hwnd: HWND) -> bool {
    let mut v: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut v as *mut u32 as *mut c_void, 4).is_ok() && v != 0
    }
}
