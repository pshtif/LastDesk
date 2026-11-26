// Created by Peter @sHTiF Stefcek
//! Plugins: optional parts of LastDesk, each its own program, off until turned on in Settings.
//!
//! A plugin is a folder next to lastdesk.exe, `plugins\<id>\`, with a `plugin.json`
//! (`{"id", "name", "version", "description", "exe"}`) and its program. LastDesk starts the
//! program when it's turned on and talks to it in JSON lines over its stdin / stdout. A plugin
//! crashing takes nothing else down; LastDesk stays the small exe it is.
//!
//! LastDesk → plugin:
//! ```text
//! {"type":"init","dataDir":"…","version":"…"}           once, first
//! {"type":"click","button":"left|right|middle","monitor":{"left","top","right","bottom"}}
//! {"type":"menu","id":"…","monitor":{…}}                  a menu item was chosen
//! {"type":"hotkey","id":"…","monitor":{…}}                one of its hotkeys was pressed
//! ```
//! `monitor`: the work area of the monitor the user is on, in physical pixels.
//!
//! Plugin → LastDesk:
//! ```text
//! {"type":"item", one of: "glyph":"\uE722" | "text":"REC" | "icon":"C:\…\x.ico (or an .exe)" |
//!               "shape":"grid" (3×3 dots, the lit ones in "color"),
//!               "tooltip":"…", "color":"text|muted|accent|attention|red", "badge":"…",
//!               "dim":true (shown as off), "dot":"<color>" (a dot in the corner),
//!               "menuOnClick":true (a left click shows the menu too)}
//! {"type":"item"}                                          no bar item
//! {"type":"menu","items":[{"id","label","checked","enabled","items":[…a submenu…]} | {"separator":true}]}
//! {"type":"hotkeys","keys":[{"id","key":"ctrl+shift+s"}]}
//! {"type":"notify","title":"…","text":"…","kind":"warning"?}   a notification
//! {"type":"open","target":"https://…"}     opened by LastDesk (a browser window already showing
//!                                          it is brought forward instead)
//! ```
//! A left click goes to the plugin as "click" (or shows the menu, with menuOnClick); a right click
//! shows its menu (a click when it has none).

use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Posted (as WM_STATUS's wparam) when a plugin changed its item, menu, hotkeys, or stopped.
pub const STATUS_PLUGINS: usize = 5;
/// Hotkey ids for plugins: HK_BASE + n, n into `hotkeys()`.
pub const HK_BASE: i32 = 500;
const HK_MAX: i32 = 100;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[allow(dead_code)]
    pub version: String,
    pub description: String,
    pub exe: PathBuf,
}

#[derive(Clone, Default, Debug)]
pub struct Item {
    pub glyph: String,
    pub text: String,
    pub tooltip: String,
    pub color: String,
    pub badge: String,
    /// An icon file (.ico, or an .exe / .dll whose icon is used).
    pub icon: String,
    /// A drawn shape instead ("grid": 3×3 dots).
    pub shape: String,
    pub dim: bool,
    pub dot: String,
    pub menu_on_click: bool,
}

#[derive(Clone, Default, Debug)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub checked: bool,
    pub enabled: bool,
    pub separator: bool,
    /// A submenu.
    pub items: Vec<MenuItem>,
}

fn menu_items(v: &Value) -> Vec<MenuItem> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|m| MenuItem {
                    id: m["id"].as_str().unwrap_or("").to_string(),
                    label: m["label"].as_str().unwrap_or("").to_string(),
                    checked: m["checked"].as_bool().unwrap_or(false),
                    enabled: m["enabled"].as_bool().unwrap_or(true),
                    separator: m["separator"].as_bool().unwrap_or(false),
                    items: menu_items(&m["items"]),
                })
                .collect()
        })
        .unwrap_or_default()
}

struct Running {
    id: String,
    child: Child,
    stdin: Option<ChildStdin>,
    item: Option<Item>,
    menu: Vec<MenuItem>,
    hotkeys: Vec<(String, String)>,
}

static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());
/// Notifications asked for: (title, text, a warning).
static NOTICES: Mutex<Vec<(String, String, bool)>> = Mutex::new(Vec::new());
/// Addresses plugins asked LastDesk to open.
static OPENS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The hotkeys registered now: (plugin id, its hotkey id), by HK_BASE + index.
static REGISTERED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static MAIN: AtomicIsize = AtomicIsize::new(0);

