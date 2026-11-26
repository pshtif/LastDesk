// Created by Peter @sHTiF Stefcek
//! The Tailscale plugin for LastDesk: Tailscale's state in the bar (its dots: lit when connected,
//! accent with an exit node in use, orange when something needs attention), the details on
//! hover, and a menu (connect / disconnect, copy this device's IP, exit nodes with Mullvad's by
//! country and city, unattended mode, the app and the admin console). It talks to the Tailscale
//! service directly (tailscale.rs), so Tailscale's own tray app isn't needed.
#![windows_subsystem = "windows"]

mod tailscale;

use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};
use tailscale::State;

fn out(v: Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

enum Event {
    Line(String),
    Changed,
    Eof,
}

/// The bar item: the dots, lit by state.
fn item() -> Value {
    if !tailscale::installed() {
        return json!({ "type": "item" });
    }
    // Not heard from the service yet (it takes a moment after starting): nothing claimed.
    let Some(st) = tailscale::status() else {
        return json!({ "type": "item", "shape": "grid", "color": "muted", "tooltip": "Tailscale", "menuOnClick": true });
    };
    let color = match st.state {
        State::Running if !st.warnings().is_empty() => "attention",
        State::Running if st.exit.is_some() => "accent",
        State::Running => "text",
        State::NeedsLogin => "attention",
        State::Starting => "muted",
        State::Stopped | State::Unavailable => "none",
    };
    json!({ "type": "item", "shape": "grid", "color": color, "tooltip": tooltip(&st), "menuOnClick": true })
}

fn tooltip(st: &tailscale::Status) -> String {
    match st.state {
        State::Running => {
            let mut parts = vec!["Tailscale: connected".to_string()];
            if !st.tailnet.is_empty() {
                parts.push(st.tailnet.clone());
            }
            if !st.ip.is_empty() {
                parts.push(st.ip.clone());
            }
            if let Some(e) = st.exit_label() {
                parts.push(format!("via {e}"));
            }
            let mut text = parts.join("  \u{00B7}  ");
            for w in st.warnings() {
                text.push_str("\n\u{26A0} ");
                text.push_str(&w);
            }
            text
        }
        State::Starting => "Tailscale: connecting\u{2026}".into(),
        State::NeedsLogin => "Tailscale: needs login (click)".into(),
        State::Stopped => "Tailscale: disconnected".into(),
        State::Unavailable => "Tailscale: service not running".into(),
    }
}

fn entry(id: &str, label: &str) -> Value {
    json!({ "id": id, "label": label })
}

fn grayed(label: &str) -> Value {
    json!({ "id": "", "label": label, "enabled": false })
}

/// The menu as things are now.
fn menu() -> Value {
    let st = tailscale::status().unwrap_or_default();
    let nodes = tailscale::exit_nodes();
    let active = st.exit.as_ref().map(|(id, _)| id.clone());
    let active_node = active.as_ref().and_then(|id| nodes.iter().find(|n| &n.id == id).cloned());
    let sep = json!({ "separator": true });
    let mut items = Vec::new();
    items.push(grayed(&match st.state {
        State::Running if !st.tailnet.is_empty() => format!("Connected to {}", st.tailnet),
        State::Running => "Connected".into(),
        State::Starting => "Connecting\u{2026}".into(),
        State::NeedsLogin => "Needs login".into(),
        State::Stopped => "Disconnected".into(),
        State::Unavailable => "Tailscale service isn't running".into(),
    }));
    if let Some(l) = st.exit_label() {
        items.push(grayed(&format!("Exit node: {l}")));
    }
    for w in st.warnings() {
        let w: String = if w.chars().count() > 90 { w.chars().take(87).collect::<String>() + "\u{2026}" } else { w };
        items.push(grayed(&format!("\u{26A0} {w}")));
    }
    items.push(sep.clone());
    match st.state {
        State::Running | State::Starting => items.push(entry("toggle", "Disconnect")),
        State::Stopped => items.push(entry("toggle", "Connect")),
        State::NeedsLogin => items.push(entry("app", "Log in\u{2026}")),
        State::Unavailable => {}
    }
    if st.state == State::Running {
        if !st.ip.is_empty() {
            items.push(entry("copy-ip", &format!("Copy this device's IP ({})", st.ip)));
        }
        if !nodes.is_empty() || active.is_some() {
            let mut sub = vec![
                json!({ "id": "exit:none", "label": "None", "checked": active.is_none() }),
                entry("exit:suggested", "Fastest (Tailscale's suggestion)"),
            ];
            // Your own exit nodes, by name.
            let own: Vec<&tailscale::ExitNode> = nodes.iter().filter(|n| !n.is_located()).collect();
            if !own.is_empty() {
                sub.push(sep.clone());
                for n in own {
                    let on = active.as_deref() == Some(n.id.as_str());
                    let label = if n.online { n.name.clone() } else { format!("{} (offline)", n.name) };
                    sub.push(json!({ "id": format!("exit:node:{}", n.id), "label": label, "checked": on, "enabled": n.online || on }));
                }
            }
            // Located ones (Mullvad): Country → City; picking one uses its best server.
            let mut countries: Vec<&str> = nodes.iter().filter(|n| n.is_located()).map(|n| n.country.as_str()).collect();
            countries.dedup();
            if !countries.is_empty() {
                sub.push(sep.clone());
            }
            for country in countries {
                let in_country: Vec<&tailscale::ExitNode> = nodes.iter().filter(|n| n.country == country).collect();
                let any_online = in_country.iter().any(|n| n.online);
                let here = active_node.as_ref().is_some_and(|a| a.country == country);
                let mut cities: Vec<&str> = in_country.iter().map(|n| n.city.as_str()).collect();
                cities.dedup();
                let best = format!("exit:best:{country}\t");
                if cities.len() <= 1 {
                    sub.push(json!({ "id": best, "label": country, "checked": here, "enabled": any_online }));
                    continue;
                }
                let mut csub = vec![json!({ "id": best, "label": format!("Any city in {country}"), "enabled": any_online }), sep.clone()];
                for city in cities {
                    let online = in_country.iter().any(|n| n.city == city && n.online);
                    let on = here && active_node.as_ref().is_some_and(|a| a.city == city);
                    csub.push(json!({ "id": format!("exit:best:{country}\t{city}"), "label": city, "checked": on, "enabled": online }));
                }
                sub.push(json!({ "id": "", "label": country, "checked": here, "enabled": any_online, "items": csub }));
            }
            sub.push(sep.clone());
            sub.push(json!({ "id": if st.lan_access { "lan:off" } else { "lan:on" }, "label": "Allow local network access", "checked": st.lan_access }));
            items.push(json!({ "id": "", "label": "Exit node", "checked": active.is_some(), "items": sub }));
        }
    }
    if st.state != State::Unavailable {
        items.push(json!({ "id": "unattended", "label": "Stay connected without LastDesk (unattended)", "checked": st.unattended }));
    }
    items.push(sep);
    items.push(entry("app", "Open Tailscale app"));
    items.push(entry("admin", "Admin console"));
    json!({ "type": "menu", "items": items })
}

fn publish() {
    out(item());
    out(menu());
    if let Some(text) = tailscale::take_alert() {
        out(json!({ "type": "notify", "title": "Tailscale", "text": text, "kind": "warning" }));
    }
}

fn shell_open(target: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let t: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    unsafe {
        ShellExecuteW(None, windows::core::w!("open"), windows::core::PCWSTR(t.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

fn set_clipboard_text(text: &str) {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(h);
                if SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h.0)).is_err() {
                    let _ = GlobalFree(h);
                }
            }
        }
        let _ = CloseClipboard();
    }
}

