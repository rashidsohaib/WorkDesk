//! The local web server that WorkDesk's Chrome window talks to (127.0.0.1 only).
//!  * `GET /`            the embedded frontend (after the one-time `?t=<token>` hand-over sets a cookie)
//!  * `POST /rpc/<cmd>`  the same commands the Tauri build exposed, as JSON
//!  * `GET /events`      Server-Sent Events (sync progress, scheduled-mail results, website tab state)
use crate::shim::AppHandle;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

struct Req {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &TcpStream) -> Option<Req> {
    let mut r = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut headers = HashMap::new();
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).ok()? == 0 {
            return None;
        }
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
        if headers.len() > 100 {
            return None;
        }
    }
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > 600 << 20 {
        return None;
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).ok()?;
    Some(Req { method, path, query, headers, body })
}

fn respond(mut s: &TcpStream, status: &str, ctype: &str, extra: &[(&str, String)], body: &[u8]) {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n", body.len());
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
    let _ = s.flush();
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "wasm" => "application/wasm",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn query_param<'a>(q: &'a str, key: &str) -> Option<&'a str> {
    q.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

fn handle(stream: TcpStream, app: AppHandle, token: String, port: u16) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let Some(req) = read_request(&stream) else { return };
    let host = req.headers.get("host").map(|s| s.as_str()).unwrap_or("");
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return respond(&stream, "403 Forbidden", "text/plain", &[], b"bad host");
    }
    let authed_cookie = req.headers.get("cookie").map(|c| c.split(';').any(|p| p.trim() == format!("wd={token}"))).unwrap_or(false);
    // 1. one-time hand-over from the launch URL
    if query_param(&req.query, "t") == Some(token.as_str()) {
        if req.path == "/raise" {
            crate::chrome::raise_main();
            return respond(&stream, "200 OK", "text/plain", &[], b"ok");
        }
        return respond(
            &stream,
            "302 Found",
            "text/plain",
            &[("Location", req.path.clone()), ("Set-Cookie", format!("wd={token}; Path=/; HttpOnly; SameSite=Strict"))],
            b"",
        );
    }
    if !authed_cookie {
        return respond(&stream, "403 Forbidden", "text/plain", &[], b"forbidden");
    }
    if req.method == "POST" && req.path.starts_with("/rpc/") {
        if req.headers.get("x-wd").map(|v| v.as_str()) != Some("1") {
            return respond(&stream, "403 Forbidden", "text/plain", &[], b"forbidden");
        }
        let cmd = &req.path[5..];
        let args: serde_json::Value = if req.body.is_empty() { serde_json::json!({}) } else { serde_json::from_slice(&req.body).unwrap_or(serde_json::json!({})) };
        let out = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::commands::dispatch(&app, cmd, args))) {
            Ok(Ok(v)) => serde_json::json!({ "ok": v }),
            Ok(Err(e)) => serde_json::json!({ "err": e }),
            Err(_) => serde_json::json!({ "err": "Internal error in the WorkDesk backend" }),
        };
        return respond(&stream, "200 OK", "application/json", &[("Cache-Control", "no-store".into())], out.to_string().as_bytes());
    }
    if req.method == "GET" && req.path == "/events" {
        return sse(stream, app);
    }
    if req.method == "GET" || req.method == "HEAD" {
        let rel = req.path.trim_start_matches('/');
        let rel = if rel.is_empty() { "index.html" } else { rel };
        if rel.contains("..") {
            return respond(&stream, "400 Bad Request", "text/plain", &[], b"bad path");
        }
        if let Some((_, data)) = ASSETS.iter().find(|(p, _)| *p == rel) {
            let cache = if rel.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-store" };
            let body: &[u8] = if req.method == "HEAD" { &[] } else { data };
            return respond(&stream, "200 OK", mime(rel), &[("Cache-Control", cache.into())], body);
        }
    }
    respond(&stream, "404 Not Found", "text/plain", &[], b"not found");
}

fn sse(mut stream: TcpStream, app: AppHandle) {
    let _ = stream.set_read_timeout(None);
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\n\r\nretry: 1000\n\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let rx = app.subscribe();
    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(m) => m,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => ": keep-alive\n\n".to_string(),
            Err(_) => return,
        };
        if stream.write_all(msg.as_bytes()).is_err() || stream.flush().is_err() {
            return;
        }
    }
}

/// Start listening on a random free port; returns the port.
pub fn start(app: AppHandle, token: String) -> Result<u16, String> {
    let want: u16 = std::env::var("WORKDESK_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", want)).or_else(|_| TcpListener::bind("127.0.0.1:0")).map_err(|e| format!("cannot open the local server: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let (app, token) = (app.clone(), token.clone());
            std::thread::spawn(move || handle(s, app, token, port));
        }
    });
    Ok(port)
}
