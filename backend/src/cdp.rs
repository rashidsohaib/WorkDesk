//! A minimal Chrome DevTools Protocol client: a hand-written WebSocket (RFC 6455) over `std::net`.
//! Only what WorkDesk needs: text frames, JSON-RPC style request/response, and event delivery.
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};

pub struct Event {
    pub method: String,
    pub params: Value,
    pub session: Option<String>,
}

pub struct Cdp {
    writer: Mutex<TcpStream>,
    next_id: AtomicU64,
    mask_seed: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
}

fn seed() -> u64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    t ^ ((std::process::id() as u64) << 32) | 1
}

impl Cdp {
    /// Connect to `ws://127.0.0.1:<port><path>`. Returns the client and the stream of protocol events.
    pub fn connect(port: u16, path: &str) -> Result<(Arc<Cdp>, mpsc::Receiver<Event>), String> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("cannot reach Chrome's DevTools port: {e}"))?;
        let _ = stream.set_nodelay(true);
        let mut key = [0u8; 16];
        let mut s = seed();
        for b in key.iter_mut() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *b = (s & 0xff) as u8;
        }
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
            B64.encode(key)
        );
        stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let mut status = String::new();
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
        reader.read_line(&mut status).map_err(|e| e.to_string())?;
        if !status.contains(" 101 ") {
            return Err(format!("DevTools refused the connection: {}", status.trim()));
        }
        loop {
            let mut l = String::new();
            reader.read_line(&mut l).map_err(|e| e.to_string())?;
            if l == "\r\n" || l.is_empty() {
                break;
            }
        }
        stream.set_read_timeout(None).ok();
        let cdp = Arc::new(Cdp {
            writer: Mutex::new(stream),
            next_id: AtomicU64::new(1),
            mask_seed: AtomicU64::new(s | 1),
            pending: Mutex::new(HashMap::new()),
        });
        let (etx, erx) = mpsc::channel();
        let c2 = cdp.clone();
        std::thread::spawn(move || {
            c2.read_loop(reader, etx);
            // connection gone: fail every waiting caller
            if let Ok(mut p) = c2.pending.lock() {
                for (_, tx) in p.drain() {
                    let _ = tx.send(Err("Chrome closed the DevTools connection".into()));
                }
            }
        });
        Ok((cdp, erx))
    }

    fn read_loop(&self, mut r: BufReader<TcpStream>, events: mpsc::Sender<Event>) {
        let mut msg: Vec<u8> = vec![];
        loop {
            let mut h = [0u8; 2];
            if r.read_exact(&mut h).is_err() {
                return;
            }
            let fin = h[0] & 0x80 != 0;
            let op = h[0] & 0x0f;
            let masked = h[1] & 0x80 != 0;
            let mut len = (h[1] & 0x7f) as u64;
            if len == 126 {
                let mut b = [0u8; 2];
                if r.read_exact(&mut b).is_err() {
                    return;
                }
                len = u16::from_be_bytes(b) as u64;
            } else if len == 127 {
                let mut b = [0u8; 8];
                if r.read_exact(&mut b).is_err() {
                    return;
                }
                len = u64::from_be_bytes(b);
            }
            if len > 512 << 20 {
                return;
            }
            let mut mask = [0u8; 4];
            if masked && r.read_exact(&mut mask).is_err() {
                return;
            }
            let mut payload = vec![0u8; len as usize];
            if r.read_exact(&mut payload).is_err() {
                return;
            }
            if masked {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            match op {
                0 | 1 | 2 => {
                    msg.extend_from_slice(&payload);
                    if fin {
                        let data = std::mem::take(&mut msg);
                        self.on_message(&data, &events);
                    }
                }
                8 => return,
                9 => {
                    let _ = self.write_frame(10, &payload);
                }
                _ => {}
            }
        }
    }

    fn on_message(&self, data: &[u8], events: &mpsc::Sender<Event>) {
        let Ok(v) = serde_json::from_slice::<Value>(data) else { return };
        if let Some(id) = v.get("id").and_then(|x| x.as_u64()) {
            let tx = self.pending.lock().ok().and_then(|mut p| p.remove(&id));
            if let Some(tx) = tx {
                let r = match v.get("error") {
                    Some(e) => Err(e.get("message").and_then(|m| m.as_str()).unwrap_or("DevTools error").to_string()),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(r);
            }
        } else if let Some(m) = v.get("method").and_then(|x| x.as_str()) {
            let _ = events.send(Event {
                method: m.to_string(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
                session: v.get("sessionId").and_then(|s| s.as_str()).map(String::from),
            });
        }
    }

    fn write_frame(&self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut m = self.mask_seed.load(Ordering::Relaxed);
        m ^= m << 13;
        m ^= m >> 7;
        m ^= m << 17;
        self.mask_seed.store(m, Ordering::Relaxed);
        let mask = [(m & 0xff) as u8, ((m >> 8) & 0xff) as u8, ((m >> 16) & 0xff) as u8, ((m >> 24) & 0xff) as u8];
        let mut f = Vec::with_capacity(payload.len() + 14);
        f.push(0x80 | opcode);
        let n = payload.len();
        if n < 126 {
            f.push(0x80 | n as u8);
        } else if n <= 0xffff {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let mut w = self.writer.lock().map_err(|_| "DevTools writer poisoned".to_string())?;
        w.write_all(&f).map_err(|e| e.to_string())
    }

    /// Send one command and wait (up to 20 s) for its result.
    pub fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().map_err(|_| "poisoned")?.insert(id, tx);
        let mut m = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            m["sessionId"] = json!(s);
        }
        if let Err(e) = self.write_frame(1, m.to_string().as_bytes()) {
            let _ = self.pending.lock().map(|mut p| p.remove(&id));
            return Err(e);
        }
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(r) => r,
            Err(_) => {
                let _ = self.pending.lock().map(|mut p| p.remove(&id));
                Err(format!("Chrome did not answer {method} in time"))
            }
        }
    }
}
