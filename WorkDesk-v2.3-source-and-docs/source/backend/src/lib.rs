//! WorkDesk Chrome: Gmail IMAP/SMTP client whose window is a kiosk-mode Chrome.
mod cdp;
mod chrome;
mod commands;
mod db;
mod http;
mod imapx;
mod mail;
mod model;
mod shim;

use shim::{AppHandle, PathResolver};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{atomic::AtomicBool, Mutex},
    time::Duration,
};

fn random_token() -> String {
    let mut buf = [0u8; 24];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    } else {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(7);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((t >> ((i % 16) * 8)) & 0xff) as u8 ^ (i as u8).wrapping_mul(37);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// A second launch just brings the running WorkDesk to the front.
fn raise_running(data: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(data.join("instance.json")) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let (Some(port), Some(token)) = (v["port"].as_u64(), v["token"].as_str()) else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port as u16)), Duration::from_millis(600)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let req = format!("GET /raise?t={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut resp = String::new();
    let _ = s.read_to_string(&mut resp);
    resp.contains(" 200 ")
}

fn notify(msg: &str) {
    eprintln!("WorkDesk Chrome: {msg}");
    let _ = std::process::Command::new("notify-send").args(["WorkDesk Chrome", msg]).spawn();
}

pub fn run() {
    let paths = PathResolver::new();
    commands::migrate_old_dirs(&paths);
    let data = match paths.app_data_dir() {
        Ok(d) => d,
        Err(e) => return notify(&e),
    };
    let _ = std::fs::create_dir_all(&data);
    // Started by the Electron shell (WorkDesk Chrome v2): the shell owns the window and single-instance lock.
    let shell = std::env::var("WORKDESK_SHELL").map(|v| v == "electron").unwrap_or(false);
    if !shell && raise_running(&data) {
        return;
    }
    let db = match db::Db::open(&data.join("headers.sqlite")) {
        Ok(d) => d,
        Err(e) => return notify(&format!("cannot open the mail cache: {e}")),
    };
    let app = AppHandle::new(
        commands::AppState { db, syncing: AtomicBool::new(false), search: Mutex::new(None), draft_sess: Mutex::new(None) },
        paths,
    );
    commands::start_scheduler(app.clone());
    let token = random_token();
    let port = match http::start(app.clone(), token.clone()) {
        Ok(p) => p,
        Err(e) => return notify(&e),
    };
    let _ = std::fs::write(data.join("instance.json"), serde_json::json!({ "pid": std::process::id(), "port": port, "token": token }).to_string());
    let url = format!("http://127.0.0.1:{port}/?t={token}");
    if shell {
        println!("WD_READY {}", serde_json::json!({ "port": port, "token": token, "url": url }));
        let _ = std::io::stdout().flush();
        // The shell keeps our stdin open; when it goes away (closed or crashed) we stop too.
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        let _ = std::fs::remove_file(data.join("instance.json"));
        return;
    }

    // preferences needed before the window exists
    let prefs: serde_json::Value = app
        .path()
        .app_config_dir()
        .ok()
        .and_then(|d| std::fs::read(d.join("preferences.json")).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(serde_json::json!({}));
    let kiosk = std::env::var("WORKDESK_WINDOW").map(|v| v == "kiosk").unwrap_or(prefs["window_mode"].as_str() == Some("kiosk"));
    let browser = prefs["browser"].as_str().map(String::from);
    let ua = prefs["whatsapp_ua"].as_str().filter(|u| u.starts_with("Mozilla/5.0") && u.len() < 400 && !u.chars().any(|c| c.is_control())).map(String::from);

    if let Err(e) = chrome::start(&app, &url, kiosk, browser.as_deref(), ua) {
        notify(&format!("{e} Opening WorkDesk in your default browser instead (website tabs will be limited). Log: {}", chrome::log_path()));
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
