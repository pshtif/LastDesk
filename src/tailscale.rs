// Created by Peter @sHTiF Stefcek
//! Tailscale on the bar, replacing its tray app. Talks to the Tailscale service (tailscaled)
//! over its local API pipe, like the tray app does: one long-lived "watch" connection that
//! tells us when something changes (and, like the tray app's, keeps the service connected
//! for this user), plus short requests for status and for changing settings.
//! Nothing shows up if Tailscale isn't installed.
//!
//! Exit nodes: with the Mullvad add-on there are hundreds, so the full list (which needs the
//! big peer listing) is only loaded at start and when the menu opens; routine updates use
//! the small status (state, health, which exit node is in use and whether it's online).

use std::sync::Mutex;
use std::time::Duration;

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE, GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING, SECURITY_IMPERSONATION, SECURITY_SQOS_PRESENT};
use windows::Win32::System::Pipes::PeekNamedPipe;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_RT_REG_EXPAND_SZ, RRF_NOEXPAND};

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum State {
    /// Service not running / not reachable.
    #[default]
    Unavailable,
    Stopped,
    Starting,
    NeedsLogin,
    Running,
}

/// An exit node. Mullvad ones have a country and city; your own don't.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct ExitNode {
    pub id: String,
    pub name: String,
    pub country: String,
    pub city: String,
    pub priority: i64,
    pub online: bool,
}

impl ExitNode {
    pub fn is_located(&self) -> bool {
        !self.country.is_empty()
    }

    /// "Los Angeles, CA, USA" for Mullvad, the device name otherwise.
    pub fn label(&self) -> String {
        if self.is_located() {
            if self.city.is_empty() { self.country.clone() } else { format!("{}, {}", self.city, self.country) }
        } else {
            self.name.clone()
        }
    }
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Status {
    pub state: State,
    pub tailnet: String,
    pub ip: String,
    /// Tailscale's own health warnings.
    pub health: Vec<String>,
    /// Exit node in use: (id, online).
    pub exit: Option<(String, bool)>,
    pub lan_access: bool,
    /// "Run unattended": stays connected without any app (tray app or LastDesk) attached.
    pub unattended: bool,
}

impl Status {
    /// The exit node in use, if it's in the loaded list.
    pub fn exit_node(&self) -> Option<ExitNode> {
        let (id, _) = self.exit.as_ref()?;
        exit_nodes().into_iter().find(|n| &n.id == id)
    }

    /// Display name of the exit node in use.
    pub fn exit_label(&self) -> Option<String> {
        self.exit.as_ref()?;
        Some(self.exit_node().map(|n| n.label()).unwrap_or_else(|| "exit node".into()))
    }

    /// Problems worth showing: Tailscale's health warnings, and an offline exit node (which
    /// blocks all internet traffic until it's back or another is picked).
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if self.state == State::Running {
            if let Some((_, false)) = self.exit {
                w.push(format!("Exit node {} is offline: internet is blocked", self.exit_label().unwrap_or_default()));
            }
            w.extend(self.health.iter().cloned());
        }
        w
    }
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);
static NODES: Mutex<Vec<ExitNode>> = Mutex::new(Vec::new());
static ALERT: Mutex<Option<String>> = Mutex::new(None);

/// Latest known status (None until the first read).
pub fn status() -> Option<Status> {
    STATUS.lock().ok()?.clone()
}

/// Exit nodes as last loaded (sorted: own first, then by country, city).
pub fn exit_nodes() -> Vec<ExitNode> {
    NODES.lock().map(|n| n.clone()).unwrap_or_default()
}

/// A one-off notice to show (exit node went offline), taken once.
pub fn take_alert() -> Option<String> {
    ALERT.lock().ok()?.take()
}

/// Is Tailscale installed? (Its service is registered; checked once.)
pub fn installed() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| service_image().is_some())
}

fn service_image() -> Option<String> {
    let mut buf = [0u16; 512];
    let mut n = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SYSTEM\CurrentControlSet\Services\Tailscale"),
            w!("ImagePath"),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            None,
            Some(buf.as_mut_ptr() as _),
            Some(&mut n),
        )
        .ok()
        .ok()?;
    }
    let s = String::from_utf16_lossy(&buf[..(n as usize / 2).saturating_sub(1)]);
    let s = s.trim().trim_start_matches('"');
    let exe = s.split('"').next().unwrap_or(s).trim().to_string();
    (!exe.is_empty()).then_some(exe)
}

/// The Tailscale app (for logging in and its own settings).
pub fn app_path() -> Option<String> {
    let svc = service_image()?;
    let dir = svc.rsplit_once('\\')?.0;
    Some(format!("{dir}\\tailscale-ipn.exe"))
}

