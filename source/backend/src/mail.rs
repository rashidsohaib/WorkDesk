//! Reading a full message (MIME → text / HTML / attachments) and building outgoing messages.

use crate::imapx::{decode_header_text, err, Sess};
use crate::model::*;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use lettre::message::{header::ContentType, Attachment as MailAttachment, Mailbox, Message, MultiPart, SinglePart};
use mailparse::{MailAddr, MailHeaderMap, ParsedMail};
use regex::Regex;

/// Inline (cid:) images up to this size are delivered with the message so it renders at once.
const INLINE_LIMIT: usize = 8 * 1024 * 1024;

struct Walk {
    text: String,
    html: String,
    attachments: Vec<AttachmentData>,
}

fn is_attachment_part(p: &ParsedMail<'_>, ct: &str, filename: &Option<String>, content_id: &Option<String>) -> bool {
    let disp = p.get_content_disposition();
    disp.disposition == mailparse::DispositionType::Attachment
        || filename.is_some()
        || (ct.starts_with("image/") && content_id.is_some())
        || ct == "message/rfc822"
}

fn estimate_size(p: &ParsedMail<'_>) -> usize {
    let enc = p.headers.iter().find(|h| h.get_key().eq_ignore_ascii_case("Content-Transfer-Encoding")).map(|h| h.get_value().to_lowercase());
    let raw = p.raw_bytes.len();
    let body_len = p.raw_bytes.len().saturating_sub(p.headers.iter().map(|h| h.get_key_ref().len() + h.get_value_raw().len() + 4).sum::<usize>());
    let _ = raw;
    match enc.as_deref() {
        Some("base64") => body_len / 4 * 3,
        _ => body_len,
    }
}

fn walk(p: &ParsedMail<'_>, w: &mut Walk, with_data: bool) {
    let ct = p.ctype.mimetype.to_lowercase();
    let disp = p.get_content_disposition();
    let raw_name = disp.params.get("filename").cloned().or_else(|| p.ctype.params.get("name").cloned());
    let filename = raw_name.map(|n| decode_header_text(Some(n.as_bytes())));
    let content_id = p
        .headers
        .iter()
        .find(|h| h.get_key().eq_ignore_ascii_case("Content-ID"))
        .map(|h| h.get_value().trim().trim_start_matches('<').trim_end_matches('>').to_string())
        .filter(|v| !v.is_empty());

    if p.subparts.is_empty() || ct == "message/rfc822" {
        if is_attachment_part(p, &ct, &filename, &content_id) {
            let inline = ct.starts_with("image/") && content_id.is_some() && disp.disposition != mailparse::DispositionType::Attachment;
            let want_data = with_data || (inline && estimate_size(p) <= INLINE_LIMIT);
            let (data, size) = if want_data {
                match p.get_body_raw() {
                    Ok(b) => (BASE64.encode(&b), b.len()),
                    Err(_) => (String::new(), 0),
                }
            } else {
                (String::new(), estimate_size(p))
            };
            let fallback = if ct == "message/rfc822" { "forwarded-message.eml" } else if inline { "inline-image" } else { "attachment" };
            let index = w.attachments.len() as u32;
            w.attachments.push(AttachmentData {
                index,
                filename: filename.unwrap_or_else(|| fallback.to_string()),
                content_type: ct,
                data_base64: data,
                size,
                content_id,
                inline,
            });
            return;
        }
        if let Ok(body) = p.get_body() {
            if ct == "text/plain" && w.text.is_empty() {
                w.text = body;
            } else if ct == "text/html" && w.html.is_empty() {
                w.html = body;
            }
        }
        return;
    }
    for sp in &p.subparts {
        walk(sp, w, with_data);
    }
}

fn header(p: &ParsedMail<'_>, name: &str) -> String {
    p.headers.get_first_value(name).unwrap_or_default().trim().to_string()
}

/// (display names joined, addresses joined) for a header such as `To:`.
fn address_summary(value: &str) -> (String, String) {
    let mut names = Vec::new();
    let mut emails = Vec::new();
    if let Ok(list) = mailparse::addrparse(value) {
        for a in list.iter() {
            match a {
                MailAddr::Single(s) => {
                    emails.push(s.addr.clone());
                    names.push(s.display_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| s.addr.clone()));
                }
                MailAddr::Group(g) => {
                    for s in &g.addrs {
                        emails.push(s.addr.clone());
                        names.push(s.display_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| s.addr.clone()));
                    }
                }
            }
        }
    }
    if names.is_empty() && !value.trim().is_empty() {
        return (value.trim().to_string(), value.trim().to_string());
    }
    (names.join(", "), emails.join(", "))
}

