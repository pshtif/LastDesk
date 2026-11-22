// Created by Peter @sHTiF Stefcek
//! Memory meter for the bar (optional, Settings > Bar): how much RAM is in use, and on hover
//! which apps use the most.

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

/// (used bytes, total bytes) of physical memory.
pub fn usage() -> Option<(u64, u64)> {
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok()?;
    Some((m.ullTotalPhys - m.ullAvailPhys, m.ullTotalPhys))
}

/// Percent of RAM in use.
pub fn percent() -> Option<u32> {
    let (used, total) = usage()?;
    (total > 0).then(|| (used * 100 / total) as u32)
}

/// The `n` apps using the most memory (private bytes, all processes of an app added up),
/// largest first. Processes we can't open (system, other users) are skipped.
pub fn top(n: usize) -> Vec<(String, u64)> {
    let mut by_app: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Vec::new() };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            if let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID) {
                let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
                if GetProcessMemoryInfo(p, &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb).is_ok() {
                    let end = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                    let name = String::from_utf16_lossy(&e.szExeFile[..end]);
                    let name = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(&name).to_string();
                    *by_app.entry(name).or_insert(0) += c.PrivateUsage as u64;
                }
                let _ = CloseHandle(p);
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let mut v: Vec<(String, u64)> = by_app.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(n);
    v
}

pub fn gb(bytes: u64) -> String {
    let g = bytes as f64 / (1u64 << 30) as f64;
    if g >= 10.0 { format!("{g:.0} GB") } else if g >= 1.0 { format!("{g:.1} GB") } else { format!("{:.0} MB", bytes as f64 / (1u64 << 20) as f64) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_memory() {
        let p = super::percent().expect("memory status");
        assert!((1..=100).contains(&p));
        let top = super::top(5);
        assert!(!top.is_empty(), "should see at least our own process");
        assert!(top.windows(2).all(|w| w[0].1 >= w[1].1), "largest first");
        assert!(super::summary().starts_with("Memory\t"));
    }
}

/// Tooltip text.
pub fn summary() -> String {
    let Some((used, total)) = usage() else { return "Memory".into() };
    let g = |b: u64| b as f64 / (1u64 << 30) as f64;
    let mut s = format!("Memory\t{:.1}/{:.1} GB", g(used), g(total));
    // "name<TAB>amount": the tooltip draws these as a table (name left, amount right).
    for (name, b) in top(5) {
        s.push_str(&format!("\n{name}\t{}", gb(b)));
    }
    s
}
