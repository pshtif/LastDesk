// Created by Peter @sHTiF Stefcek
//! Volume and network state for the bar's status area (next to the battery).
//!
//! Volume changes arrive through a Core Audio callback and network changes through
//! NotifyIpInterfaceChange; both only post WM_STATUS to the main window, so nothing polls.
//! The default output device and the Wi-Fi signal are re-read on the bar's clock tick.
//! Microphone / camera use: see the end of the file.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::core::{implement, Result};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl};
use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator, AUDIO_VOLUME_NOTIFICATION_DATA};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, NotifyIpInterfaceChange, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::NetworkManagement::WiFi::{
    wlan_interface_state_connected, wlan_intf_opcode_current_connection, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle,
    WlanQueryInterface, WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
};
use windows::Win32::Networking::WinSock::AF_UNSPEC;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the main window when volume or network state may have changed.
pub const WM_STATUS: u32 = WM_APP + 7;
pub const STATUS_VOLUME: usize = 0;
pub const STATUS_NET: usize = 1;

static NOTIFY: AtomicIsize = AtomicIsize::new(0);
static VOL: AtomicU32 = AtomicU32::new(0);
static MUTED: AtomicBool = AtomicBool::new(false);

fn post(kind: usize) {
    let h = NOTIFY.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as *mut _), WM_STATUS, WPARAM(kind), LPARAM(0));
        }
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback;

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        if let Some(d) = unsafe { data.as_ref() } {
            VOL.store(d.fMasterVolume.to_bits(), Ordering::Relaxed);
            MUTED.store(d.bMuted.as_bool(), Ordering::Relaxed);
            post(STATUS_VOLUME);
        }
        Ok(())
    }
}

