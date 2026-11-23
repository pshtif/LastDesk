// Created by Peter @sHTiF Stefcek
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

/// A pinned app button on the bar. `target` is an exe path, "shell:AppsFolder\<AUMID>" or a
/// web address (a web app, e.g. LastTerminal at http://127.0.0.1:3101). `icon`: optional icon
/// file (.ico / .exe), environment variables allowed; empty = the target's own icon.
#[derive(Clone, PartialEq, Debug)]
pub struct Launcher {
    pub name: String,
    pub target: String,
    pub icon: String,
}

impl Launcher {
    /// "target" or "target | icon", as written in the config.
    pub fn value(&self) -> String {
        if self.icon.is_empty() { self.target.clone() } else { format!("{} | {}", self.target, self.icon) }
    }

    fn parse(name: &str, val: &str) -> Launcher {
        let (target, icon) = match val.split_once('|') {
            Some((t, i)) => (t.trim(), i.trim()),
            None => (val.trim(), ""),
        };
        Launcher { name: name.to_string(), target: target.to_string(), icon: icon.to_string() }
    }

    pub fn is_web(&self) -> bool {
        let t = self.target.to_ascii_lowercase();
        t.starts_with("http://") || t.starts_with("https://")
    }
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
    /// Apps (exe names) that keep their own window position when launched from Start.
    pub launch_ignore: Vec<String>,
    pub switch: HOT_KEY_MODIFIERS,
    pub switch_text: String,
    pub move_: HOT_KEY_MODIFIERS,
    pub move_text: String,
    pub pin: Hotkey,
    pub overview: Hotkey,
    pub next: Hotkey,
    pub prev: Hotkey,
    pub pinned_apps: Vec<String>,
    pub launchers: Vec<Launcher>,
    pub rules: Vec<Rule>,
    pub bar: bool,
    pub bar_edge: Edge,
    pub icons_only: bool,
    pub align: Align,
    pub hide_taskbar: bool,
    pub tray: bool,
    /// Memory meter in the bar.
    pub memory: bool,
    /// Tailscale status and menu in the bar (if Tailscale is installed).
    pub tailscale: bool,
    /// The Win key opens Start on the monitor you're on (needs a keyboard hook).
    pub win_key_start: bool,
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
        launch_ignore: Vec::new(),
        switch: MOD_ALT,
        switch_text: "alt".into(),
        move_: MOD_ALT | MOD_SHIFT,
        move_text: "alt+shift".into(),
        pin: parse_hotkey("alt+shift+p").unwrap(),
        overview: parse_hotkey("alt+`").unwrap(),
        next: parse_hotkey("ctrl+alt+right").unwrap(),
        prev: parse_hotkey("ctrl+alt+left").unwrap(),
        pinned_apps: Vec::new(),
        launchers: Vec::new(),
        rules: Vec::new(),
        bar: true,
        bar_edge: Edge::Top,
        icons_only: false,
        align: Align::Left,
        hide_taskbar: false,
        tray: true,
        memory: false,
        tailscale: false,
        win_key_start: false,
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
            ("", "launch_ignore") => {
                c.launch_ignore = val.split(',').map(|s| s.trim().trim_end_matches(".exe").to_lowercase()).filter(|s| !s.is_empty()).collect()
            }
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
            ("bar", "memory") => match bool_of(val) {
                Some(b) => c.memory = b,
                None => bad(),
            },
            ("bar", "tailscale") => match bool_of(val) {
                Some(b) => c.tailscale = b,
                None => bad(),
            },
            ("bar", "win_key_start") => match bool_of(val) {
                Some(b) => c.win_key_start = b,
                None => bad(),
            },
            ("pinned_apps", app) => c.pinned_apps.push(app.trim_end_matches(".exe").to_string()),
            ("launchers", name) if !val.is_empty() => c.launchers.push(Launcher::parse(name, val)),
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
    s.push_str(&format!("animation = {}\n", anim_name(c.animation)));
    s.push_str("# Apps launched from Start open on that monitor. List apps that should keep their own\n# window position instead, e.g. launch_ignore = steam, vlc\n");
    s.push_str(&format!("launch_ignore = {}\n\n", c.launch_ignore.join(", ")));
    s.push_str("[keys]\n");
    s.push_str("# Switch workspace:            <switch>+1..N\n");
    s.push_str(&format!("switch = {}\n", c.switch_text));
    s.push_str("# Move focused window there:   <move>+1..N\n");
    s.push_str(&format!("move = {}\n", c.move_text));
    s.push_str(&format!("pin = {}\n", c.pin.text));
    s.push_str(&format!("overview = {}\n", c.overview.text));
    s.push_str("# Next / previous workspace on the monitor you're on.\n");
    s.push_str(&format!("next = {}\n", c.next.text));
    s.push_str(&format!("prev = {}\n", c.prev.text));
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
    s.push_str(&format!("tray = {}\n", c.tray));
    s.push_str("# Memory use in the bar (hover for the apps using the most).\n");
    s.push_str(&format!("memory = {}\n", c.memory));
    s.push_str("# Tailscale status and menu in the bar, if Tailscale is installed (replaces its tray app).\n");
    s.push_str(&format!("tailscale = {}\n", c.tailscale));
    s.push_str("# The Win key opens Start on the monitor you're on (uses a keyboard hook that only looks at the Win key).\n");
    s.push_str(&format!("win_key_start = {}\n\n", c.win_key_start));
    s.push_str("[pinned_apps]\n");
    s.push_str("# Apps shown on every workspace. One exe name per line (without .exe), e.g. spotify\n");
    for a in &c.pinned_apps {
        s.push_str(&format!("{a}\n"));
    }
    s.push_str("\n[launchers]\n");
    s.push_str("# Pinned app buttons on the bar (right-click a window button > Pin to bar).\n");
    s.push_str("# name = exe path   or   name = shell:AppsFolder\\<app id>   or   name = web address\n");
    s.push_str("# Optional icon after a |, e.g.  lastterminal = http://127.0.0.1:3101 | %LOCALAPPDATA%\\LastTerminal\\icon.ico\n");
    for l in &c.launchers {
        s.push_str(&format!("{} = {}\n", l.name, l.value()));
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
        // Pins: plain, and a web app with its own icon, both survive a save.
        let mut p = parse("[launchers]\ncalc = shell:AppsFolder\\x!App\nlastterminal = http://127.0.0.1:3101 | %LOCALAPPDATA%\\LastTerminal\\icon.ico\n");
        p.errors.clear();
        let pb = parse(&render(&p));
        assert_eq!(pb.launchers, p.launchers);
        assert!(pb.launchers[1].is_web() && pb.launchers[1].icon.ends_with("icon.ico") && !pb.launchers[0].is_web());
        // Integrations: off unless turned on, and kept when saved.
        assert!(!back.tailscale && !back.win_key_start && !back.memory);
        let mut on = parse("[bar]\ntailscale = true\nwin_key_start = true\nmemory = true\n");
        on.errors.clear();
        let on_back = parse(&render(&on));
        assert!(on_back.tailscale && on_back.win_key_start && on_back.memory && on_back.errors.is_empty());
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