struct Pipe(HANDLE);

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl Pipe {
    fn open() -> Option<Pipe> {
        unsafe {
            // The service checks who's asking by impersonating the caller, so allow that.
            let flags = FILE_FLAGS_AND_ATTRIBUTES(SECURITY_SQOS_PRESENT.0 | SECURITY_IMPERSONATION.0);
            CreateFileW(w!(r"\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled"), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_NONE, None, OPEN_EXISTING, flags, None)
                .ok()
                .map(Pipe)
        }
    }

    fn write(&self, data: &[u8]) -> bool {
        let mut done = 0;
        while done < data.len() {
            let mut n = 0u32;
            if unsafe { WriteFile(self.0, Some(&data[done..]), Some(&mut n), None) }.is_err() || n == 0 {
                return false;
            }
            done += n as usize;
        }
        true
    }

    /// Blocking read; 0 = closed.
    fn read(&self, buf: &mut [u8]) -> usize {
        let mut n = 0u32;
        if unsafe { ReadFile(self.0, Some(buf), Some(&mut n), None) }.is_err() {
            return 0;
        }
        n as usize
    }

    fn available(&self) -> u32 {
        let mut n = 0u32;
        let _ = unsafe { PeekNamedPipe(self.0, None, 0, None, Some(&mut n), None) };
        n
    }
}

fn request_bytes(method: &str, path: &str, body: Option<&str>) -> Vec<u8> {
    let mut r = format!("{method} {path} HTTP/1.1\r\nHost: local-tailscaled.sock\r\nSec-Tailscale: localapi\r\nConnection: close\r\n");
    if let Some(b) = body {
        r.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
    }
    r.push_str("\r\n");
    if let Some(b) = body {
        r.push_str(b);
    }
    r.into_bytes()
}

/// One request/response; returns the body if the status was 2xx.
fn request(method: &str, path: &str, body: Option<&str>) -> Option<String> {
    let p = Pipe::open()?;
    if !p.write(&request_bytes(method, path, body)) {
        return None;
    }
    let mut all = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = p.read(&mut buf);
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&all);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let code: u16 = head.split_whitespace().nth(1)?.parse().ok()?;
    if !(200..300).contains(&code) {
        return None;
    }
    let chunked = head.lines().any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));
    Some(if chunked { dechunk(body) } else { body.to_string() })
}

fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else { break };
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        s = rest[n..].trim_start_matches("\r\n");
    }
    out
}

fn get_json(path: &str) -> Option<serde_json::Value> {
    serde_json::from_str(&request("GET", path, None)?).ok()
}