fn post() {
    let h = MAIN.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as *mut _), crate::status::WM_STATUS, WPARAM(STATUS_PLUGINS), LPARAM(0));
        }
    }
}

/// Where plugins are installed: `plugins\` next to lastdesk.exe.
pub fn dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("plugins"))).unwrap_or_else(|| PathBuf::from("plugins"))
}

/// The plugins installed, by name.
pub fn discover() -> Vec<Manifest> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir()) else { return out };
    for e in rd.flatten() {
        let folder = e.path();
        let Ok(text) = std::fs::read_to_string(folder.join("plugin.json")) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        let id = s("id");
        let exe = folder.join(s("exe"));
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || !exe.is_file() {
            continue;
        }
        out.push(Manifest { name: if s("name").is_empty() { id.clone() } else { s("name") }, id, version: s("version"), description: s("description"), exe });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn data_dir(id: &str) -> PathBuf {
    crate::config::path().parent().map(|p| p.join("plugins").join(id)).unwrap_or_default()
}

/// Starts the plugins turned on and stops the ones turned off.
pub fn apply(enabled: &[String], main: HWND) {
    MAIN.store(main.0 as isize, Ordering::Relaxed);
    let installed = discover();
    let mut changed = false;
    {
        let mut run = RUNNING.lock().unwrap();
        run.retain_mut(|r| {
            if enabled.contains(&r.id) {
                return true;
            }
            stop(r);
            changed = true;
            false
        });
        let missing: Vec<&Manifest> = installed.iter().filter(|m| enabled.contains(&m.id) && !run.iter().any(|r| r.id == m.id)).collect();
        for m in missing {
            match start(m) {
                Ok(r) => {
                    run.push(r);
                    changed = true;
                }
                Err(e) => NOTICES.lock().unwrap().push((m.name.clone(), format!("Couldn't start: {e}"), true)),
            }
        }
    }
    if changed {
        post();
    }
}

fn start(m: &Manifest) -> std::io::Result<Running> {
    let dir = data_dir(&m.id);
    let _ = std::fs::create_dir_all(&dir);
    let mut child = Command::new(&m.exe)
        .current_dir(m.exe.parent().unwrap_or(&dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    let mut stdin = child.stdin.take();
    if let Some(w) = stdin.as_mut() {
        let _ = writeln!(w, "{}", json!({ "type": "init", "dataDir": dir.display().to_string(), "version": env!("CARGO_PKG_VERSION") }));
        let _ = w.flush();
    }
    if let Some(out) = child.stdout.take() {
        let id = m.id.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    take(&id, &v);
                }
            }
            // Gone (crashed, or stopped): its item and hotkeys go too.
            RUNNING.lock().unwrap().retain(|r| r.id != id);
            post();
        });
    }
    Ok(Running { id: m.id.clone(), child, stdin, item: None, menu: Vec::new(), hotkeys: Vec::new() })
}

fn stop(r: &mut Running) {
    r.stdin = None; // its stdin closes: a plugin may tidy up and exit
    let _ = r.child.kill();
    let _ = r.child.wait();
}

/// All plugins stopped (LastDesk exiting).
pub fn stop_all() {
    let mut run = RUNNING.lock().unwrap();
    for r in run.iter_mut() {
        stop(r);
    }
    run.clear();
}

/// A line from a plugin.
fn take(id: &str, v: &Value) {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    match v["type"].as_str() {
        Some("notify") => NOTICES.lock().unwrap().push((s("title"), s("text"), s("kind") == "warning")),
        Some("open") => OPENS.lock().unwrap().push(s("target")),
        Some(kind @ ("item" | "menu" | "hotkeys")) => {
            let mut run = RUNNING.lock().unwrap();
            let Some(r) = run.iter_mut().find(|r| r.id == id) else { return };
            match kind {
                "item" => {
                    r.item = ["glyph", "text", "icon", "shape"].iter().any(|k| v.get(*k).is_some()).then(|| Item {
                        glyph: s("glyph"),
                        text: s("text"),
                        tooltip: s("tooltip"),
                        color: s("color"),
                        badge: s("badge"),
                        icon: s("icon"),
                        shape: s("shape"),
                        dim: v["dim"].as_bool().unwrap_or(false),
                        dot: s("dot"),
                        menu_on_click: v["menuOnClick"].as_bool().unwrap_or(false),
                    });
                }
                "menu" => {
                    r.menu = menu_items(&v["items"]);
                }
                _ => {
                    r.hotkeys = v["keys"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|k| Some((k["id"].as_str()?.to_string(), k["key"].as_str()?.to_string()))).collect())
                        .unwrap_or_default();
                }
            }
        }
        _ => return,
    }
    post();
}

