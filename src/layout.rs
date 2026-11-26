// Created by Peter @sHTiF Stefcek
//! Remembering which window is on which workspace across LastDesk restarts and reboots.
//!
//! `layout.txt` (tab-separated, one record per line):
//!   A <monitor> <active workspace>
//!   N <monitor> <how many workspaces it has>
//!   W <monitor> <workspace | P for pinned> <hwnd> <exe> <class> <title>
//!
//! On start, a window is matched by hwnd first (LastDesk restarted, window still alive),
//! then by exact exe + class + title (after a reboot). Unmatched entries are ignored, so a
//! window is never hidden unless we're confident it's the same one.

use std::path::PathBuf;

pub struct Entry {
    pub mon: String,
    /// None = pinned (shown on every workspace)
    pub ws: Option<usize>,
    pub hwnd: isize,
    pub exe: String,
    pub class: String,
    pub title: String,
}

#[derive(Default)]
pub struct Layout {
    pub active: Vec<(String, usize)>,
    /// Workspaces per monitor (older files have none: as many as their windows need).
    pub counts: Vec<(String, usize)>,
    pub entries: Vec<Entry>,
}

fn path() -> PathBuf {
    crate::state_path().with_file_name("layout.txt")
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

pub fn save(l: &Layout) {
    let mut out = String::new();
    for (m, a) in &l.active {
        out.push_str(&format!("A\t{}\t{}\n", clean(m), a));
    }
    for (m, n) in &l.counts {
        out.push_str(&format!("N\t{}\t{}\n", clean(m), n));
    }
    for e in &l.entries {
        let ws = e.ws.map(|w| w.to_string()).unwrap_or_else(|| "P".into());
        out.push_str(&format!("W\t{}\t{}\t{}\t{}\t{}\t{}\n", clean(&e.mon), ws, e.hwnd, clean(&e.exe), clean(&e.class), clean(&e.title)));
    }
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, out);
}

pub fn load() -> Layout {
    let mut l = Layout::default();
    let Ok(text) = std::fs::read_to_string(path()) else { return l };
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["A", m, a] => {
                if let Ok(a) = a.parse() {
                    l.active.push((m.to_string(), a));
                }
            }
            ["N", m, n] => {
                if let Ok(n) = n.parse() {
                    l.counts.push((m.to_string(), n));
                }
            }
            ["W", m, ws, h, exe, class, title] => {
                let ws = if *ws == "P" { None } else { match ws.parse() { Ok(w) => Some(w), Err(_) => continue } };
                l.entries.push(Entry {
                    mon: m.to_string(),
                    ws,
                    hwnd: h.parse().unwrap_or(0),
                    exe: exe.to_string(),
                    class: class.to_string(),
                    title: title.to_string(),
                });
            }
            _ => {}
        }
    }
    l
}
