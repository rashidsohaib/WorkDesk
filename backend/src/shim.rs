//! A very small stand-in for the pieces of the Tauri API that the command code relies on
//! (`AppHandle`, `State`, `emit`, path resolving, `spawn_blocking`), so the v1.0 mail logic is reused unchanged.
use serde::Serialize;
use std::{
    any::Any,
    future::Future,
    path::PathBuf,
    pin::pin,
    sync::{mpsc, Arc, Mutex},
    task::{Context, Poll, Waker},
};

pub struct State<'a, T>(pub &'a T);
impl<T> std::ops::Deref for State<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0
    }
}

#[derive(Clone)]
pub struct PathResolver {
    home: PathBuf,
}

const ID: &str = "com.workdesk.app";

impl PathResolver {
    pub fn new() -> Self {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));
        PathResolver { home }
    }
    fn xdg(&self, var: &str, fallback: &str) -> PathBuf {
        match std::env::var(var) {
            Ok(v) if v.starts_with('/') => PathBuf::from(v),
            _ => self.home.join(fallback),
        }
    }
    pub fn app_data_dir(&self) -> Result<PathBuf, String> {
        Ok(self.xdg("XDG_DATA_HOME", ".local/share").join(ID))
    }
    pub fn app_config_dir(&self) -> Result<PathBuf, String> {
        Ok(self.xdg("XDG_CONFIG_HOME", ".config").join(ID))
    }
    pub fn app_cache_dir(&self) -> Result<PathBuf, String> {
        Ok(self.xdg("XDG_CACHE_HOME", ".cache").join(ID))
    }
    /// The user's Downloads folder (honours `XDG_DOWNLOAD_DIR` in `~/.config/user-dirs.dirs`).
    pub fn download_dir(&self) -> Result<PathBuf, String> {
        let cfg = self.xdg("XDG_CONFIG_HOME", ".config").join("user-dirs.dirs");
        if let Ok(text) = std::fs::read_to_string(cfg) {
            for l in text.lines() {
                let l = l.trim();
                if let Some(v) = l.strip_prefix("XDG_DOWNLOAD_DIR=") {
                    let v = v.trim().trim_matches('"');
                    let v = v.replace("$HOME", &self.home.to_string_lossy());
                    if v.starts_with('/') {
                        return Ok(PathBuf::from(v));
                    }
                }
            }
        }
        Ok(self.home.join("Downloads"))
    }
}

struct Inner {
    state: Box<dyn Any + Send + Sync>,
    subs: Mutex<Vec<mpsc::Sender<String>>>,
    paths: PathResolver,
}

#[derive(Clone)]
pub struct AppHandle {
    inner: Arc<Inner>,
}

impl AppHandle {
    pub fn new<T: Any + Send + Sync>(state: T, paths: PathResolver) -> Self {
        AppHandle { inner: Arc::new(Inner { state: Box::new(state), subs: Mutex::new(vec![]), paths }) }
    }
    pub fn state<T: Any + Send + Sync>(&self) -> State<'_, T> {
        State(self.inner.state.downcast_ref::<T>().expect("application state of the requested type"))
    }
    pub fn path(&self) -> PathResolver {
        self.inner.paths.clone()
    }
    /// Push an event to every open window (delivered as a Server-Sent Event).
    pub fn emit<S: Serialize>(&self, event: &str, payload: S) -> Result<(), String> {
        let json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
        let msg = format!("event: {event}\ndata: {json}\n\n");
        if let Ok(mut subs) = self.inner.subs.lock() {
            subs.retain(|s| s.send(msg.clone()).is_ok());
        }
        Ok(())
    }
    pub fn subscribe(&self) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut subs) = self.inner.subs.lock() {
            subs.push(tx);
        }
        rx
    }
}

/// Runs the closure right here: every request already has its own thread, so nothing needs to be moved.
pub fn spawn_blocking<T, F: FnOnce() -> T>(f: F) -> std::future::Ready<Result<T, String>> {
    std::future::ready(Ok(f()))
}

/// Drives a future that never actually suspends (all our "async" commands finish synchronously).
pub fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}
