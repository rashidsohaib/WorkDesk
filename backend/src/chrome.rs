//! Runs WorkDesk inside Chrome (kiosk / app window) and drives it over the DevTools protocol.
//!
//! * The WorkDesk UI is one Chrome tab that shows the local frontend.
//! * Every website tab (WhatsApp, Meet, bookmarks, extensions pages …) is a real Chrome tab in the
//!   same window, so the sites see a genuine Chrome: WebRTC, camera, microphone, calls all work.
//! * Each website tab gets a hover-reveal bar at the top edge (see `assets/overlay.js`) to go back to WorkDesk.
use crate::{
    cdp::{Cdp, Event},
    shim::AppHandle,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

const OVERLAY_JS: &str = include_str!("../assets/overlay.js");
pub const PROBE_JS: &str = r#"(function(){try{var d={ua:navigator.userAgent,secure:window.isSecureContext,rtcPeerConnection:typeof RTCPeerConnection,webkitRTC:typeof webkitRTCPeerConnection,mediaDevices:!!(navigator.mediaDevices&&navigator.mediaDevices.getUserMedia),getCapabilities:(typeof RTCRtpSender!=='undefined')&&!!RTCRtpSender.getCapabilities,audioWorklet:typeof AudioWorklet,clientHints:!!navigator.userAgentData,chromeObj:!!window.chrome,mediaRecorder:typeof MediaRecorder};return JSON.stringify(d)}catch(e){return JSON.stringify({error:String(e)})}})()"#;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SiteState {
    pub url: String,
    pub title: String,
    pub can_back: bool,
    pub can_forward: bool,
    pub loading: bool,
}

pub fn clean_label(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(60).collect()
}

pub fn check_url(url: &str) -> Result<String, String> {
    let u = url.trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        return Err("Only HTTP and HTTPS websites can be opened".into());
    }
    Ok(u.to_string())
}

/// Addresses that may be opened by "Open in Chrome" (Chrome's own pages and extension pages as well).
pub fn check_chrome_url(url: &str) -> Result<String, String> {
    let u = url.trim();
    let ok = u.starts_with("chrome://") || u.starts_with("chrome-extension://") || u.starts_with("https://") || u.starts_with("http://");
    if !ok || u.len() > 2000 || u.chars().any(|c| c.is_control() || c == ' ') {
        return Err("This address cannot be opened in Chrome".into());
    }
    Ok(u.to_string())
}

struct Site {
    target: String,
    session: String,
    state: SiteState,
}

pub struct Chrome {
    cdp: Arc<Cdp>,
    app: AppHandle,
    main: Mutex<String>,
    sites: Mutex<HashMap<String, Site>>,
    n: AtomicU32,
    kiosk: bool,
    ua: Mutex<Option<String>>,
    child: Mutex<Option<Child>>,
    profile: PathBuf,
    main_session: Mutex<String>,
    embedded: Mutex<HashSet<String>>,
    frames: Mutex<HashMap<String, String>>,
    frame_sessions: Mutex<HashMap<String, String>>,
    emb: Mutex<HashMap<String, SiteState>>,
    hist: Mutex<HashMap<String, (Vec<String>, usize)>>,
}

static CHROME: OnceLock<Arc<Chrome>> = OnceLock::new();

pub fn get() -> Result<&'static Arc<Chrome>, String> {
    CHROME.get().ok_or_else(|| {
        let e = start_error();
        format!("WorkDesk could not start or attach to its Chrome window, so website tabs are unavailable. Reason: {}. Details are in {}", if e.is_empty() { "Chrome has not finished starting yet".to_string() } else { e }, log_path())
    })
}

pub fn is_kiosk() -> bool {
    CHROME.get().map(|c| c.kiosk).unwrap_or(false)
}

pub fn raise_main() {
    if let Ok(c) = get() {
        let m = c.main.lock().map(|m| m.clone()).unwrap_or_default();
        let _ = c.cdp.call("Target.activateTarget", json!({ "targetId": m }), None);
    }
}

pub fn shutdown() -> ! {
    if let Ok(c) = get() {
        let _ = c.cdp.call("Browser.close", json!({}), None);
        std::thread::sleep(Duration::from_millis(600));
        if let Ok(mut g) = c.child.lock() {
            if let Some(ch) = g.as_mut() {
                let _ = ch.kill();
            }
        }
    }
    std::process::exit(0);
}

/// Where the dedicated WorkDesk profile lives for this browser (sandboxed browsers can only write in their own folders).
fn profile_dir(cmd: &[String], data: &std::path::Path) -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    if let Some(id) = flatpak_id(cmd) {
        return home.join(".var/app").join(id).join("workdesk-chrome-profile");
    }
    if cmd[0].starts_with("/snap/") {
        let name = if cmd[0].contains("chromium") { "chromium" } else { "chrome" };
        return home.join("snap").join(name).join("common").join("workdesk-chrome-profile");
    }
    data.join("chrome-profile")
}

/// The Flatpak application id when this command starts a Flatpak app (`flatpak run <id>` or its exports wrapper).
fn flatpak_id(cmd: &[String]) -> Option<String> {
    if cmd[0].ends_with("flatpak") {
        return cmd.last().cloned();
    }
    if cmd[0].contains("/flatpak/exports/bin/") {
        return cmd[0].rsplit('/').next().map(String::from);
    }
    None
}

fn read_active_port(profile: &std::path::Path) -> Option<(u16, String)> {
    let t = std::fs::read_to_string(profile.join("DevToolsActivePort")).ok()?;
    let mut l = t.lines();
    let port: u16 = l.next()?.trim().parse().ok()?;
    let path = l.next()?.trim().to_string();
    Some((port, path))
}

