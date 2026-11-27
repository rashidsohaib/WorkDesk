//! WorkDesk Chrome backend: the commands the frontend calls (served over the local RPC endpoint).
//!
//! Architecture
//!  * `imapx`  – IMAP connection, header sync, Gmail server-side search, flag/move operations
//!  * `db`     – local SQLite cache of message *headers* (the mailbox list is always served from it)
//!  * `mail`   – full-message decoding and outgoing message building
//!  * `chrome` – website tabs, as real tabs of the kiosk Chrome window

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use crate::db::Db;
use crate::imapx::{self, err, Sess};
use crate::{mail, model::*};
use lettre::{transport::smtp::authentication::Credentials, SmtpTransport, Transport};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};
use std::os::unix::process::CommandExt;
use crate::shim::{AppHandle, PathResolver, State};
use serde_json::Value;

const SERVICE: &str = "com.workdesk.app";
/// Keyring service used before the rename to WorkDesk; read once and copied across.
const OLD_SERVICE: &str = "com.maildesk.app";

pub struct SearchCache {
    key: String,
    items: Vec<Conversation>,
    truncated: bool,
}

pub struct AppState {
    pub db: Db,
    pub syncing: AtomicBool,
    pub search: Mutex<Option<SearchCache>>,
    pub draft_sess: Mutex<Option<(String, Sess)>>,
}

/// Clears the "sync running" flag however the sync thread ends.
struct SyncGuard<'a>(&'a AtomicBool);
impl Drop for SyncGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn acct(username: &str) -> String {
    username.trim().to_lowercase()
}

fn secret(account: &str) -> Result<String, String> {
    if let Some(p) = crate::vault::get(account) {
        return Ok(p);
    }
    // Saved by an earlier build (desktop keyring): copy it into the vault once.
    for svc in [SERVICE, OLD_SERVICE] {
        if let Ok(p) = keyring::Entry::new(svc, account).and_then(|e| e.get_password()) {
            let _ = crate::vault::set(account, &p);
            return Ok(p);
        }
    }
    Err(format!("No saved App Password for {account}"))
}

fn open_session(account: &str) -> Result<Sess, String> {
    let password = secret(account)?;
    imapx::connect(account, &password)
}

fn finish(mut s: Sess) {
    let _ = s.logout();
}

// ---------------------------------------------------------- ready-to-use connections
// Logging in to Gmail (TLS + LOGIN) takes seconds, far longer than fetching one message. Finished connections are
// therefore kept for a few minutes and handed to the next request instead of being closed.

static POOL: Mutex<Vec<(String, Sess, std::time::Instant)>> = Mutex::new(Vec::new());
const POOL_KEEP: usize = 4;
const POOL_IDLE: std::time::Duration = std::time::Duration::from_secs(240);

fn take_pooled(account: &str) -> Result<Sess, String> {
    loop {
        let cand = {
            let mut g = POOL.lock().unwrap_or_else(|p| p.into_inner());
            g.iter().position(|(w, _, at)| w == account && at.elapsed() < POOL_IDLE).map(|i| g.remove(i))
        };
        let Some((_, mut s, _)) = cand else { break };
        if s.noop().is_ok() {
            return Ok(s);
        }
    }
    open_session(account)
}

fn give_back(account: &str, s: Sess) {
    let mut g = POOL.lock().unwrap_or_else(|p| p.into_inner());
    let mut stale: Vec<Sess> = Vec::new();
    let mut i = 0;
    while i < g.len() {
        if g[i].2.elapsed() >= POOL_IDLE {
            stale.push(g.remove(i).1);
        } else {
            i += 1;
        }
    }
    g.push((account.to_string(), s, std::time::Instant::now()));
    while g.len() > POOL_KEEP {
        stale.push(g.remove(0).1);
    }
    drop(g);
    for s in stale {
        finish_quiet(s);
    }
}

fn pool_clear() {
    let all: Vec<_> = POOL.lock().unwrap_or_else(|p| p.into_inner()).drain(..).collect();
    for (_, s, _) in all {
        finish_quiet(s);
    }
}

/// Run `f` on a ready connection; a connection that worked is kept for the next request.
fn with_session<T>(account: &str, f: impl FnOnce(&mut Sess) -> Result<T, String>) -> Result<T, String> {
    let mut s = take_pooled(account)?;
    let r = f(&mut s);
    if r.is_ok() {
        give_back(account, s);
    } else {
        finish_quiet(s);
    }
    r
}

// Messages being downloaded right now, so a click and a background download never fetch the same message twice.
static INFLIGHT: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

fn inflight_claim(folder: &str, uid: u32) -> bool {
    let mut g = INFLIGHT.lock().unwrap_or_else(|p| p.into_inner());
    if g.iter().any(|(f, u)| f == folder && *u == uid) {
        return false;
    }
    g.push((folder.to_string(), uid));
    true
}

fn inflight_release(folder: &str, uid: u32) {
    INFLIGHT.lock().unwrap_or_else(|p| p.into_inner()).retain(|(f, u)| !(f == folder && *u == uid));
}

fn inflight_busy(folder: &str, uid: u32) -> bool {
    INFLIGHT.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|(f, u)| f == folder && *u == uid)
}

/// True when the message is in memory: under this label, or already downloaded under another label of the same Gmail message.
fn raw_ready(folder: &str, uid: u32, msgid: &str) -> bool {
    mail::raw_has(folder, uid) || mail::raw_alias(folder, uid, msgid)
}

// ------------------------------------------------------------------ credentials

fn save_credentials(username: String, app_password: String) -> Result<(), String> {
    let u = acct(&username);
    let p: String = app_password.chars().filter(|c| !c.is_whitespace()).collect();
    if !u.contains('@') || p.is_empty() {
        return Err("Enter a valid Gmail address and App Password".into());
    }
    mail::raw_clear();
    details_clear();
    pool_clear();
    crate::vault::set(&u, &p)
}

fn delete_credentials(state: State<'_, AppState>, username: String) -> Result<(), String> {
    let a = acct(&username);
    mail::raw_clear();
    details_clear();
    crate::vault::remove(&a);
    let _ = keyring::Entry::new(SERVICE, &a).and_then(|e| e.delete_credential());
    let _ = keyring::Entry::new(OLD_SERVICE, &a).and_then(|e| e.delete_credential());
    let _ = state.db.clear_account(&a);
    let _ = state.db.replace_folder_list(&a, &[]);
    *state.search.lock().unwrap_or_else(|p| p.into_inner()) = None;
    Ok(())
}

async fn test_connection(username: String) -> Result<String, String> {
    let a = acct(&username);
    let mut s = open_session(&a)?;
    let r = s.select("INBOX").map_err(err)?;
    finish(s);
    Ok(format!("Connected to Gmail. Inbox contains {} messages.", r.exists))
}

// ---------------------------------------------------------------------- folders

fn refresh_folder_list(db: &Db, a: &str) -> Result<(), String> {
    let mut s = open_session(a)?;
    let folders = imapx::list_folders(&mut s)?;
    finish(s);
    db.replace_folder_list(a, &folders)
}

async fn list_folders(state: State<'_, AppState>, username: String, refresh: bool) -> Result<FolderList, String> {
    let a = acct(&username);
    let cached = state.db.cached_folders(&a)?;
    if !refresh && !cached.is_empty() {
        return Ok(FolderList { folders: cached, offline: false });
    }
    match refresh_folder_list(&state.db, &a) {
        Ok(()) => Ok(FolderList { folders: state.db.cached_folders(&a)?, offline: false }),
        Err(e) if cached.is_empty() => Err(e),
        Err(_) => Ok(FolderList { folders: cached, offline: true }),
    }
}

async fn create_folder(state: State<'_, AppState>, username: String, folder: String) -> Result<(), String> {
    let name = folder.trim();
    if name.is_empty() || name.len() > 120 || name.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return Err("Enter a valid folder name (1–120 characters)".into());
    }
    let a = acct(&username);
    let mut s = open_session(&a)?;
    s.create(name).map_err(err)?;
    finish(s);
    refresh_folder_list(&state.db, &a)
}

async fn delete_folder(state: State<'_, AppState>, username: String, folder: String) -> Result<(), String> {
    let name = folder.trim();
    if name.eq_ignore_ascii_case("INBOX") || name.starts_with("[Gmail]/") || name.starts_with("[Google Mail]/") {
        return Err("System Gmail folders cannot be deleted from WorkDesk".into());
    }
    let a = acct(&username);
    let mut s = open_session(&a)?;
    s.delete(name).map_err(err)?;
    finish(s);
    state.db.delete_folder(&a, name)?;
    refresh_folder_list(&state.db, &a)
}

// -------------------------------------------------------------------------- sync

fn emit_sync(app: &AppHandle, ev: SyncEvent) {
    let _ = app.emit("sync", ev);
}

fn sync_event(a: &str, folder: &str, phase: &str, message: Option<String>) -> SyncEvent {
    SyncEvent { account: a.into(), folder: folder.into(), phase: phase.into(), done: 0, total: 0, message }
}

