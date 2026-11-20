//! `%APPDATA%\LastDesk\config.ini`: workspace count, shortcuts, bar options, pinned apps and
//! app rules. A commented file is written on first start; the Settings window rewrites it
//! with `render`. Tray > "Reload config" applies hand edits.

use std::path::PathBuf;

use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

#[derive(Clone)]
pub struct Hotkey {
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: u32,
    pub text: String,
}

#[derive(Clone)]
pub struct Rule {
    pub app: String,
    pub monitor: usize,
    pub workspace: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    Top,
    Bottom,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone)]
pub struct Config {
    pub workspaces: usize,
    /// Workspace switch slide duration in ms; 0 = off.
    pub animation: u32,
    pub switch: HOT_KEY_MODIFIERS,
    pub switch_text: String,
    pub move_: HOT_KEY_MODIFIERS,
    pub move_text: String,
    pub pin: Hotkey,
    pub overview: Hotkey,
    pub next: Hotkey,
    pub prev: Hotkey,
    pub pinned_apps: Vec<String>,
    pub rules: Vec<Rule>,
    pub bar: bool,
    pub bar_edge: Edge,
    pub icons_only: bool,
    pub align: Align,
    pub hide_taskbar: bool,
    pub tray: bool,
    pub errors: Vec<String>,
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("LastDesk").join("config.ini")
}

pub fn parse_mods(s: &str) -> Option<HOT_KEY_MODIFIERS> {
    let mut m = HOT_KEY_MODIFIERS(0);
    for t in s.split('+').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()) {
        m |= match t.as_str() {
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "ctrl" | "control" => MOD_CONTROL,
            "win" => MOD_WIN,
            _ => return None,
        };
    }
    (m.0 != 0).then_some(m)
}

fn parse_key(s: &str) -> Option<u32> {
    let k = s.trim().to_lowercase();
    let vk = match k.as_str() {
        "`" | "backtick" => 0xC0,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "space" => 0x20,
        "tab" => 0x09,
        "enter" => 0x0D,
        "-" => 0xBD,
        "=" => 0xBB,
        ";" => 0xBA,
        "," => 0xBC,
        "." => 0xBE,
        "/" => 0xBF,
        "[" => 0xDB,
        "]" => 0xDD,
        _ if k.len() > 1 && k.starts_with('f') => {
            let n: u32 = k[1..].parse().ok()?;
            if (1..=24).contains(&n) { 0x6F + n } else { return None }
        }
        _ if k.chars().count() == 1 => {
            let c = k.chars().next()?.to_ascii_uppercase();
            if c.is_ascii_alphanumeric() { c as u32 } else { return None }
        }
        _ => return None,
    };
    Some(vk)
}

pub fn parse_hotkey(s: &str) -> Option<Hotkey> {
    let (mods, key) = s.rsplit_once('+')?;
    Some(Hotkey { mods: parse_mods(mods)?, vk: parse_key(key)?, text: s.trim().to_string() })
}

pub fn default_text() -> String {
    render(&parse(""))
}

pub fn load() -> Config {
    let p = path();
    if !p.exists() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, default_text());
    }
    let text = std::fs::read_to_string(&p).unwrap_or_else(|_| default_text());
    parse(&text)
}

pub fn save(c: &Config) -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, render(c))
}

pub fn anim_ms(v: &str) -> Option<u32> {
    match v.trim().to_lowercase().as_str() {
        "off" | "none" | "false" | "0" => Some(0),
        "fast" => Some(130),
        "normal" | "on" | "true" => Some(200),
        "slow" => Some(320),
        n => n.trim_end_matches("ms").trim().parse::<u32>().ok().filter(|v| *v <= 2000),
    }
}

pub fn anim_name(ms: u32) -> String {
    match ms {
        0 => "off".into(),
        130 => "fast".into(),
        200 => "normal".into(),
        320 => "slow".into(),
        n => n.to_string(),
    }
}

