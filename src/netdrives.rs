//! Network drives that follow you: each drive letter has a path for home and one for
//! everywhere else, e.g. a NAS by its local address at home and through Tailscale or a VPN
//! when away. LastDesk switches the mapping when you change networks.
//!
//! "Home" means a network whose router (default gateway) has one of the listed hardware
//! (MAC) addresses. Local addresses like 192.168.1.10 repeat on every other network, so this
//! keeps a café that happens to use the same numbers from ever being sent your login.
//!
//! Set up in drives.ini next to the config file. No file = the feature is off.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{BOOL, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GetIpNetEntry2, ResolveIpNetEntry2, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    IP_ADAPTER_ADDRESSES_LH, MIB_IPNET_ROW2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::NetworkManagement::WNet::{WNetAddConnection2W, WNetCancelConnection2W, WNetGetConnectionW, CONNECT_UPDATE_PROFILE, NETRESOURCEW, RESOURCETYPE_DISK};
use windows::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC, SOCKADDR_IN};

#[derive(Clone, Debug, PartialEq)]
pub struct Drive {
    /// "V:"
    pub letter: String,
    pub home: String,
    pub away: String,
}

#[derive(Clone, Debug, Default)]
pub struct Drives {
    /// Routers that mean "home" (MAC addresses, lowercase aa-bb-cc-dd-ee-ff).
    pub homes: Vec<String>,
    pub drives: Vec<Drive>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Place {
    Offline,
    Home,
    Away,
}

pub fn path() -> PathBuf {
    crate::config::path().with_file_name("drives.ini")
}

fn norm_mac(s: &str) -> String {
    s.trim().to_lowercase().replace(':', "-")
}

/// The settings, or None when drives.ini doesn't exist (feature off).
pub fn load() -> Option<Drives> {
    let text = std::fs::read_to_string(path()).ok()?;
    let mut d = Drives::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k.eq_ignore_ascii_case("home") {
            d.homes.extend(v.split(',').map(norm_mac).filter(|m| !m.is_empty()));
        } else if k.len() == 2 && k.ends_with(':') && k.as_bytes()[0].is_ascii_alphabetic() {
            let (home, away) = match v.split_once('|') {
                Some((h, a)) => (h.trim().to_string(), a.trim().to_string()),
                None => (v.to_string(), v.to_string()),
            };
            if !home.is_empty() {
                d.drives.push(Drive { letter: k.to_uppercase(), away: if away.is_empty() { home.clone() } else { away }, home });
            }
        }
    }
    Some(d)
}

fn save(d: &Drives) -> std::io::Result<()> {
    let mut s = String::new();
    s.push_str("# LastDesk network drives: each drive letter has a path for home and one for everywhere else.\n");
    s.push_str("# LastDesk switches between them when you change networks.\n#\n");
    s.push_str("#   X: = <path at home> | <path elsewhere>\n");
    s.push_str("#   e.g. V: = \\\\192.168.1.10\\Media | \\\\my-nas\\Media   (local address at home, Tailscale/VPN name away)\n#\n");
    s.push_str("# home = your router's hardware address(es), comma separated. Easiest: right-click LastDesk >\n");
    s.push_str("# Network drives > \"This network is home\" while connected at home.\n\n");
    s.push_str(&format!("home = {}\n\n", d.homes.join(", ")));
    for dr in &d.drives {
        s.push_str(&format!("{} = {} | {}\n", dr.letter, dr.home, dr.away));
    }
    std::fs::write(path(), s)
}