fn run_full_sync(app: &AppHandle, a: &str, priority: Option<String>) -> Result<(), String> {
    let st = app.state::<AppState>();
    let mut sess = open_session(a)?;
    let folders = imapx::list_folders(&mut sess)?;
    st.db.replace_folder_list(a, &folders)?;
    emit_sync(app, sync_event(a, "", "folders", None));
    let order = imapx::sync_order(&folders, priority.as_deref());
    for f in order {
        let progress = |ev: SyncEvent| emit_sync(app, ev);
        if let Err(e) = imapx::sync_folder(&mut sess, &st.db, a, &f, &progress) {
            emit_sync(app, sync_event(a, &f, "error", Some(e)));
            // The connection may have dropped: try to carry on with a fresh one.
            if let Ok(fresh) = open_session(a) {
                sess = fresh;
            }
        }
    }
    finish(sess);
    Ok(())
}

/// Start (or leave running) the background header download of every folder.
fn start_sync(app: AppHandle, state: State<'_, AppState>, username: String, priority: Option<String>) -> Result<bool, String> {
    if state.syncing.swap(true, Ordering::SeqCst) {
        return Ok(false);
    }
    let a = acct(&username);
    let app2 = app.clone();
    std::thread::spawn(move || {
        let st = app2.state::<AppState>();
        let _guard = SyncGuard(&st.syncing);
        match run_full_sync(&app2, &a, priority) {
            Ok(()) => emit_sync(&app2, sync_event(&a, "", "finished", None)),
            Err(e) => emit_sync(&app2, sync_event(&a, "", "error", Some(e))),
        }
    });
    Ok(true)
}

/// Synchronise one folder right now and wait for it (used by Refresh and when opening a folder).
async fn sync_folder_now(app: AppHandle, state: State<'_, AppState>, username: String, folder: String) -> Result<SyncOutcome, String> {
    let a = acct(&username);
    let mut sess = open_session(&a)?;
    let progress = |ev: SyncEvent| emit_sync(&app, ev);
    let out = imapx::sync_folder(&mut sess, &state.db, &a, &folder, &progress);
    finish(sess);
    let mut out = out?;
    out.in_progress = state.syncing.load(Ordering::SeqCst);
    Ok(out)
}

fn sync_running(state: State<'_, AppState>) -> bool {
    state.syncing.load(Ordering::SeqCst)
}

fn clear_cache(state: State<'_, AppState>, username: String) -> Result<(), String> {
    *state.search.lock().unwrap_or_else(|p| p.into_inner()) = None;
    state.db.clear_account(&acct(&username))
}

// ------------------------------------------------------------------- list / search

fn page_slice(mut items: Vec<Conversation>, page: u32, page_size: u32, truncated: bool) -> ConversationPage {
    let total = items.len() as u32;
    let total_messages: u32 = items.iter().map(|c| c.matched).sum();
    let size = page_size.clamp(1, 200) as usize;
    let start = (page.max(1) as usize - 1) * size;
    let items: Vec<Conversation> = if start >= items.len() { Vec::new() } else { items.drain(start..).take(size).collect() };
    ConversationPage { items, total, total_messages, synced: true, truncated }
}

/// One page of a folder, straight from the local header cache.
#[allow(clippy::too_many_arguments)]
fn list_page(
    state: State<'_, AppState>,
    username: String,
    folder: String,
    grouped: bool,
    sort: String,
    desc: bool,
    page: u32,
    page_size: u32,
) -> Result<ConversationPage, String> {
    let a = acct(&username);
    let all = state.db.role_folder(&a, "all")?;
    let size = page_size.clamp(1, 200);
    state.db.list_page(&a, &folder, grouped, &sort, desc, size, (page.max(1) - 1) * size, all.as_deref())
}

fn sort_conversations(items: &mut [Conversation], sort: &str, desc: bool) {
    items.sort_by(|a, b| {
        let o = match sort {
            "subject" => crate::db::normalize_subject(&a.subject).cmp(&crate::db::normalize_subject(&b.subject)),
            "from" => a.from.to_lowercase().cmp(&b.from.to_lowercase()),
            "to" => a.to.cmp(&b.to),
            "size" => a.size.cmp(&b.size),
            "attachment" => a.has_attachment.cmp(&b.has_attachment),
            "flags" => a.unread.cmp(&b.unread),
            _ => a.date_ts.cmp(&b.date_ts),
        };
        let o = if desc { o.reverse() } else { o };
        o.then(b.date_ts.cmp(&a.date_ts))
    });
}

/// Gmail search executed on Google's servers (X-GM-RAW) in All Mail, Trash and Spam.
/// Hits are turned into conversations (one row per Gmail thread) just like a normal folder.
#[allow(clippy::too_many_arguments)]
async fn search_mail(
    state: State<'_, AppState>,
    username: String,
    query: String,
    grouped: bool,
    sort: String,
    desc: bool,
    page: u32,
    page_size: u32,
    fresh: bool,
) -> Result<ConversationPage, String> {
    let a = acct(&username);
    let key = format!("{a}|{grouped}|{query}");
    if !fresh {
        let cache = state.search.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = cache.as_ref().filter(|c| c.key == key) {
            let mut items = c.items.clone();
            sort_conversations(&mut items, &sort, desc);
            return Ok(page_slice(items, page, page_size, c.truncated));
        }
    }
    if state.db.cached_folders(&a)?.is_empty() {
        refresh_folder_list(&state.db, &a)?;
    }
    let mut targets: Vec<String> = Vec::new();
    for role in ["all", "trash", "spam"] {
        if let Some(f) = state.db.role_folder(&a, role)? {
            targets.push(f);
        }
    }
    if targets.is_empty() {
        targets = vec!["[Gmail]/All Mail".into(), "[Gmail]/Trash".into(), "[Gmail]/Spam".into()];
    }
    let mut sess = open_session(&a)?;
    let result = (|| -> Result<(Vec<CachedMsg>, bool), String> {
        let hits = imapx::gmail_search(&mut sess, &targets, &query)?;
        let mut members: Vec<CachedMsg> = Vec::new();
        let mut truncated = false;
        for (folder, ids, cut) in hits {
            truncated |= cut;
            let rows = imapx::ensure_headers(&mut sess, &state.db, &a, &folder, &ids, imapx::SEARCH_FETCH_CAP)?;
            if rows.len() < ids.len() {
                truncated = true;
            }
            members.extend(rows);
        }
        Ok((members, truncated))
    })();
    finish(sess);
    let (members, truncated) = result?;

    let all_mail = state.db.role_folder(&a, "all")?;
    let mut groups: HashMap<String, Vec<CachedMsg>> = HashMap::new();
    for m in members {
        let k = if grouped { m.group_key() } else { format!("{}:{}", m.folder, m.uid) };
        groups.entry(k).or_default().push(m);
    }
    let real: HashMap<String, u32> = match (&all_mail, grouped) {
        (Some(am), true) => state.db.count_by_group(&a, am, &groups.keys().cloned().collect::<Vec<_>>())?,
        _ => HashMap::new(),
    };
    let mut items: Vec<Conversation> =
        groups.into_iter().map(|(k, ms)| crate::db::build_conversation(&k, ms, real.get(&k).copied(), grouped)).collect();
    sort_conversations(&mut items, &sort, desc);
    *state.search.lock().unwrap_or_else(|p| p.into_inner()) =
        Some(SearchCache { key, items: items.clone(), truncated });
    Ok(page_slice(items, page, page_size, truncated))
}