unsafe extern "system" fn ip_changed(_: *const core::ffi::c_void, _: *const MIB_IPINTERFACE_ROW, _: MIB_NOTIFICATION_TYPE) {
    post(STATUS_NET);
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum Net {
    #[default]
    Offline,
    Ethernet,
    /// Signal quality 0-100 (100 when Windows won't tell us).
    Wifi(u8),
}

pub struct Status {
    devices: Option<IMMDeviceEnumerator>,
    endpoint: Option<IAudioEndpointVolume>,
    callback: Option<IAudioEndpointVolumeCallback>,
    device_id: String,
    pub net: Net,
    pub ssid: String,
    wlan: HANDLE,
    pub mic: Vec<String>,
    pub cam: Vec<String>,
    privacy: Privacy,
    _ip_notify: HANDLE,
}

impl Status {
    pub fn new(main: HWND) -> Status {
        NOTIFY.store(main.0 as isize, Ordering::Relaxed);
        let devices = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() };
        let mut ip_notify = HANDLE::default();
        unsafe {
            let _ = NotifyIpInterfaceChange(AF_UNSPEC, Some(ip_changed), None, false, &mut ip_notify);
        }
        let mut s = Status {
            devices,
            endpoint: None,
            callback: None,
            device_id: String::new(),
            net: Net::Offline,
            ssid: String::new(),
            wlan: HANDLE::default(),
            mic: Vec::new(),
            cam: Vec::new(),
            privacy: Privacy::default(),
            _ip_notify: ip_notify,
        };
        s.refresh_audio();
        s.refresh_net();
        s.privacy.start_camera();
        s.refresh_privacy();
        s
    }

    /// (level 0..1, muted), or None without an output device.
    pub fn volume(&self) -> Option<(f32, bool)> {
        self.endpoint.as_ref()?;
        Some((f32::from_bits(VOL.load(Ordering::Relaxed)), MUTED.load(Ordering::Relaxed)))
    }

    /// Follow the default output device (headphones plugged in, etc.).
    pub fn refresh_audio(&mut self) {
        let Some(devices) = &self.devices else { return };
        let device = unsafe { devices.GetDefaultAudioEndpoint(eRender, eMultimedia) }.ok();
        let id = device
            .as_ref()
            .and_then(|d| unsafe { d.GetId().ok() })
            .map(|p| unsafe {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            })
            .unwrap_or_default();
        if id == self.device_id && self.endpoint.is_some() == device.is_some() {
            return;
        }
        if let (Some(ep), Some(cb)) = (&self.endpoint, &self.callback) {
            unsafe {
                let _ = ep.UnregisterControlChangeNotify(cb);
            }
        }
        self.endpoint = None;
        self.callback = None;
        self.device_id = id;
        let Some(device) = device else { return };
        unsafe {
            let Ok(ep) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else { return };
            if let Ok(v) = ep.GetMasterVolumeLevelScalar() {
                VOL.store(v.to_bits(), Ordering::Relaxed);
            }
            if let Ok(m) = ep.GetMute() {
                MUTED.store(m.as_bool(), Ordering::Relaxed);
            }
            let cb: IAudioEndpointVolumeCallback = VolumeCallback.into();
            let _ = ep.RegisterControlChangeNotify(&cb);
            self.callback = Some(cb);
            self.endpoint = Some(ep);
        }
    }

    pub fn set_volume(&self, v: f32) {
        let Some(ep) = &self.endpoint else { return };
        let v = v.clamp(0.0, 1.0);
        unsafe {
            if MUTED.load(Ordering::Relaxed) && v > 0.0 {
                let _ = ep.SetMute(false, std::ptr::null());
                MUTED.store(false, Ordering::Relaxed);
            }
            let _ = ep.SetMasterVolumeLevelScalar(v, std::ptr::null());
        }
        VOL.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn toggle_mute(&self) {
        let Some(ep) = &self.endpoint else { return };
        let m = !MUTED.load(Ordering::Relaxed);
        unsafe {
            let _ = ep.SetMute(m, std::ptr::null());
        }
        MUTED.store(m, Ordering::Relaxed);
    }

    /// Re-read the connection type and Wi-Fi signal. Returns whether anything changed.
    pub fn refresh_net(&mut self) -> bool {
        let (mut eth, mut wifi) = (false, false);
        unsafe {
            // u64 buffer: IP_ADAPTER_ADDRESSES needs 8-byte alignment.
            let mut len: u32 = 32 * 1024;
            let mut buf: Vec<u64> = vec![0; len as usize / 8];
            let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
            let mut r = GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut _), &mut len);
            if r == 111 {
                // ERROR_BUFFER_OVERFLOW
                buf = vec![0; len as usize / 8 + 1];
                r = GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut _), &mut len);
            }
            if r == 0 {
                let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
                while let Some(a) = p.as_ref() {
                    // Only adapters with a default gateway are "the" connection; VPNs and
                    // virtual switches usually have none.
                    if a.OperStatus == IfOperStatusUp && !a.FirstGatewayAddress.is_null() {
                        match a.IfType {
                            6 => eth = true,    // IF_TYPE_ETHERNET_CSMACD
                            71 => wifi = true, // IF_TYPE_IEEE80211
                            _ => {}
                        }
                    }
                    p = a.Next;
                }
            }
        }
        let (quality, ssid) = if wifi && !eth { self.wifi_signal() } else { (100, String::new()) };
        let net = if eth { Net::Ethernet } else if wifi { Net::Wifi(quality) } else { Net::Offline };
        let changed = net != self.net || ssid != self.ssid;
        self.net = net;
        self.ssid = ssid;
        changed
    }

    fn wifi_signal(&mut self) -> (u8, String) {
        unsafe {
            if self.wlan.is_invalid() {
                let mut ver = 0u32;
                let mut h = HANDLE::default();
                if WlanOpenHandle(2, None, &mut ver, &mut h) != 0 {
                    return (100, String::new());
                }
                self.wlan = h;
            }
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            if WlanEnumInterfaces(self.wlan, None, &mut list) != 0 || list.is_null() {
                return (100, String::new());
            }
            let mut out = (100u8, String::new());
            let n = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
            for it in items {
                if it.isState != wlan_interface_state_connected {
                    continue;
                }
                let mut size = 0u32;
                let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
                if WlanQueryInterface(self.wlan, &it.InterfaceGuid, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None) == 0
                    && !data.is_null()
                {
                    let c = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                    let a = &c.wlanAssociationAttributes;
                    let len = (a.dot11Ssid.uSSIDLength as usize).min(32);
                    out = (a.wlanSignalQuality.min(100) as u8, String::from_utf8_lossy(&a.dot11Ssid.ucSSID[..len]).into_owned());
                    WlanFreeMemory(data);
                    break;
                }
            }
            WlanFreeMemory(list as *const _);
            out
        }
    }
}

