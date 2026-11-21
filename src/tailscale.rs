//! Tailscale on the bar, replacing its tray app. Talks to the Tailscale service (tailscaled)
//! over its local API pipe, like the tray app does: one long-lived "watch" connection that
//! tells us when something changes (and, like the tray app's, keeps the service connected
//! for this user), plus short requests for status and for changing settings.
//! Nothing shows up if Tailscale isn't installed.

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

#[derive(Clone, Default, Debug)]
pub struct ExitNode {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub active: bool,
}

#[derive(Clone, Default, Debug)]
pub struct Status {
    pub state: State,
    pub tailnet: String,
    pub ip: String,
    pub exit_nodes: Vec<ExitNode>,
}

impl Status {
    pub fn exit_node(&self) -> Option<&ExitNode> {
        self.exit_nodes.iter().find(|e| e.active)
    }
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

/// Latest known status (None until the first read).
pub fn status() -> Option<Status> {
    STATUS.lock().ok()?.clone()
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

fn parse(body: &str) -> Option<Status> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let state = match v["BackendState"].as_str().unwrap_or("") {
        "Running" => State::Running,
        "Starting" => State::Starting,
        "NeedsLogin" | "NeedsMachineAuth" => State::NeedsLogin,
        "Stopped" => State::Stopped,
        _ => State::Unavailable,
    };
    let tailnet = v["CurrentTailnet"]["Name"].as_str().unwrap_or("").to_string();
    let ip = v["Self"]["TailscaleIPs"].as_array().and_then(|a| a.iter().filter_map(|x| x.as_str()).find(|x| x.contains('.'))).unwrap_or("").to_string();
    let mut exit_nodes: Vec<ExitNode> = v["Peer"]
        .as_object()
        .map(|peers| {
            peers
                .values()
                .filter(|p| p["ExitNodeOption"].as_bool() == Some(true))
                .map(|p| {
                    let dns = p["DNSName"].as_str().unwrap_or("").split('.').next().unwrap_or("").to_string();
                    ExitNode {
                        id: p["ID"].as_str().unwrap_or("").to_string(),
                        name: if dns.is_empty() { p["HostName"].as_str().unwrap_or("?").to_string() } else { dns },
                        online: p["Online"].as_bool().unwrap_or(false),
                        active: p["ExitNode"].as_bool().unwrap_or(false),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    exit_nodes.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Some(Status { state, tailnet, ip, exit_nodes })
}

fn refresh(notify: &dyn Fn()) {
    let st = request("GET", "/localapi/v0/status", None).and_then(|b| parse(&b)).unwrap_or_default();
    let changed = {
        let mut cur = STATUS.lock().unwrap();
        let changed = cur.as_ref().map(|c| format!("{c:?}")) != Some(format!("{st:?}"));
        *cur = Some(st);
        changed
    };
    if changed {
        notify();
    }
}

/// Start watching in the background; `notify` is called (on that thread) when the status changes.
pub fn start(notify: impl Fn() + Send + 'static) {
    std::thread::spawn(move || loop {
        let Some(p) = Pipe::open() else {
            refresh(&notify); // shows "unavailable"
            std::thread::sleep(Duration::from_secs(10));
            continue;
        };
        // mask 2 = send the current state at once; later messages arrive on every change.
        let req = "GET /localapi/v0/watch-ipn-bus?mask=2 HTTP/1.1\r\nHost: local-tailscaled.sock\r\nSec-Tailscale: localapi\r\n\r\n";
        if !p.write(req.as_bytes()) {
            std::thread::sleep(Duration::from_secs(10));
            continue;
        }
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if p.read(&mut buf) == 0 {
                break; // service restarted or stopped
            }
            // Changes come in bursts: let it settle, skip what's queued, then read the status once.
            std::thread::sleep(Duration::from_millis(300));
            while p.available() > 0 {
                if p.read(&mut buf) == 0 {
                    break;
                }
            }
            refresh(&notify);
        }
        refresh(&notify);
        std::thread::sleep(Duration::from_secs(3));
    });
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