/// Every message of a conversation (headers only), oldest first. The mailbox view only knows the
/// messages carrying the current label, so the rest of the Gmail thread is looked up in All Mail.
async fn thread_messages(
    state: State<'_, AppState>,
    username: String,
    thread_id: Option<String>,
    folder: String,
    uid: u32,
    refresh: Option<bool>,
) -> Result<Vec<MailRow>, String> {
    let a = acct(&username);
    let own = state.db.rows_for_uids(&a, &folder, &[uid])?;
    let Some(thrid) = thread_id.filter(|t| !t.is_empty()) else {
        return Ok(own.iter().map(MailRow::from).collect());
    };
    let mut scope: Vec<String> = Vec::new();
    if let Some(all) = state.db.role_folder(&a, "all")? {
        scope.push(all);
    }
    let role = state.db.folder_role(&a, &folder)?;
    if matches!(role.as_deref(), Some("trash") | Some("spam")) || scope.is_empty() {
        scope.push(folder.clone());
    }
    scope.dedup();
    // Look the thread up on the server; if that fails (offline) the cached members are still shown.
    // `refresh: false` answers from the local cache at once; the UI asks again with `refresh: true` to catch up.
    if refresh.unwrap_or(true) {
        let _ = with_session(&a, |sess| {
            for f in &scope {
                let _ = imapx::refresh_thread_in(sess, &state.db, &a, f, &thrid);
            }
            Ok(())
        });
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<MailRow> = Vec::new();
    for m in state.db.thread_members(&a, &thrid, &scope)? {
        if !m.gm_msgid.is_empty() && !seen.insert(m.gm_msgid.clone()) {
            continue;
        }
        out.push(MailRow::from(&m));
    }
    if out.is_empty() {
        out = own.iter().map(MailRow::from).collect();
    }
    // Start downloading the other messages of the conversation now, newest first, so expanding one is instant.
    let upcoming: Vec<(String, u32, String)> =
        out.iter().rev().take(12).map(|r| (r.folder.clone(), r.uid, r.gm_msgid.clone().unwrap_or_default())).collect();
    prefetch_many(a.clone(), upcoming);
    Ok(out)
}

// ---------------------------------------------------------------------- messages

// Decoded messages (most recent 1000), kept in memory only.
type DKey = (String, String, u32);
static DETAILS: Mutex<Option<(HashMap<DKey, MailDetail>, VecDeque<DKey>)>> = Mutex::new(None);
const DETAILS_MAX: usize = 1000;

fn details_get(a: &str, folder: &str, uid: u32) -> Option<MailDetail> {
    let g = DETAILS.lock().unwrap_or_else(|p| p.into_inner());
    g.as_ref()?.0.get(&(a.to_string(), folder.to_string(), uid)).cloned()
}

fn details_put(a: &str, d: &MailDetail) {
    let mut g = DETAILS.lock().unwrap_or_else(|p| p.into_inner());
    let (map, order) = g.get_or_insert_with(|| (HashMap::new(), VecDeque::new()));
    let k = (a.to_string(), d.folder.clone(), d.uid);
    if map.insert(k.clone(), d.clone()).is_none() {
        order.push_back(k);
    }
    while order.len() > DETAILS_MAX {
        if let Some(old) = order.pop_front() {
            map.remove(&old);
        }
    }
}

fn details_forget(a: &str, refs: &[MsgRef]) {
    let mut g = DETAILS.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((map, order)) = g.as_mut() {
        for r in refs {
            let k = (a.to_string(), r.folder.clone(), r.uid);
            if map.remove(&k).is_some() {
                order.retain(|x| *x != k);
            }
        }
    }
}

fn details_clear() {
    *DETAILS.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Mark a message read in the cache at once and on the server in the background.
fn mark_seen_bg(state: &AppState, a: &str, folder: &str, uid: u32) {
    let r = MsgRef { folder: folder.to_string(), uid };
    let _ = state.db.apply_flag(a, std::slice::from_ref(&r), "Seen", true);
    let a = a.to_string();
    std::thread::spawn(move || {
        let _ = with_session(&a, |s| imapx::set_flag(s, std::slice::from_ref(&r), "Seen", true));
    });
}

async fn get_message(state: State<'_, AppState>, username: String, folder: String, uid: u32, mark_seen: Option<bool>) -> Result<MailDetail, String> {
    let a = acct(&username);
    let mark = mark_seen.unwrap_or(true);
    let row = state.db.rows_for_uids(&a, &folder, &[uid])?.into_iter().next();
    let thread_id = row.as_ref().map(|m| m.thrid.clone()).filter(|t| !t.is_empty());
    let has_seen = |f: &[String]| f.iter().any(|x| x.eq_ignore_ascii_case("Seen"));
    // 1. already decoded   2. downloaded (background download or earlier open)   3. ask Gmail
    if let Some(mut d) = details_get(&a, &folder, uid) {
        if let Some(r) = &row {
            d.flags = r.flags.clone();
        }
        if mark && !has_seen(&d.flags) {
            d.flags.push("Seen".into());
            mark_seen_bg(&state, &a, &folder, uid);
            details_put(&a, &d);
        }
        return Ok(d);
    }
    let msgid = row.as_ref().map(|r| r.gm_msgid.clone()).unwrap_or_default();
    // A background download of this very message may be under way: wait for it rather than fetching it twice.
    let mut waited = 0;
    while inflight_busy(&folder, uid) && !mail::raw_has(&folder, uid) && waited < 400 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
    }
    if raw_ready(&folder, uid, &msgid) {
        if let Some(raw) = mail::raw_get(&folder, uid) {
            let mut flags = row.as_ref().map(|r| r.flags.clone()).unwrap_or_default();
            if let Ok(mut d) = mail::decode_message(&raw, &folder, uid, thread_id.clone(), flags.clone()) {
                if mark && !has_seen(&flags) {
                    flags.push("Seen".into());
                    d.flags = flags;
                    mark_seen_bg(&state, &a, &folder, uid);
                }
                details_put(&a, &d);
                return Ok(d);
            }
        }
    }
    let claimed = inflight_claim(&folder, uid);
    let fetched = with_session(&a, |s| mail::fetch_message(s, &folder, uid, thread_id.clone(), mark, &msgid));
    if claimed {
        inflight_release(&folder, uid);
    }
    let detail = fetched?;
    if mark {
        let _ = state.db.apply_flag(&a, &[MsgRef { folder: folder.clone(), uid }], "Seen", true);
    }
    details_put(&a, &detail);
    Ok(detail)
}

static PREFETCH_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Download a message ahead of the click (e.g. when the pointer rests on it). Returns at once.
fn prefetch_message(state: State<'_, AppState>, username: String, folder: String, uid: u32) -> Result<(), String> {
    let a = acct(&username);
    let msgid = state.db.rows_for_uids(&a, &folder, &[uid])?.first().map(|m| m.gm_msgid.clone()).unwrap_or_default();
    prefetch_many(a, vec![(folder, uid, msgid)]);
    Ok(())
}

/// Download messages in the background, one after another, on one ready connection.
fn prefetch_many(a: String, items: Vec<(String, u32, String)>) {
    let todo: Vec<(String, u32, String)> = items.into_iter().filter(|(f, u, m)| !raw_ready(f, *u, m)).collect();
    if todo.is_empty() || PREFETCH_ACTIVE.load(Ordering::SeqCst) >= 3 {
        return;
    }
    PREFETCH_ACTIVE.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        let _ = with_session(&a, |s| {
            for (folder, uid, msgid) in &todo {
                if raw_ready(folder, *uid, msgid) || !inflight_claim(folder, *uid) {
                    continue;
                }
                let r = mail::prefetch(s, folder, *uid, msgid);
                inflight_release(folder, *uid);
                r?;
            }
            Ok(())
        });
        PREFETCH_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    });
}

// ------------------------------------------------- background download of the newest messages

static WARM_RUNNING: AtomicBool = AtomicBool::new(false);
static WARM_AGAIN: AtomicBool = AtomicBool::new(false);
/// How many of the newest messages of each of Inbox, Sent Mail and All Mail are downloaded into memory.
const WARM_COUNT: usize = 500;
const WARM_ONE_MAX: u32 = 15 * 1024 * 1024;

