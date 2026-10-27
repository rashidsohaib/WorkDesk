//! Local SQLite cache of message *headers* (never bodies).
//!
//! The IMAP layer fills this cache (`imapx::sync_folder`); the mailbox list, folder counts and
//! conversation grouping are all served from it, so folders open instantly and keep working
//! offline. Rows are keyed by (account, folder, uid) and carry the Gmail thread id (X-GM-THRID),
//! which is what conversation view is built on.

use crate::model::*;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct FolderMeta {
    pub uidvalidity: Option<i64>,
    pub uidnext: Option<i64>,
    pub synced_at: Option<i64>,
    pub complete: bool,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders(
  account TEXT NOT NULL,
  name TEXT NOT NULL,
  display TEXT NOT NULL,
  delimiter TEXT,
  attributes TEXT NOT NULL DEFAULT '',
  role TEXT,
  selectable INTEGER NOT NULL DEFAULT 1,
  sort_order INTEGER NOT NULL DEFAULT 0,
  uidvalidity INTEGER,
  uidnext INTEGER,
  synced_at INTEGER,
  complete INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(account, name)
);
CREATE TABLE IF NOT EXISTS messages(
  account TEXT NOT NULL,
  folder TEXT NOT NULL,
  uid INTEGER NOT NULL,
  thrid TEXT NOT NULL DEFAULT '',
  gm_msgid TEXT NOT NULL DEFAULT '',
  grp TEXT NOT NULL,
  from_name TEXT NOT NULL DEFAULT '',
  from_email TEXT NOT NULL DEFAULT '',
  to_addrs TEXT NOT NULL DEFAULT '',
  subject TEXT NOT NULL DEFAULT '',
  subject_lc TEXT NOT NULL DEFAULT '',
  date_ts INTEGER NOT NULL DEFAULT 0,
  flags TEXT NOT NULL DEFAULT '',
  seen INTEGER NOT NULL DEFAULT 0,
  flagged INTEGER NOT NULL DEFAULT 0,
  size INTEGER NOT NULL DEFAULT 0,
  has_att INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(account, folder, uid)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_msg_date ON messages(account, folder, date_ts DESC);
CREATE INDEX IF NOT EXISTS idx_msg_grp ON messages(account, folder, grp);
CREATE INDEX IF NOT EXISTS idx_msg_thrid ON messages(account, thrid);
CREATE INDEX IF NOT EXISTS idx_msg_msgid ON messages(account, gm_msgid);
";

const MSG_COLS: &str =
    "folder, uid, thrid, gm_msgid, from_name, from_email, to_addrs, subject, date_ts, flags, size, has_att";

pub fn flags_to_string(flags: &[String]) -> String {
    let mut v: Vec<&str> = flags.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect();
    v.sort_unstable();
    v.dedup();
    v.join(" ")
}

pub fn string_to_flags(s: &str) -> Vec<String> {
    s.split_whitespace().map(|x| x.to_string()).collect()
}

fn row_to_msg(r: &rusqlite::Row<'_>) -> rusqlite::Result<CachedMsg> {
    let flags: String = r.get(9)?;
    Ok(CachedMsg {
        folder: r.get(0)?,
        uid: r.get::<_, i64>(1)? as u32,
        thrid: r.get(2)?,
        gm_msgid: r.get(3)?,
        from_name: r.get(4)?,
        from_email: r.get(5)?,
        to_addrs: r.get(6)?,
        subject: r.get(7)?,
        date_ts: r.get(8)?,
        flags: string_to_flags(&flags),
        size: r.get::<_, i64>(10)? as u32,
        has_attachment: r.get::<_, i64>(11)? != 0,
    })
}

fn e<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(e)?;
        }
        let conn = Connection::open(path).map_err(e)?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        Self::init(conn)
    }

    #[allow(dead_code)]
    pub fn open_memory() -> Result<Db, String> {
        Self::init(Connection::open_in_memory().map_err(e)?)
    }

    fn init(conn: Connection) -> Result<Db, String> {
        conn.execute_batch(SCHEMA).map_err(e)?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ---------------------------------------------------------------- folders

    /// Store the folder list fetched from the server. Sync bookkeeping of existing folders is kept;
    /// folders that no longer exist are removed together with their cached headers.
    pub fn replace_folder_list(&self, acct: &str, folders: &[FolderInfo]) -> Result<(), String> {
        let mut c = self.lock();
        let tx = c.transaction().map_err(e)?;
        let mut keep: HashSet<&str> = HashSet::new();
        for (i, f) in folders.iter().enumerate() {
            keep.insert(f.name.as_str());
            tx.execute(
                "INSERT INTO folders(account,name,display,delimiter,attributes,role,selectable,sort_order)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(account,name) DO UPDATE SET display=excluded.display, delimiter=excluded.delimiter,
                   attributes=excluded.attributes, role=excluded.role, selectable=excluded.selectable,
                   sort_order=excluded.sort_order",
                params![acct, f.name, f.display, f.delimiter, f.attributes.join(" "), f.role, f.selectable as i64, i as i64],
            )
            .map_err(e)?;
        }
        let existing: Vec<String> = {
            let mut st = tx.prepare("SELECT name FROM folders WHERE account=?1").map_err(e)?;
            let rows = st.query_map(params![acct], |r| r.get::<_, String>(0)).map_err(e)?;
            rows.collect::<Result<_, _>>().map_err(e)?
        };
        for name in existing {
            if !keep.contains(name.as_str()) {
                tx.execute("DELETE FROM folders WHERE account=?1 AND name=?2", params![acct, name]).map_err(e)?;
                tx.execute("DELETE FROM messages WHERE account=?1 AND folder=?2", params![acct, name]).map_err(e)?;
            }
        }
        tx.commit().map_err(e)
    }

    pub fn cached_folders(&self, acct: &str) -> Result<Vec<FolderInfo>, String> {
        let c = self.lock();
        let mut counts: HashMap<String, (u32, u32)> = HashMap::new();
        {
            let mut st = c
                .prepare(
                    "SELECT folder, COUNT(*), COUNT(DISTINCT CASE WHEN seen=0 THEN grp END)
                     FROM messages WHERE account=?1 GROUP BY folder",
                )
                .map_err(e)?;
            let rows = st
                .query_map(params![acct], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32, r.get::<_, i64>(2)? as u32)))
                .map_err(e)?;
            for row in rows {
                let (f, t, u) = row.map_err(e)?;
                counts.insert(f, (t, u));
            }
        }
        let mut st = c
            .prepare(
                "SELECT name, display, delimiter, attributes, role, selectable, complete
                 FROM folders WHERE account=?1 ORDER BY sort_order, name",
            )
            .map_err(e)?;
        let rows = st
            .query_map(params![acct], |r| {
                let attrs: String = r.get(3)?;
                Ok(FolderInfo {
                    name: r.get(0)?,
                    display: r.get(1)?,
                    delimiter: r.get(2)?,
                    attributes: attrs.split_whitespace().map(|s| s.to_string()).collect(),
                    role: r.get(4)?,
                    selectable: r.get::<_, i64>(5)? != 0,
                    synced: r.get::<_, i64>(6)? != 0,
                    total: 0,
                    unread: 0,
                })
            })
            .map_err(e)?;
        let mut out = Vec::new();
        for row in rows {
            let mut f = row.map_err(e)?;
            if let Some((t, u)) = counts.get(&f.name) {
                f.total = *t;
                f.unread = *u;
            }
            out.push(f);
        }
        Ok(out)
    }

    pub fn folder_meta(&self, acct: &str, name: &str) -> Result<Option<FolderMeta>, String> {
        self.lock()
            .query_row(
                "SELECT uidvalidity, uidnext, synced_at, complete FROM folders WHERE account=?1 AND name=?2",
                params![acct, name],
                |r| {
                    Ok(FolderMeta {
                        uidvalidity: r.get(0)?,
                        uidnext: r.get(1)?,
                        synced_at: r.get(2)?,
                        complete: r.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()
            .map_err(e)
    }

    pub fn set_folder_synced(&self, acct: &str, name: &str, validity: i64, uidnext: Option<i64>, complete: bool) -> Result<(), String> {
        let now = chrono::Utc::now().timestamp();
        self.lock()
            .execute(
                "UPDATE folders SET uidvalidity=?3, uidnext=?4, synced_at=?5, complete=?6 WHERE account=?1 AND name=?2",
                params![acct, name, validity, uidnext, now, complete as i64],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn role_folder(&self, acct: &str, role: &str) -> Result<Option<String>, String> {
        self.lock()
            .query_row(
                "SELECT name FROM folders WHERE account=?1 AND role=?2 ORDER BY sort_order LIMIT 1",
                params![acct, role],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)
    }

    pub fn delete_folder(&self, acct: &str, name: &str) -> Result<(), String> {
        let c = self.lock();
        c.execute("DELETE FROM messages WHERE account=?1 AND folder=?2", params![acct, name]).map_err(e)?;
        c.execute("DELETE FROM folders WHERE account=?1 AND name=?2", params![acct, name]).map_err(e)?;
        Ok(())
    }

    /// Forget every cached header of an account (and the sync state), forcing a full re-sync.
    pub fn clear_account(&self, acct: &str) -> Result<(), String> {
        let c = self.lock();
        c.execute("DELETE FROM messages WHERE account=?1", params![acct]).map_err(e)?;
        c.execute(
            "UPDATE folders SET uidvalidity=NULL, uidnext=NULL, synced_at=NULL, complete=0 WHERE account=?1",
            params![acct],
        )
        .map_err(e)?;
        Ok(())
    }

    // --------------------------------------------------------------- messages

    pub fn purge_folder_messages(&self, acct: &str, folder: &str) -> Result<u32, String> {
        let n = self
            .lock()
            .execute("DELETE FROM messages WHERE account=?1 AND folder=?2", params![acct, folder])
            .map_err(e)?;
        Ok(n as u32)
    }

    pub fn cached_flags(&self, acct: &str, folder: &str) -> Result<HashMap<u32, String>, String> {
        let c = self.lock();
        let mut st = c.prepare("SELECT uid, flags FROM messages WHERE account=?1 AND folder=?2").map_err(e)?;
        let rows = st
            .query_map(params![acct, folder], |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?)))
            .map_err(e)?;
        let mut out = HashMap::new();
        for row in rows {
            let (u, f) = row.map_err(e)?;
            out.insert(u, f);
        }
        Ok(out)
    }

    pub fn upsert_messages(&self, acct: &str, msgs: &[CachedMsg]) -> Result<(), String> {
        if msgs.is_empty() {
            return Ok(());
        }
        let mut c = self.lock();
        let tx = c.transaction().map_err(e)?;
        {
            let mut st = tx
                .prepare(
                    "INSERT OR REPLACE INTO messages(account,folder,uid,thrid,gm_msgid,grp,from_name,from_email,to_addrs,
                       subject,subject_lc,date_ts,flags,seen,flagged,size,has_att)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                )
                .map_err(e)?;
            for m in msgs {
                st.execute(params![
                    acct,
                    m.folder,
                    m.uid as i64,
                    m.thrid,
                    m.gm_msgid,
                    m.group_key(),
                    m.from_name,
                    m.from_email,
                    m.to_addrs,
                    m.subject,
                    normalize_subject(&m.subject),
                    m.date_ts,
                    flags_to_string(&m.flags),
                    m.is_seen() as i64,
                    m.is_flagged() as i64,
                    m.size as i64,
                    m.has_attachment as i64,
                ])
                .map_err(e)?;
            }
        }
        tx.commit().map_err(e)
    }

    pub fn delete_uids(&self, acct: &str, folder: &str, uids: &[u32]) -> Result<(), String> {
        let mut c = self.lock();
        let tx = c.transaction().map_err(e)?;
        for chunk in uids.chunks(500) {
            let sql = format!("DELETE FROM messages WHERE account=?1 AND folder=?2 AND uid IN ({})", placeholders(chunk.len()));
            let mut p: Vec<rusqlite::types::Value> = vec![acct.to_string().into(), folder.to_string().into()];
            p.extend(chunk.iter().map(|u| rusqlite::types::Value::Integer(*u as i64)));
            tx.execute(&sql, params_from_iter(p)).map_err(e)?;
        }
        tx.commit().map_err(e)
    }

    pub fn update_flags(&self, acct: &str, folder: &str, updates: &[(u32, Vec<String>)]) -> Result<(), String> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut c = self.lock();
        let tx = c.transaction().map_err(e)?;
        {
            let mut st = tx
                .prepare("UPDATE messages SET flags=?4, seen=?5, flagged=?6 WHERE account=?1 AND folder=?2 AND uid=?3")
                .map_err(e)?;
            for (uid, flags) in updates {
                st.execute(params![
                    acct,
                    folder,
                    *uid as i64,
                    flags_to_string(flags),
                    has_flag(flags, "Seen") as i64,
                    has_flag(flags, "Flagged") as i64
                ])
                .map_err(e)?;
            }
        }
        tx.commit().map_err(e)
    }

    /// Set or clear one flag on the cached copies of the given messages. Gmail keeps one set of
    /// flags per *message*, shared by every label it carries, so the copies of the same message
    /// (same X-GM-MSGID) cached under other folders are updated too.
    pub fn apply_flag(&self, acct: &str, refs: &[MsgRef], flag: &str, enabled: bool) -> Result<(), String> {
        let mut by_folder: HashMap<&str, Vec<u32>> = HashMap::new();
        for r in refs {
            by_folder.entry(r.folder.as_str()).or_default().push(r.uid);
        }
        let mut targets: HashMap<(String, u32), CachedMsg> = HashMap::new();
        for (folder, uids) in by_folder {
            for m in self.rows_for_uids(acct, folder, &uids)? {
                targets.insert((m.folder.clone(), m.uid), m);
            }
        }
        let msgids: Vec<String> = targets.values().map(|m| m.gm_msgid.clone()).filter(|id| !id.is_empty()).collect();
        for m in self.rows_by_msgids(acct, &msgids)? {
            targets.entry((m.folder.clone(), m.uid)).or_insert(m);
        }
        let mut updates: HashMap<String, Vec<(u32, Vec<String>)>> = HashMap::new();
        for ((folder, uid), m) in targets {
            let mut flags: Vec<String> = m.flags.into_iter().filter(|f| !f.eq_ignore_ascii_case(flag)).collect();
            if enabled {
                flags.push(flag.to_string());
            }
            updates.entry(folder).or_default().push((uid, flags));
        }
        for (folder, list) in updates {
            self.update_flags(acct, &folder, &list)?;
        }
        Ok(())
    }

    /// Every cached copy (in any folder) of the given Gmail messages.
    pub fn rows_by_msgids(&self, acct: &str, msgids: &[String]) -> Result<Vec<CachedMsg>, String> {
        let c = self.lock();
        let mut out = Vec::new();
        let mut uniq: Vec<&String> = msgids.iter().collect();
        uniq.sort();
        uniq.dedup();
        for chunk in uniq.chunks(500) {
            let sql = format!("SELECT {MSG_COLS} FROM messages WHERE account=?1 AND gm_msgid IN ({})", placeholders(chunk.len()));
            let mut p: Vec<rusqlite::types::Value> = vec![acct.to_string().into()];
            p.extend(chunk.iter().map(|m| (*m).clone().into()));
            let mut st = c.prepare(&sql).map_err(e)?;
            let rows = st.query_map(params_from_iter(p), row_to_msg).map_err(e)?;
            for r in rows {
                out.push(r.map_err(e)?);
            }
        }
        Ok(out)
    }

    /// Forget the cached copies of these messages in every folder except `keep_folder`
    /// (used when a message is trashed / spammed: Gmail then removes it from all its labels).
    pub fn delete_msgids_except(&self, acct: &str, msgids: &[String], keep_folder: &str) -> Result<(), String> {
        let mut c = self.lock();
        let tx = c.transaction().map_err(e)?;
        let mut uniq: Vec<&String> = msgids.iter().filter(|m| !m.is_empty()).collect();
        uniq.sort();
        uniq.dedup();
        for chunk in uniq.chunks(500) {
            let sql = format!(
                "DELETE FROM messages WHERE account=?1 AND folder<>?2 AND gm_msgid IN ({})",
                placeholders(chunk.len())
            );
            let mut p: Vec<rusqlite::types::Value> = vec![acct.to_string().into(), keep_folder.to_string().into()];
            p.extend(chunk.iter().map(|m| (*m).clone().into()));
            tx.execute(&sql, params_from_iter(p)).map_err(e)?;
        }
        tx.commit().map_err(e)
    }

    pub fn folder_role(&self, acct: &str, name: &str) -> Result<Option<String>, String> {
        Ok(self
            .lock()
            .query_row("SELECT role FROM folders WHERE account=?1 AND name=?2", params![acct, name], |r| r.get::<_, Option<String>>(0))
            .optional()
            .map_err(e)?
            .flatten())
    }

    pub fn rows_for_uids(&self, acct: &str, folder: &str, uids: &[u32]) -> Result<Vec<CachedMsg>, String> {
        let c = self.lock();
        let mut out = Vec::new();
        for chunk in uids.chunks(500) {
            let sql = format!(
                "SELECT {MSG_COLS} FROM messages WHERE account=?1 AND folder=?2 AND uid IN ({})",
                placeholders(chunk.len())
            );
            let mut p: Vec<rusqlite::types::Value> = vec![acct.to_string().into(), folder.to_string().into()];
            p.extend(chunk.iter().map(|u| rusqlite::types::Value::Integer(*u as i64)));
            let mut st = c.prepare(&sql).map_err(e)?;
            let rows = st.query_map(params_from_iter(p), row_to_msg).map_err(e)?;
            for r in rows {
                out.push(r.map_err(e)?);
            }
        }
        Ok(out)
    }

    /// Raw material for the contact list: sender / recipients of every cached header.
    pub fn contact_rows(&self, acct: &str) -> Result<Vec<(String, String, String, String)>, String> {
        let conn = self.conn.lock().map_err(e)?;
        let mut st = conn
            .prepare("SELECT m.from_name, m.from_email, m.to_addrs, COALESCE(f.role,'') FROM messages m LEFT JOIN folders f ON f.account=m.account AND f.name=m.folder WHERE m.account=?1")
            .map_err(e)?;
        let rows = st
            .query_map([acct], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
            .map_err(e)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(e)
    }

    pub fn cached_uid_set(&self, acct: &str, folder: &str, uids: &[u32]) -> Result<HashSet<u32>, String> {
        Ok(self.rows_for_uids(acct, folder, uids)?.into_iter().map(|m| m.uid).collect())
    }

    /// Every cached message of one Gmail thread, restricted to the given folders.
    pub fn thread_members(&self, acct: &str, thrid: &str, folders: &[String]) -> Result<Vec<CachedMsg>, String> {
        if folders.is_empty() {
            return Ok(vec![]);
        }
        let c = self.lock();
        let sql = format!(
            "SELECT {MSG_COLS} FROM messages WHERE account=?1 AND thrid=?2 AND folder IN ({}) ORDER BY date_ts, uid",
            placeholders(folders.len())
        );
        let mut p: Vec<rusqlite::types::Value> = vec![acct.to_string().into(), thrid.to_string().into()];
        p.extend(folders.iter().map(|f| f.clone().into()));
        let mut st = c.prepare(&sql).map_err(e)?;
        let rows = st.query_map(params_from_iter(p), row_to_msg).map_err(e)?;
        rows.collect::<Result<_, _>>().map_err(e)
    }

    /// Number of cached messages per conversation key in one folder (used to show real thread sizes).
    pub fn count_by_group(&self, acct: &str, folder: &str, keys: &[String]) -> Result<HashMap<String, u32>, String> {
        let c = self.lock();
        let mut out = HashMap::new();
        for chunk in keys.chunks(500) {
            let sql = format!(
                "SELECT grp, COUNT(*) FROM messages WHERE account=?1 AND folder=?2 AND grp IN ({}) GROUP BY grp",
                placeholders(chunk.len())
            );
            let mut p: Vec<rusqlite::types::Value> = vec![acct.to_string().into(), folder.to_string().into()];
            p.extend(chunk.iter().map(|k| k.clone().into()));
            let mut st = c.prepare(&sql).map_err(e)?;
            let rows = st.query_map(params_from_iter(p), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32))).map_err(e)?;
            for r in rows {
                let (k, n) = r.map_err(e)?;
                out.insert(k, n);
            }
        }
        Ok(out)
    }

    /// One page of the mailbox list. `grouped` = conversation view (one row per Gmail thread);
    /// otherwise one row per message. `all_mail` is the All Mail folder name, used to show the
    /// real size of each thread even when only some of its messages carry this folder's label.
    #[allow(clippy::too_many_arguments)]
    pub fn list_page(
        &self,
        acct: &str,
        folder: &str,
        grouped: bool,
        sort: &str,
        desc: bool,
        limit: u32,
        offset: u32,
        all_mail: Option<&str>,
    ) -> Result<ConversationPage, String> {
        let dir = if desc { "DESC" } else { "ASC" };
        let (total, total_messages) = {
            let c = self.lock();
            let msgs: i64 = c
                .query_row("SELECT COUNT(*) FROM messages WHERE account=?1 AND folder=?2", params![acct, folder], |r| r.get(0))
                .map_err(e)?;
            let groups: i64 = if grouped {
                c.query_row(
                    "SELECT COUNT(DISTINCT grp) FROM messages WHERE account=?1 AND folder=?2",
                    params![acct, folder],
                    |r| r.get(0),
                )
                .map_err(e)?
            } else {
                msgs
            };
            (groups as u32, msgs as u32)
        };
        let synced = self.folder_meta(acct, folder)?.map(|m| m.complete).unwrap_or(false);

        let keys: Vec<rusqlite::types::Value> = {
            let c = self.lock();
            if grouped {
                let order = match sort {
                    "subject" => "MIN(subject_lc)",
                    "from" => "MIN(lower(from_name))",
                    "to" => "MIN(to_addrs)",
                    "size" => "SUM(size)",
                    "attachment" => "MAX(has_att)",
                    "flags" => "SUM(1-seen)",
                    _ => "MAX(date_ts)",
                };
                let sql = format!(
                    "SELECT grp FROM messages WHERE account=?1 AND folder=?2 GROUP BY grp
                     ORDER BY {order} {dir}, MAX(date_ts) DESC, MAX(uid) DESC LIMIT ?3 OFFSET ?4"
                );
                let mut st = c.prepare(&sql).map_err(e)?;
                let rows = st
                    .query_map(params![acct, folder, limit as i64, offset as i64], |r| r.get::<_, String>(0))
                    .map_err(e)?;
                rows.map(|r| r.map(rusqlite::types::Value::Text)).collect::<Result<_, _>>().map_err(e)?
            } else {
                let order = match sort {
                    "subject" => "subject_lc",
                    "from" => "lower(from_name)",
                    "to" => "to_addrs",
                    "size" => "size",
                    "attachment" => "has_att",
                    "flags" => "seen",
                    _ => "date_ts",
                };
                let sql = format!(
                    "SELECT uid FROM messages WHERE account=?1 AND folder=?2
                     ORDER BY {order} {dir}, date_ts DESC, uid DESC LIMIT ?3 OFFSET ?4"
                );
                let mut st = c.prepare(&sql).map_err(e)?;
                let rows = st
                    .query_map(params![acct, folder, limit as i64, offset as i64], |r| r.get::<_, i64>(0))
                    .map_err(e)?;
                rows.map(|r| r.map(rusqlite::types::Value::Integer)).collect::<Result<_, _>>().map_err(e)?
            }
        };

        let mut members: HashMap<String, Vec<CachedMsg>> = HashMap::new();
        if !keys.is_empty() {
            let c = self.lock();
            let col = if grouped { "grp" } else { "uid" };
            let sql = format!(
                "SELECT {MSG_COLS}, grp FROM messages WHERE account=?1 AND folder=?2 AND {col} IN ({}) ORDER BY date_ts, uid",
                placeholders(keys.len())
            );
            let mut p: Vec<rusqlite::types::Value> = vec![acct.to_string().into(), folder.to_string().into()];
            p.extend(keys.iter().cloned());
            let mut st = c.prepare(&sql).map_err(e)?;
            let rows = st
                .query_map(params_from_iter(p), |r| {
                    let m = row_to_msg(r)?;
                    let grp: String = r.get(12)?;
                    Ok((grp, m))
                })
                .map_err(e)?;
            for r in rows {
                let (grp, m) = r.map_err(e)?;
                let key = if grouped { grp } else { m.uid.to_string() };
                members.entry(key).or_default().push(m);
            }
        }

        let mut real_counts: HashMap<String, u32> = HashMap::new();
        if grouped {
            if let Some(am) = all_mail {
                if am != folder {
                    let group_keys: Vec<String> = keys
                        .iter()
                        .filter_map(|k| if let rusqlite::types::Value::Text(t) = k { Some(t.clone()) } else { None })
                        .collect();
                    real_counts = self.count_by_group(acct, am, &group_keys)?;
                }
            }
        }

        let mut items = Vec::new();
        for k in &keys {
            let key = match k {
                rusqlite::types::Value::Text(t) => t.clone(),
                rusqlite::types::Value::Integer(i) => i.to_string(),
                _ => continue,
            };
            if let Some(ms) = members.remove(&key) {
                let total_count = real_counts.get(&key).copied();
                items.push(build_conversation(&key, ms, total_count, grouped));
            }
        }
        Ok(ConversationPage { items, total, total_messages, synced, truncated: false })
    }
}