/// Small status (no peer list) + prefs.
fn read_status() -> Status {
    let Some(v) = get_json("/localapi/v0/status?peers=false") else { return Status::default() };
    let state = match v["BackendState"].as_str().unwrap_or("") {
        "Running" => State::Running,
        "Starting" => State::Starting,
        "NeedsLogin" | "NeedsMachineAuth" => State::NeedsLogin,
        "Stopped" => State::Stopped,
        _ => State::Unavailable,
    };
    let tailnet = v["CurrentTailnet"]["Name"].as_str().unwrap_or("").to_string();
    let ip = v["Self"]["TailscaleIPs"].as_array().and_then(|a| a.iter().filter_map(|x| x.as_str()).find(|x| x.contains('.'))).unwrap_or("").to_string();
    let health = v["Health"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    let exit = v["ExitNodeStatus"]["ID"].as_str().filter(|s| !s.is_empty()).map(|id| (id.to_string(), v["ExitNodeStatus"]["Online"].as_bool().unwrap_or(false)));
    let prefs = get_json("/localapi/v0/prefs");
    let pref = |k: &str| prefs.as_ref().and_then(|p| p[k].as_bool()).unwrap_or(false);
    let (lan_access, unattended) = (pref("ExitNodeAllowLANAccess"), pref("ForceDaemon"));
    Status { state, tailnet, ip, health, exit, lan_access, unattended }
}

/// Load the full exit node list (big: includes every peer).
pub fn load_exit_nodes() {
    let Some(v) = get_json("/localapi/v0/status") else { return };
    let mut nodes: Vec<ExitNode> = v["Peer"]
        .as_object()
        .map(|peers| {
            peers
                .values()
                .filter(|p| p["ExitNodeOption"].as_bool() == Some(true))
                .map(|p| {
                    let dns = p["DNSName"].as_str().unwrap_or("").split('.').next().unwrap_or("").to_string();
                    let loc = &p["Location"];
                    ExitNode {
                        id: p["ID"].as_str().unwrap_or("").to_string(),
                        name: if dns.is_empty() { p["HostName"].as_str().unwrap_or("?").to_string() } else { dns },
                        country: loc["Country"].as_str().unwrap_or("").to_string(),
                        city: loc["City"].as_str().unwrap_or("").to_string(),
                        priority: loc["Priority"].as_i64().unwrap_or(0),
                        online: p["Online"].as_bool().unwrap_or(false),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    nodes.sort_by(|a, b| {
        (a.is_located(), a.country.to_lowercase(), a.city.to_lowercase(), a.name.to_lowercase()).cmp(&(b.is_located(), b.country.to_lowercase(), b.city.to_lowercase(), b.name.to_lowercase()))
    });
    if let Ok(mut n) = NODES.lock() {
        *n = nodes;
    }
}

/// Best online node in a country (and city): highest priority, a random one among equals
/// (spreads load like Tailscale's own app does).
pub fn best_node(country: &str, city: Option<&str>) -> Option<String> {
    let nodes = exit_nodes();
    let pick: Vec<&ExitNode> = nodes.iter().filter(|n| n.online && n.country == country && city.map_or(true, |c| n.city == c)).collect();
    let top = pick.iter().map(|n| n.priority).max()?;
    let best: Vec<&&ExitNode> = pick.iter().filter(|n| n.priority == top).collect();
    let r = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as usize).unwrap_or(0);
    best.get(r % best.len()).map(|n| n.id.clone())
}

fn refresh(notify: &dyn Fn()) {
    let st = read_status();
    let (changed, need_nodes) = {
        let mut cur = STATUS.lock().unwrap();
        let prev = cur.clone();
        // Exit node went from online to offline: worth a notice.
        if let (Some(Some((pid, true))), Some((id, false))) = (prev.as_ref().map(|p| p.exit.clone()), st.exit.as_ref()) {
            if pid == *id {
                let name = st.exit_label().unwrap_or_else(|| "exit node".into());
                *ALERT.lock().unwrap() = Some(format!("Tailscale exit node {name} went offline. Internet traffic is blocked until it's back or you pick another one."));
            }
        }
        // Load the list if the exit node in use isn't in it, or if it's empty while connected
        // (e.g. it was loaded while Tailscale was still starting); at most once a minute.
        let unknown = st.exit.as_ref().is_some_and(|(id, _)| !exit_nodes().iter().any(|n| &n.id == id));
        let empty = st.state == State::Running && exit_nodes().is_empty();
        let need_nodes = (unknown || empty) && {
            static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
            let mut last = LAST.lock().unwrap();
            let due = last.map_or(true, |t| t.elapsed() > Duration::from_secs(60));
            if due {
                *last = Some(std::time::Instant::now());
            }
            due
        };
        let changed = prev.as_ref() != Some(&st);
        *cur = Some(st);
        (changed, need_nodes)
    };
    if need_nodes {
        load_exit_nodes(); // switched to a node we don't know yet: get its name
    }
    if changed {
        notify();
    }
}

static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Handle of the open watch connection (0 = none), so turning off can interrupt its read.
static WATCH: Mutex<isize> = Mutex::new(0);

/// Shown and connected? (Installed and turned on in Settings.)
pub fn active() -> bool {
    installed() && ENABLED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Turn the integration on or off (Settings > Integrations). Off closes the connection to
/// the Tailscale service; Tailscale itself is left as it is.
pub fn set_enabled(on: bool, notify: impl Fn() + Send + Sync + 'static) {
    use std::sync::atomic::Ordering::SeqCst;
    let on = on && installed();
    if ENABLED.swap(on, SeqCst) == on {
        return;
    }
    if on {
        if !STARTED.swap(true, SeqCst) {
            start(notify);
        } else {
            std::thread::spawn(load_exit_nodes);
        }
    } else {
        if let Ok(w) = WATCH.lock() {
            if *w != 0 {
                unsafe {
                    let _ = windows::Win32::System::IO::CancelIoEx(HANDLE(*w as *mut _), None);
                }
            }
        }
        if let Ok(mut s) = STATUS.lock() {
            *s = None;
        }
    }
}

fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Start watching in the background; `notify` is called (on a background thread) when the
/// status changes. The threads idle while the integration is off.
fn start(notify: impl Fn() + Send + Sync + 'static) {
    let notify = std::sync::Arc::new(notify);
    // Exit node list, once at start.
    std::thread::spawn(load_exit_nodes);
    // Safety net: re-read every 30 s (an exit node's online state can change without a
    // message on the watch connection).
    let n2 = notify.clone();
    std::thread::spawn(move || {
        let mut stuck_checks = 0;
        loop {
            std::thread::sleep(Duration::from_secs(30));
            if enabled() {
                recover_if_stuck(&mut stuck_checks);
                refresh(&*n2);
            }
        }
    });
    // This connection is what keeps Tailscale connected without its tray app, so if the
    // service ever refuses the options, fall back to the plainest watch request.
    let mut basic = false;
    std::thread::spawn(move || loop {
        if !enabled() {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let Some(p) = Pipe::open() else {
            refresh(&*notify); // shows "unavailable"
            std::thread::sleep(Duration::from_secs(10));
            continue;
        };
        // 2 = send the current state at once, 128 = include health changes.
        // Later messages arrive on every change.
        let mask = if basic { 2 } else { 130 };
        let req = format!("GET /localapi/v0/watch-ipn-bus?mask={mask} HTTP/1.1\r\nHost: local-tailscaled.sock\r\nSec-Tailscale: localapi\r\n\r\n");
        if !p.write(req.as_bytes()) {
            std::thread::sleep(Duration::from_secs(10));
            continue;
        }
        let mut buf = vec![0u8; 64 * 1024];
        let n = p.read(&mut buf);
        if n == 0 {
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        if !buf[..n].starts_with(b"HTTP/1.1 200") {
            crate::trace(&format!("tailscale watch refused: {}", String::from_utf8_lossy(&buf[..n.min(200)]).lines().last().unwrap_or("")));
            basic = !basic; // try the other form
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        *WATCH.lock().unwrap() = p.0 .0 as isize;
        refresh(&*notify);
        loop {
            if p.read(&mut buf) == 0 || !enabled() {
                break; // service restarted or stopped, or turned off here
            }
            // Changes come in bursts: let it settle, skip what's queued, then read the status once.
            std::thread::sleep(Duration::from_millis(300));
            while p.available() > 0 {
                if p.read(&mut buf) == 0 {
                    break;
                }
            }
            refresh(&*notify);
        }
        {
            // Close it while holding the lock, so turning off never cancels a reused handle.
            let mut w = WATCH.lock().unwrap();
            *w = 0;
            drop(p);
        }
        if enabled() {
            refresh(&*notify);
            std::thread::sleep(Duration::from_secs(3));
        } else {
            notify(); // repaint without the icon
        }
    });
}

/// The service can sit in "NoState" and never connect: after boot it waits for its tray app
/// to start it (unless unattended), and a failed first contact with the coordination server
/// isn't always retried. When it has been stuck there for two checks in a row while it is
/// meant to be connected (wanted running, not logged out), start it the way the tray app
/// does. A Tailscale turned off on purpose ("Stopped") or needing a login is left alone.
fn recover_if_stuck(stuck_checks: &mut u32) {
    let stuck = get_json("/localapi/v0/status?peers=false").is_some_and(|v| v["BackendState"].as_str() == Some("NoState"))
        && get_json("/localapi/v0/prefs")
            .is_some_and(|p| p["WantRunning"].as_bool() == Some(true) && p["LoggedOut"].as_bool() != Some(true));
    *stuck_checks = if stuck { *stuck_checks + 1 } else { 0 };
    if *stuck_checks >= 2 {
        *stuck_checks = 0;
        request("POST", "/localapi/v0/start", Some("{}"));
    }
}

fn patch_prefs(json: String) {
    std::thread::spawn(move || {
        request("PATCH", "/localapi/v0/prefs", Some(&json));
    });
}

pub fn set_running(on: bool) {
    patch_prefs(format!(r#"{{"WantRunning":{on},"WantRunningSet":true}}"#));
}

/// Use an exit node (its id), or none.
pub fn set_exit_node(id: Option<&str>) {
    let id = serde_json::Value::String(id.unwrap_or("").to_string());
    patch_prefs(format!(r#"{{"ExitNodeID":{id},"ExitNodeIDSet":true,"ExitNodeIP":"","ExitNodeIPSet":true}}"#));
}

/// Use the exit node Tailscale suggests (closest / fastest).
pub fn use_suggested() {
    std::thread::spawn(|| {
        if let Some(id) = get_json("/localapi/v0/suggest-exit-node").and_then(|v| v["ID"].as_str().map(String::from)).filter(|s| !s.is_empty()) {
            request("PATCH", "/localapi/v0/prefs", Some(&format!(r#"{{"ExitNodeID":{},"ExitNodeIDSet":true,"ExitNodeIP":"","ExitNodeIPSet":true}}"#, serde_json::Value::String(id))));
        }
    });
}

pub fn set_unattended(on: bool) {
    patch_prefs(format!(r#"{{"ForceDaemon":{on},"ForceDaemonSet":true}}"#));
}

pub fn set_lan_access(on: bool) {
    patch_prefs(format!(r#"{{"ExitNodeAllowLANAccess":{on},"ExitNodeAllowLANAccessSet":true}}"#));
}