fn warm_once(app: &AppHandle, a: &str) {
    let st = app.state::<AppState>();
    let mut folders: Vec<String> = vec!["INBOX".to_string()];
    for role in ["sent", "all"] {
        if let Ok(Some(f)) = st.db.role_folder(a, role) {
            if !folders.contains(&f) {
                folders.push(f);
            }
        }
    }
    let mut sess: Option<Sess> = None;
    'folders: for folder in folders {
        let Ok(rows) = st.db.recent_uids(a, &folder, WARM_COUNT) else { continue };
        let mut need: Vec<(u32, String)> = Vec::new();
        for (uid, size, _thrid, msgid) in rows {
            if size > WARM_ONE_MAX || mail::raw_has(&folder, uid) || mail::raw_alias(&folder, uid, &msgid) {
                continue;
            }
            need.push((uid, msgid));
        }
        if need.is_empty() {
            continue;
        }
        if sess.is_none() {
            match open_session(a) {
                Ok(s) => sess = Some(s),
                Err(_) => break 'folders,
            }
        }
        let s = sess.as_mut().expect("session just opened");
        if s.select(&folder).is_err() {
            sess = None;
            continue;
        }
        for chunk in need.chunks(10) {
            let set = imapx::uid_set(&chunk.iter().map(|c| c.0).collect::<Vec<_>>());
            let Ok(fetched) = s.uid_fetch(set, "(UID BODY.PEEK[])") else {
                sess = None;
                continue 'folders;
            };
            for m in fetched.iter() {
                if let (Some(uid), Some(body)) = (m.uid, m.body()) {
                    let msgid = chunk.iter().find(|c| c.0 == uid).map(|c| c.1.as_str()).unwrap_or("");
                    mail::raw_put_id(&folder, uid, msgid, body);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    }
    if let Some(s) = sess {
        give_back(a, s);
    }
}

/// Download the newest 500 messages of Inbox, Sent Mail and All Mail into memory (never to disk), so they open at once.
fn warm_inbox(app: AppHandle, username: String) {
    let a = acct(&username);
    if WARM_RUNNING.swap(true, Ordering::SeqCst) {
        WARM_AGAIN.store(true, Ordering::SeqCst);
        return;
    }
    std::thread::spawn(move || {
        loop {
            WARM_AGAIN.store(false, Ordering::SeqCst);
            warm_once(&app, &a);
            if !WARM_AGAIN.load(Ordering::SeqCst) {
                break;
            }
        }
        WARM_RUNNING.store(false, Ordering::SeqCst);
    });
}

/// UIDVALIDITY of a folder (used to recognise the saved copy of a draft being edited).
async fn folder_validity(state: State<'_, AppState>, username: String, folder: String) -> Result<Option<u32>, String> {
    let a = acct(&username);
    if let Some(meta) = state.db.folder_meta(&a, &folder)? {
        if let Some(v) = meta.uidvalidity.filter(|v| *v > 0) {
            return Ok(Some(v as u32));
        }
    }
    let mut s = open_session(&a)?;
    let r = s.select(&folder).map_err(err);
    finish(s);
    Ok(r?.uid_validity)
}

/// Remove a saved draft from the server (after it was sent, scheduled or discarded).
async fn discard_draft(state: State<'_, AppState>, username: String, folder: String, uid: u32, validity: Option<u32>) -> Result<(), String> {
    let a = acct(&username);
    let mut sess = take_draft_session(&state, &a)?;
    let r = (|| -> Result<(), String> {
        let mb = sess.select(&folder).map_err(err)?;
        if let Some(v) = validity {
            if mb.uid_validity != Some(v) {
                return Ok(()); // the folder was renumbered: this uid may now be a different message
            }
        }
        sess.uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)").map_err(err)?;
        sess.uid_expunge(uid.to_string()).map_err(err)?;
        Ok(())
    })();
    match &r {
        Ok(()) => {
            let _ = state.db.delete_uids(&a, &folder, &[uid]);
            details_forget(&a, &[MsgRef { folder: folder.clone(), uid }]);
            *state.draft_sess.lock().map_err(err)? = Some((a, sess));
        }
        Err(_) => finish_quiet(sess),
    }
    *state.search.lock().unwrap_or_else(|p| p.into_inner()) = None;
    r
}

/// "Move to Inbox" from Sent Mail / All Mail: add the Inbox label (a COPY into INBOX). The message also stays where it was.
async fn add_to_inbox(state: State<'_, AppState>, username: String, refs: Vec<MsgRef>) -> Result<(), String> {
    let a = acct(&username);
    let mut sess = open_session(&a)?;
    let r = imapx::copy_to_label(&mut sess, &refs, "INBOX");
    if r.is_ok() && state.db.folder_meta(&a, "INBOX")?.map(|m| m.complete).unwrap_or(false) {
        let _ = imapx::sync_folder(&mut sess, &state.db, &a, "INBOX", &|_| {});
    }
    finish(sess);
    *state.search.lock().unwrap_or_else(|p| p.into_inner()) = None;
    r
}

async fn get_attachment(username: String, folder: String, uid: u32, index: u32) -> Result<AttachmentData, String> {
    if let Some(raw) = mail::raw_get(&folder, uid) {
        if let Ok(a) = mail::attachment_from_raw(&raw, index) {
            return Ok(a);
        }
    }
    with_session(&acct(&username), |s| mail::fetch_attachment(s, &folder, uid, index))
}

async fn set_flag(state: State<'_, AppState>, username: String, refs: Vec<MsgRef>, flag: String, enabled: bool) -> Result<(), String> {
    let a = acct(&username);
    // The cache is updated first so the list reacts at once; the server follows.
    state.db.apply_flag(&a, &refs, &flag, enabled)?;
    details_forget(&a, &refs);
    let mut sess = open_session(&a)?;
    let r = imapx::set_flag(&mut sess, &refs, &flag, enabled);
    finish(sess);
    r
}

fn resolve_destination(db: &Db, a: &str, target: &str) -> Result<String, String> {
    let role = if target == "archive" { "all" } else { target };
    if ["inbox", "trash", "spam", "all", "sent", "drafts"].contains(&role) {
        if role == "inbox" {
            return Ok("INBOX".into());
        }
        return db.role_folder(a, role)?.ok_or_else(|| format!("This account has no {role} folder"));
    }
    Ok(target.to_string())
}

/// Move messages to a role ("trash", "spam", "archive", "inbox") or to a named label.
async fn move_messages(state: State<'_, AppState>, username: String, refs: Vec<MsgRef>, target: String) -> Result<(), String> {
    let a = acct(&username);
    let dest = resolve_destination(&state.db, &a, &target)?;
    details_forget(&a, &refs);
    // remember the Gmail ids so every cached copy can be forgotten afterwards
    let mut msgids: Vec<String> = Vec::new();
    for (folder, uids) in imapx::group_by_folder(&refs) {
        for m in state.db.rows_for_uids(&a, &folder, &uids)? {
            msgids.push(m.gm_msgid);
        }
    }
    let mut sess = open_session(&a)?;
    let r = imapx::move_messages(&mut sess, &refs, &dest);
    if r.is_ok() {
        for (folder, uids) in imapx::group_by_folder(&refs) {
            if folder != dest {
                state.db.delete_uids(&a, &folder, &uids)?;
            }
        }
        if ["trash", "spam"].contains(&target.as_str()) {
            state.db.delete_msgids_except(&a, &msgids, &dest)?;
        }
        // pick up the moved messages in the destination (only if that folder is already cached)
        if state.db.folder_meta(&a, &dest)?.map(|m| m.complete).unwrap_or(false) {
            let _ = imapx::sync_folder(&mut sess, &state.db, &a, &dest, &|_| {});
        }
    }
    finish(sess);
    *state.search.lock().unwrap_or_else(|p| p.into_inner()) = None;
    r
}

async fn delete_forever(state: State<'_, AppState>, username: String, refs: Vec<MsgRef>) -> Result<(), String> {
    let a = acct(&username);
    details_forget(&a, &refs);
    let mut sess = open_session(&a)?;
    let r = imapx::delete_messages(&mut sess, &refs);
    finish(sess);
    r?;
    for (folder, uids) in imapx::group_by_folder(&refs) {
        state.db.delete_uids(&a, &folder, &uids)?;
    }
    *state.search.lock().unwrap_or_else(|p| p.into_inner()) = None;
    Ok(())
}

// ----------------------------------------------------------------------- sending

/// Send through Gmail SMTP (implicit TLS on 465). Blocking: always call from a worker thread.
fn smtp_send(req: &SendRequest) -> Result<(), String> {
    let a = acct(&req.username);
    let password = secret(&a)?;
    let msg = mail::build_message(req, true)?;
    let mailer = SmtpTransport::relay("smtp.gmail.com")
        .map_err(err)?
        .port(465)
        .timeout(Some(std::time::Duration::from_secs(60)))
        .credentials(Credentials::new(a, password))
        .authentication(vec![lettre::transport::smtp::authentication::Mechanism::Plain])
        .build();
    mailer.send(&msg).map(|_| ()).map_err(err)
}

async fn send_email(req: SendRequest) -> Result<String, String> {
    crate::shim::spawn_blocking(move || smtp_send(&req).map(|_| "Message accepted by Gmail SMTP".to_string()))
        .await
        .map_err(err)?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DraftSaved {
    message: String,
    uid: Option<u32>,
    validity: Option<u32>,
}

/// Reuse the cached Drafts connection when it is still alive; otherwise log in again.
fn take_draft_session(st: &AppState, account: &str) -> Result<Sess, String> {
    let cached = st.draft_sess.lock().map_err(err)?.take();
    if let Some((who, mut sess)) = cached {
        if who == account && sess.noop().is_ok() {
            return Ok(sess);
        }
        finish_quiet(sess);
    }
    open_session(account)
}

fn finish_quiet(sess: Sess) {
    std::thread::spawn(move || finish(sess));
}

async fn save_draft(app: AppHandle, req: SendRequest, replace_uid: Option<u32>, replace_validity: Option<u32>) -> Result<DraftSaved, String> {
    crate::shim::spawn_blocking(move || {
        let a = acct(&req.username);
        let message = mail::build_message(&req, false)?;
        let st = app.state::<AppState>();
        let folder = st.db.role_folder(&a, "drafts")?.unwrap_or_else(|| "[Gmail]/Drafts".to_string());
        let bytes = message.formatted();
        let mut sess = take_draft_session(&st, &a)?;
        let mut result = sess.append(&folder, &bytes).flag(imap::types::Flag::Draft).finish().map_err(err);
        if result.is_err() {
            // The cached connection may have died between the NOOP and the APPEND: retry once on a fresh login.
            finish_quiet(sess);
            sess = open_session(&a)?;
            result = sess.append(&folder, &bytes).flag(imap::types::Flag::Draft).finish().map_err(err);
        }
        let appended = match result {
            Ok(x) => x,
            Err(e) => {
                finish_quiet(sess);
                return Err(e);
            }
        };
        let uid = appended.uids.as_ref().and_then(|v| v.first()).and_then(|m| match m {
            imap_proto::UidSetMember::Uid(u) => Some(*u),
            imap_proto::UidSetMember::UidRange(r) => Some(*r.start()),
        });
        // Replace the previous autosave of the same draft instead of piling up copies.
        if let (Some(old), Some(v_old)) = (replace_uid, replace_validity) {
            if appended.uid_validity == Some(v_old) && Some(old) != uid {
                let _ = sess.select(&folder).and_then(|_| sess.uid_store(old.to_string(), "+FLAGS.SILENT (\\Deleted)")).and_then(|_| sess.uid_expunge(old.to_string()));
            }
        }
        *st.draft_sess.lock().map_err(err)? = Some((a, sess));
        Ok(DraftSaved { message: "Draft saved to Gmail".into(), uid, validity: appended.uid_validity })
    })
    .await
    .map_err(err)?
}

/// Open the IMAP connection used for drafts ahead of time, so the first "Save draft" is quick.
fn warm_connections(app: AppHandle, username: String) {
    let a = acct(&username);
    {
        let a = a.clone();
        std::thread::spawn(move || {
            let have = POOL.lock().map(|g| g.iter().filter(|(w, _, _)| *w == a).count()).unwrap_or(0);
            for _ in have..2 {
                match open_session(&a) {
                    Ok(s) => give_back(&a, s),
                    Err(_) => break,
                }
            }
        });
    }
    std::thread::spawn(move || {
        let st = app.state::<AppState>();
        let has = st.draft_sess.lock().map(|g| g.as_ref().map(|(w, _)| *w == a).unwrap_or(false)).unwrap_or(true);
        if !has {
            if let Ok(s) = open_session(&a) {
                if let Ok(mut g) = st.draft_sess.lock() {
                    if g.is_none() {
                        *g = Some((a, s));
                        return;
                    }
                }
                finish(s);
            }
        }
    });
}

// ------------------------------------------------------------------ contacts

#[derive(serde::Serialize)]
struct Contact {
    name: String,
    email: String,
    score: u32,
}

/// Address-book suggestions built from every cached header (senders, and recipients of mail you sent).
async fn list_contacts(app: AppHandle, username: String) -> Result<Vec<Contact>, String> {
    crate::shim::spawn_blocking(move || {
        let a = acct(&username);
        let st = app.state::<AppState>();
        let rows = st.db.contact_rows(&a)?;
        let mut map: HashMap<String, (String, u32)> = HashMap::new();
        let mut add = |name: &str, email: &str, w: u32| {
            let e = email.trim().trim_matches(|c| c == '<' || c == '>').to_lowercase();
            if e.is_empty() || !e.contains('@') || e == a || e.starts_with("noreply") || e.starts_with("no-reply") || e.contains("mailer-daemon") || e.contains(' ') {
                return;
            }
            let ent = map.entry(e).or_insert_with(|| (String::new(), 0));
            if ent.0.is_empty() && !name.trim().is_empty() {
                ent.0 = name.trim().trim_matches('"').to_string();
            }
            ent.1 += w;
        };
        for (fname, femail, to, role) in rows {
            let sent = role == "sent";
            if !sent {
                add(&fname, &femail, 1);
            }
            for part in to.split(',') {
                let p = part.trim();
                if p.is_empty() {
                    continue;
                }
                let (n, e) = match (p.rfind('<'), p.rfind('>')) {
                    (Some(l), Some(r)) if r > l => (p[..l].trim(), &p[l + 1..r]),
                    _ => ("", p),
                };
                add(n, e, if sent { 5 } else { 1 });
            }
        }
        let mut v: Vec<Contact> = map.into_iter().map(|(email, (name, score))| Contact { name, email, score }).collect();
        v.sort_by(|x, y| y.score.cmp(&x.score).then(x.email.cmp(&y.email)));
        v.truncate(4000);
        Ok(v)
    })
    .await
    .map_err(err)?
}

// ------------------------------------------------------------------ scheduled send
//
// A scheduled message is a file in <data>/scheduled. A detached helper process (a private copy of this program,
// started with --send-scheduled) sends the messages when they are due, so they still go out after WorkDesk is closed.
// WorkDesk only has to be running (or started again) for the helper to be launched; it needs the computer to be on.

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ScheduledItem {
    id: String,
    send_at: i64,
    #[serde(default)]
    next_try: i64,
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    error: Option<String>,
    req: SendRequest,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ScheduledInfo {
    id: String,
    send_at: i64,
    to: String,
    subject: String,
    attempts: u32,
    error: Option<String>,
}

fn data_dir(p: &crate::shim::PathResolver) -> Result<PathBuf, String> {
    let d = p.app_data_dir().map_err(err)?;
    fs::create_dir_all(&d).map_err(err)?;
    Ok(d)
}

fn sched_dir(p: &crate::shim::PathResolver) -> Result<PathBuf, String> {
    let d = data_dir(p)?.join("scheduled");
    fs::create_dir_all(&d).map_err(err)?;
    Ok(d)
}

/// Scheduled messages waiting (`.json`) and, with `sending`, those a sender has claimed right now (`.sending`).
fn sched_load(p: &crate::shim::PathResolver, sending: bool) -> Vec<ScheduledItem> {
    let Ok(dir) = sched_dir(p) else { return vec![] };
    let mut out: Vec<ScheduledItem> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.ends_with(".json") || (sending && n.ends_with(".sending"))
                })
                .filter_map(|e| fs::read_to_string(e.path()).ok())
                .filter_map(|t| serde_json::from_str(&t).ok())
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|i| i.send_at);
    out
}