fn bool_of(v: &str) -> Option<bool> {
    match v.to_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

pub fn parse(text: &str) -> Config {
    let mut c = Config {
        workspaces: 4,
        animation: 200,
        switch: MOD_ALT,
        switch_text: "alt".into(),
        move_: MOD_ALT | MOD_SHIFT,
        move_text: "alt+shift".into(),
        pin: parse_hotkey("alt+shift+p").unwrap(),
        overview: parse_hotkey("alt+`").unwrap(),
        next: parse_hotkey("ctrl+alt+right").unwrap(),
        prev: parse_hotkey("ctrl+alt+left").unwrap(),
        pinned_apps: Vec::new(),
        rules: Vec::new(),
        bar: true,
        bar_edge: Edge::Top,
        icons_only: false,
        align: Align::Left,
        hide_taskbar: false,
        tray: true,
        errors: Vec::new(),
    };
    let mut section = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_lowercase();
            continue;
        }
        let (key, val) = match line.split_once('=') {
            Some((k, v)) => (k.trim().to_lowercase(), v.trim()),
            None => (line.to_lowercase(), ""),
        };
        let mut bad = || c.errors.push(format!("line {}: {}", n + 1, raw.trim()));
        match (section.as_str(), key.as_str()) {
            ("", "workspaces") => match val.parse::<usize>() {
                Ok(v) if (1..=9).contains(&v) => c.workspaces = v,
                _ => bad(),
            },
            ("", "animation") => match anim_ms(val) {
                Some(v) => c.animation = v,
                None => bad(),
            },
            ("keys", "switch") => match parse_mods(val) {
                Some(m) => (c.switch, c.switch_text) = (m, val.to_lowercase()),
                None => bad(),
            },
            ("keys", "move") => match parse_mods(val) {
                Some(m) => (c.move_, c.move_text) = (m, val.to_lowercase()),
                None => bad(),
            },
            ("keys", "pin") => match parse_hotkey(val) {
                Some(h) => c.pin = h,
                None => bad(),
            },
            ("keys", "overview") => match parse_hotkey(val) {
                Some(h) => c.overview = h,
                None => bad(),
            },
            ("keys", "next") => match parse_hotkey(val) {
                Some(h) => c.next = h,
                None => bad(),
            },
            ("keys", "prev") => match parse_hotkey(val) {
                Some(h) => c.prev = h,
                None => bad(),
            },
            ("bar", "enabled") => match bool_of(val) {
                Some(b) => c.bar = b,
                None => bad(),
            },
            ("bar", "position") => match val.to_lowercase().as_str() {
                "top" => c.bar_edge = Edge::Top,
                "bottom" => c.bar_edge = Edge::Bottom,
                _ => bad(),
            },
            ("bar", "buttons") => match val.to_lowercase().as_str() {
                "full" | "title" | "icon+title" => c.icons_only = false,
                "icons" | "icon" | "icons_only" => c.icons_only = true,
                _ => bad(),
            },
            ("bar", "align") => match val.to_lowercase().as_str() {
                "left" => c.align = Align::Left,
                "center" | "centre" => c.align = Align::Center,
                "right" => c.align = Align::Right,
                _ => bad(),
            },
            ("bar", "hide_taskbar") => match bool_of(val) {
                Some(b) => c.hide_taskbar = b,
                None => bad(),
            },
            ("bar", "tray") => match bool_of(val) {
                Some(b) => c.tray = b,
                None => bad(),
            },
            ("pinned_apps", app) => c.pinned_apps.push(app.trim_end_matches(".exe").to_string()),
            ("rules", app) => {
                let nums: Vec<Option<usize>> = val.split(',').map(|s| s.trim().parse().ok()).collect();
                match nums.as_slice() {
                    [Some(m), Some(w)] if *m >= 1 && *w >= 1 => c.rules.push(Rule {
                        app: app.trim_end_matches(".exe").to_string(),
                        monitor: m - 1,
                        workspace: w - 1,
                    }),
                    _ => bad(),
                }
            }
            _ => bad(),
        }
    }
    c
}