/// Hardware address of the current default gateway (the router), if online.
pub fn gateway_mac() -> Option<String> {
    unsafe {
        let mut len: u32 = 32 * 1024;
        let mut buf: Vec<u64> = vec![0; len as usize / 8];
        let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut r = GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut _), &mut len);
        if r == 111 {
            buf = vec![0; len as usize / 8 + 1];
            r = GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut _), &mut len);
        }
        if r != 0 {
            return None;
        }
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while let Some(a) = p.as_ref() {
            // Real connections only (Ethernet / Wi-Fi with a gateway), not VPN adapters.
            if a.OperStatus == IfOperStatusUp && matches!(a.IfType, 6 | 71) {
                let mut g = a.FirstGatewayAddress;
                while let Some(gw) = g.as_ref() {
                    let sa = gw.Address.lpSockaddr;
                    if !sa.is_null() && (*sa).sa_family == AF_INET {
                        let mut row = MIB_IPNET_ROW2::default();
                        row.Address.Ipv4 = *(sa as *const SOCKADDR_IN);
                        row.InterfaceIndex = a.Anonymous1.Anonymous.IfIndex;
                        let ok = GetIpNetEntry2(&mut row) == ERROR_SUCCESS || ResolveIpNetEntry2(&mut row, None) == ERROR_SUCCESS;
                        let n = row.PhysicalAddressLength as usize;
                        if ok && n > 0 && n <= row.PhysicalAddress.len() && row.PhysicalAddress[..n].iter().any(|b| *b != 0) {
                            return Some(row.PhysicalAddress[..n].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join("-"));
                        }
                    }
                    g = gw.Next;
                }
            }
            p = a.Next;
        }
        None
    }
}

pub fn place(d: &Drives) -> Place {
    match gateway_mac() {
        None => Place::Offline,
        Some(m) if d.homes.contains(&m) => Place::Home,
        Some(_) => Place::Away,
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// What a drive letter points to now.
pub fn current(letter: &str) -> Option<String> {
    let l = wide(letter);
    let mut buf = [0u16; 1024];
    let mut n = buf.len() as u32;
    unsafe {
        if WNetGetConnectionW(PCWSTR(l.as_ptr()), PWSTR(buf.as_mut_ptr()), &mut n) != ERROR_SUCCESS {
            return None;
        }
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Tell running programs (file managers like Double Commander, Explorer) that a network drive
/// came or went, as Explorer does when you map one; mapping through the API doesn't.
fn announce(letter: &str, arrived: bool) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, DEV_BROADCAST_HDR, DEV_BROADCAST_VOLUME, DBT_DEVTYP_VOLUME, SMTO_ABORTIFHUNG, WM_DEVICECHANGE};
    const DBT_DEVICEARRIVAL: usize = 0x8000;
    const DBT_DEVICEREMOVECOMPLETE: usize = 0x8004;
    const DBTF_NET: u16 = 2;
    let Some(c) = letter.chars().next().filter(|c| c.is_ascii_alphabetic()) else { return };
    let v = DEV_BROADCAST_VOLUME {
        dbcv_size: std::mem::size_of::<DEV_BROADCAST_VOLUME>() as u32,
        dbcv_devicetype: DBT_DEVTYP_VOLUME.0,
        dbcv_reserved: 0,
        dbcv_unitmask: 1 << (c.to_ascii_uppercase() as u8 - b'A'),
        dbcv_flags: windows::Win32::UI::WindowsAndMessaging::DEV_BROADCAST_VOLUME_FLAGS(DBTF_NET),
    };
    let _ = std::mem::size_of::<DEV_BROADCAST_HDR>();
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND(0xffff as *mut _), // HWND_BROADCAST
            WM_DEVICECHANGE,
            WPARAM(if arrived { DBT_DEVICEARRIVAL } else { DBT_DEVICEREMOVECOMPLETE }),
            LPARAM(&v as *const _ as isize),
            SMTO_ABORTIFHUNG,
            2000,
            None,
        );
    }
}

/// Point a drive letter at `remote` (saved, so it comes back after a restart). Uses the
/// login saved in Windows for that server.
fn map(letter: &str, remote: &str) -> Result<(), u32> {
    let mut l = wide(letter);
    let mut r = wide(remote);
    unsafe {
        let _ = WNetCancelConnection2W(PCWSTR(l.as_ptr()), CONNECT_UPDATE_PROFILE, BOOL(1));
        let res = NETRESOURCEW { dwType: RESOURCETYPE_DISK, lpLocalName: PWSTR(l.as_mut_ptr()), lpRemoteName: PWSTR(r.as_mut_ptr()), ..Default::default() };
        let e = WNetAddConnection2W(&res, PCWSTR::null(), PCWSTR::null(), CONNECT_UPDATE_PROFILE);
        // Either way the letter changed underneath open windows: let them refresh.
        announce(letter, e == ERROR_SUCCESS);
        if e == ERROR_SUCCESS { Ok(()) } else { Err(e.0) }
    }
}