/// The bar items: (plugin id, item), in the order the plugins were started.
pub fn items() -> Vec<(String, Item)> {
    RUNNING.lock().unwrap().iter().filter_map(|r| r.item.clone().map(|i| (r.id.clone(), i))).collect()
}

pub fn menu(id: &str) -> Vec<MenuItem> {
    RUNNING.lock().unwrap().iter().find(|r| r.id == id).map(|r| r.menu.clone()).unwrap_or_default()
}

/// Notifications plugins asked for since last time: (title, text, a warning).
pub fn take_notices() -> Vec<(String, String, bool)> {
    std::mem::take(&mut *NOTICES.lock().unwrap())
}

/// Addresses plugins asked to open since last time.
pub fn take_opens() -> Vec<String> {
    std::mem::take(&mut *OPENS.lock().unwrap())
}

fn rect_json(r: RECT) -> Value {
    json!({ "left": r.left, "top": r.top, "right": r.right, "bottom": r.bottom })
}

/// Sends a message to a plugin (nothing if it isn't running).
fn send(id: &str, mut v: Value, monitor: RECT) {
    v["monitor"] = rect_json(monitor);
    let mut run = RUNNING.lock().unwrap();
    let Some(r) = run.iter_mut().find(|r| r.id == id) else { return };
    // The user just clicked or pressed a key for it: it may bring its own window forward (a
    // capture overlay needs the keyboard for Esc / Enter).
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(r.child.id());
    }
    if let Some(w) = r.stdin.as_mut() {
        let _ = writeln!(w, "{v}");
        let _ = w.flush();
    }
}

pub fn click(id: &str, button: &str, monitor: RECT) {
    send(id, json!({ "type": "click", "button": button }), monitor);
}

pub fn menu_chosen(id: &str, item: &str, monitor: RECT) {
    send(id, json!({ "type": "menu", "id": item }), monitor);
}

/// Registers every running plugin's hotkeys (after unregistering the old ones); returns the
/// ones another app already owns, as "key (plugin)".
pub fn register_hotkeys(main: HWND) -> Vec<String> {
    unregister_hotkeys(main);
    let mut failed = Vec::new();
    let mut reg = REGISTERED.lock().unwrap();
    let run = RUNNING.lock().unwrap();
    for r in run.iter() {
        for (hid, key) in &r.hotkeys {
            if reg.len() as i32 >= HK_MAX {
                break;
            }
            let Some(h) = crate::config::parse_hotkey(key) else {
                failed.push(format!("{key} ({}: not understood)", r.id));
                continue;
            };
            let n = HK_BASE + reg.len() as i32;
            if unsafe { RegisterHotKey(main, n, h.mods | MOD_NOREPEAT, h.vk) }.is_err() {
                failed.push(format!("{key} ({})", r.id));
                continue;
            }
            reg.push((r.id.clone(), hid.clone()));
        }
    }
    failed
}

pub fn unregister_hotkeys(main: HWND) {
    let mut reg = REGISTERED.lock().unwrap();
    for n in 0..reg.len() as i32 {
        unsafe {
            let _ = UnregisterHotKey(main, HK_BASE + n);
        }
    }
    reg.clear();
}

/// The hotkeys plugins want now, to see whether they changed: "plugin:id=key" each.
pub fn hotkey_signature() -> String {
    RUNNING.lock().unwrap().iter().flat_map(|r| r.hotkeys.iter().map(move |(h, k)| format!("{}:{h}={k}", r.id))).collect::<Vec<_>>().join(";")
}

/// A WM_HOTKEY id that's a plugin's: sent to it. False when it isn't one.
pub fn hotkey(n: i32, monitor: RECT) -> bool {
    if !(HK_BASE..HK_BASE + HK_MAX).contains(&n) {
        return false;
    }
    let target = REGISTERED.lock().unwrap().get((n - HK_BASE) as usize).cloned();
    if let Some((id, hid)) = target {
        send(&id, json!({ "type": "hotkey", "id": hid }), monitor);
    }
    true
}

/// Plugins changed the hotkeys they want since this was last asked (they're registered again).
pub fn hotkeys_changed() -> bool {
    static LAST: Mutex<String> = Mutex::new(String::new());
    let now = hotkey_signature();
    let mut last = LAST.lock().unwrap();
    if *last == now {
        return false;
    }
    *last = now;
    true
}