/// The whole config file, with comments.
pub fn render(c: &Config) -> String {
    let edge = match c.bar_edge {
        Edge::Top => "top",
        Edge::Bottom => "bottom",
    };
    let align = match c.align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
    };
    let mut s = String::new();
    s.push_str("# LastDesk configuration. Use tray > Settings, or edit this file and use tray > \"Reload config\".\n");
    s.push_str("# Lines starting with # are comments.\n\n");
    s.push_str("# Workspaces per monitor (1-9).\n");
    s.push_str(&format!("workspaces = {}\n", c.workspaces));
    s.push_str("# Slide when switching workspaces: off | fast | normal | slow (or milliseconds).\n");
    s.push_str(&format!("animation = {}\n\n", anim_name(c.animation)));
    s.push_str("[keys]\n");
    s.push_str("# Switch workspace:            <switch>+1..N\n");
    s.push_str(&format!("switch = {}\n", c.switch_text));
    s.push_str("# Move focused window there:   <move>+1..N\n");
    s.push_str(&format!("move = {}\n", c.move_text));
    s.push_str(&format!("pin = {}\n", c.pin.text));
    s.push_str(&format!("overview = {}\n", c.overview.text));
    s.push_str("# Tip: Windows' own Win+Shift+Left/Right moves a window to the other monitor and works with LastDesk.\n\n");
    s.push_str("[bar]\n");
    s.push_str("# LastDesk's own taskbar on every monitor.\n");
    s.push_str(&format!("enabled = {}\n", c.bar));
    s.push_str("# top | bottom\n");
    s.push_str(&format!("position = {edge}\n"));
    s.push_str("# Window buttons: full (icon + title) | icons\n");
    s.push_str(&format!("buttons = {}\n", if c.icons_only { "icons" } else { "full" }));
    s.push_str("# Window buttons alignment: left | center | right\n");
    s.push_str(&format!("align = {align}\n"));
    s.push_str("# Hide the Windows taskbar while the LastDesk bar is on.\n");
    s.push_str(&format!("hide_taskbar = {}\n", c.hide_taskbar));
    s.push_str("# Show apps' tray icons in the LastDesk bar.\n");
    s.push_str(&format!("tray = {}\n\n", c.tray));
    s.push_str("[pinned_apps]\n");
    s.push_str("# Apps shown on every workspace. One exe name per line (without .exe), e.g. spotify\n");
    for a in &c.pinned_apps {
        s.push_str(&format!("{a}\n"));
    }
    s.push_str("\n[rules]\n");
    s.push_str("# New windows of an app open on a given monitor and workspace (monitor 1 = leftmost):\n");
    s.push_str("# app = monitor, workspace      e.g.  slack = 2, 3\n");
    for r in &c.rules {
        s.push_str(&format!("{} = {}, {}\n", r.app, r.monitor + 1, r.workspace + 1));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_cleanly() {
        let c = parse(&default_text());
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.workspaces, 4);
        assert_eq!(c.switch, MOD_ALT);
        assert_eq!(c.move_, MOD_ALT | MOD_SHIFT);
        assert_eq!((c.pin.mods, c.pin.vk), (MOD_ALT | MOD_SHIFT, 'P' as u32));
        assert_eq!((c.overview.mods, c.overview.vk), (MOD_ALT, 0xC0));
        assert!(c.pinned_apps.is_empty() && c.rules.is_empty());
        assert_eq!((c.bar_edge, c.icons_only, c.align), (Edge::Top, false, Align::Left));
    }

    #[test]
    fn custom_values_rules_and_errors() {
        let c = parse("workspaces = 6\n[keys]\nswitch = win+alt\noverview = ctrl+alt+f2\n[bar]\nposition = bottom\nbuttons = icons\nalign = center\n[pinned_apps]\nSpotify.exe\n[rules]\nslack = 2, 3\nbroken = x\n");
        assert_eq!(c.workspaces, 6);
        assert_eq!(c.switch, MOD_WIN | MOD_ALT);
        assert_eq!((c.overview.mods, c.overview.vk), (MOD_CONTROL | MOD_ALT, 0x71));
        assert_eq!((c.bar_edge, c.icons_only, c.align), (Edge::Bottom, true, Align::Center));
        assert_eq!(c.pinned_apps, vec!["spotify".to_string()]);
        assert_eq!((c.rules[0].app.as_str(), c.rules[0].monitor, c.rules[0].workspace), ("slack", 1, 2));
        assert_eq!(c.errors.len(), 1);
    }

    #[test]
    fn render_round_trips() {
        let mut c = parse("workspaces = 3\n[keys]\nswitch = ctrl+alt\n[bar]\nposition = bottom\nbuttons = icons\nalign = right\nhide_taskbar = true\ntray = false\n[pinned_apps]\nspotify\n[rules]\nslack = 2, 3\n");
        c.errors.clear();
        let back = parse(&render(&c));
        assert!(back.errors.is_empty(), "{:?}", back.errors);
        assert_eq!(back.workspaces, 3);
        assert_eq!(back.switch, MOD_CONTROL | MOD_ALT);
        assert_eq!((back.bar_edge, back.icons_only, back.align), (Edge::Bottom, true, Align::Right));
        assert!(back.hide_taskbar && !back.tray);
        assert_eq!(back.pinned_apps, vec!["spotify".to_string()]);
        assert_eq!((back.rules[0].monitor, back.rules[0].workspace), (1, 2));
    }

    #[test]
    fn out_of_range_workspaces_is_an_error() {
        let c = parse("workspaces = 12");
        assert_eq!(c.workspaces, 4);
        assert_eq!(c.errors.len(), 1);
    }
}