// ---- Microphone / camera in use -------------------------------------------------------------
//
// Microphone: active capture sessions on every recording device (Core Audio); new sessions and
// session state changes arrive as callbacks.
// Camera: Media Foundation's sensor activity monitor reports which processes stream a camera.
// (The ConsentStore registry values are no longer written on current Windows builds.)
// Callbacks only post STATUS_PRIVACY; the camera one stores the process ids first.

use std::sync::Mutex;

use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL};
use windows::Win32::Media::Audio::{
    eCapture, AudioSessionDisconnectReason, AudioSessionState, AudioSessionStateActive, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2, IAudioSessionNotification, IAudioSessionNotification_Impl,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::Media::MediaFoundation::{
    IMFSensorActivitiesReport, IMFSensorActivitiesReportCallback, IMFSensorActivitiesReportCallback_Impl, IMFSensorActivityMonitor,
    MFCreateSensorActivityMonitor,
};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

pub const STATUS_PRIVACY: usize = 2;
/// Tailscale status changed (posted by the tailscale module).
pub const STATUS_TAILSCALE: usize = 3;
/// LastTerminal sessions changed (posted by the lastterminal module).
pub const STATUS_LASTTERMINAL: usize = 4;

static CAMERA_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

#[implement(IAudioSessionNotification)]
struct NewSession;

impl IAudioSessionNotification_Impl for NewSession_Impl {
    fn OnSessionCreated(&self, _: Option<&IAudioSessionControl>) -> Result<()> {
        post(STATUS_PRIVACY);
        Ok(())
    }
}

#[implement(IAudioSessionEvents)]
struct SessionEvents;

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> Result<()> {
        Ok(())
    }
    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> Result<()> {
        Ok(())
    }
    fn OnSimpleVolumeChanged(&self, _: f32, _: BOOL, _: *const GUID) -> Result<()> {
        Ok(())
    }
    fn OnChannelVolumeChanged(&self, _: u32, _: *const f32, _: u32, _: *const GUID) -> Result<()> {
        Ok(())
    }
    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> Result<()> {
        Ok(())
    }
    fn OnStateChanged(&self, _: AudioSessionState) -> Result<()> {
        post(STATUS_PRIVACY);
        Ok(())
    }
    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> Result<()> {
        post(STATUS_PRIVACY);
        Ok(())
    }
}

#[implement(IMFSensorActivitiesReportCallback)]
struct CameraActivity;

impl IMFSensorActivitiesReportCallback_Impl for CameraActivity_Impl {
    fn OnActivitiesReport(&self, report: Option<&IMFSensorActivitiesReport>) -> Result<()> {
        let mut pids = Vec::new();
        if let Some(r) = report {
            unsafe {
                for i in 0..r.GetCount().unwrap_or(0) {
                    let Ok(a) = r.GetActivityReport(i) else { continue };
                    for j in 0..a.GetProcessCount().unwrap_or(0) {
                        let Ok(p) = a.GetProcessActivity(j) else { continue };
                        if p.GetStreamingState().map(|s| s.as_bool()).unwrap_or(false) {
                            if let Ok(pid) = p.GetProcessId() {
                                pids.push(pid);
                            }
                        }
                    }
                }
            }
        }
        pids.sort();
        pids.dedup();
        let mut cur = CAMERA_PIDS.lock().unwrap_or_else(|e| e.into_inner());
        if *cur != pids {
            *cur = pids;
            post(STATUS_PRIVACY);
        }
        Ok(())
    }
}