fn sched_path(p: &crate::shim::PathResolver, id: &str, ext: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("Invalid scheduled message id".into());
    }
    Ok(sched_dir(p)?.join(format!("{id}.{ext}")))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn sched_log(p: &crate::shim::PathResolver, v: serde_json::Value) {
    use std::io::Write;
    if let Ok(d) = data_dir(p) {
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(d.join("scheduled-log.jsonl")) {
            let _ = writeln!(f, "{v}");
        }
    }
}

/// Send one due message. Claiming it (rename to `.sending`) is atomic, so two senders can never send it twice.
fn run_scheduled_in(p: &crate::shim::PathResolver, mut item: ScheduledItem) {
    let (Ok(json), Ok(busy)) = (sched_path(p, &item.id, "json"), sched_path(p, &item.id, "sending")) else { return };
    if fs::rename(&json, &busy).is_err() {
        return;
    }
    let res = smtp_send(&item.req);
    match res {
        Ok(()) => {
            let _ = fs::remove_file(&busy);
            sched_log(p, serde_json::json!({ "id": item.id, "ok": true, "subject": item.req.subject, "to": item.req.to, "at": now_ms() }));
        }
        Err(e) => {
            item.attempts += 1;
            item.error = Some(e.clone());
            item.next_try = now_ms() + 60_000;
            if let Ok(t) = serde_json::to_string(&item) {
                let _ = fs::write(&json, t);
            }
            let _ = fs::remove_file(&busy);
            sched_log(p, serde_json::json!({ "id": item.id, "ok": false, "subject": item.req.subject, "to": item.req.to, "error": e, "attempts": item.attempts, "at": now_ms() }));
        }
    }
}

fn sched_pending(p: &crate::shim::PathResolver) -> bool {
    sched_load(p, true).iter().any(|i| i.attempts < 3)
}

fn pid_alive(pid: u32) -> bool {
    fs::read(format!("/proc/{pid}/cmdline")).map(|c| String::from_utf8_lossy(&c).contains("--send-scheduled")).unwrap_or(false)
}

fn sender_running(p: &crate::shim::PathResolver) -> bool {
    let Ok(d) = data_dir(p) else { return false };
    fs::read_to_string(d.join("sender.pid")).ok().and_then(|t| t.trim().parse::<u32>().ok()).map(pid_alive).unwrap_or(false)
}

/// Start the detached sender if it is not already running. It runs from a private copy of the program (the AppImage
/// disappears when WorkDesk closes) together with the OpenSSL libraries it was shipped with.
fn ensure_sender(p: &crate::shim::PathResolver) -> Result<(), String> {
    if sender_running(p) {
        return Ok(());
    }
    let data = data_dir(p)?;
    let me = std::env::current_exe().map_err(err)?;
    let dst = data.join("workdesk-sender");
    let stamp = format!("{} {}", env!("CARGO_PKG_VERSION"), fs::metadata(&me).map(|m| m.len()).unwrap_or(0));
    if !dst.exists() || fs::read_to_string(data.join("sender.ver")).ok().as_deref() != Some(stamp.as_str()) {
        let tmp = data.join("workdesk-sender.new");
        fs::copy(&me, &tmp).map_err(err)?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755));
        }
        fs::rename(&tmp, &dst).map_err(err)?;
        let _ = fs::write(data.join("sender.ver"), &stamp);
    }
    let libdir = data.join("sender-lib");
    if let Ok(ld) = std::env::var("LD_LIBRARY_PATH") {
        for dir in ld.split(':').filter(|d| !d.is_empty()) {
            for lib in ["libssl.so.3", "libcrypto.so.3"] {
                let src = PathBuf::from(dir).join(lib);
                if src.exists() {
                    let _ = fs::create_dir_all(&libdir);
                    let to = libdir.join(lib);
                    if fs::metadata(&to).map(|m| m.len()).ok() != fs::metadata(&src).map(|m| m.len()).ok() {
                        let _ = fs::copy(&src, &to);
                    }
                }
            }
        }
    }
    let mut c = Command::new(&dst);
    c.arg("--send-scheduled")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env_remove("WORKDESK_SHELL")
        .process_group(0);
    if libdir.exists() {
        c.env("LD_LIBRARY_PATH", &libdir);
    } else {
        c.env_remove("LD_LIBRARY_PATH");
    }
    c.spawn().map(|_| ()).map_err(err)
}

