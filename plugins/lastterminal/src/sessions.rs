// Created by Peter @sHTiF Stefcek
//! LastTerminal's headless server (http://127.0.0.1:3101) lists its terminal sessions at
//! /api/sessions for requests from this PC. Only names, programs and folders are read, never output.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub const PORT: u16 = 3101;

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub id: String,
    pub name: String,
    /// The program running in it ("" at a shell prompt); for SSH, "ssh".
    pub running: String,
    /// Folder (local) or user@host (SSH).
    pub place: String,
}

/// GET a path from the local server; the body if it answered 200.
fn get(path: &str) -> Option<String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], PORT));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(800)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    // Host must be 127.0.0.1: the server lets this PC's own requests in without a key.
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\nConnection: close\r\n\r\n").ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.1 200") {
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

fn str_of<'a>(v: &'a serde_json::Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

pub fn read_sessions() -> Option<Vec<Session>> {
    parse(&get("/api/sessions")?)
}

fn parse(body: &str) -> Option<Vec<Session>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let list = v.as_array()?;
    Some(
        list.iter()
            .filter(|s| s["ended"].as_bool() != Some(true))
            .map(|s| {
                let meta = &s["meta"];
                let ssh = str_of(s, "type") == "ssh";
                let host = str_of(meta, "host");
                let place = if ssh && !host.is_empty() {
                    let user = str_of(meta, "username");
                    if user.is_empty() { host.to_string() } else { format!("{user}@{host}") }
                } else {
                    str_of(s, "cwd").to_string()
                };
                // Same naming as LastTerminal's own Sessions list.
                let name = [str_of(meta, "name"), str_of(meta, "command"), str_of(s, "title")]
                    .into_iter()
                    .find(|n| !n.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| if ssh { place.clone() } else { place.rsplit(['\\', '/']).find(|p| !p.is_empty()).unwrap_or("Local").to_string() });
                Session {
                    id: str_of(s, "id").to_string(),
                    name,
                    running: if ssh { "ssh".into() } else { str_of(s, "running").to_string() },
                    place,
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_sessions() {
        let body = r#"[
            {"id":"a1","type":"local","cwd":"D:\\code\\LastDesk","title":"","running":"vim.exe","lastLine":"","viewers":1,"meta":{"startedAt":1}},
            {"id":"b2","type":"ssh","title":"","running":"","lastLine":"","viewers":0,"meta":{"host":"nas","username":"peter"}},
            {"id":"c3","type":"local","cwd":"C:\\x","title":"","running":"","lastLine":"","viewers":0,"meta":{"name":"build"},"ended":true}
        ]"#;
        let s = super::parse(body).unwrap();
        assert_eq!(s.len(), 2, "ended sessions are left out");
        assert_eq!((s[0].name.as_str(), s[0].running.as_str()), ("LastDesk", "vim.exe"));
        assert_eq!((s[1].name.as_str(), s[1].running.as_str(), s[1].place.as_str()), ("peter@nas", "ssh", "peter@nas"));
        // Against a running LastTerminal, if there is one here.
        if let Some(live) = super::read_sessions() {
            assert!(live.iter().all(|x| !x.id.is_empty() && !x.name.is_empty()));
        }
    }
}

