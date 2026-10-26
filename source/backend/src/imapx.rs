//! IMAP layer: connection, folder discovery, header synchronisation into the local cache,
//! Gmail server-side search (X-GM-RAW) and message flag / move operations.
//!
//! FETCH responses are read as raw bytes and parsed with `imap-proto` directly, because the
//! typed `imap::types::Fetch` does not expose Gmail's X-GM-THRID / X-GM-MSGID attributes.

use crate::db::Db;
use crate::model::*;
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use imap::ClientBuilder;
use imap_proto::types::{AttributeValue, BodyStructure, Response};
use std::collections::{HashMap, HashSet};

pub type Sess = imap::Session<imap::Connection>;

pub fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Headers per FETCH command during synchronisation.
const HEADER_CHUNK: usize = 250;
/// Upper bound on hits examined per folder for one search.
pub const SEARCH_CAP: usize = 2000;
/// Upper bound on uncached search hits whose headers are downloaded per folder for one search.
pub const SEARCH_FETCH_CAP: usize = 400;

// ------------------------------------------------------------------ connection

/// (host, port, plaintext). Debug builds may point at a local test server through
/// `MAILDESK_IMAP_ADDR=host:port`; release builds always use Gmail over TLS.
fn endpoint() -> (String, u16, bool) {
    if cfg!(debug_assertions) {
        if let Ok(addr) = std::env::var("MAILDESK_IMAP_ADDR") {
            if let Some((h, p)) = addr.rsplit_once(':') {
                if let Ok(port) = p.parse::<u16>() {
                    return (h.to_string(), port, true);
                }
            }
        }
    }
    ("imap.gmail.com".to_string(), 993, false)
}

pub fn connect(username: &str, password: &str) -> Result<Sess, String> {
    let (host, port, plain) = endpoint();
    let mut builder = ClientBuilder::new(host.as_str(), port);
    if plain {
        builder = builder.mode(imap::ConnectionMode::Plaintext);
    }
    let client = builder.connect().map_err(err)?;
    client.login(username, password).map_err(|(e, _)| err(e))
}

// --------------------------------------------------------------------- folders

/// Decode IMAP modified UTF-7 (RFC 3501 §5.1.3) mailbox names for display.
pub fn decode_mutf7(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let mut enc = String::new();
        for ch in chars.by_ref() {
            if ch == '-' {
                break;
            }
            enc.push(ch);
        }
        if enc.is_empty() {
            out.push('&');
            continue;
        }
        let b64 = enc.replace(',', "/");
        match STANDARD_NO_PAD.decode(b64.trim_end_matches('=')) {
            Ok(bytes) => {
                let units: Vec<u16> = bytes.chunks_exact(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect();
                out.push_str(&String::from_utf16_lossy(&units));
            }
            Err(_) => {
                out.push('&');
                out.push_str(&enc);
                out.push('-');
            }
        }
    }
    out
}

fn attr_text(a: &imap_proto::NameAttribute<'_>) -> String {
    use imap_proto::NameAttribute as N;
    match a {
        N::NoInferiors => "\\Noinferiors".into(),
        N::NoSelect => "\\Noselect".into(),
        N::Marked => "\\Marked".into(),
        N::Unmarked => "\\Unmarked".into(),
        N::All => "\\All".into(),
        N::Archive => "\\Archive".into(),
        N::Drafts => "\\Drafts".into(),
        N::Flagged => "\\Flagged".into(),
        N::Junk => "\\Junk".into(),
        N::Sent => "\\Sent".into(),
        N::Trash => "\\Trash".into(),
        N::Extension(s) => s.to_string(),
        #[allow(unreachable_patterns)]
        other => format!("{other:?}"),
    }
}