static START_ERR: Mutex<String> = Mutex::new(String::new());
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn log(msg: &str) {
    eprintln!("{msg}");
    if let Some(p) = LOG_PATH.lock().ok().and_then(|g| g.clone()) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let _ = writeln!(f, "[{secs}] {msg}");
        }
    }
}

pub fn log_path() -> String {
    LOG_PATH.lock().ok().and_then(|g| g.clone()).map(|p| p.display().to_string()).unwrap_or_default()
}

pub fn start_error() -> String {
    START_ERR.lock().map(|e| e.clone()).unwrap_or_default()
}

/// Start (or re-attach to) Chrome showing `app_url`. `kiosk` = full-screen kiosk, otherwise a normal app window.
pub fn start(app: &AppHandle, app_url: &str, kiosk: bool, browser: Option<&str>, ua: Option<String>) -> Result<(), String> {
    let data = app.path().app_data_dir()?;
    let logp = data.join("workdesk-chrome.log");
    if std::fs::metadata(&logp).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&logp);
    }
    *LOG_PATH.lock().unwrap() = Some(logp.clone());
    let r = start_inner(app, app_url, kiosk, browser, ua, &data, &logp);
    if let Err(e) = &r {
        log(&format!("START FAILED: {e}"));
        if let Ok(mut g) = START_ERR.lock() {
            *g = e.clone();
        }
    }
    r
}