/// "C:\...\Teams.exe" -> "Teams"
fn process_name(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return format!("process {pid}") };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return format!("process {pid}");
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let file = path.rsplit('\\').next().unwrap_or(&path);
        file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file).to_string()
    }
}

/// Recording devices with their new-session notification, per-session listeners, camera monitor.
#[derive(Default)]
struct Privacy {
    managers: Vec<(String, IAudioSessionManager2, IAudioSessionNotification)>,
    sessions: Vec<(IAudioSessionControl, IAudioSessionEvents)>,
    camera: Option<IMFSensorActivityMonitor>,
}

impl Privacy {
    fn start_camera(&mut self) {
        unsafe {
            let cb: IMFSensorActivitiesReportCallback = CameraActivity.into();
            if let Ok(m) = MFCreateSensorActivityMonitor(&cb) {
                if m.Start().is_ok() {
                    self.camera = Some(m);
                }
            }
        }
    }

    /// Follow recording devices being added or removed.
    fn sync_devices(&mut self, devices: &IMMDeviceEnumerator) {
        unsafe {
            let Ok(coll) = devices.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) else { return };
            let mut ids = Vec::new();
            for i in 0..coll.GetCount().unwrap_or(0) {
                let Ok(d) = coll.Item(i) else { continue };
                let Ok(p) = d.GetId() else { continue };
                let id = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                if !self.managers.iter().any(|(m, _, _)| *m == id) {
                    if let Ok(mgr) = d.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) {
                        let n: IAudioSessionNotification = NewSession.into();
                        // Notifications only start once the session list has been asked for.
                        let _ = mgr.GetSessionEnumerator();
                        let _ = mgr.RegisterSessionNotification(&n);
                        self.managers.push((id.clone(), mgr, n));
                    }
                }
                ids.push(id);
            }
            self.managers.retain(|(id, mgr, n)| {
                let keep = ids.contains(id);
                if !keep {
                    let _ = mgr.UnregisterSessionNotification(n);
                }
                keep
            });
        }
    }

    /// Apps with an active recording session; (re)subscribes to every session's state changes.
    fn microphone_apps(&mut self) -> Vec<String> {
        for (s, e) in self.sessions.drain(..) {
            unsafe {
                let _ = s.UnregisterAudioSessionNotification(&e);
            }
        }
        let me = std::process::id();
        let mut out = Vec::new();
        unsafe {
            for (_, mgr, _) in &self.managers {
                let Ok(list) = mgr.GetSessionEnumerator() else { continue };
                for i in 0..list.GetCount().unwrap_or(0) {
                    let Ok(s) = list.GetSession(i) else { continue };
                    let e: IAudioSessionEvents = SessionEvents.into();
                    let _ = s.RegisterAudioSessionNotification(&e);
                    if s.GetState().ok() == Some(AudioSessionStateActive) {
                        if let Ok(pid) = s.cast::<IAudioSessionControl2>().and_then(|c| c.GetProcessId()) {
                            if pid != 0 && pid != me {
                                out.push(process_name(pid));
                            }
                        }
                    }
                    self.sessions.push((s, e));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

impl Status {
    /// Re-read which apps use the microphone / camera. Returns whether anything changed.
    pub fn refresh_privacy(&mut self) -> bool {
        if let Some(d) = self.devices.clone() {
            self.privacy.sync_devices(&d);
        }
        let mic = self.privacy.microphone_apps();
        let pids = CAMERA_PIDS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut cam: Vec<String> = pids.into_iter().map(process_name).collect();
        cam.sort();
        cam.dedup();
        let changed = mic != self.mic || cam != self.cam;
        self.mic = mic;
        self.cam = cam;
        changed
    }
}