fn full_message(sess: &mut Sess, folder: &str, uid: u32) -> Result<(Vec<u8>, Vec<String>), String> {
    sess.select(folder).map_err(err)?;
    let fetched = sess.uid_fetch(uid.to_string(), "(UID FLAGS BODY.PEEK[])").map_err(err)?;
    let m = fetched.iter().next().ok_or("This message no longer exists on the server")?;
    let body = m.body().ok_or("Gmail did not return the message body")?.to_vec();
    let flags = crate::imapx::normalize_flags(&m.flags().iter().map(|f| f.to_string()).collect::<Vec<_>>());
    Ok((body, flags))
}

/// Download and decode one message. Marks it as read on the server.
pub fn fetch_message(sess: &mut Sess, folder: &str, uid: u32, thread_id: Option<String>) -> Result<MailDetail, String> {
    let (raw, mut flags) = full_message(sess, folder, uid)?;
    let parsed = mailparse::parse_mail(&raw).map_err(err)?;
    let mut w = Walk { text: String::new(), html: String::new(), attachments: Vec::new() };
    walk(&parsed, &mut w, false);
    if w.text.is_empty() && !w.html.is_empty() {
        w.text = "This message contains HTML content. Use the HTML view to read it.".into();
    }
    let (from, email) = address_summary(&header(&parsed, "From"));
    let (_, to) = address_summary(&header(&parsed, "To"));
    let (_, cc) = address_summary(&header(&parsed, "Cc"));
    let (_, reply_to) = address_summary(&header(&parsed, "Reply-To"));
    let date_header = header(&parsed, "Date");
    let date = mailparse::dateparse(&date_header).map(iso_date).unwrap_or(date_header);
    if !flags.iter().any(|f| f.eq_ignore_ascii_case("Seen")) {
        sess.uid_store(uid.to_string(), "+FLAGS.SILENT (\\Seen)").map_err(err)?;
        flags.push("Seen".into());
    }
    Ok(MailDetail {
        uid,
        folder: folder.to_string(),
        thread_id,
        from,
        email,
        to,
        cc,
        reply_to,
        subject: header(&parsed, "Subject"),
        date,
        flags,
        text: w.text,
        html: w.html,
        message_id: header(&parsed, "Message-ID"),
        references: header(&parsed, "References"),
        attachments: w.attachments,
    })
}

/// Fetch one attachment (with data) of a message by its index in `MailDetail.attachments`.
pub fn fetch_attachment(sess: &mut Sess, folder: &str, uid: u32, index: u32) -> Result<AttachmentData, String> {
    let (raw, _) = full_message(sess, folder, uid)?;
    let parsed = mailparse::parse_mail(&raw).map_err(err)?;
    let mut w = Walk { text: String::new(), html: String::new(), attachments: Vec::new() };
    walk(&parsed, &mut w, true);
    w.attachments.into_iter().find(|a| a.index == index).ok_or_else(|| "Attachment not found".to_string())
}

// ------------------------------------------------------------------- outgoing

pub fn safe_filename(s: &str) -> String {
    let base = s.rsplit(['/', '\\']).next().unwrap_or("attachment.bin");
    let clean: String = base.chars().filter(|c| !c.is_control() && *c != '/' && *c != '\\').take(180).collect();
    if clean.trim().is_empty() {
        "attachment.bin".into()
    } else {
        clean
    }
}

fn parse_mailboxes(list: &str) -> Result<Vec<Mailbox>, String> {
    let list = list.replace(';', ",");
    if list.trim().is_empty() {
        return Ok(vec![]);
    }
    let parsed = mailparse::addrparse(&list).map_err(|e| format!("Invalid address list: {e}"))?;
    let mut out = Vec::new();
    let mut push = |name: Option<String>, addr: &str| -> Result<(), String> {
        let address: lettre::Address = addr.trim().parse().map_err(|e| format!("Invalid address “{addr}”: {e}"))?;
        out.push(Mailbox::new(name.filter(|n| !n.is_empty()), address));
        Ok(())
    };
    for a in parsed.iter() {
        match a {
            MailAddr::Single(s) => push(s.display_name.clone(), &s.addr)?,
            MailAddr::Group(g) => {
                for s in &g.addrs {
                    push(s.display_name.clone(), &s.addr)?;
                }
            }
        }
    }
    Ok(out)
}