/// Entry point of the detached sender (`workdesk-chrome --send-scheduled`): sends due messages until none are left.
pub fn run_sender() {
    let p = crate::shim::PathResolver::new();
    let Ok(data) = data_dir(&p) else { return };
    let pidf = data.join("sender.pid");
    if sender_running(&p) {
        let me = std::process::id();
        let other = fs::read_to_string(&pidf).ok().and_then(|t| t.trim().parse::<u32>().ok());
        if other != Some(me) {
            return;
        }
    }
    let _ = fs::write(&pidf, std::process::id().to_string());
    let mut idle = 0;
    loop {
        let now = now_ms();
        let items = sched_load(&p, false);
        if items.iter().any(|i| i.attempts < 3) || !sched_load(&p, true).is_empty() {
            idle = 0;
        } else {
            idle += 1;
            if idle >= 3 {
                break;
            }
        }
        for item in items {
            if item.send_at <= now && item.next_try <= now && item.attempts < 3 {
                run_scheduled_in(&p, item);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    let _ = fs::remove_file(&pidf);
}

/// In the app: keep the sender alive while something is waiting, give back files a crashed sender left claimed,
/// and turn the sender's log into "scheduled" events (so a result that happened while WorkDesk was closed is still reported).
pub fn start_scheduler(app: AppHandle) {
    std::thread::spawn(move || {
        let p = app.path();
        let seen_file = data_dir(&p).map(|d| d.join("scheduled-log.seen")).ok();
        let log_file = data_dir(&p).map(|d| d.join("scheduled-log.jsonl")).ok();
        loop {
            if sched_pending(&p) && !sender_running(&p) {
                // a claimed file with no sender alive was left by a crash: make it available again
                if let Ok(dir) = sched_dir(&p) {
                    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
                        let path = e.path();
                        if path.extension().map(|x| x == "sending").unwrap_or(false) {
                            let _ = fs::rename(&path, path.with_extension("json"));
                        }
                    }
                }
                let _ = ensure_sender(&p);
            }
            if let (Some(seen), Some(log)) = (&seen_file, &log_file) {
                let done: u64 = fs::read_to_string(seen).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
                if let Ok(text) = fs::read(log) {
                    if (text.len() as u64) > done {
                        let chunk = String::from_utf8_lossy(&text[done as usize..]).into_owned();
                        for line in chunk.lines() {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                                let _ = app.emit("scheduled", v);
                            }
                        }
                        let _ = fs::write(seen, text.len().to_string());
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(8));
        }
    });
}

fn schedule_send(app: AppHandle, req: SendRequest, send_at: i64) -> Result<String, String> {
    mail::build_message(&req, true)?; // validate recipients and attachments now, not at send time
    if send_at < now_ms() - 60_000 {
        return Err("Choose a time in the future".into());
    }
    let p = app.path();
    let id = format!("{}-{:04x}", now_ms(), std::process::id() & 0xffff);
    let item = ScheduledItem { id: id.clone(), send_at, next_try: 0, attempts: 0, error: None, req };
    fs::write(sched_path(&p, &id, "json")?, serde_json::to_string(&item).map_err(err)?).map_err(err)?;
    let _ = ensure_sender(&p);
    Ok(id)
}

fn list_scheduled(app: AppHandle) -> Vec<ScheduledInfo> {
    sched_load(&app.path(), true)
        .into_iter()
        .map(|i| ScheduledInfo { id: i.id, send_at: i.send_at, to: i.req.to, subject: i.req.subject, attempts: i.attempts, error: i.error })
        .collect()
}

fn cancel_scheduled(app: AppHandle, id: String) -> Result<(), String> {
    let p = app.path();
    if sched_path(&p, &id, "sending")?.exists() {
        return Err("This message is being sent right now".into());
    }
    fs::remove_file(sched_path(&p, &id, "json")?).map_err(err)
}

/// Bring a scheduled message forward (also retries one that failed).
fn send_scheduled_now(app: AppHandle, id: String) -> Result<(), String> {
    let p = app.path();
    let path = sched_path(&p, &id, "json")?;
    let mut item: ScheduledItem = serde_json::from_str(&fs::read_to_string(&path).map_err(err)?).map_err(err)?;
    item.send_at = now_ms();
    item.next_try = 0;
    item.attempts = 0;
    fs::write(&path, serde_json::to_string(&item).map_err(err)?).map_err(err)?;
    let _ = ensure_sender(&p);
    Ok(())
}

fn save_attachment(app: AppHandle, filename: String, data_base64: String) -> Result<String, String> {
    let bytes = BASE64.decode(data_base64.as_bytes()).map_err(err)?;
    if bytes.len() > 100 * 1024 * 1024 {
        return Err("Attachment is larger than the 100 MB save limit".into());
    }
    let dir = app.path().download_dir().map_err(err)?;
    fs::create_dir_all(&dir).map_err(err)?;
    let name = mail::safe_filename(&filename);
    let mut path = dir.join(&name);
    if path.exists() {
        let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("attachment").to_string();
        let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("").to_string();
        let mut n = 2;
        loop {
            let candidate = if ext.is_empty() { dir.join(format!("{stem} ({n})")) } else { dir.join(format!("{stem} ({n}).{ext}")) };
            if !candidate.exists() {
                path = candidate;
                break;
            }
            n += 1;
            if n > 10000 {
                return Err("Could not choose a unique download filename".into());
            }
        }
    }
    fs::write(&path, bytes).map_err(err)?;
    Ok(path.to_string_lossy().into_owned())
}

// ------------------------------------------------------------------ preferences

fn preferences_path(app: &AppHandle) -> Result<PathBuf, String> {
    let d = app.path().app_config_dir().map_err(err)?;
    fs::create_dir_all(&d).map_err(err)?;
    Ok(d.join("preferences.json"))
}

/// Preferences are a free-form JSON object, so settings added by the UI are never dropped.
fn load_preferences(app: AppHandle) -> Result<serde_json::Value, String> {
    let p = preferences_path(&app)?;
    if !p.exists() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_slice(&fs::read(p).map_err(err)?).or_else(|_| Ok(serde_json::json!({})))
}

fn save_preferences(app: AppHandle, preferences: serde_json::Value) -> Result<(), String> {
    let p = preferences_path(&app)?;
    let bytes = serde_json::to_vec_pretty(&preferences).map_err(err)?;
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, bytes).map_err(err)?;
    fs::rename(tmp, p).map_err(err)
}

// -------------------------------------------------------------------- browsers

const BROWSERS: [(&str, &str); 10] = [
    ("chromium", "Chromium"),
    ("chromium-browser", "Chromium Browser"),
    ("google-chrome", "Google Chrome"),
    ("google-chrome-stable", "Google Chrome Stable"),
    ("brave-browser", "Brave"),
    ("firefox", "Firefox"),
    ("firefox-esr", "Firefox ESR"),
    ("microsoft-edge", "Microsoft Edge"),
    ("vivaldi-stable", "Vivaldi"),
    ("opera", "Opera"),
];

fn list_browsers() -> Vec<String> {
    BROWSERS
        .iter()
        .filter_map(|(bin, label)| {
            find_exe(bin).map(|_| format!("{bin}|{label}"))
        })
        .collect()
}

fn open_external(url: String, browser: Option<String>) -> Result<(), String> {
    let u = url.trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        return Err("Only http and https URLs can be opened".into());
    }
    let bin = browser.as_deref().filter(|b| BROWSERS.iter().any(|(n, _)| n == b)).unwrap_or("xdg-open");
    Command::new(bin).arg(u).spawn().map(|_| ()).map_err(err)
}


const CHROME_FAMILY: [&str; 16] = [
    "google-chrome",
    "google-chrome-stable",
    "google-chrome-beta",
    "google-chrome-unstable",
    "chrome",
    "chromium",
    "chromium-browser",
    "brave-browser",
    "brave-browser-stable",
    "brave",
    "microsoft-edge",
    "microsoft-edge-stable",
    "vivaldi",
    "vivaldi-stable",
    "opera",
    "thorium-browser",
];

/// Fixed install locations that are often missing from an AppImage's PATH.
const CHROME_PATHS: [&str; 10] = [
    "/opt/google/chrome/chrome",
    "/opt/google/chrome/google-chrome",
    "/opt/brave.com/brave/brave-browser",
    "/opt/microsoft/msedge/microsoft-edge",
    "/opt/vivaldi/vivaldi",
    "/usr/lib/chromium/chromium",
    "/usr/lib/chromium-browser/chromium-browser",
    "/snap/bin/chromium",
    "/var/lib/flatpak/exports/bin/com.google.Chrome",
    "/var/lib/flatpak/exports/bin/org.chromium.Chromium",
];

/// Find an executable by name using PATH plus the usual system directories (no `which` needed).
fn find_exe(name: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if name.contains('/') {
        let p = std::path::PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs: Vec<String> = std::env::var("PATH").unwrap_or_default().split(':').map(String::from).collect();
    for d in ["/usr/bin", "/usr/local/bin", "/bin", "/snap/bin", "/opt/google/chrome", "/var/lib/flatpak/exports/bin"] {
        dirs.push(d.into());
    }
    dirs.push(format!("{home}/.local/bin"));
    dirs.push(format!("{home}/.local/share/flatpak/exports/bin"));
    dirs.into_iter()
        .filter(|d| !d.is_empty())
        .map(|d| std::path::Path::new(&d).join(name))
        .find(|p| p.is_file() && p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false))
}

fn is_chromium_exec(exe: &str) -> bool {
    let n = exe.rsplit('/').next().unwrap_or(exe).to_lowercase();
    ["chrome", "chromium", "brave", "edge", "vivaldi", "opera", "thorium"].iter().any(|k| n.contains(k))
}

/// The default browser from xdg-settings / the .desktop file, if it is Chromium based.
fn default_browser_exec() -> Option<String> {
    let out = Command::new("xdg-settings").args(["get", "default-web-browser"]).output().ok()?;
    let desktop = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if desktop.is_empty() {
        return None;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs = [
        format!("{home}/.local/share/applications"),
        "/usr/share/applications".to_string(),
        "/usr/local/share/applications".to_string(),
        "/var/lib/flatpak/exports/share/applications".to_string(),
        format!("{home}/.local/share/flatpak/exports/share/applications"),
        "/var/lib/snapd/desktop/applications".to_string(),
    ];
    for d in dirs {
        let Ok(text) = std::fs::read_to_string(std::path::Path::new(&d).join(&desktop)) else { continue };
        let line = text.lines().find(|l| l.starts_with("Exec="))?;
        let exec: Vec<&str> = line[5..].split_whitespace().filter(|t| !t.starts_with('%')).collect();
        let first = *exec.first()?;
        if first == "flatpak" || first == "env" || first == "/usr/bin/env" {
            // keep the whole command line (e.g. flatpak run ... com.google.Chrome)
            if exec.iter().any(|t| is_chromium_exec(t)) {
                return Some(exec.join(" "));
            }
            return None;
        }
        return is_chromium_exec(first).then(|| first.to_string());
    }
    None
}

/// Remove the AppImage's library overrides so a Chrome started from here does not inherit them.
pub fn clean_env(cmd: &mut Command) {
    for k in ["LD_LIBRARY_PATH", "LD_PRELOAD", "GTK_PATH", "GIO_MODULE_DIR", "GIO_EXTRA_MODULES", "GDK_PIXBUF_MODULE_FILE", "GDK_PIXBUF_MODULEDIR", "GSETTINGS_SCHEMA_DIR", "GTK_EXE_PREFIX", "GTK_DATA_PREFIX", "PYTHONHOME", "PYTHONPATH", "WEBKIT_EXEC_PATH", "APPDIR", "APPIMAGE", "ARGV0", "OWD"] {
        cmd.env_remove(k);
    }
    if let Ok(path) = std::env::var("PATH") {
        let clean: Vec<&str> = path.split(':').filter(|p| !p.contains("/tmp/.mount_") && !p.contains("/.mount_")).collect();
        cmd.env("PATH", clean.join(":"));
    }
}

/// Launch from a clean environment so a Chrome started from the AppImage does not inherit its libraries.
fn launch(cmdline: &[String], url: &str) -> Result<(), String> {
    let mut cmd = Command::new(&cmdline[0]);
    cmd.args(&cmdline[1..]).arg(url);
    clean_env(&mut cmd);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn().map(|_| ()).map_err(err)
}

/// Every way of starting a Chromium-based browser that we can find, best first.
pub fn chrome_candidates(browser: Option<&str>, allow_flatpak: bool) -> Vec<Vec<String>> {
    let mut candidates: Vec<Vec<String>> = vec![];
    if let Ok(p) = std::env::var("WORKDESK_CHROME_BIN") {
        if find_exe(&p).is_some() {
            candidates.push(vec![p]);
        }
    }
    if let Some(b) = browser.filter(|b| CHROME_FAMILY.contains(b)) {
        if let Some(p) = find_exe(b) {
            candidates.push(vec![p.to_string_lossy().into()]);
        }
    }
    for n in ["google-chrome", "google-chrome-stable", "chrome"] {
        if let Some(p) = find_exe(n) {
            candidates.push(vec![p.to_string_lossy().into()]);
        }
    }
    if let Some(d) = default_browser_exec() {
        let parts: Vec<String> = d.split_whitespace().map(String::from).collect();
        if let Some(p) = find_exe(&parts[0]) {
            if parts[0] != "flatpak" || allow_flatpak {
                let mut v = vec![p.to_string_lossy().to_string()];
                v.extend(parts[1..].iter().cloned());
                candidates.push(v);
            }
        }
    }
    for n in CHROME_FAMILY {
        if let Some(p) = find_exe(n) {
            candidates.push(vec![p.to_string_lossy().into()]);
        }
    }
    for p in CHROME_PATHS {
        if let Some(p) = find_exe(p) {
            candidates.push(vec![p.to_string_lossy().into()]);
        }
    }
    if allow_flatpak {
        for flat in ["com.google.Chrome", "org.chromium.Chromium", "com.brave.Browser", "com.microsoft.Edge"] {
            let installed = Command::new("flatpak").args(["info", flat]).output().map(|o| o.status.success()).unwrap_or(false);
            if installed {
                candidates.push(vec!["flatpak".into(), "run".into(), flat.into()]);
            }
        }
    }
    let mut seen = HashSet::new();
    candidates.retain(|c| seen.insert(c.join(" ")));
    candidates
}

/// Open a page (including chrome:// and extension pages) as a new tab of WorkDesk's own Chrome window.
/// Falls back to starting a separate Chrome only if the kiosk window is not available.
fn open_in_chrome(url: String, browser: Option<String>) -> Result<String, String> {
    let u = crate::chrome::check_chrome_url(&url)?;
    if let Ok(c) = crate::chrome::get() {
        c.open_any(&u)?;
        return Ok("WorkDesk Chrome".into());
    }
    let mut last = String::new();
    for c in chrome_candidates(browser.as_deref(), true) {
        match launch(&c, &u) {
            Ok(()) => return Ok(c.last().cloned().unwrap_or_default().rsplit('/').next().unwrap_or("browser").to_string()),
            Err(e) => last = e,
        }
    }
    Err(if last.is_empty() {
        "No Chromium-based browser was found on this computer (looked in PATH, /opt, snap, flatpak and your default-browser setting).".into()
    } else {
        format!("Could not start the browser: {last}")
    })
}

// ------------------------------------------------------------------ notepad storage

fn notes_path(app: &AppHandle) -> Result<PathBuf, String> {
    let d = app.path().app_data_dir().map_err(err)?;
    fs::create_dir_all(&d).map_err(err)?;
    Ok(d.join("notes.json"))
}

fn notes_load(app: AppHandle) -> Result<String, String> {
    let p = notes_path(&app)?;
    match fs::read_to_string(&p) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(err(e)),
    }
}

/// Atomic write (temp file + rename) so a crash never leaves a half-written notebook; keeps one .bak.
fn notes_save(app: AppHandle, data: String) -> Result<(), String> {
    if data.len() > 200 * 1024 * 1024 {
        return Err("The notebook is larger than 200 MB".into());
    }
    serde_json::from_str::<serde_json::Value>(&data).map_err(|e| format!("Refusing to save invalid notes data: {e}"))?;
    let p = notes_path(&app)?;
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, data).map_err(err)?;
    if p.exists() {
        let _ = fs::copy(&p, p.with_extension("json.bak"));
    }
    fs::rename(&tmp, &p).map_err(err)
}

