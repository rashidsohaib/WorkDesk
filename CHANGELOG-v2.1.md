# WorkDesk Chrome v2.1

* Delivered as an **AppImage** (Chromium shell and mail engine inside; nothing to download on first run).
* Removed from Settings, because the website tabs now run in WorkDesk's own Chromium window:
  * WhatsApp user agent
  * Website tabs and Window mode choices
  * "Browser for open externally"
  * Chrome extensions (and the extension buttons in the left rail)
  * Chrome browser (Settings / Extensions / Passwords / History shortcuts) and the "Open in Chrome" button in the website bar
  * The per-bookmark "Open in" choice
* Kept: the "Open in the system browser" button in the website bar, and the Calls check.
* Everything else is as in v2.0. Data stays in `~/.local/share/com.workdesk.app`, `~/.config/com.workdesk.app`, the keyring and `~/Downloads/WorkDesk Notes`.