/// Map a mailbox to a role from its RFC 6154 special-use attributes (language independent),
/// falling back to well-known English names.
pub fn role_for(name: &str, attrs: &[String]) -> Option<String> {
    if name.eq_ignore_ascii_case("INBOX") {
        return Some("inbox".into());
    }
    for a in attrs {
        let role = match a.to_ascii_lowercase().as_str() {
            "\\all" => "all",
            "\\archive" => "archive",
            "\\drafts" => "drafts",
            "\\flagged" => "starred",
            "\\junk" => "spam",
            "\\sent" => "sent",
            "\\trash" => "trash",
            "\\important" => "important",
            _ => continue,
        };
        return Some(role.into());
    }
    let tail = name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    let in_gmail = name.starts_with("[Gmail]") || name.starts_with("[Google Mail]");
    if in_gmail {
        return match tail.as_str() {
            "all mail" => Some("all".into()),
            "sent mail" => Some("sent".into()),
            "drafts" => Some("drafts".into()),
            "trash" | "bin" => Some("trash".into()),
            "spam" => Some("spam".into()),
            "starred" => Some("starred".into()),
            "important" => Some("important".into()),
            "snoozed" => Some("snoozed".into()),
            _ => None,
        };
    }
    None
}

pub fn list_folders(sess: &mut Sess) -> Result<Vec<FolderInfo>, String> {
    let names = sess.list(None, Some("*")).map_err(err)?;
    let mut out: Vec<FolderInfo> = Vec::new();
    for n in names.iter() {
        let attributes: Vec<String> = n.attributes().iter().map(attr_text).collect();
        let selectable = !attributes.iter().any(|a| a.eq_ignore_ascii_case("\\Noselect") || a.eq_ignore_ascii_case("\\NonExistent"));
        let name = n.name().to_string();
        let role = role_for(&name, &attributes);
        let display = decode_mutf7(&name);
        out.push(FolderInfo {
            display: match display.strip_prefix("[Gmail]/").or_else(|| display.strip_prefix("[Google Mail]/")) {
                Some(rest) => rest.to_string(),
                None => display,
            },
            name,
            delimiter: n.delimiter().map(|d| d.to_string()),
            attributes,
            role,
            selectable,
            ..Default::default()
        });
    }
    let rank = |f: &FolderInfo| match f.role.as_deref() {
        Some("inbox") => 0,
        Some("starred") => 1,
        Some("snoozed") => 2,
        Some("important") => 3,
        Some("sent") => 4,
        Some("drafts") => 5,
        Some("all") => 6,
        Some("spam") => 7,
        Some("trash") => 8,
        _ => 20,
    };
    out.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.display.to_lowercase().cmp(&b.display.to_lowercase())));
    Ok(out)
}

// ------------------------------------------------------------------ FETCH parse

