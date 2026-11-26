// Created by Peter @sHTiF Stefcek
//! The LastTerminal plugin for LastDesk: LastTerminal's icon in the bar (faded while its server
//! isn't running) with the number of running sessions, the list on hover, and a menu: each
//! session (opens LastTerminal on it), Open LastTerminal, or Start LastTerminal when it isn't
//! running. Shows nothing where LastTerminal isn't installed.
#![windows_subsystem = "windows"]

mod sessions;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};
use sessions::Session;

fn out(v: Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

/// Where LastTerminal (headless) is installed.
fn exe() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default();
    base.join("LastTerminal").join("lastterminal-headless.exe")
}

fn url() -> String {
    format!("http://127.0.0.1:{}/", sessions::PORT)
}

enum Event {
    Line(String),
    Sessions(Option<Vec<Session>>),
    Eof,
}

/// The bar item and the menu for these sessions (None: the server isn't running).
fn publish(list: &Option<Vec<Session>>) {
    if !exe().exists() {
        out(json!({ "type": "item" }));
        return;
    }
    let icon = exe().with_file_name("icon.ico").display().to_string();
    let tooltip = match list {
        None => "LastTerminal isn't running (click to start it)".to_string(),
        Some(l) => {
            let mut t = format!("LastTerminal\t{}", if l.len() == 1 { "1 session".to_string() } else { format!("{} sessions", l.len()) });
            for s in l.iter().take(10) {
                let what = if s.running.is_empty() { "idle" } else { s.running.as_str() };
                t.push_str(&format!("\n{}\t{what}", s.name));
            }
            t
        }
    };
    let badge = list.as_ref().map(|l| l.len()).filter(|n| *n > 0).map(|n| n.min(99).to_string()).unwrap_or_default();
    out(json!({ "type": "item", "icon": icon, "dim": list.is_none(), "badge": badge, "tooltip": tooltip, "menuOnClick": true }));

    let sep = json!({ "separator": true });
    let items: Vec<Value> = match list {
        None => vec![json!({ "id": "", "label": "LastTerminal isn't running", "enabled": false }), sep, json!({ "id": "start", "label": "Start LastTerminal" })],
        Some(l) => {
            let mut v: Vec<Value> = Vec::new();
            if l.is_empty() {
                v.push(json!({ "id": "", "label": "No sessions running", "enabled": false }));
            }
            for s in l.iter().take(20) {
                let what = if s.running.is_empty() { String::new() } else { format!("  \u{00B7}  {}", s.running) };
                let mut label = format!("{}{what}", s.name);
                if label.chars().count() > 70 {
                    label = label.chars().take(67).collect::<String>() + "\u{2026}";
                }
                v.push(json!({ "id": format!("session:{}", s.id), "label": label }));
            }
            v.push(sep);
            v.push(json!({ "id": "open", "label": "Open LastTerminal" }));
            v
        }
    };
    out(json!({ "type": "menu", "items": items }));
}

/// Starts LastTerminal's server (it runs without a window).
fn start_server() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let _ = std::process::Command::new(exe()).creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS).spawn();
}

/// A session link always goes to a new page (which brings that session forward).
fn launch(target: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let t: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    unsafe {
        ShellExecuteW(None, windows::core::w!("open"), windows::core::PCWSTR(t.as_ptr()), None, None, SW_SHOWNORMAL);
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
    // The session list, every few seconds; sent on when it changes.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut last: Option<Option<Vec<Session>>> = None;
            loop {
                let now = sessions::read_sessions();
                if last.as_ref() != Some(&now) {
                    last = Some(now.clone());
                    if tx.send(Event::Sessions(now)).is_err() {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_secs(4));
            }
        });
    }
    let mut list: Option<Vec<Session>> = None;
    while let Ok(ev) = rx.recv() {
        match ev {
            Event::Eof => break,
            Event::Sessions(l) => {
                list = l;
                publish(&list);
            }
            Event::Line(line) => {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                match (v["type"].as_str().unwrap_or(""), v["id"].as_str().unwrap_or("")) {
                    ("init", _) => publish(&list),
                    ("menu", "start") => {
                        start_server();
                        // Give the server a moment, then open the page.
                        std::thread::sleep(Duration::from_millis(1500));
                        out(json!({ "type": "open", "target": url() }));
                    }
                    // Opened as a pinned web app: a browser window already showing it comes forward.
                    ("menu", "open") => out(json!({ "type": "open", "target": url() })),
                    ("menu", id) if id.starts_with("session:") => launch(&format!("{}?session={}", url(), &id[8..])),
                    _ => {}
                }
            }
        }
    }
}
