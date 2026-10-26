//! Plain data types shared between the cache, the IMAP layer and the Tauri commands.
//! Everything that crosses the IPC boundary is serialised as camelCase.

use serde::{Deserialize, Serialize};

/// A message reference: enough to address one message on the server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct MsgRef {
    pub folder: String,
    pub uid: u32,
}

/// One mailbox (Gmail label) as shown in the folder pane.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    pub name: String,
    pub display: String,
    pub delimiter: Option<String>,
    pub attributes: Vec<String>,
    /// inbox | sent | drafts | trash | spam | starred | all | important | archive | snoozed
    pub role: Option<String>,
    pub selectable: bool,
    #[serde(default)]
    pub total: u32,
    #[serde(default)]
    pub unread: u32,
    #[serde(default)]
    pub synced: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FolderList {
    pub folders: Vec<FolderInfo>,
    /// true when the list came from the local cache because the server could not be reached
    pub offline: bool,
}

/// One cached message header.
#[derive(Clone, Debug, Default)]
pub struct CachedMsg {
    pub folder: String,
    pub uid: u32,
    pub thrid: String,
    pub gm_msgid: String,
    pub from_name: String,
    pub from_email: String,
    pub to_addrs: String,
    pub subject: String,
    pub date_ts: i64,
    pub flags: Vec<String>,
    pub size: u32,
    pub has_attachment: bool,
}

impl CachedMsg {
    pub fn is_seen(&self) -> bool {
        has_flag(&self.flags, "Seen")
    }
    pub fn is_flagged(&self) -> bool {
        has_flag(&self.flags, "Flagged")
    }
    /// The conversation key: the Gmail thread id, or a per-message key when Gmail gave none.
    pub fn group_key(&self) -> String {
        if self.thrid.is_empty() {
            format!("m{}-{}", self.uid, self.folder)
        } else {
            self.thrid.clone()
        }
    }
}

pub fn has_flag(flags: &[String], name: &str) -> bool {
    flags.iter().any(|f| f.eq_ignore_ascii_case(name))
}

/// A single message header row, as sent to the UI.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MailRow {
    pub uid: u32,
    pub folder: String,
    pub thread_id: Option<String>,
    pub gm_msgid: Option<String>,
    pub from: String,
    pub email: String,
    pub to: String,
    pub subject: String,
    pub date: String,
    pub date_ts: i64,
    pub flags: Vec<String>,
    pub size: u32,
    pub has_attachment: bool,
}

pub fn iso_date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

impl From<&CachedMsg> for MailRow {
    fn from(m: &CachedMsg) -> Self {
        MailRow {
            uid: m.uid,
            folder: m.folder.clone(),
            thread_id: if m.thrid.is_empty() { None } else { Some(m.thrid.clone()) },
            gm_msgid: if m.gm_msgid.is_empty() { None } else { Some(m.gm_msgid.clone()) },
            from: if m.from_name.is_empty() { m.from_email.clone() } else { m.from_name.clone() },
            email: m.from_email.clone(),
            to: m.to_addrs.clone(),
            subject: m.subject.clone(),
            date: iso_date(m.date_ts),
            date_ts: m.date_ts,
            flags: m.flags.clone(),
            size: m.size,
            has_attachment: m.has_attachment,
        }
    }
}

/// One row of the mailbox list: a Gmail conversation (or a single message in flat mode).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub key: String,
    pub thread_id: Option<String>,
    /// Folder of the newest message in this context
    pub folder: String,
    /// UID of the newest message in this context
    pub uid: u32,
    pub subject: String,
    /// Participants summary: "Alice, Bob, Carol"
    pub from: String,
    pub email: String,
    pub to: String,
    pub date: String,
    pub date_ts: i64,
    /// Messages in the whole conversation (as far as known) …
    pub count: u32,
    /// … and how many of them are in this folder / matched the search
    pub matched: u32,
    pub unread: u32,
    pub flagged: bool,
    pub has_attachment: bool,
    pub size: u64,
    pub folders: Vec<String>,
    pub refs: Vec<MsgRef>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPage {
    pub items: Vec<Conversation>,
    /// number of conversations (or messages in flat mode) matching
    pub total: u32,
    pub total_messages: u32,
    /// whether this folder has completed at least one full header sync
    pub synced: bool,
    /// true if the search result was capped
    pub truncated: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SyncEvent {
    pub account: String,
    pub folder: String,
    /// headers | flags | folder-done | finished | error
    pub phase: String,
    pub done: u32,
    pub total: u32,
    pub message: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub added: u32,
    pub removed: u32,
    pub flags_changed: u32,
    pub total: u32,
    pub in_progress: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentData {
    pub index: u32,
    pub filename: String,
    pub content_type: String,
    /// Only populated for inline (cid:) images and for `get_attachment`
    pub data_base64: String,
    pub size: usize,
    pub content_id: Option<String>,
    pub inline: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MailDetail {
    pub uid: u32,
    pub folder: String,
    pub thread_id: Option<String>,
    pub from: String,
    pub email: String,
    pub to: String,
    pub cc: String,
    pub reply_to: String,
    pub subject: String,
    pub date: String,
    pub flags: Vec<String>,
    pub text: String,
    pub html: String,
    pub message_id: String,
    pub references: String,
    pub attachments: Vec<AttachmentData>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingAttachment {
    pub filename: String,
    pub content_type: String,
    pub data_base64: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    pub username: String,
    pub from_email: Option<String>,
    pub from_name: Option<String>,
    pub to: String,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    pub subject: String,
    pub text: Option<String>,
    pub html: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub attachments: Option<Vec<OutgoingAttachment>>,
}