/// Decode an IMAP envelope string (RFC 2047 encoded words included).
pub fn decode_header_text(v: Option<&[u8]>) -> String {
    let Some(bytes) = v else { return String::new() };
    let mut header = b"X-WorkDesk: ".to_vec();
    header.extend(bytes.iter().map(|b| if *b == b'\r' || *b == b'\n' { b' ' } else { *b }));
    if let Ok((parsed, _)) = mailparse::parse_header(&header) {
        return parsed.get_value().trim().to_string();
    }
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn address_list(v: Option<&Vec<imap_proto::Address<'_>>>) -> (String, String) {
    let mut names = Vec::new();
    let mut emails = Vec::new();
    if let Some(addrs) = v {
        for a in addrs {
            let mailbox = decode_header_text(a.mailbox.as_deref());
            let host = decode_header_text(a.host.as_deref());
            let name = decode_header_text(a.name.as_deref());
            let email = if mailbox.is_empty() {
                String::new()
            } else if host.is_empty() {
                mailbox.clone()
            } else {
                format!("{mailbox}@{host}")
            };
            if !name.is_empty() {
                names.push(name);
            } else if !email.is_empty() {
                names.push(email.clone());
            }
            if !email.is_empty() {
                emails.push(email);
            }
        }
    }
    (names.join(", "), emails.join(", "))
}

fn disposition_is(common: &imap_proto::types::BodyContentCommon<'_>, what: &str) -> bool {
    common.disposition.as_ref().map(|d| d.ty.eq_ignore_ascii_case(what)).unwrap_or(false)
}

fn has_filename(common: &imap_proto::types::BodyContentCommon<'_>) -> bool {
    let in_params = |p: &Option<Vec<(std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>)>>, keys: &[&str]| {
        p.as_ref().map(|v| v.iter().any(|(k, _)| keys.iter().any(|x| k.eq_ignore_ascii_case(x)))).unwrap_or(false)
    };
    in_params(&common.ty.params, &["name"])
        || common.disposition.as_ref().map(|d| in_params(&d.params, &["filename", "filename*"])).unwrap_or(false)
}

/// Does this MIME structure carry a real attachment (the Gmail paperclip)?
pub fn has_attachment(b: &BodyStructure<'_>) -> bool {
    match b {
        BodyStructure::Multipart { bodies, common, .. } => disposition_is(common, "attachment") || bodies.iter().any(has_attachment),
        BodyStructure::Basic { common, other, .. } => {
            if disposition_is(common, "attachment") {
                return true;
            }
            let inline_image = common.ty.ty.eq_ignore_ascii_case("image") && other.id.is_some();
            has_filename(common) && !inline_image
        }
        BodyStructure::Text { common, .. } => disposition_is(common, "attachment"),
        BodyStructure::Message { common, .. } => disposition_is(common, "attachment"),
    }
}

/// Header data parsed from one `* n FETCH (...)` response. Fields are `None` when the
/// attribute was not requested / not returned.
#[derive(Debug, Default, Clone)]
pub struct ParsedFetch {
    pub uid: u32,
    pub flags: Option<Vec<String>>,
    pub size: Option<u32>,
    pub internal_ts: Option<i64>,
    pub envelope_ts: Option<i64>,
    pub from_name: Option<String>,
    pub from_email: Option<String>,
    pub to_addrs: Option<String>,
    pub subject: Option<String>,
    pub has_attachment: Option<bool>,
    pub thrid: Option<u64>,
    pub msgid: Option<u64>,
}

pub fn normalize_flags<S: AsRef<str>>(flags: &[S]) -> Vec<String> {
    let mut v: Vec<String> = flags
        .iter()
        .map(|f| f.as_ref().trim_start_matches('\\').to_string())
        .filter(|f| !f.is_empty() && !f.eq_ignore_ascii_case("Recent") && f != "*")
        .map(|f| {
            // canonical capitalisation for the system flags
            match f.to_ascii_lowercase().as_str() {
                "seen" => "Seen".to_string(),
                "flagged" => "Flagged".to_string(),
                "answered" => "Answered".to_string(),
                "draft" => "Draft".to_string(),
                "deleted" => "Deleted".to_string(),
                _ => f,
            }
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

fn parse_internal_date(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(s.trim(), "%d-%b-%Y %H:%M:%S %z").ok().map(|d| d.timestamp())
}

pub fn parse_fetch(raw: &[u8]) -> Result<Vec<ParsedFetch>, String> {
    let mut out = Vec::new();
    let mut rest: &[u8] = raw;
    while !rest.is_empty() {
        match imap_proto::parser::parse_response(rest) {
            Ok((remaining, Response::Fetch(_, attrs))) => {
                let mut p = ParsedFetch::default();
                for a in &attrs {
                    match a {
                        AttributeValue::Uid(u) => p.uid = *u,
                        AttributeValue::Flags(f) => p.flags = Some(normalize_flags(f)),
                        AttributeValue::Rfc822Size(s) => p.size = Some(*s),
                        AttributeValue::InternalDate(d) => p.internal_ts = parse_internal_date(d),
                        AttributeValue::GmailThrId(t) => p.thrid = Some(*t),
                        AttributeValue::GmailMsgId(m) => p.msgid = Some(*m),
                        AttributeValue::BodyStructure(b) => p.has_attachment = Some(has_attachment(b)),
                        AttributeValue::Envelope(env) => {
                            let (from_name, from_email) = address_list(env.from.as_ref());
                            let (_, to) = address_list(env.to.as_ref());
                            p.from_name = Some(from_name);
                            p.from_email = Some(from_email);
                            p.to_addrs = Some(to);
                            p.subject = Some(decode_header_text(env.subject.as_deref()));
                            p.envelope_ts = env
                                .date
                                .as_deref()
                                .and_then(|d| std::str::from_utf8(d).ok())
                                .and_then(|d| mailparse::dateparse(d).ok());
                        }
                        _ => {}
                    }
                }
                if p.uid != 0 {
                    out.push(p);
                }
                rest = remaining;
            }
            Ok((remaining, _)) => rest = remaining,
            Err(nom::Err::Incomplete(_)) => break,
            Err(_) => {
                // One response line the parser does not understand (an unusual extension, say) must not
                // abort the whole synchronisation: skip to the next line and carry on.
                match rest.windows(2).position(|w| w == b"\r\n") {
                    Some(i) => rest = &rest[i + 2..],
                    None => break,
                }
            }
        }
    }
    Ok(out)
}

fn to_cached(folder: &str, p: ParsedFetch) -> CachedMsg {
    let date_ts = p.internal_ts.or(p.envelope_ts).unwrap_or(0);
    CachedMsg {
        folder: folder.to_string(),
        uid: p.uid,
        thrid: p.thrid.map(|t| t.to_string()).unwrap_or_default(),
        gm_msgid: p.msgid.map(|t| t.to_string()).unwrap_or_default(),
        from_name: p.from_name.unwrap_or_default(),
        from_email: p.from_email.unwrap_or_default(),
        to_addrs: p.to_addrs.unwrap_or_default(),
        subject: p.subject.unwrap_or_default(),
        date_ts,
        flags: p.flags.unwrap_or_default(),
        size: p.size.unwrap_or(0),
        has_attachment: p.has_attachment.unwrap_or(false),
    }
}

/// Compress UIDs (any order) into an IMAP sequence set: `5:9,12,20:21`.
pub fn uid_set(uids: &[u32]) -> String {
    let mut v: Vec<u32> = uids.to_vec();
    v.sort_unstable();
    v.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        let mut end = start;
        while i + 1 < v.len() && v[i + 1] == end + 1 {
            i += 1;
            end = v[i];
        }
        parts.push(if start == end { start.to_string() } else { format!("{start}:{end}") });
        i += 1;
    }
    parts.join(",")
}

const HEADER_ATTRS: &str = "(UID FLAGS RFC822.SIZE INTERNALDATE ENVELOPE BODYSTRUCTURE X-GM-THRID X-GM-MSGID)";

/// Download headers for `uids` of the *currently selected* folder and store them in the cache.
pub fn fetch_headers(sess: &mut Sess, db: &Db, acct: &str, folder: &str, uids: &[u32]) -> Result<Vec<CachedMsg>, String> {
    let mut all = Vec::new();
    for chunk in uids.chunks(HEADER_CHUNK) {
        let raw = sess
            .run_command_and_read_response(format!("UID FETCH {} {HEADER_ATTRS}", uid_set(chunk)))
            .map_err(err)?;
        let msgs: Vec<CachedMsg> = parse_fetch(&raw)?.into_iter().map(|p| to_cached(folder, p)).collect();
        db.upsert_messages(acct, &msgs)?;
        all.extend(msgs);
    }
    Ok(all)
}

// ------------------------------------------------------------------------ sync

/// Synchronise one folder's headers into the cache:
///  1. detect UIDVALIDITY changes (cache is discarded),
///  2. drop messages that no longer exist on the server,
///  3. download headers of new messages, newest first, in chunks (progress is reported),
///  4. refresh the flags of everything already cached.
pub fn sync_folder(
    sess: &mut Sess,
    db: &Db,
    acct: &str,
    folder: &str,
    progress: &dyn Fn(SyncEvent),
) -> Result<SyncOutcome, String> {
    let ev = |phase: &str, done: u32, total: u32| SyncEvent {
        account: acct.to_string(),
        folder: folder.to_string(),
        phase: phase.to_string(),
        done,
        total,
        message: None,
    };
    let mailbox = sess.select(folder).map_err(err)?;
    let validity = mailbox.uid_validity.unwrap_or(0) as i64;
    let mut outcome = SyncOutcome::default();

    if let Some(meta) = db.folder_meta(acct, folder)? {
        if meta.uidvalidity.is_some() && meta.uidvalidity != Some(validity) {
            outcome.removed += db.purge_folder_messages(acct, folder)?;
        }
    }

    let mut server: Vec<u32> = if mailbox.exists == 0 { Vec::new() } else { sess.uid_search("ALL").map_err(err)?.into_iter().collect() };
    server.sort_unstable();
    let server_set: HashSet<u32> = server.iter().copied().collect();
    let cached = db.cached_flags(acct, folder)?;

    let gone: Vec<u32> = cached.keys().copied().filter(|u| !server_set.contains(u)).collect();
    if !gone.is_empty() {
        db.delete_uids(acct, folder, &gone)?;
        outcome.removed += gone.len() as u32;
    }

    // Newest first, so the top of the mailbox is usable while older headers are still arriving.
    let mut missing: Vec<u32> = server.iter().copied().filter(|u| !cached.contains_key(u)).collect();
    missing.sort_unstable_by(|a, b| b.cmp(a));
    let total_missing = missing.len() as u32;
    let mut done = 0u32;
    for chunk in missing.chunks(HEADER_CHUNK) {
        let got = fetch_headers(sess, db, acct, folder, chunk)?;
        done += chunk.len() as u32;
        outcome.added += got.len() as u32;
        progress(ev("headers", done, total_missing));
    }

    // Flag refresh for the messages that were already cached.
    if !cached.is_empty() && !server.is_empty() {
        let raw = sess.run_command_and_read_response("UID FETCH 1:* (UID FLAGS)").map_err(err)?;
        let mut updates: Vec<(u32, Vec<String>)> = Vec::new();
        for p in parse_fetch(&raw)? {
            if let (Some(old), Some(flags)) = (cached.get(&p.uid), p.flags) {
                if *old != crate::db::flags_to_string(&flags) {
                    updates.push((p.uid, flags));
                }
            }
        }
        outcome.flags_changed = updates.len() as u32;
        db.update_flags(acct, folder, &updates)?;
    }

    db.set_folder_synced(acct, folder, validity, mailbox.uid_next.map(|n| n as i64), true)?;
    outcome.total = server.len() as u32;
    progress(ev("folder-done", outcome.total, outcome.total));
    Ok(outcome)
}

/// Order in which folders are synchronised: the one the user is looking at first, then the
/// everyday mailboxes, then custom labels, and the (huge) All Mail last.
pub fn sync_order(folders: &[FolderInfo], priority: Option<&str>) -> Vec<String> {
    let rank = |f: &FolderInfo| -> i32 {
        if Some(f.name.as_str()) == priority {
            return -1;
        }
        match f.role.as_deref() {
            Some("inbox") => 0,
            Some("sent") => 1,
            Some("drafts") => 2,
            Some("starred") => 3,
            Some("spam") => 4,
            Some("trash") => 5,
            Some("important") => 6,
            Some("snoozed") => 7,
            Some("all") => 100,
            _ => 10,
        }
    };
    let mut v: Vec<&FolderInfo> = folders.iter().filter(|f| f.selectable).collect();
    v.sort_by_key(|f| rank(f));
    v.into_iter().map(|f| f.name.clone()).collect()
}

// ---------------------------------------------------------------------- search

fn clean_query(q: &str) -> String {
    q.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string()
}

/// Build the argument of a Gmail raw search. Non-ASCII queries are sent as a non-synchronising
/// literal (Gmail advertises LITERAL-), plain ASCII as a quoted string.
pub fn x_gm_raw(query: &str) -> Result<String, String> {
    let q = clean_query(query);
    if q.is_empty() {
        return Err("Enter something to search for".into());
    }
    if q.len() > 3800 {
        return Err("The search is too long".into());
    }
    if q.is_ascii() {
        Ok(format!("X-GM-RAW \"{}\"", q.replace('\\', "\\\\").replace('"', "\\\"")))
    } else {
        Ok(format!("X-GM-RAW {{{}+}}\r\n{}", q.len(), q))
    }
}

/// Run a Gmail search on the server in every listed folder.
/// Returns, per folder, the matching UIDs newest first (capped).
pub fn gmail_search(sess: &mut Sess, folders: &[String], query: &str) -> Result<Vec<(String, Vec<u32>, bool)>, String> {
    let criterion = x_gm_raw(query)?;
    let mut out = Vec::new();
    for f in folders {
        if sess.select(f).is_err() {
            continue; // e.g. a folder that does not exist for this account
        }
        let mut ids: Vec<u32> = sess.uid_search(&criterion).map_err(err)?.into_iter().collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        let truncated = ids.len() > SEARCH_CAP;
        ids.truncate(SEARCH_CAP);
        out.push((f.clone(), ids, truncated));
    }
    Ok(out)
}

/// Make sure the headers of `uids` (in `folder`) are cached; downloads the missing ones
/// (newest first, capped). Returns all cached rows for the uids.
pub fn ensure_headers(sess: &mut Sess, db: &Db, acct: &str, folder: &str, uids: &[u32], fetch_cap: usize) -> Result<Vec<CachedMsg>, String> {
    let have = db.cached_uid_set(acct, folder, uids)?;
    let mut missing: Vec<u32> = uids.iter().copied().filter(|u| !have.contains(u)).collect();
    missing.sort_unstable_by(|a, b| b.cmp(a));
    missing.truncate(fetch_cap);
    if !missing.is_empty() {
        sess.select(folder).map_err(err)?;
        fetch_headers(sess, db, acct, folder, &missing)?;
    }
    db.rows_for_uids(acct, folder, uids)
}

/// Bring every message of one Gmail thread that lives in `folder` into the cache.
pub fn refresh_thread_in(sess: &mut Sess, db: &Db, acct: &str, folder: &str, thrid: &str) -> Result<(), String> {
    if sess.select(folder).is_err() {
        return Ok(());
    }
    let ids: Vec<u32> = sess.uid_search(format!("X-GM-THRID {thrid}")).map_err(err)?.into_iter().collect();
    let have = db.cached_uid_set(acct, folder, &ids)?;
    let missing: Vec<u32> = ids.into_iter().filter(|u| !have.contains(u)).collect();
    if !missing.is_empty() {
        fetch_headers(sess, db, acct, folder, &missing)?;
    }
    Ok(())
}

// ------------------------------------------------------------- flags and moves

pub fn group_by_folder(refs: &[MsgRef]) -> HashMap<String, Vec<u32>> {
    let mut m: HashMap<String, Vec<u32>> = HashMap::new();
    for r in refs {
        m.entry(r.folder.clone()).or_default().push(r.uid);
    }
    m
}

pub fn set_flag(sess: &mut Sess, refs: &[MsgRef], flag: &str, enabled: bool) -> Result<(), String> {
    if !["Seen", "Flagged", "Answered", "Draft"].contains(&flag) {
        return Err("Unsupported message flag".into());
    }
    for (folder, uids) in group_by_folder(refs) {
        sess.select(&folder).map_err(err)?;
        let op = if enabled { "+FLAGS.SILENT" } else { "-FLAGS.SILENT" };
        sess.uid_store(uid_set(&uids), format!("{op} (\\{flag})")).map_err(err)?;
    }
    Ok(())
}

/// Move messages to another mailbox: UID MOVE, falling back to COPY + \Deleted + EXPUNGE.
pub fn move_messages(sess: &mut Sess, refs: &[MsgRef], destination: &str) -> Result<(), String> {
    for (folder, uids) in group_by_folder(refs) {
        if folder == destination {
            continue;
        }
        sess.select(&folder).map_err(err)?;
        let set = uid_set(&uids);
        if let Err(move_error) = sess.uid_mv(&set, destination) {
            sess.uid_copy(&set, destination)
                .map_err(|copy_error| format!("IMAP MOVE failed ({move_error}); fallback copy to {destination} also failed: {copy_error}"))?;
            sess.uid_store(&set, "+FLAGS.SILENT (\\Deleted)")
                .map_err(|e| format!("Copied to {destination}, but the original could not be marked for removal: {e}"))?;
            sess.uid_expunge(&set).map_err(|e| format!("Copied to {destination}, but the original could not be removed: {e}"))?;
        }
    }
    Ok(())
}

/// Permanently delete messages (used from Trash / Spam).
pub fn delete_messages(sess: &mut Sess, refs: &[MsgRef]) -> Result<(), String> {
    for (folder, uids) in group_by_folder(refs) {
        sess.select(&folder).map_err(err)?;
        let set = uid_set(&uids);
        sess.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").map_err(err)?;
        sess.uid_expunge(&set).map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"* 3 FETCH (X-GM-THRID 1700000000000000001 X-GM-MSGID 1700000000000000002 UID 42 RFC822.SIZE 1234 FLAGS (\\Seen $Forwarded \\Recent) INTERNALDATE \"17-Jul-2026 02:44:25 -0700\" ENVELOPE (\"Fri, 17 Jul 2026 02:44:20 -0700\" \"=?UTF-8?Q?Hello_W=C3=B6rld?=\" ((\"Alice A\" NIL \"alice\" \"example.com\")) ((\"Alice A\" NIL \"alice\" \"example.com\")) ((\"Alice A\" NIL \"alice\" \"example.com\")) ((\"Bob\" NIL \"bob\" \"example.org\")(NIL NIL \"carol\" \"example.org\")) NIL NIL NIL \"<msg1@example.com>\") BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL)(\"APPLICATION\" \"PDF\" (\"NAME\" \"a.pdf\") NIL NIL \"BASE64\" 100 NIL (\"ATTACHMENT\" (\"FILENAME\" \"a.pdf\")) NIL) \"MIXED\" (\"BOUNDARY\" \"xx\") NIL NIL))\r\n* 4 FETCH (UID 43 FLAGS () X-GM-THRID 1700000000000000001 BODYSTRUCTURE (\"TEXT\" \"HTML\" (\"CHARSET\" \"UTF-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL))\r\n";

    #[test]
    fn parses_gmail_fetch_with_thread_ids() {
        let rows = parse_fetch(SAMPLE).unwrap();
        assert_eq!(rows.len(), 2);
        let a = &rows[0];
        assert_eq!(a.uid, 42);
        assert_eq!(a.thrid, Some(1700000000000000001));
        assert_eq!(a.msgid, Some(1700000000000000002));
        assert_eq!(a.size, Some(1234));
        assert_eq!(a.flags.as_deref(), Some(&["$Forwarded".to_string(), "Seen".to_string()][..]));
        assert_eq!(a.subject.as_deref(), Some("Hello Wörld"));
        assert_eq!(a.from_name.as_deref(), Some("Alice A"));
        assert_eq!(a.from_email.as_deref(), Some("alice@example.com"));
        assert_eq!(a.to_addrs.as_deref(), Some("bob@example.org, carol@example.org"));
        assert_eq!(a.has_attachment, Some(true));
        assert!(a.internal_ts.is_some());
        let b = &rows[1];
        assert_eq!(b.uid, 43);
        assert_eq!(b.flags.as_deref(), Some(&[][..]));
        assert_eq!(b.has_attachment, Some(false));
    }

    #[test]
    fn one_bad_line_does_not_abort_parsing() {
        let raw = b"* 1 FETCH (UID 7 FLAGS (\\Seen))\r\n* THIS IS NOT VALID IMAP \x01\x02\r\n* 2 FETCH (UID 8 FLAGS ())\r\nA1 OK done\r\n";
        let rows = parse_fetch(raw).unwrap();
        assert_eq!(rows.iter().map(|r| r.uid).collect::<Vec<_>>(), vec![7, 8]);
    }

    #[test]
    fn internal_date_with_space_padded_day() {
        assert!(parse_internal_date(" 7-Jul-2026 02:44:25 -0700").is_some());
        assert!(parse_internal_date("17-Jul-2026 02:44:25 +0800").is_some());
    }

    #[test]
    fn sequence_sets_are_compressed() {
        assert_eq!(uid_set(&[9, 5, 6, 7, 12, 20, 21, 5]), "5:7,9,12,20:21");
        assert_eq!(uid_set(&[3]), "3");
        assert_eq!(uid_set(&[]), "");
    }

    #[test]
    fn gmail_raw_search_quoting() {
        assert_eq!(x_gm_raw("from:bob \"big deal\"").unwrap(), "X-GM-RAW \"from:bob \\\"big deal\\\"\"");
        assert_eq!(x_gm_raw("café").unwrap(), "X-GM-RAW {5+}\r\ncafé");
        assert!(x_gm_raw("   ").is_err());
    }

    #[test]
    fn modified_utf7_names() {
        assert_eq!(decode_mutf7("INBOX"), "INBOX");
        assert_eq!(decode_mutf7("Caf&AOk-"), "Café");
        assert_eq!(decode_mutf7("A &- B"), "A & B");
        assert_eq!(decode_mutf7("&BCcENQRABDoENA-"), "Черкд");
    }

    #[test]
    fn roles_follow_special_use_attributes() {
        assert_eq!(role_for("INBOX", &[]).as_deref(), Some("inbox"));
        assert_eq!(role_for("[Gmail]/Todos", &["\\All".into()]).as_deref(), Some("all"));
        assert_eq!(role_for("[Gmail]/Papelera", &["\\Trash".into()]).as_deref(), Some("trash"));
        assert_eq!(role_for("[Gmail]/Spam", &[]).as_deref(), Some("spam"));
        assert_eq!(role_for("Clients", &[]), None);
    }

    #[test]
    fn flag_normalisation() {
        assert_eq!(normalize_flags(&["\\Seen", "\\Recent", "\\flagged", "$Phishing"]), vec!["$Phishing", "Flagged", "Seen"]);
    }
}