/// Subject without any leading Re:/Fwd: prefixes, lower-cased (for sorting).
pub fn normalize_subject(s: &str) -> String {
    let mut t = s.trim();
    loop {
        let lower = t.to_ascii_lowercase();
        let mut stripped = false;
        for p in ["re:", "fwd:", "fw:", "aw:", "sv:", "wg:"] {
            if lower.starts_with(p) {
                t = t[p.len()..].trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    t.to_lowercase()
}

fn short_name(m: &CachedMsg) -> String {
    if !m.from_name.is_empty() {
        m.from_name.clone()
    } else if let Some((local, _)) = m.from_email.split_once('@') {
        local.to_string()
    } else {
        m.from_email.clone()
    }
}

/// Turn the cached members of one conversation (any order) into a list row.
pub fn build_conversation(key: &str, mut members: Vec<CachedMsg>, total_count: Option<u32>, grouped: bool) -> Conversation {
    members.sort_by(|a, b| a.date_ts.cmp(&b.date_ts).then(a.uid.cmp(&b.uid)));
    let latest = members.last().cloned().unwrap_or_default();
    let mut seen_senders: HashSet<String> = HashSet::new();
    let mut names: Vec<String> = Vec::new();
    for m in &members {
        let id = if m.from_email.is_empty() { m.from_name.to_lowercase() } else { m.from_email.to_lowercase() };
        if seen_senders.insert(id) {
            names.push(short_name(m));
        }
    }
    let from = if names.len() > 3 {
        format!("{}, … {}, {}", names[0], names[names.len() - 2], names[names.len() - 1])
    } else {
        names.join(", ")
    };
    let mut folders: Vec<String> = Vec::new();
    for m in &members {
        if !folders.contains(&m.folder) {
            folders.push(m.folder.clone());
        }
    }
    let matched = members.len() as u32;
    Conversation {
        key: if grouped { key.to_string() } else { format!("{}:{}", latest.folder, latest.uid) },
        thread_id: if latest.thrid.is_empty() { None } else { Some(latest.thrid.clone()) },
        folder: latest.folder.clone(),
        uid: latest.uid,
        subject: latest.subject.clone(),
        from,
        email: latest.from_email.clone(),
        to: latest.to_addrs.clone(),
        date: iso_date(latest.date_ts),
        date_ts: latest.date_ts,
        count: total_count.unwrap_or(matched).max(matched),
        matched,
        unread: members.iter().filter(|m| !m.is_seen()).count() as u32,
        flagged: members.iter().any(|m| m.is_flagged()),
        has_attachment: members.iter().any(|m| m.has_attachment),
        size: members.iter().map(|m| m.size as u64).sum(),
        folders,
        refs: members.iter().map(|m| MsgRef { folder: m.folder.clone(), uid: m.uid }).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(folder: &str, uid: u32, thrid: &str, subject: &str, ts: i64, flags: &[&str]) -> CachedMsg {
        CachedMsg {
            folder: folder.into(),
            uid,
            thrid: thrid.into(),
            gm_msgid: format!("g{uid}"),
            from_name: if uid % 2 == 0 { "Alice".into() } else { "Bob".into() },
            from_email: if uid % 2 == 0 { "alice@example.com".into() } else { "bob@example.com".into() },
            to_addrs: "me@example.com".into(),
            subject: subject.into(),
            date_ts: ts,
            flags: flags.iter().map(|s| s.to_string()).collect(),
            size: 100,
            has_attachment: uid == 3,
        }
    }

    #[test]
    fn groups_by_gmail_thread_and_counts_all_mail() {
        let db = Db::open_memory().unwrap();
        db.upsert_messages(
            "me",
            &[
                msg("INBOX", 1, "100", "Hello", 1000, &["Seen"]),
                msg("INBOX", 2, "100", "Re: Hello", 2000, &[]),
                msg("INBOX", 3, "200", "Other", 1500, &["Seen", "Flagged"]),
                msg("[Gmail]/All Mail", 10, "100", "Hello", 1000, &["Seen"]),
                msg("[Gmail]/All Mail", 11, "100", "Re: Hello", 2000, &[]),
                msg("[Gmail]/All Mail", 12, "100", "Re: Re: Hello", 3000, &["Seen"]),
                msg("[Gmail]/All Mail", 13, "200", "Other", 1500, &["Seen"]),
            ],
        )
        .unwrap();
        let page = db.list_page("me", "INBOX", true, "date", true, 25, 0, Some("[Gmail]/All Mail")).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.total_messages, 3);
        assert_eq!(page.items.len(), 2);
        let first = &page.items[0];
        assert_eq!(first.thread_id.as_deref(), Some("100"));
        assert_eq!(first.matched, 2);
        assert_eq!(first.count, 3, "thread size comes from All Mail");
        assert_eq!(first.unread, 1);
        assert_eq!(first.uid, 2);
        assert_eq!(first.from, "Bob, Alice");
        let second = &page.items[1];
        assert!(second.flagged);
        assert!(second.has_attachment);
        assert_eq!(second.unread, 0);
        // flat mode lists every message
        let flat = db.list_page("me", "INBOX", false, "date", true, 25, 0, None).unwrap();
        assert_eq!(flat.items.len(), 3);
        assert_eq!(flat.items[0].uid, 2);
    }

    #[test]
    fn messages_without_thread_id_stay_separate() {
        let db = Db::open_memory().unwrap();
        db.upsert_messages("me", &[msg("INBOX", 1, "", "Same", 1, &[]), msg("INBOX", 2, "", "Same", 2, &[])]).unwrap();
        let page = db.list_page("me", "INBOX", true, "date", true, 25, 0, None).unwrap();
        assert_eq!(page.items.len(), 2);
    }

    #[test]
    fn flags_and_deletes_update_cache() {
        let db = Db::open_memory().unwrap();
        db.upsert_messages("me", &[msg("INBOX", 1, "1", "A", 1, &[]), msg("INBOX", 2, "2", "B", 2, &[])]).unwrap();
        db.apply_flag("me", &[MsgRef { folder: "INBOX".into(), uid: 1 }], "Seen", true).unwrap();
        let rows = db.rows_for_uids("me", "INBOX", &[1, 2]).unwrap();
        assert!(rows.iter().find(|m| m.uid == 1).unwrap().is_seen());
        assert!(!rows.iter().find(|m| m.uid == 2).unwrap().is_seen());
        db.delete_uids("me", "INBOX", &[1]).unwrap();
        assert_eq!(db.cached_flags("me", "INBOX").unwrap().len(), 1);
    }

    #[test]
    fn flags_follow_the_gmail_message_across_labels() {
        let db = Db::open_memory().unwrap();
        db.upsert_messages(
            "me",
            &[
                msg("INBOX", 1, "7", "A", 1, &[]),
                CachedMsg { gm_msgid: "g1".into(), ..msg("INBOX", 1, "7", "A", 1, &[]) },
                CachedMsg { gm_msgid: "g1".into(), ..msg("[Gmail]/All Mail", 50, "7", "A", 1, &[]) },
                CachedMsg { gm_msgid: "g2".into(), ..msg("[Gmail]/All Mail", 51, "8", "B", 2, &[]) },
            ],
        )
        .unwrap();
        db.apply_flag("me", &[MsgRef { folder: "[Gmail]/All Mail".into(), uid: 50 }], "Seen", true).unwrap();
        assert!(db.rows_for_uids("me", "INBOX", &[1]).unwrap()[0].is_seen(), "inbox copy follows the All Mail copy");
        assert!(!db.rows_for_uids("me", "[Gmail]/All Mail", &[51]).unwrap()[0].is_seen(), "other messages untouched");
        db.delete_msgids_except("me", &["g1".to_string()], "[Gmail]/Trash").unwrap();
        assert!(db.rows_for_uids("me", "INBOX", &[1]).unwrap().is_empty());
        assert_eq!(db.rows_for_uids("me", "[Gmail]/All Mail", &[51]).unwrap().len(), 1);
    }

    #[test]
    fn folder_list_roundtrip_with_counts() {
        let db = Db::open_memory().unwrap();
        let f = |n: &str, role: Option<&str>| FolderInfo {
            name: n.into(),
            display: n.into(),
            delimiter: Some("/".into()),
            attributes: vec![],
            role: role.map(|s| s.to_string()),
            selectable: true,
            ..Default::default()
        };
        db.replace_folder_list("me", &[f("INBOX", Some("inbox")), f("[Gmail]/Trash", Some("trash"))]).unwrap();
        db.upsert_messages("me", &[msg("INBOX", 1, "1", "A", 1, &[]), msg("INBOX", 2, "1", "A", 2, &[])]).unwrap();
        let folders = db.cached_folders("me").unwrap();
        assert_eq!(folders[0].name, "INBOX");
        assert_eq!(folders[0].total, 2);
        assert_eq!(folders[0].unread, 1, "unread counts conversations, not messages");
        assert_eq!(db.role_folder("me", "trash").unwrap().as_deref(), Some("[Gmail]/Trash"));
        // dropping a folder removes its headers
        db.replace_folder_list("me", &[f("INBOX", Some("inbox"))]).unwrap();
        assert!(db.role_folder("me", "trash").unwrap().is_none());
    }

    #[test]
    fn subject_normalisation() {
        assert_eq!(normalize_subject("Re: RE: Fwd: Hello"), "hello");
    }
}