/// A menu item chosen.
fn choose(id: &str) {
    let st = tailscale::status().unwrap_or_default();
    match id {
        "toggle" => tailscale::set_running(st.state == State::Stopped),
        "copy-ip" => set_clipboard_text(&st.ip),
        "unattended" => tailscale::set_unattended(!st.unattended),
        "app" => {
            if let Some(p) = tailscale::app_path() {
                shell_open(&p);
            }
        }
        "admin" => out(json!({ "type": "open", "target": "https://login.tailscale.com/admin/machines" })),
        "exit:none" => tailscale::set_exit_node(None),
        "exit:suggested" => tailscale::use_suggested(),
        "lan:on" => tailscale::set_lan_access(true),
        "lan:off" => tailscale::set_lan_access(false),
        _ => {
            if let Some(node) = id.strip_prefix("exit:node:") {
                tailscale::set_exit_node(Some(node));
            } else if let Some(place) = id.strip_prefix("exit:best:") {
                let (country, city) = place.split_once('\t').unwrap_or((place, ""));
                match tailscale::best_node(country, (!city.is_empty()).then_some(city)) {
                    Some(n) => tailscale::set_exit_node(Some(&n)),
                    None => out(json!({ "type": "notify", "title": "Tailscale", "text": "No server there is online right now.", "kind": "warning" })),
                }
            }
        }
    }
}

fn main() {
    let (tx, rx) = mpsc::channel::<Event>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                let _ = tx.send(Event::Line(line));
            }
            let _ = tx.send(Event::Eof);
        });
    }
    // Exit nodes: loaded now and again every few minutes (online states change), the menu sent
    // again each time.
    {
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            if tailscale::installed() {
                tailscale::load_exit_nodes();
                let _ = tx.send(Event::Changed);
            }
            std::thread::sleep(Duration::from_secs(180));
        });
    }
    while let Ok(ev) = rx.recv() {
        match ev {
            Event::Eof => break,
            Event::Changed => publish(),
            Event::Line(line) => {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                match v["type"].as_str() {
                    Some("init") => {
                        let tx = tx.clone();
                        tailscale::set_enabled(true, move || {
                            let _ = tx.send(Event::Changed);
                        });
                        publish();
                    }
                    Some("menu") => {
                        choose(v["id"].as_str().unwrap_or(""));
                        publish();
                    }
                    // A click when the item has no menu yet (Tailscale not answering): the app.
                    Some("click") => {
                        if let Some(p) = tailscale::app_path() {
                            shell_open(&p);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
