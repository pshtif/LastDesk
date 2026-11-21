//! ASUS laptops: fan speed, read through the ASUS ACPI device (\\.\ATKACPI, the interface
//! MyASUS uses; needs no admin rights). On other laptops the device doesn't exist and
//! nothing shows up.
//!
//! (Setting the battery charge limit through the same device was tried: on the Zenbook A14
//! the firmware ignores it, so it isn't offered. MyASUS sets it some other way there.)

use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows::Win32::System::IO::DeviceIoControl;

const IOCTL: u32 = 0x0022240C;
const DSTS: u32 = 0x5354_5344; // "DSTS": read a device's status
const DEV_FAN: u32 = 0x0011_0013;
const PRESENT: u32 = 0x0001_0000;

fn status(id: u32) -> Option<u32> {
    unsafe {
        let h = CreateFileW(w!(r"\\.\ATKACPI"), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None).ok()?;
        let mut inp = [0u8; 16];
        inp[0..4].copy_from_slice(&DSTS.to_le_bytes());
        inp[4..8].copy_from_slice(&8u32.to_le_bytes());
        inp[8..12].copy_from_slice(&id.to_le_bytes());
        let mut out = [0u8; 16];
        let mut ret = 0u32;
        let ok = DeviceIoControl(h, IOCTL, Some(inp.as_ptr() as _), 16, Some(out.as_mut_ptr() as _), 16, Some(&mut ret), None).is_ok();
        let _ = CloseHandle(h);
        let s = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
        (ok && s != 0xFFFF_FFFE && s & PRESENT != 0).then_some(s)
    }
}

/// Fan speed in rpm (0 = stopped), if the laptop reports it.
pub fn fan_rpm() -> Option<u32> {
    static S: OnceLock<bool> = OnceLock::new();
    if !*S.get_or_init(|| status(DEV_FAN).is_some()) {
        return None;
    }
    status(DEV_FAN).map(|s| (s & 0xFFFF) * 100)
}
