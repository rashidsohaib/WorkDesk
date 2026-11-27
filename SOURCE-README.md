# WorkDesk Chrome v2.10.4 – source

What is in here
* `backend/`  – Rust backend, **complete and current** (rebuilt from v2.3 + every backend change up to v2.10.1, plus the inline-image fix).
* `electron/` – Electron shell, **current** (main.js, preload.js recovered from the v2.10.1 AppImage; they are plain readable files).
* `dist/`     – the **current frontend (v2.10)**, finished/minified build that is embedded into the backend binary. Recovered from the v2.10.1 AppImage, with the inline-image fix applied.
* `main.js`, `sanitize.js`, `editor.js`, `styles.css`, `index.html`, … in this folder are the **readable v2.3 frontend sources**. They are OUTDATED (no contacts manager, advanced search, alias table, Move to Inbox, etc.). Do not rebuild `dist/` from them.
* `tests/`    – fake Gmail IMAP server + end-to-end driver used to test the backend (`python3 tests/drive.py <debug binary>`).

Build: `cargo build --release --manifest-path backend/Cargo.toml` (embeds `dist/`), then place the binary as `bin/workdesk-chrome` in the AppImage tree.

Backend additions since v2.3 (all re-implemented): encrypted vault for the App Password (compatible with v2.10.1 files), detached background sender for scheduled mail, edit-and-continue drafts (`folder_validity`, `discard_draft`), BCC in message details, `get_message` markSeen + decoded-message cache, memory-only background download of the newest 500 messages of Inbox/Sent/All Mail (`warm_inbox`, `prefetch_message`, 600 MB cache shared across labels), `thread_messages` refresh flag, Move to Inbox (`add_to_inbox`), inline-image detection for mail sent from Gmail web.