/// Does the server in `\\server\share` answer for file sharing (port 445)?
fn reachable(unc: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let host = unc.trim_start_matches('\\').split('\\').next().unwrap_or("");
    if host.is_empty() {
        return false;
    }
    let Ok(addrs) = (host, 445u16).to_socket_addrs() else { return false };
    addrs.take(3).any(|a| TcpStream::connect_timeout(&a, std::time::Duration::from_millis(1500)).is_ok())
}

/// Map each drive for where we are now. Returns what was done (for the log) and whether
/// some drive still needs switching (its server didn't answer yet: try again later).
pub fn apply() -> (Vec<String>, bool) {
    let Some(d) = load() else { return (Vec::new(), false) };
    let p = place(&d);
    if p == Place::Offline {
        return (Vec::new(), false); // a short drop: leave everything as it is
    }
    let mut done = Vec::new();
    let mut pending = false;
    for dr in &d.drives {
        let want = if p == Place::Home { &dr.home } else { &dr.away };
        let cur = current(&dr.letter);
        if cur.as_ref().is_some_and(|c| c.eq_ignore_ascii_case(want)) {
            continue;
        }
        // Only switch once the new path answers (e.g. a VPN may still be connecting); until
        // then the drive keeps its current path.
        if !reachable(want) {
            pending = true;
            done.push(format!("{} waiting: {} path not reachable yet", dr.letter, if p == Place::Home { "home" } else { "away" }));
            continue;
        }
        match map(&dr.letter, want) {
            Ok(()) => done.push(format!("{} -> {} path", dr.letter, if p == Place::Home { "home" } else { "away" })),
            Err(e) => {
                // Never leave the letter unmapped: put the previous path back.
                if let Some(c) = &cur {
                    let _ = map(&dr.letter, c);
                }
                pending = true;
                done.push(format!("{} mapping failed ({e}), kept the previous path", dr.letter));
            }
        }
    }
    (done, pending)
}

static GEN: AtomicU32 = AtomicU32::new(0);

/// The network changed: check the drives once things have settled (changes come in bursts
/// while a connection comes up). If a path doesn't answer yet, try again every 15 s for a
/// few minutes.
pub fn schedule_check() {
    if !path().exists() {
        return;
    }
    let g = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        for _ in 0..16 {
            if GEN.load(Ordering::SeqCst) != g {
                return; // a newer check took over
            }
            let (lines, pending) = apply();
            for line in lines {
                crate::trace(&format!("drives: {line}"));
            }
            if !pending {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(15));
        }
    });
}

/// Add the network we're on as a home network, then re-map.
pub fn mark_home() -> Result<(), String> {
    let mut d = load().unwrap_or_default();
    let mac = gateway_mac().ok_or("Not connected to a network.")?;
    if !d.homes.contains(&mac) {
        d.homes.push(mac);
    }
    save(&d).map_err(|e| e.to_string())?;
    schedule_check();
    Ok(())
}

/// Create drives.ini from the drives mapped now (home and away paths the same, to edit).
pub fn create_from_current() -> std::io::Result<()> {
    let mut d = Drives::default();
    for c in b'D'..=b'Z' {
        let letter = format!("{}:", c as char);
        if let Some(r) = current(&letter) {
            d.drives.push(Drive { letter, home: r.clone(), away: r });
        }
    }
    save(&d)
}

/// For the menu: where we are, and what each drive points to.
pub fn summary() -> Option<(Place, Vec<(String, bool)>)> {
    let d = load()?;
    let p = place(&d);
    let lines = d
        .drives
        .iter()
        .map(|dr| {
            let cur = current(&dr.letter);
            let which = match &cur {
                Some(c) if c.eq_ignore_ascii_case(&dr.home) => "home path",
                Some(c) if c.eq_ignore_ascii_case(&dr.away) => "away path",
                Some(_) => "other path",
                None => "not connected",
            };
            let connected = cur.is_some();
            (format!("{}  {}  ({which})", dr.letter, cur.unwrap_or_default()), connected)
        })
        .collect();
    Some((p, lines))
}