/// Backup of every note: `<Downloads>/WorkDesk Notes/workdesk-notes-backup.json`.
/// An existing backup is first renamed to `workdesk-notes-backup.json.bak` (replacing an older .bak).
fn notes_backup(app: AppHandle, data: String) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(&data).map_err(|e| format!("Refusing to back up invalid notes data: {e}"))?;
    let dir = app.path().download_dir().map_err(err)?.join("WorkDesk Notes");
    fs::create_dir_all(&dir).map_err(err)?;
    let file = dir.join("workdesk-notes-backup.json");
    if file.exists() {
        let bak = dir.join("workdesk-notes-backup.json.bak");
        let _ = fs::remove_file(&bak);
        fs::rename(&file, &bak).map_err(err)?;
    }
    fs::write(&file, data).map_err(err)?;
    Ok(file.to_string_lossy().into_owned())
}

#[derive(serde::Deserialize)]
struct NoteFile {
    name: String,
    content: String,
}

/// Write each note as `<Downloads>/<subfolder>/<name>.md`.
fn notes_export(app: AppHandle, subfolder: String, files: Vec<NoteFile>) -> Result<String, String> {
    let sub = mail::safe_filename(subfolder.trim());
    let sub = if sub.is_empty() { "WorkDesk Notes".to_string() } else { sub };
    let dir = app.path().download_dir().map_err(err)?.join(sub);
    fs::create_dir_all(&dir).map_err(err)?;
    for f in files {
        let mut name = mail::safe_filename(&f.name);
        if name.is_empty() {
            name = "Untitled".into();
        }
        fs::write(dir.join(format!("{name}.md")), f.content).map_err(err)?;
    }
    Ok(dir.to_string_lossy().into_owned())
}

// ------------------------------------------------------------------ call support check