fn start_inner(app: &AppHandle, app_url: &str, kiosk: bool, browser: Option<&str>, ua: Option<String>, data: &std::path::Path, logp: &std::path::Path) -> Result<(), String> {
    let cands = crate::commands::chrome_candidates(browser, true);
    log(&format!("candidates: {:?}", cands));
    if cands.is_empty() {
        return Err("No Chromium-based browser (Google Chrome, Chromium, Brave, Edge …) was found on this computer.".into());
    }
    let mut failures: Vec<String> = vec![];
    let mut found: Option<(Arc<Cdp>, mpsc::Receiver<Event>, Option<Child>, PathBuf, bool)> = None;
    let mut tried: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    'cand: for c in &cands {
        let profile = profile_dir(c, data);
        if !tried.insert(profile.clone()) {
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(&profile) {
            failures.push(format!("{}: cannot create profile folder {}: {e}", c[0], profile.display()));
            continue;
        }
        // A Chrome left over from a crashed session still owns the profile: attach to it instead of fighting for the lock.
        if let Some((p, path)) = read_active_port(&profile) {
            if let Ok((cdp, ev)) = Cdp::connect(p, &path) {
                log(&format!("attached to the Chrome already running on port {p}"));
                found = Some((cdp, ev, None, profile, true));
                break 'cand;
            }
        }
        let _ = std::fs::remove_file(profile.join("DevToolsActivePort"));
        let mut cmd = Command::new(&c[0]);
        cmd.args(&c[1..]);
        cmd.arg(format!("--user-data-dir={}", profile.display()));
        cmd.arg("--remote-debugging-port=0");
        for f in [
            "--no-first-run",
            "--no-default-browser-check",
            "--password-store=basic",
            "--disable-session-crashed-bubble",
            "--hide-crash-restore-bubble",
            "--noerrdialogs",
            "--autoplay-policy=no-user-gesture-required",
            "--disable-background-timer-throttling",
            "--disable-renderer-backgrounding",
            "--disable-backgrounding-occluded-windows",
            "--disable-features=Translate,ChromeWhatsNewUI,PrivacySandboxSettings4",
            "--enable-features=WebRTCPipeWireCapturer",
        ] {
            cmd.arg(f);
        }
        if let Ok(extra) = std::env::var("WORKDESK_CHROME_ARGS") {
            cmd.args(extra.split_whitespace());
        }
        if kiosk {
            cmd.arg("--kiosk");
        }
        // A normal browser window (shown without any browser frame in kiosk mode): website tabs become tabs of THIS window.
        cmd.arg("--new-window");
        cmd.arg(app_url);
        crate::commands::clean_env(&mut cmd);
        let errlog = std::fs::OpenOptions::new().create(true).append(true).open(logp).ok();
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        match errlog {
            Some(f) => cmd.stderr(Stdio::from(f)),
            None => cmd.stderr(Stdio::null()),
        };
        log(&format!("starting {:?} with profile {}", c, profile.display()));
        match cmd.spawn() {
            Ok(mut ch) => {
                let t0 = Instant::now();
                let mut why = String::new();
                while t0.elapsed() < Duration::from_secs(45) {
                    if let Ok(Some(st)) = ch.try_wait() {
                        why = format!("{} exited right after starting ({st})", c.join(" "));
                        break;
                    }
                    if let Some((p, path)) = read_active_port(&profile) {
                        match Cdp::connect(p, &path) {
                            Ok((cdp, ev)) => {
                                log(&format!("connected to DevTools on port {p}"));
                                found = Some((cdp, ev, Some(ch), profile, false));
                                break 'cand;
                            }
                            Err(e) => why = format!("DevTools connection failed: {e}"),
                        }
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                if why.is_empty() {
                    why = format!("{} did not open its DevTools port within 45 s", c.join(" "));
                }
                let _ = ch.kill();
                if let Some(id) = flatpak_id(c) {
                    let _ = Command::new("flatpak").args(["kill", &id]).output();
                }
                log(&why);
                failures.push(why);
            }
            Err(e) => failures.push(format!("{}: {e}", c[0])),
        }
    }
    let Some((cdp, events, child, profile, reused)) = found else {
        return Err(format!("Could not start Chrome. {}", failures.join(" | ")));
    };
    let chrome = Arc::new(Chrome {
        cdp: cdp.clone(),
        app: app.clone(),
        main: Mutex::new(String::new()),
        sites: Mutex::new(HashMap::new()),
        n: AtomicU32::new(0),
        kiosk,
        ua: Mutex::new(ua),
        child: Mutex::new(child),
        profile,
        main_session: Mutex::new(String::new()),
        embedded: Mutex::new(HashSet::new()),
        frames: Mutex::new(HashMap::new()),
        frame_sessions: Mutex::new(HashMap::new()),
        emb: Mutex::new(HashMap::new()),
        hist: Mutex::new(HashMap::new()),
    });
    let _ = chrome.profile.display();
    cdp.call("Target.setDiscoverTargets", json!({ "discover": true }), None).map_err(|e| format!("Chrome DevTools: {e}"))?;
    if reused {
        let r = cdp.call("Target.createTarget", json!({ "url": app_url }), None)?;
        if let Some(id) = r["targetId"].as_str() {
            *chrome.main.lock().unwrap() = id.to_string();
        }
    }
    let t0 = Instant::now();
    let base = app_url.split('?').next().unwrap_or(app_url).to_string();
    while chrome.main.lock().unwrap().is_empty() {
        let list = cdp.call("Target.getTargets", json!({}), None)?;
        if let Some(t) = list["targetInfos"].as_array().and_then(|a| a.iter().find(|t| t["type"] == "page" && t["url"].as_str().map(|u| u.starts_with(&base)).unwrap_or(false))) {
            *chrome.main.lock().unwrap() = t["targetId"].as_str().unwrap_or("").to_string();
        } else if t0.elapsed() > Duration::from_secs(25) {
            // Chrome is up but did not open our page by itself (some builds ignore --app with a kiosk): open it ourselves.
            if t0.elapsed() < Duration::from_secs(26) {
                if let Ok(r) = cdp.call("Target.createTarget", json!({ "url": app_url }), None) {
                    if let Some(id) = r["targetId"].as_str() {
                        *chrome.main.lock().unwrap() = id.to_string();
                        break;
                    }
                }
            }
            if t0.elapsed() > Duration::from_secs(40) {
                return Err("Chrome started but the WorkDesk page did not open in it".into());
            }
            std::thread::sleep(Duration::from_millis(150));
        } else {
            std::thread::sleep(Duration::from_millis(150));
        }
    }
    chrome.setup_embedding(app_url);
    let _ = CHROME.set(chrome.clone());
    log("WorkDesk Chrome window ready");
    let c2 = chrome.clone();
    std::thread::spawn(move || {
        c2.event_loop(events);
        // The DevTools connection ended: Chrome was closed, so WorkDesk ends with it.
        std::process::exit(0);
    });
    if let Some(ch) = chrome.child.lock().unwrap().take() {
        let mut ch = ch;
        std::thread::spawn(move || {
            let _ = ch.wait();
            std::process::exit(0);
        });
    }
    Ok(())
}

const FETCH_PATTERNS: &str = r#"[{"urlPattern":"*","resourceType":"Document","requestStage":"Request"},{"urlPattern":"*","resourceType":"Document","requestStage":"Response"}]"#;

/// Remove the `frame-ancestors` directive from a Content-Security-Policy header value.
fn strip_frame_ancestors(v: &str) -> String {
    v.split(',')
        .map(|policy| policy.split(';').filter(|d| !d.trim_start().to_lowercase().starts_with("frame-ancestors")).collect::<Vec<_>>().join(";"))
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

fn origin_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?;
    if rest.0 != "http" && rest.0 != "https" {
        return None;
    }
    let host = rest.1.split(['/', '?', '#']).next()?;
    Some(format!("{}://{}", rest.0, host))
}

impl Chrome {
    // ------------------------------------------------------------ websites embedded in the WorkDesk page

    /// Let the WorkDesk page host websites in iframes: strip the headers that forbid framing, resume attached frames.
    fn setup_embedding(&self, app_url: &str) {
        let main = self.main_id();
        let Ok(a) = self.cdp.call("Target.attachToTarget", json!({ "targetId": main, "flatten": true }), None) else {
            log("could not attach to the WorkDesk page; embedded websites unavailable");
            return;
        };
        let Some(sess) = a["sessionId"].as_str().map(String::from) else { return };
        *self.main_session.lock().unwrap() = sess.clone();
        let s = Some(sess.as_str());
        let _ = self.cdp.call("Page.enable", json!({}), s);
        let patterns: Value = serde_json::from_str(FETCH_PATTERNS).unwrap();
        if let Err(e) = self.cdp.call("Fetch.enable", json!({ "patterns": patterns }), s) {
            log(&format!("Fetch.enable failed: {e}"));
        }
        let _ = self.cdp.call("Target.setAutoAttach", json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }), s);
        if let Some(origin) = origin_of(app_url) {
            // camera / microphone / screen capture are delegated from the WorkDesk page to the embedded sites
            let full = ["audioCapture", "videoCapture", "notifications", "clipboardReadWrite", "clipboardSanitizedWrite", "displayCapture", "wakeLockScreen"];
            if self.cdp.call("Browser.grantPermissions", json!({ "origin": origin, "permissions": full }), None).is_err() {
                let _ = self.cdp.call("Browser.grantPermissions", json!({ "origin": origin, "permissions": ["audioCapture", "videoCapture", "notifications"] }), None);
            }
        }
    }

    fn is_embedded(&self, label: &str) -> bool {
        self.embedded.lock().map(|e| e.contains(label)).unwrap_or(false)
    }

    fn collect_frames(node: &Value, out: &mut HashMap<String, String>) {
        let f = &node["frame"];
        if let (Some(id), Some(name)) = (f["id"].as_str(), f["name"].as_str()) {
            if let Some(l) = name.strip_prefix("wd-") {
                out.insert(id.to_string(), l.to_string());
            }
        }
        for c in node["childFrames"].as_array().into_iter().flatten() {
            Self::collect_frames(c, out);
        }
    }

    fn refresh_frames(&self) {
        let sess = self.main_session.lock().map(|s| s.clone()).unwrap_or_default();
        if let Ok(t) = self.cdp.call("Page.getFrameTree", json!({}), Some(&sess)) {
            let mut m = HashMap::new();
            Self::collect_frames(&t["frameTree"], &mut m);
            if let Ok(mut f) = self.frames.lock() {
                f.extend(m);
            }
        }
    }

    fn label_of_frame(&self, frame: &str) -> Option<String> {
        if let Some(l) = self.frames.lock().ok().and_then(|f| f.get(frame).cloned()) {
            return Some(l);
        }
        self.refresh_frames();
        self.frames.lock().ok().and_then(|f| f.get(frame).cloned())
    }

    fn frame_of(&self, label: &str) -> Option<String> {
        let find = || self.frames.lock().ok().and_then(|f| f.iter().find(|(_, l)| l.as_str() == label).map(|(id, _)| id.clone()));
        find().or_else(|| {
            self.refresh_frames();
            find()
        })
    }

    fn eval_frame(&self, label: &str, expr: &str) -> Result<Value, String> {
        let fid = self.frame_of(label).ok_or("The website is still loading")?;
        let main_sess = self.main_session.lock().map(|s| s.clone()).unwrap_or_default();
        let r = if let Some(sess) = self.frame_sessions.lock().ok().and_then(|f| f.get(&fid).cloned()) {
            self.cdp.call("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true }), Some(&sess))?
        } else {
            let w = self.cdp.call("Page.createIsolatedWorld", json!({ "frameId": fid, "worldName": "workdesk" }), Some(&main_sess))?;
            self.cdp.call("Runtime.evaluate", json!({ "expression": expr, "contextId": w["executionContextId"], "returnByValue": true }), Some(&main_sess))?
        };
        Ok(r["result"]["value"].clone())
    }

    fn emit_emb(&self, label: &str) {
        let st = self.emb.lock().ok().and_then(|s| s.get(label).cloned());
        if let Some(st) = st {
            let mut v = serde_json::to_value(&st).unwrap_or(json!({}));
            v["label"] = json!(label);
            let _ = self.app.emit("site-state", v);
        }
    }

    fn refresh_embedded(&self, label: &str) {
        let expr = r#"JSON.stringify({u:location.href,t:document.title,b:!!(window.navigation&&navigation.canGoBack),f:!!(window.navigation&&navigation.canForward)})"#;
        if let Ok(v) = self.eval_frame(label, expr) {
            if let Some(j) = v.as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()) {
                if let Ok(mut m) = self.emb.lock() {
                    let x = m.entry(label.to_string()).or_default();
                    if let Some(u) = j["u"].as_str().filter(|u| *u != "about:blank") {
                        x.url = u.to_string();
                    }
                    if let Some(t) = j["t"].as_str().filter(|t| !t.is_empty()) {
                        x.title = t.to_string();
                    }
                    // Our own history: Chrome serves history navigations from a cache that still has the original
                    // X-Frame-Options headers, so Back / Forward are done as ordinary navigations to the previous address.
                    let cur = x.url.clone();
                    if !cur.starts_with("chrome-error:") && !cur.is_empty() {
                        if let Ok(mut hm) = self.hist.lock() {
                            let h = hm.entry(label.to_string()).or_insert((vec![], 0));
                            if h.0.is_empty() {
                                h.0.push(cur.clone());
                                h.1 = 0;
                            } else if h.0[h.1] != cur {
                                if h.1 > 0 && h.0[h.1 - 1] == cur {
                                    h.1 -= 1;
                                } else if h.1 + 1 < h.0.len() && h.0[h.1 + 1] == cur {
                                    h.1 += 1;
                                } else {
                                    let keep = h.1 + 1;
                                    h.0.truncate(keep);
                                    h.0.push(cur.clone());
                                    h.1 = h.0.len() - 1;
                                }
                            }
                            x.can_back = h.1 > 0;
                            x.can_forward = h.1 + 1 < h.0.len();
                        }
                    }
                }
            }
        }
        self.emit_emb(label);
    }

    fn on_paused(&self, ev: &Event) {
        let sess = ev.session.as_deref();
        let p = &ev.params;
        let rid = p["requestId"].clone();
        let frame = p["frameId"].as_str().unwrap_or("");
        if std::env::var("WORKDESK_DEBUG").is_ok() {
            log(&format!("paused frame={frame} code={:?} err={:?} hdrs={} type={:?} url={}", p["responseStatusCode"], p["responseErrorReason"], p["responseHeaders"].is_array(), p["resourceType"], p["request"]["url"]));
        }
        if p["responseStatusCode"].is_null() && p["responseErrorReason"].is_null() {
            // request stage: nothing to change, let it go to the network
            // Framed documents must always come from the network: a cached copy would still carry the original
            // X-Frame-Options / frame-ancestors headers (e.g. when going Back), which Chrome would enforce.
            if frame != self.main_id() {
                if let Some(h) = p["request"]["headers"].as_object() {
                    let mut hs: Vec<Value> = h.iter().filter(|(k, _)| !matches!(k.to_lowercase().as_str(), "cache-control" | "pragma")).map(|(k, v)| json!({ "name": k, "value": v })).collect();
                    hs.push(json!({ "name": "Cache-Control", "value": "no-cache" }));
                    if self.cdp.call("Fetch.continueRequest", json!({ "requestId": rid, "headers": hs }), sess).is_ok() {
                        return;
                    }
                }
            }
            let _ = self.cdp.call("Fetch.continueRequest", json!({ "requestId": rid }), sess);
            return;
        }
        if let (Some(h), Some(code)) = (p["responseHeaders"].as_array(), p["responseStatusCode"].as_i64()) {
            if frame != self.main_id() && !(300..400).contains(&code) {
                let mut out: Vec<Value> = vec![];
                let mut changed = false;
                for x in h {
                    let name = x["name"].as_str().unwrap_or("").to_lowercase();
                    match name.as_str() {
                        "x-frame-options" => changed = true,
                        "content-security-policy" | "content-security-policy-report-only" => {
                            let v = x["value"].as_str().unwrap_or("");
                            let nv = strip_frame_ancestors(v);
                            if nv != v {
                                changed = true;
                            }
                            if !nv.trim().is_empty() {
                                out.push(json!({ "name": x["name"], "value": nv }));
                            }
                        }
                        _ => out.push(x.clone()),
                    }
                }
                if changed {
                    // Chrome enforces X-Frame-Options / frame-ancestors on the original response, so serve a rewritten copy instead.
                    match self.cdp.call("Fetch.getResponseBody", json!({ "requestId": rid }), sess) {
                        Ok(b) => {
                            let body = b["body"].as_str().unwrap_or("").to_string();
                            let body = if b["base64Encoded"].as_bool().unwrap_or(false) { body } else { base64::Engine::encode(&base64::engine::general_purpose::STANDARD, body.as_bytes()) };
                            let out: Vec<Value> = out.into_iter().filter(|x| !matches!(x["name"].as_str().map(|n| n.to_lowercase()).as_deref(), Some("content-length") | Some("content-encoding") | Some("transfer-encoding"))).collect();
                            match self.cdp.call("Fetch.fulfillRequest", json!({ "requestId": rid, "responseCode": code, "responseHeaders": out, "body": body }), sess) {
                                Ok(_) => return,
                                Err(e) => log(&format!("fulfillRequest failed: {e}")),
                            }
                        }
                        Err(e) => log(&format!("getResponseBody failed: {e}")),
                    }
                }
            }
        }
        if self.cdp.call("Fetch.continueResponse", json!({ "requestId": rid }), sess).is_err() {
            let _ = self.cdp.call("Fetch.continueRequest", json!({ "requestId": rid }), sess);
        }
    }

    fn on_attached(&self, ev: &Event) {
        let p = &ev.params;
        let (Some(sid), Some(tid)) = (p["sessionId"].as_str(), p["targetInfo"]["targetId"].as_str()) else { return };
        if p["targetInfo"]["type"] == "iframe" {
            if let Ok(mut f) = self.frame_sessions.lock() {
                f.insert(tid.to_string(), sid.to_string());
            }
            let patterns: Value = serde_json::from_str(FETCH_PATTERNS).unwrap();
            let _ = self.cdp.call("Page.enable", json!({}), Some(sid));
            let _ = self.cdp.call("Fetch.enable", json!({ "patterns": patterns }), Some(sid));
            let _ = self.cdp.call("Target.setAutoAttach", json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }), Some(sid));
        }
        if p["waitingForDebugger"].as_bool().unwrap_or(false) {
            let _ = self.cdp.call("Runtime.runIfWaitingForDebugger", json!({}), Some(sid));
        }
    }

    fn is_frame_session(&self, sess: &str) -> bool {
        self.main_session.lock().map(|m| *m == sess).unwrap_or(false) || self.frame_sessions.lock().map(|f| f.values().any(|v| v == sess)).unwrap_or(false)
    }

    fn on_frame_event(&self, ev: &Event) {
        let p = &ev.params;
        let fid = match ev.method.as_str() {
            "Page.frameNavigated" => p["frame"]["id"].as_str(),
            _ => p["frameId"].as_str(),
        };
        let Some(fid) = fid else { return };
        if fid == self.main_id() {
            return;
        }
        let Some(label) = self.label_of_frame(fid) else { return };
        match ev.method.as_str() {
            "Page.frameStartedLoading" | "Page.frameStoppedLoading" => {
                let loading = ev.method == "Page.frameStartedLoading";
                if let Ok(mut m) = self.emb.lock() {
                    m.entry(label.clone()).or_default().loading = loading;
                }
                if loading {
                    self.emit_emb(&label);
                } else {
                    self.refresh_embedded(&label);
                }
            }
            _ => self.refresh_embedded(&label),
        }
    }

    fn main_id(&self) -> String {
        self.main.lock().map(|m| m.clone()).unwrap_or_default()
    }


    fn label_for_session(&self, session: &str) -> Option<String> {
        self.sites.lock().ok()?.iter().find(|(_, s)| s.session == session).map(|(l, _)| l.clone())
    }
    fn label_for_target(&self, target: &str) -> Option<String> {
        self.sites.lock().ok()?.iter().find(|(_, s)| s.target == target).map(|(l, _)| l.clone())
    }

    fn grant(&self, url: &str) {
        let Some(origin) = origin_of(url) else { return };
        let full = ["audioCapture", "videoCapture", "notifications", "clipboardReadWrite", "clipboardSanitizedWrite", "displayCapture", "wakeLockScreen"];
        let r = self.cdp.call("Browser.grantPermissions", json!({ "origin": origin, "permissions": full }), None);
        if r.is_err() {
            let _ = self.cdp.call("Browser.grantPermissions", json!({ "origin": origin, "permissions": ["audioCapture", "videoCapture", "notifications"] }), None);
        }
    }

    /// Create the Chrome tab for a site and prepare it (overlay, permissions, UA). Returns the label.
    fn create_tab(&self, label: &str, url: &str, announce: bool) -> Result<(), String> {
        let (target, session);
        {
            // hold the registry lock until the tab is registered so the event worker does not mistake it for a pop-up
            let mut sites = self.sites.lock().map_err(|_| "poisoned")?;
            if sites.contains_key(label) {
                return Ok(());
            }
            let r = self.cdp.call("Target.createTarget", json!({ "url": "about:blank" }), None)?;
            target = r["targetId"].as_str().ok_or("Chrome did not return a tab id")?.to_string();
            let a = self.cdp.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None)?;
            session = a["sessionId"].as_str().ok_or("could not attach to the tab")?.to_string();
            sites.insert(label.to_string(), Site { target: target.clone(), session: session.clone(), state: SiteState { url: url.to_string(), title: String::new(), loading: true, ..Default::default() } });
        }
        self.prepare(&session)?;
        self.grant(url);
        self.cdp.call("Target.activateTarget", json!({ "targetId": target }), None)?;
        self.cdp.call("Page.navigate", json!({ "url": url }), Some(&session))?;
        let win = |t: &str| self.cdp.call("Browser.getWindowForTarget", json!({ "targetId": t }), None).ok().and_then(|w| w["windowId"].as_i64());
        if win(&target) != win(&self.main_id()) {
            log(&format!("WARNING: tab {label} opened in a different Chrome window than WorkDesk"));
        }
        if announce {
            let _ = self.app.emit("site-new", json!({ "label": label, "url": url, "title": "" }));
        }
        self.push_overlays();
        Ok(())
    }

    fn prepare(&self, session: &str) -> Result<(), String> {
        let s = Some(session);
        self.cdp.call("Page.enable", json!({}), s)?;
        self.cdp.call("Runtime.enable", json!({}), s)?;
        self.cdp.call("Runtime.addBinding", json!({ "name": "__wdsite" }), s)?;
        if self.kiosk {
            // no browser tab strip in kiosk mode: give each website tab a hover bar to get back to WorkDesk
            self.cdp.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": OVERLAY_JS }), s)?;
        }
        if let Some(ua) = self.ua.lock().ok().and_then(|u| u.clone()) {
            let _ = self.cdp.call("Emulation.setUserAgentOverride", json!({ "userAgent": ua }), s);
        }
        Ok(())
    }

    fn overlay_json(&self, active: &str) -> Value {
        let sites = self.sites.lock().unwrap();
        let mut labels: Vec<&String> = sites.keys().collect();
        labels.sort();
        let tabs: Vec<Value> = labels.iter().map(|l| json!({ "label": l, "title": sites[*l].state.title })).collect();
        let st = sites.get(active).map(|s| s.state.clone()).unwrap_or_default();
        json!({ "tabs": tabs, "active": active, "url": st.url, "canBack": st.can_back, "canForward": st.can_forward, "loading": st.loading })
    }

    fn push_overlay(&self, label: &str) {
        let (session,) = match self.sites.lock().ok().and_then(|s| s.get(label).map(|x| (x.session.clone(),))) {
            Some(x) => x,
            None => return,
        };
        let j = self.overlay_json(label);
        let expr = format!("window.__wdBar&&window.__wdBar.set({})", j);
        let _ = self.cdp.call("Runtime.evaluate", json!({ "expression": expr }), Some(&session));
    }

    fn push_overlays(&self) {
        let labels: Vec<String> = self.sites.lock().map(|s| s.keys().cloned().collect()).unwrap_or_default();
        for l in labels {
            self.push_overlay(&l);
        }
    }

    fn emit_state(&self, label: &str) {
        let st = self.sites.lock().ok().and_then(|s| s.get(label).map(|x| x.state.clone()));
        if let Some(st) = st {
            let mut v = serde_json::to_value(&st).unwrap_or(json!({}));
            v["label"] = json!(label);
            let _ = self.app.emit("site-state", v);
            self.push_overlay(label);
        }
    }

    fn refresh_history(&self, label: &str) {
        let Some(session) = self.sites.lock().ok().and_then(|s| s.get(label).map(|x| x.session.clone())) else { return };
        if let Ok(h) = self.cdp.call("Page.getNavigationHistory", json!({}), Some(&session)) {
            let idx = h["currentIndex"].as_i64().unwrap_or(0);
            let n = h["entries"].as_array().map(|a| a.len() as i64).unwrap_or(0);
            let url = h["entries"][idx as usize]["url"].as_str().map(String::from);
            let title = self
                .cdp
                .call("Runtime.evaluate", json!({ "expression": "document.title", "returnByValue": true }), Some(&session))
                .ok()
                .and_then(|r| r["result"]["value"].as_str().map(String::from))
                .filter(|t| !t.is_empty());
            if let Ok(mut s) = self.sites.lock() {
                if let Some(x) = s.get_mut(label) {
                    x.state.can_back = idx > 0;
                    x.state.can_forward = idx < n - 1;
                    if let Some(u) = url.filter(|u| u != "about:blank") {
                        x.state.url = u;
                    }
                    if let Some(t) = title {
                        x.state.title = t;
                    }
                }
            }
        }
    }

    fn event_loop(&self, events: mpsc::Receiver<Event>) {
        for ev in events {
            match ev.method.as_str() {
                "Target.targetCreated" | "Target.targetInfoChanged" => {
                    let i = &ev.params["targetInfo"];
                    let (id, ty) = (i["targetId"].as_str().unwrap_or(""), i["type"].as_str().unwrap_or(""));
                    if ty != "page" || id == self.main_id() {
                        continue;
                    }
                    if let Some(label) = self.label_for_target(id) {
                        let title = i["title"].as_str().unwrap_or("").to_string();
                        let url = i["url"].as_str().unwrap_or("").to_string();
                        if let Ok(mut s) = self.sites.lock() {
                            if let Some(x) = s.get_mut(&label) {
                                if !title.is_empty() && !title.starts_with("about:") {
                                    x.state.title = title;
                                }
                                if !url.is_empty() && url != "about:blank" {
                                    x.state.url = url;
                                }
                            }
                        }
                        self.emit_state(&label);
                        self.push_overlays();
                    } else if ev.method == "Target.targetCreated" {
                        // a page opened by a website (window.open, target=_blank): treat it as one more website tab
                        let url = i["url"].as_str().unwrap_or("").to_string();
                        if i["openerId"].as_str().is_some() || url.starts_with("http") {
                            self.adopt(id, &url);
                        }
                    }
                }
                "Target.targetDestroyed" => {
                    let id = ev.params["targetId"].as_str().unwrap_or("");
                    if id == self.main_id() {
                        shutdown();
                    }
                    if let Some(label) = self.label_for_target(id) {
                        if let Ok(mut s) = self.sites.lock() {
                            s.remove(&label);
                        }
                        let _ = self.app.emit("site-closed", json!({ "label": label }));
                        self.push_overlays();
                    }
                }
                "Fetch.requestPaused" => self.on_paused(&ev),
                "Target.attachedToTarget" => self.on_attached(&ev),
                "Target.detachedFromTarget" => {
                    if let Some(sid) = ev.params["sessionId"].as_str() {
                        if let Ok(mut f) = self.frame_sessions.lock() {
                            f.retain(|_, v| v != sid);
                        }
                    }
                }
                "Page.frameStartedLoading" | "Page.frameStoppedLoading" | "Page.navigatedWithinDocument" | "Page.frameNavigated" => {
                    let Some(sess) = ev.session.as_deref() else { continue };
                    if self.is_frame_session(sess) {
                        self.on_frame_event(&ev);
                        continue;
                    }
                    let Some(label) = self.label_for_session(sess) else { continue };
                    if ev.method == "Page.frameNavigated" && !ev.params["frame"]["parentId"].is_null() {
                        continue;
                    }
                    if ev.method == "Page.frameStartedLoading" || ev.method == "Page.frameStoppedLoading" {
                        let loading = ev.method == "Page.frameStartedLoading";
                        if let Ok(mut s) = self.sites.lock() {
                            if let Some(x) = s.get_mut(&label) {
                                x.state.loading = loading;
                            }
                        }
                    }
                    self.refresh_history(&label);
                    self.emit_state(&label);
                }
                "Runtime.bindingCalled" => {
                    let Some(sess) = ev.session.as_deref() else { continue };
                    let Some(label) = self.label_for_session(sess) else { continue };
                    let msg: Value = serde_json::from_str(ev.params["payload"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                    self.on_bar(&label, &msg);
                }
                _ => {}
            }
        }
    }

    fn adopt(&self, target: &str, url: &str) {
        let label = format!("popup-{}", self.n.fetch_add(1, Ordering::SeqCst) + 1);
        let Ok(a) = self.cdp.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None) else { return };
        let Some(session) = a["sessionId"].as_str().map(String::from) else { return };
        if let Ok(mut s) = self.sites.lock() {
            if s.values().any(|x| x.target == target) {
                return;
            }
            s.insert(label.clone(), Site { target: target.to_string(), session: session.clone(), state: SiteState { url: url.to_string(), ..Default::default() } });
        }
        let _ = self.prepare(&session);
        self.grant(url);
        let _ = self.cdp.call("Runtime.runIfWaitingForDebugger", json!({}), Some(&session));
        let _ = self.app.emit("site-new", json!({ "label": label, "url": url, "title": "" }));
        self.push_overlays();
    }

    fn on_bar(&self, label: &str, m: &Value) {
        match m["a"].as_str().unwrap_or("") {
            "hello" => {
                self.refresh_history(label);
                self.push_overlay(label);
            }
            "home" => {
                raise_main();
                let _ = self.app.emit("goto", json!({ "home": true }));
            }
            "switch" => {
                if let Some(l) = m["label"].as_str() {
                    let _ = self.show(Some(l));
                    let _ = self.app.emit("goto", json!({ "web": l }));
                }
            }
            "close" => {
                if let Some(l) = m["label"].as_str() {
                    let _ = self.close(l);
                }
            }
            a @ ("back" | "forward" | "reload") => {
                let _ = self.action(label, a, None);
            }
            "load" => {
                let _ = self.action(label, "load", m["url"].as_str().map(String::from));
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ commands used by the frontend

    pub fn open(&self, label: &str, url: &str, embed: bool) -> Result<(), String> {
        let url = check_url(url)?;
        let label = clean_label(label);
        if embed {
            if let Ok(mut e) = self.embedded.lock() {
                e.insert(label.clone());
            }
            if let Ok(mut m) = self.emb.lock() {
                m.insert(label.clone(), SiteState { url: url.clone(), loading: true, ..Default::default() });
            }
            self.grant(&url);
            return Ok(());
        }
        self.create_tab(&label, &url, false)
    }

    pub fn open_any(&self, url: &str) -> Result<String, String> {
        let url = check_chrome_url(url)?;
        let label = format!("chrome-{}", self.n.fetch_add(1, Ordering::SeqCst) + 1);
        self.create_tab(&label, &url, true)?;
        Ok(label)
    }

    pub fn show(&self, label: Option<&str>) -> Result<(), String> {
        let target = match label {
            None => self.main_id(),
            Some(l) if self.is_embedded(&clean_label(l)) => self.main_id(),
            Some(l) => self.sites.lock().ok().and_then(|s| s.get(&clean_label(l)).map(|x| x.target.clone())).ok_or("This website tab is not open")?,
        };
        self.cdp.call("Target.activateTarget", json!({ "targetId": target }), None).map(|_| ())
    }

    pub fn close(&self, label: &str) -> Result<(), String> {
        let label = clean_label(label);
        if self.is_embedded(&label) {
            if let Ok(mut e) = self.embedded.lock() {
                e.remove(&label);
            }
            if let Ok(mut m) = self.emb.lock() {
                m.remove(&label);
            }
            if let Ok(mut hm) = self.hist.lock() {
                hm.remove(&label);
            }
            if let Ok(mut f) = self.frames.lock() {
                f.retain(|_, l| *l != label);
            }
            return Ok(());
        }
        let t = self.sites.lock().ok().and_then(|mut s| s.remove(&label).map(|x| x.target));
        if let Some(t) = t {
            let _ = self.cdp.call("Target.closeTarget", json!({ "targetId": t }), None);
            self.push_overlays();
        }
        Ok(())
    }

    fn embedded_navigate(&self, label: &str, u: &str) -> Result<(), String> {
        let fid = self.frame_of(label).ok_or("The website is still loading")?;
        if let Some(sess) = self.frame_sessions.lock().ok().and_then(|f| f.get(&fid).cloned()) {
            self.cdp.call("Page.navigate", json!({ "url": u }), Some(&sess))?;
        } else {
            let ms = self.main_session.lock().map(|s| s.clone()).unwrap_or_default();
            self.cdp.call("Page.navigate", json!({ "url": u, "frameId": fid }), Some(&ms))?;
        }
        Ok(())
    }

    fn action_embedded(&self, label: &str, action: &str, url: Option<String>) -> Result<SiteState, String> {
        match action {
            "back" | "forward" => {
                let target = self.hist.lock().ok().and_then(|hm| {
                    hm.get(label).and_then(|(v, i)| if action == "back" { i.checked_sub(1).and_then(|k| v.get(k).cloned()) } else { v.get(i + 1).cloned() })
                });
                if let Some(u) = target {
                    self.embedded_navigate(label, &u)?;
                }
            }
            "reload" => {
                self.eval_frame(label, "location.reload()")?;
            }
            "load" => {
                let u = check_url(&url.unwrap_or_default())?;
                self.embedded_navigate(label, &u)?;
            }
            _ => return Err("Unknown website action".into()),
        }
        self.state(label)
    }

    pub fn action(&self, label: &str, action: &str, url: Option<String>) -> Result<SiteState, String> {
        let label = clean_label(label);
        if self.is_embedded(&label) {
            return self.action_embedded(&label, action, url);
        }
        let session = self.sites.lock().ok().and_then(|s| s.get(&label).map(|x| x.session.clone())).ok_or("This website tab is not open")?;
        let s = Some(session.as_str());
        match action {
            "reload" => {
                self.cdp.call("Page.reload", json!({}), s)?;
            }
            "back" | "forward" => {
                let h = self.cdp.call("Page.getNavigationHistory", json!({}), s)?;
                let idx = h["currentIndex"].as_i64().unwrap_or(0) + if action == "back" { -1 } else { 1 };
                if let Some(e) = h["entries"].as_array().and_then(|a| a.get(idx.max(0) as usize)).filter(|_| idx >= 0) {
                    self.cdp.call("Page.navigateToHistoryEntry", json!({ "entryId": e["id"] }), s)?;
                }
            }
            "load" => {
                let u = check_url(&url.unwrap_or_default())?;
                self.cdp.call("Page.navigate", json!({ "url": u }), s)?;
            }
            _ => return Err("Unknown website action".into()),
        }
        self.state(&label)
    }

    pub fn state(&self, label: &str) -> Result<SiteState, String> {
        if let Some(st) = self.emb.lock().ok().and_then(|m| m.get(&clean_label(label)).cloned()) {
            return Ok(st);
        }
        self.sites.lock().ok().and_then(|s| s.get(&clean_label(label)).map(|x| x.state.clone())).ok_or_else(|| "This website tab is not open".into())
    }

    pub fn probe(&self, label: &str) -> Result<String, String> {
        if self.is_embedded(&clean_label(label)) {
            let v = self.eval_frame(&clean_label(label), PROBE_JS)?;
            return Ok(v.as_str().unwrap_or("{\"error\":\"no result\"}").to_string());
        }
        let session = self.sites.lock().ok().and_then(|s| s.get(&clean_label(label)).map(|x| x.session.clone())).ok_or("This website tab is not open")?;
        let r = self.cdp.call("Runtime.evaluate", json!({ "expression": PROBE_JS, "returnByValue": true }), Some(&session))?;
        Ok(r["result"]["value"].as_str().unwrap_or("{\"error\":\"no result\"}").to_string())
    }

    pub fn window(&self, action: &str) -> Result<Value, String> {
        let w = self.cdp.call("Browser.getWindowForTarget", json!({ "targetId": self.main_id() }), None)?;
        let id = w["windowId"].clone();
        let state = w["bounds"]["windowState"].as_str().unwrap_or("normal").to_string();
        let set = |st: &str| self.cdp.call("Browser.setWindowBounds", json!({ "windowId": id, "bounds": { "windowState": st } }), None).map(|_| ());
        match action {
            "minimize" => {
                if set("minimized").is_err() {
                    set("normal")?;
                    set("minimized")?;
                }
                Ok(Value::Null)
            }
            "maximize" => set(if self.kiosk { "fullscreen" } else { "maximized" }).map(|_| Value::Null),
            "unmaximize" => set("normal").map(|_| Value::Null),
            "is_maximized" => Ok(json!(state == "maximized" || state == "fullscreen")),
            "close" => shutdown(),
            _ => Err("Unknown window action".into()),
        }
    }
}