/// Replace `data:image/...;base64,...` URLs in the HTML by `cid:` references. Gmail (and most
/// clients) do not display data: images, so each becomes a proper inline MIME part.
pub fn extract_inline_images(html: &str) -> (String, Vec<(String, ContentType, Vec<u8>)>) {
    let re = Regex::new(r"data:(image/[a-zA-Z0-9.+-]+);base64,([A-Za-z0-9+/=\s]+)").expect("valid regex");
    let mut images = Vec::new();
    let stamp = chrono::Utc::now().timestamp_millis();
    let out = re.replace_all(html, |caps: &regex::Captures<'_>| {
        let mime = caps[1].to_string();
        let cleaned: String = caps[2].chars().filter(|c| !c.is_whitespace()).collect();
        match (BASE64.decode(cleaned.as_bytes()), ContentType::parse(&mime)) {
            (Ok(bytes), Ok(ct)) => {
                let cid = format!("mdimg{}-{}@maildesk.local", images.len() + 1, stamp);
                images.push((cid.clone(), ct, bytes));
                format!("cid:{cid}")
            }
            _ => caps[0].to_string(),
        }
    });
    (out.into_owned(), images)
}

pub fn build_message(req: &SendRequest, require_recipient: bool) -> Result<Message, String> {
    let username = req.username.trim().to_lowercase();
    let sender_addr = req.from_email.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(&username).trim().to_lowercase();
    let sender_name = req
        .from_name
        .as_deref()
        .map(|n| n.replace(['<', '>', '\n', '\r'], "").trim().to_string())
        .filter(|n| !n.is_empty());
    let sender_address = sender_addr.parse::<lettre::Address>().map_err(|e| format!("Invalid sender address: {e}"))?;
    let from = Mailbox::new(sender_name, sender_address.clone());

    let mut b = Message::builder().from(from).subject(req.subject.as_str()).date_now();
    let to = parse_mailboxes(&req.to)?;
    let cc = parse_mailboxes(req.cc.as_deref().unwrap_or(""))?;
    let bcc = parse_mailboxes(req.bcc.as_deref().unwrap_or(""))?;
    let any_recipient = !(to.is_empty() && cc.is_empty() && bcc.is_empty());
    if require_recipient && to.is_empty() && cc.is_empty() && bcc.is_empty() {
        return Err("Enter at least one valid recipient".into());
    }
    for m in to {
        b = b.to(m);
    }
    for m in cc {
        b = b.cc(m);
    }
    for m in bcc {
        b = b.bcc(m);
    }
    if !any_recipient {
        // A draft may have no recipients yet. lettre insists on an SMTP envelope, so give it a
        // placeholder one; the envelope is never part of the formatted message we APPEND.
        let placeholder: lettre::Address = "draft@maildesk.invalid".parse().map_err(err)?;
        b = b.envelope(lettre::address::Envelope::new(Some(sender_address), vec![placeholder]).map_err(err)?);
    }
    if let Some(id) = req.in_reply_to.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        b = b.in_reply_to(id.to_string());
    }
    if let Some(refs) = req.references.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        b = b.references(refs.to_string());
    }

    let text = req.text.clone().unwrap_or_default();
    let html = req.html.clone().unwrap_or_default();
    let text_part = SinglePart::builder().header(ContentType::TEXT_PLAIN).body(text);

    let body: MultiPart = if html.trim().is_empty() {
        MultiPart::mixed().singlepart(text_part)
    } else {
        let (html, images) = extract_inline_images(&html);
        let html_part = SinglePart::builder().header(ContentType::TEXT_HTML).body(html);
        if images.is_empty() {
            MultiPart::alternative().singlepart(text_part).singlepart(html_part)
        } else {
            let mut related = MultiPart::related().singlepart(html_part);
            for (cid, ct, bytes) in images {
                related = related.singlepart(MailAttachment::new_inline(cid).body(bytes, ct));
            }
            MultiPart::alternative().singlepart(text_part).multipart(related)
        }
    };

    let mut message_body = body;
    if let Some(items) = &req.attachments {
        if !items.is_empty() {
            if items.len() > 30 {
                return Err("A maximum of 30 attachments can be sent at once".into());
            }
            let mut mixed = MultiPart::mixed().multipart(message_body);
            for item in items {
                let bytes = BASE64
                    .decode(item.data_base64.as_bytes())
                    .map_err(|_| format!("Attachment {} is not valid base64", item.filename))?;
                if bytes.len() > 25 * 1024 * 1024 {
                    return Err(format!("Attachment {} exceeds the 25 MB per-file limit", item.filename));
                }
                let ct = ContentType::parse(&item.content_type)
                    .unwrap_or_else(|_| ContentType::parse("application/octet-stream").expect("valid fallback content type"));
                mixed = mixed.singlepart(MailAttachment::new(safe_filename(&item.filename)).body(bytes, ct));
            }
            message_body = mixed;
        }
    }
    b.multipart(message_body).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_images_become_cid_parts() {
        let html = r#"<p>Hi</p><img src="data:image/png;base64,iVBORw0KGgo=" width="10"><img src='data:image/gif;base64,R0lGODlh'>"#;
        let (out, images) = extract_inline_images(html);
        assert_eq!(images.len(), 2);
        assert!(!out.contains("data:image"));
        assert!(out.contains("cid:mdimg1-"));
        assert!(out.contains("cid:mdimg2-"));
    }

    fn req(html: &str) -> SendRequest {
        SendRequest {
            username: "me@gmail.com".into(),
            from_email: None,
            from_name: Some("Me".into()),
            to: "\"Doe, John\" <john@example.com>; Ann <ann@example.com>".into(),
            cc: None,
            bcc: None,
            subject: "Hello".into(),
            text: Some("plain".into()),
            html: Some(html.into()),
            in_reply_to: Some("<abc@example.com>".into()),
            references: Some("<abc@example.com>".into()),
            attachments: None,
        }
    }

    #[test]
    fn builds_threaded_multipart_message() {
        let m = build_message(&req(r#"<b>hi</b><img src="data:image/png;base64,iVBORw0KGgo=">"#), true).unwrap();
        let s = String::from_utf8_lossy(&m.formatted()).to_string();
        assert!(s.contains("In-Reply-To: <abc@example.com>"));
        assert!(s.contains("multipart/related"));
        assert!(s.contains("Content-ID"));
        // "Doe, John" contains a comma, so lettre RFC 2047-encodes the display name (base64 of "Doe, John")
        assert!(s.contains("=?utf-8?b?RG9lLCBKb2hu?="));
        assert!(s.contains("ann@example.com"));
    }

    #[test]
    fn rejects_missing_recipient_only_when_required() {
        let mut r = req("<p>x</p>");
        r.to = String::new();
        assert!(build_message(&r, true).is_err());
        assert!(build_message(&r, false).is_ok());
    }

    #[test]
    fn reads_nested_message() {
        let raw = b"From: Alice <alice@example.com>\r\nTo: bob@example.com\r\nSubject: =?UTF-8?Q?Caf=C3=A9?=\r\nContent-Type: multipart/mixed; boundary=b1\r\n\r\n--b1\r\nContent-Type: multipart/alternative; boundary=b2\r\n\r\n--b2\r\nContent-Type: text/plain\r\n\r\nplain body\r\n--b2\r\nContent-Type: text/html\r\n\r\n<p>html body</p>\r\n--b2--\r\n--b1\r\nContent-Type: application/pdf; name=\"r.pdf\"\r\nContent-Disposition: attachment; filename=\"r.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--b1--\r\n";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let mut w = Walk { text: String::new(), html: String::new(), attachments: vec![] };
        walk(&parsed, &mut w, false);
        assert_eq!(w.text.trim(), "plain body");
        assert_eq!(w.html.trim(), "<p>html body</p>");
        assert_eq!(w.attachments.len(), 1);
        assert_eq!(w.attachments[0].filename, "r.pdf");
        assert!(w.attachments[0].data_base64.is_empty());
        let mut w2 = Walk { text: String::new(), html: String::new(), attachments: vec![] };
        walk(&parsed, &mut w2, true);
        assert_eq!(w2.attachments[0].size, 9);
        assert_eq!(header(&parsed, "Subject"), "Café");
    }
}