/// Report whether calls can work: WorkDesk Chrome runs sites in a real Chrome, so check Chrome and the camera/microphone devices.
async fn webrtc_support() -> Vec<String> {
    crate::shim::spawn_blocking(|| {
        let mut out = vec![];
        match chrome_candidates(None, false).into_iter().next() {
            None => out.push("FAIL|No Chrome / Chromium found - install Google Chrome (https://www.google.com/chrome/) and restart WorkDesk Chrome".into()),
            Some(c) => {
                let ver = Command::new(&c[0]).arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
                out.push(format!("OK|Browser engine: {} {}", c[0], ver));
            }
        }
        out.push(if crate::chrome::get().is_ok() {
            "OK|WorkDesk is running inside Chrome (WebRTC, getUserMedia and calls are built in)".into()
        } else {
            "FAIL|WorkDesk is not attached to its Chrome window - website tabs are unavailable".into()
        });
        let cams = fs::read_dir("/dev").map(|d| d.filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().starts_with("video")).count()).unwrap_or(0);
        out.push(if cams > 0 { format!("OK|{cams} camera device(s) found") } else { "FAIL|No /dev/video* camera device found".into() });
        let snd = std::path::Path::new("/dev/snd").exists() || std::path::Path::new("/run/user").exists();
        out.push(if snd { "OK|Audio system present".into() } else { "INFO|Could not detect an audio system".into() });
        out
    })
    .await
    .unwrap_or_else(|e| vec![format!("FAIL|Check failed: {e}")])
}

/// Move the data / config / cache folders of the old `com.maildesk.app` identifier to `com.workdesk.app`.
pub fn migrate_old_dirs(p: &PathResolver) {
    for new in [p.app_data_dir(), p.app_config_dir(), p.app_cache_dir()].into_iter().flatten() {
        let Some(parent) = new.parent() else { continue };
        let old = parent.join("com.maildesk.app");
        if old.exists() && !new.exists() {
            if fs::rename(&old, &new).is_err() {
                eprintln!("could not move {} to {}", old.display(), new.display());
            }
        }
    }
}


// -------------------------------------------------------------------- RPC dispatch

fn to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in s.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn arg<T: serde::de::DeserializeOwned>(a: &Value, snake: &str) -> Result<T, String> {
    let v = a.get(to_camel(snake)).or_else(|| a.get(snake)).cloned().unwrap_or(Value::Null);
    serde_json::from_value(v).map_err(|e| format!("Invalid argument '{snake}': {e}"))
}

fn out<T: serde::Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(err)
}

/// Run one command by name (the names and argument names are the ones the v1.0 frontend already uses).
pub fn dispatch(app: &AppHandle, cmd: &str, a: Value) -> Result<Value, String> {
    use crate::shim::block_on;
    let st = || app.state::<AppState>();
    match cmd {
        "save_credentials" => out(save_credentials(arg(&a, "username")?, arg(&a, "app_password")?)?),
        "delete_credentials" => out(delete_credentials(st(), arg(&a, "username")?)?),
        "test_connection" => out(block_on(test_connection(arg(&a, "username")?))?),
        "list_folders" => out(block_on(list_folders(st(), arg(&a, "username")?, arg(&a, "refresh")?))?),
        "create_folder" => out(block_on(create_folder(st(), arg(&a, "username")?, arg(&a, "folder")?))?),
        "delete_folder" => out(block_on(delete_folder(st(), arg(&a, "username")?, arg(&a, "folder")?))?),
        "start_sync" => out(start_sync(app.clone(), st(), arg(&a, "username")?, arg::<Option<String>>(&a, "priority")?)?),
        "sync_folder_now" => out(block_on(sync_folder_now(app.clone(), st(), arg(&a, "username")?, arg(&a, "folder")?))?),
        "sync_running" => out(sync_running(st())),
        "clear_cache" => out(clear_cache(st(), arg(&a, "username")?)?),
        "list_page" => out(list_page(st(), arg(&a, "username")?, arg(&a, "folder")?, arg(&a, "grouped")?, arg(&a, "sort")?, arg(&a, "desc")?, arg(&a, "page")?, arg(&a, "page_size")?)?),
        "search_mail" => out(block_on(search_mail(st(), arg(&a, "username")?, arg(&a, "query")?, arg(&a, "grouped")?, arg(&a, "sort")?, arg(&a, "desc")?, arg(&a, "page")?, arg(&a, "page_size")?, arg(&a, "fresh")?))?),
        "thread_messages" => out(block_on(thread_messages(st(), arg(&a, "username")?, arg(&a, "thread_id")?, arg(&a, "folder")?, arg(&a, "uid")?, arg::<Option<bool>>(&a, "refresh")?))?),
        "get_message" => out(block_on(get_message(st(), arg(&a, "username")?, arg(&a, "folder")?, arg(&a, "uid")?, arg::<Option<bool>>(&a, "mark_seen")?))?),
        "prefetch_message" => out(prefetch_message(st(), arg(&a, "username")?, arg(&a, "folder")?, arg(&a, "uid")?)?),
        "warm_inbox" => out(warm_inbox(app.clone(), arg(&a, "username")?)),
        "folder_validity" => out(block_on(folder_validity(st(), arg(&a, "username")?, arg(&a, "folder")?))?),
        "discard_draft" => out(block_on(discard_draft(st(), arg(&a, "username")?, arg(&a, "folder")?, arg(&a, "uid")?, arg::<Option<u32>>(&a, "validity")?))?),
        "add_to_inbox" => out(block_on(add_to_inbox(st(), arg(&a, "username")?, arg(&a, "refs")?))?),
        "get_attachment" => out(block_on(get_attachment(arg(&a, "username")?, arg(&a, "folder")?, arg(&a, "uid")?, arg(&a, "index")?))?),
        "set_flag" => out(block_on(set_flag(st(), arg(&a, "username")?, arg(&a, "refs")?, arg(&a, "flag")?, arg(&a, "enabled")?))?),
        "move_messages" => out(block_on(move_messages(st(), arg(&a, "username")?, arg(&a, "refs")?, arg(&a, "target")?))?),
        "delete_forever" => out(block_on(delete_forever(st(), arg(&a, "username")?, arg(&a, "refs")?))?),
        "send_email" => out(block_on(send_email(arg(&a, "req")?))?),
        "save_draft" => out(block_on(save_draft(app.clone(), arg(&a, "req")?, arg::<Option<u32>>(&a, "replace_uid")?, arg::<Option<u32>>(&a, "replace_validity")?))?),
        "warm_connections" => out(warm_connections(app.clone(), arg(&a, "username")?)),
        "notes_load" => out(notes_load(app.clone())?),
        "notes_save" => out(notes_save(app.clone(), arg(&a, "data")?)?),
        "notes_export" => out(notes_export(app.clone(), arg(&a, "subfolder")?, arg(&a, "files")?)?),
        "notes_backup" => out(notes_backup(app.clone(), arg(&a, "data")?)?),
        "webrtc_support" => out(block_on(webrtc_support())),
        "list_contacts" => out(block_on(list_contacts(app.clone(), arg(&a, "username")?))?),
        "schedule_send" => out(schedule_send(app.clone(), arg(&a, "req")?, arg(&a, "send_at")?)?),
        "list_scheduled" => out(list_scheduled(app.clone())),
        "cancel_scheduled" => out(cancel_scheduled(app.clone(), arg(&a, "id")?)?),
        "send_scheduled_now" => out(send_scheduled_now(app.clone(), arg(&a, "id")?)?),
        "save_attachment" => out(save_attachment(app.clone(), arg(&a, "filename")?, arg(&a, "data_base64")?)?),
        "load_preferences" => out(load_preferences(app.clone())?),
        "save_preferences" => out(save_preferences(app.clone(), arg(&a, "preferences")?)?),
        "list_browsers" => out(list_browsers()),
        "open_external" => out(open_external(arg(&a, "url")?, arg::<Option<String>>(&a, "browser")?)?),
        "open_in_chrome" => out(open_in_chrome(arg(&a, "url")?, arg::<Option<String>>(&a, "browser")?)?),
        // website tabs = real Chrome tabs
        "site_open" => out(crate::chrome::get()?.open(&arg::<String>(&a, "label")?, &arg::<String>(&a, "url")?, arg::<Option<bool>>(&a, "embed")?.unwrap_or(false))?),
        "site_layout" => Ok(Value::Null),
        "site_show" => out(crate::chrome::get()?.show(arg::<Option<String>>(&a, "label")?.as_deref())?),
        "site_close" => out(crate::chrome::get()?.close(&arg::<String>(&a, "label")?)?),
        "site_action" => out(crate::chrome::get()?.action(&arg::<String>(&a, "label")?, &arg::<String>(&a, "action")?, arg::<Option<String>>(&a, "url")?)?),
        "site_state" => out(crate::chrome::get()?.state(&arg::<String>(&a, "label")?)?),
        "site_probe" => out(crate::chrome::get()?.probe(&arg::<String>(&a, "label")?)?),
        "win_action" => out(crate::chrome::get()?.window(&arg::<String>(&a, "action")?)?),
        "app_info" => out(serde_json::json!({ "chrome": crate::chrome::get().is_ok(), "version": env!("CARGO_PKG_VERSION"), "error": crate::chrome::get().err().unwrap_or_default(), "log": crate::chrome::log_path(), "kiosk": crate::chrome::is_kiosk(), "failed": !crate::chrome::start_error().is_empty() })),
        _ => Err(format!("Unknown command: {cmd}")),
    }
}
