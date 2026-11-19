//! `%APPDATA%\LastDesk\config.ini`: workspace count, shortcuts, pinned apps and app rules.
//! A commented default is written on first start. Tray > "Reload config" applies changes.

use std::path::PathBuf;

use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

pub const DEFAULT: &str = r#"# LastDesk configuration. Edit, save, then tray > "Reload config".
# Lines starting with # are comments.

# Workspaces per monitor (1-9).
workspaces = 4

[keys]
# Switch workspace:            <switch>+1..N
switch = alt
# Move focused window there:   <move>+1..N
move = alt+shift
pin = alt+shift+p
overview = alt+`
# Tip: Windows' own Win+Shift+Left/Right moves a window to the other monitor and works with LastDesk.

[bar]
# LastDesk's own taskbar on top of every monitor.
enabled = true
# Hide the Windows taskbar while the LastDesk bar is on (its tray icons: the ^ button).
hide_taskbar = false

[pinned_apps]
# Apps shown on every workspace. One exe name per line (without .exe), e.g.:
# spotify

[rules]
# New windows of an app open on a given monitor and workspace (monitor 1 = leftmost):
# app = monitor, workspace
# slack = 2, 3
"#;

#[derive(Clone)]
pub struct Hotkey {
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: u32,
    pub text: String,
}

pub struct Rule {
    pub app: String,
    pub monitor: usize,
    pub workspace: usize,
}

pub struct Config {
    pub workspaces: usize,
    pub switch: HOT_KEY_MODIFIERS,
    pub switch_text: String,
    pub move_: HOT_KEY_MODIFIERS,
    pub move_text: String,
    pub pin: Hotkey,
    pub overview: Hotkey,
    pub pinned_apps: Vec<String>,
    pub rules: Vec<Rule>,
    pub bar: bool,
    pub hide_taskbar: bool,
    pub errors: Vec<String>,
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("LastDesk").join("config.ini")
}

fn parse_mods(s: &str) -> Option<HOT_KEY_MODIFIERS> {
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

fn parse_hotkey(s: &str) -> Option<Hotkey> {
    let (mods, key) = s.rsplit_once('+')?;
    Some(Hotkey { mods: parse_mods(mods)?, vk: parse_key(key)?, text: s.trim().to_string() })
}

pub fn load() -> Config {
    let p = path();
    if !p.exists() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, DEFAULT);
    }
    let text = std::fs::read_to_string(&p).unwrap_or_else(|_| DEFAULT.to_string());
    parse(&text)
}

pub fn parse(text: &str) -> Config {
    let mut c = Config {
        workspaces: 4,
        switch: MOD_ALT,
        switch_text: "alt".into(),
        move_: MOD_ALT | MOD_SHIFT,
        move_text: "alt+shift".into(),
        pin: parse_hotkey("alt+shift+p").unwrap(),
        overview: parse_hotkey("alt+`").unwrap(),
        pinned_apps: Vec::new(),
        rules: Vec::new(),
        bar: true,
        hide_taskbar: false,
        errors: Vec::new(),
    };
    let mut section = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        // A bare "`" key must survive comment stripping: "overview = alt+`" has no '#'.
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
            ("keys", "switch") => match parse_mods(val) {
                Some(m) => (c.switch, c.switch_text) = (m, val.to_string()),
                None => bad(),
            },
            ("keys", "move") => match parse_mods(val) {
                Some(m) => (c.move_, c.move_text) = (m, val.to_string()),
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
            ("bar", "enabled") => match val.to_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => c.bar = true,
                "false" | "no" | "off" | "0" => c.bar = false,
                _ => bad(),
            },
            ("bar", "hide_taskbar") => match val.to_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => c.hide_taskbar = true,
                "false" | "no" | "off" | "0" => c.hide_taskbar = false,
                _ => bad(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_cleanly() {
        let c = parse(DEFAULT);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.workspaces, 4);
        assert_eq!(c.switch, MOD_ALT);
        assert_eq!(c.move_, MOD_ALT | MOD_SHIFT);
        assert_eq!((c.pin.mods, c.pin.vk), (MOD_ALT | MOD_SHIFT, 'P' as u32));
        assert_eq!((c.overview.mods, c.overview.vk), (MOD_ALT, 0xC0));
        assert!(c.pinned_apps.is_empty() && c.rules.is_empty());
    }

    #[test]
    fn custom_values_rules_and_errors() {
        let c = parse("workspaces = 6\n[keys]\nswitch = win+alt\noverview = ctrl+alt+f2\n[pinned_apps]\nSpotify.exe\n[rules]\nslack = 2, 3\nbroken = x\n");
        assert_eq!(c.workspaces, 6);
        assert_eq!(c.switch, MOD_WIN | MOD_ALT);
        assert_eq!((c.overview.mods, c.overview.vk), (MOD_CONTROL | MOD_ALT, 0x71));
        assert_eq!(c.pinned_apps, vec!["spotify".to_string()]);
        assert_eq!((c.rules[0].app.as_str(), c.rules[0].monitor, c.rules[0].workspace), ("slack", 1, 2));
        assert_eq!(c.errors.len(), 1);
    }

    #[test]
    fn out_of_range_workspaces_is_an_error() {
        let c = parse("workspaces = 12");
        assert_eq!(c.workspaces, 4);
        assert_eq!(c.errors.len(), 1);
    }
}
