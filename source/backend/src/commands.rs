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
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
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
    let entry = keyring::Entry::new(SERVICE, account).map_err(err)?;
    match entry.get_password() {
        Ok(p) => Ok(p),
        Err(first) => {
            // Saved by an earlier MailDesk build: migrate it to the new service name.
            if let Ok(p) = keyring::Entry::new(OLD_SERVICE, account).and_then(|e| e.get_password()) {
                let _ = entry.set_password(&p);
                return Ok(p);
            }
            Err(format!("No saved App Password for {account}: {first}"))
        }
    }
}

fn open_session(account: &str) -> Result<Sess, String> {
    let password = secret(account)?;
    imapx::connect(account, &password)
}

fn finish(mut s: Sess) {
    let _ = s.logout();
}

// ------------------------------------------------------------------ credentials

fn save_credentials(username: String, app_password: String) -> Result<(), String> {
    let u = acct(&username);
    let p: String = app_password.chars().filter(|c| !c.is_whitespace()).collect();
    if !u.contains('@') || p.is_empty() {
        return Err("Enter a valid Gmail address and App Password".into());
    }
    keyring::Entry::new(SERVICE, &u).map_err(err)?.set_password(&p).map_err(err)
}

fn delete_credentials(state: State<'_, AppState>, username: String) -> Result<(), String> {
    let a = acct(&username);
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
    if let Ok(mut sess) = open_session(&a) {
        for f in &scope {
            let _ = imapx::refresh_thread_in(&mut sess, &state.db, &a, f, &thrid);
        }
        finish(sess);
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
    Ok(out)
}

// ---------------------------------------------------------------------- messages

async fn get_message(state: State<'_, AppState>, username: String, folder: String, uid: u32) -> Result<MailDetail, String> {
    let a = acct(&username);
    let thread_id = state.db.rows_for_uids(&a, &folder, &[uid])?.first().map(|m| m.thrid.clone()).filter(|t| !t.is_empty());
    let mut sess = open_session(&a)?;
    let detail = mail::fetch_message(&mut sess, &folder, uid, thread_id);
    finish(sess);
    let detail = detail?;
    let _ = state.db.apply_flag(&a, &[MsgRef { folder: folder.clone(), uid }], "Seen", true);
    Ok(detail)
}

async fn get_attachment(username: String, folder: String, uid: u32, index: u32) -> Result<AttachmentData, String> {
    let mut sess = open_session(&acct(&username))?;
    let r = mail::fetch_attachment(&mut sess, &folder, uid, index);
    finish(sess);
    r
}

async fn set_flag(state: State<'_, AppState>, username: String, refs: Vec<MsgRef>, flag: String, enabled: bool) -> Result<(), String> {
    let a = acct(&username);
    // The cache is updated first so the list reacts at once; the server follows.
    state.db.apply_flag(&a, &refs, &flag, enabled)?;
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

fn sched_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let d = app.path().app_data_dir().map_err(err)?.join("scheduled");
    fs::create_dir_all(&d).map_err(err)?;
    Ok(d)
}

fn sched_load(app: &AppHandle) -> Vec<ScheduledItem> {
    let Ok(dir) = sched_dir(app) else { return vec![] };
    let mut out: Vec<ScheduledItem> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| fs::read_to_string(e.path()).ok()).filter_map(|t| serde_json::from_str(&t).ok()).collect())
        .unwrap_or_default();
    out.sort_by_key(|i| i.send_at);
    out
}

fn sched_path(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("Invalid scheduled message id".into());
    }
    Ok(sched_dir(app)?.join(format!("{id}.json")))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

static IN_FLIGHT: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn run_scheduled(app: &AppHandle, mut item: ScheduledItem) {
    {
        let Ok(mut g) = IN_FLIGHT.lock() else { return };
        if g.contains(&item.id) {
            return;
        }
        g.push(item.id.clone());
    }
    let path = sched_path(app, &item.id);
    let res = smtp_send(&item.req);
    if let Ok(mut g) = IN_FLIGHT.lock() {
        g.retain(|x| *x != item.id);
    }
    let Ok(path) = path else { return };
    match res {
        Ok(()) => {
            let _ = fs::remove_file(&path);
            let _ = app.emit("scheduled", serde_json::json!({ "id": item.id, "ok": true, "subject": item.req.subject, "to": item.req.to }));
        }
        Err(e) => {
            item.attempts += 1;
            item.error = Some(e.clone());
            item.next_try = now_ms() + 60_000;
            if let Ok(t) = serde_json::to_string(&item) {
                let _ = fs::write(&path, t);
            }
            let _ = app.emit("scheduled", serde_json::json!({ "id": item.id, "ok": false, "subject": item.req.subject, "error": e, "attempts": item.attempts }));
        }
    }
}

pub fn start_scheduler(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        let now = now_ms();
        for item in sched_load(&app) {
            if item.send_at <= now && item.next_try <= now && item.attempts < 3 {
                run_scheduled(&app, item);
            }
        }
    });
}

fn schedule_send(app: AppHandle, req: SendRequest, send_at: i64) -> Result<String, String> {
    mail::build_message(&req, true)?; // validate recipients and attachments now, not at send time
    if send_at < now_ms() - 60_000 {
        return Err("Choose a time in the future".into());
    }
    let id = format!("{}-{:04x}", now_ms(), std::process::id() & 0xffff);
    let item = ScheduledItem { id: id.clone(), send_at, next_try: 0, attempts: 0, error: None, req };
    fs::write(sched_path(&app, &id)?, serde_json::to_string(&item).map_err(err)?).map_err(err)?;
    Ok(id)
}

fn list_scheduled(app: AppHandle) -> Vec<ScheduledInfo> {
    sched_load(&app)
        .into_iter()
        .map(|i| ScheduledInfo { id: i.id, send_at: i.send_at, to: i.req.to, subject: i.req.subject, attempts: i.attempts, error: i.error })
        .collect()
}

fn cancel_scheduled(app: AppHandle, id: String) -> Result<(), String> {
    let p = sched_path(&app, &id)?;
    if IN_FLIGHT.lock().map(|g| g.contains(&id)).unwrap_or(false) {
        return Err("This message is being sent right now".into());
    }
    fs::remove_file(p).map_err(err)
}

/// Bring a scheduled message forward (also retries one that failed).
fn send_scheduled_now(app: AppHandle, id: String) -> Result<(), String> {
    let p = sched_path(&app, &id)?;
    let mut item: ScheduledItem = serde_json::from_str(&fs::read_to_string(&p).map_err(err)?).map_err(err)?;
    item.send_at = now_ms();
    item.next_try = 0;
    item.attempts = 0;
    fs::write(&p, serde_json::to_string(&item).map_err(err)?).map_err(err)?;
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
        "thread_messages" => out(block_on(thread_messages(st(), arg(&a, "username")?, arg(&a, "thread_id")?, arg(&a, "folder")?, arg(&a, "uid")?))?),
        "get_message" => out(block_on(get_message(st(), arg(&a, "username")?, arg(&a, "folder")?, arg(&a, "uid")?))?),
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
