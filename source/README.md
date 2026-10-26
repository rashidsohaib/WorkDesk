# WorkDesk Chrome — Design By Rashid  (v1.0.4)

WorkDesk Chrome is the fork of WorkDesk v1.0 whose whole user interface runs in a **kiosk-mode Chrome** instead of the embedded WebKitGTK view. Every feature of WorkDesk v1.0 is kept and the UI/UX is unchanged; the difference is that websites (WhatsApp Web, Google Meet, Calendar, any bookmark) now run in a genuine Chrome engine, so WebRTC, camera, microphone and calls work.

## How it works

* `backend/` — one Rust executable (no Tauri, no WebKit). It contains all the v1.0 mail logic (IMAP header cache, Gmail search, SMTP, drafts, scheduling, contacts, Notepad storage, preferences, keyring) and
  * a tiny local web server on `127.0.0.1` (random port, secret token, cookie) that serves the embedded frontend, answers `/rpc/<command>` calls and streams events (`/events`);
  * a launcher that starts Chrome with `--kiosk --new-window <local page>` and a **dedicated profile** (`~/.local/share/com.workdesk.app/chrome-profile`), then drives it through the Chrome DevTools Protocol.
* Frontend (`index.html`, `main.js`, …) is the v1.0 frontend. Only `bridge.js` is new: it replaces the Tauri `invoke` / `listen` / window calls.
* **Default: every website is a real Chrome tab in the same Chrome window**, right next to the WorkDesk tab in Chrome's own tab strip (WorkDesk's tab strip lists them too, and clicking a pinned URL switches to its Chrome tab). One window, full compatibility: WhatsApp, Meet and Google sign-in behave as in a normal Chrome, so calls work. Camera, microphone, notifications and screen-share are pre-granted for the sites you open.
* Optional: Settings → **Website tabs** (or the *Open in* choice of each bookmark) can show a site *inside the WorkDesk page* under WorkDesk's own tab strip instead. This is done by framing the site and removing its anti-framing headers over the DevTools Protocol. It works for many sites, but Google, WhatsApp and others that need their own sign-in cookies often refuse to run in a frame, so it is opt-in.
* Settings → **Window mode**: *Normal window* (default; Chrome's tab strip is visible) or *Kiosk* (full screen, no browser frame; websites are then reached through a hover bar at the top edge).
* Pop-ups and `target=_blank` pages opened by a site become additional website tabs.
* "Open in Chrome" buttons (Chrome settings, extension pages) open a new tab inside the same window.

## What you need on the computer

* Google Chrome (recommended) or Chromium / Brave / Edge. WorkDesk Chrome finds it automatically (Settings → "Browser" can force one).
* Linux x86-64 with glibc 2.39 or newer (Debian 13 / Q4OS Trixie is fine) for the prebuilt AppImage.

## Run

```bash
chmod +x WorkDesk-Chrome-v1.0.4-x86_64.AppImage
./WorkDesk-Chrome-v1.0.4-x86_64.AppImage
```

Starting it a second time simply brings the running window to the front. Closing the WorkDesk window (the ✕ in the header) closes Chrome and ends the program.

`WORKDESK_WINDOW=kiosk` or `=window` forces the window mode for one run.

## Build from source

```bash
sudo apt install -y build-essential pkg-config libssl-dev curl file
# Node 20+ and Rust stable
./build.sh          # npm install, frontend build, cargo build --release, AppImage
```

Manual steps: `npm install && npm run build && cargo build --release --manifest-path backend/Cargo.toml`. The frontend in `dist/` is embedded into the binary at compile time, so build the frontend first. Binary: `backend/target/release/workdesk-chrome`.

## Data locations (same as v1.0 — your existing data is picked up automatically)

* Mail header cache, notes, scheduled mail: `~/.local/share/com.workdesk.app/`
* Preferences: `~/.config/com.workdesk.app/preferences.json`
* App Password: OS keyring, service `com.workdesk.app`
* Notepad backup: `~/Downloads/WorkDesk Notes/workdesk-notes-backup.json` (+ `.json.bak`)
* Chrome profile (sites' logins, cookies, extensions): `~/.local/share/com.workdesk.app/chrome-profile/`

## Things that differ from v1.0 — please read

1. **Separate Chrome profile.** Chrome refuses remote control of your everyday profile, so WorkDesk Chrome uses its own. Sign in to WhatsApp (scan the QR code once), Google, etc. inside WorkDesk Chrome; they are remembered afterwards. Chrome extensions must be installed inside this profile (open the Chrome Web Store as a bookmark and install them there); pinned extension buttons then open in a new tab as before.
2. Inside Chrome's tab strip you see two layers of tabs: Chrome's (WorkDesk, Calendar, WhatsApp …) and WorkDesk's own (Mail, message tabs, Notepad). Website tabs appear in both.
3. **Window buttons** (minimise / maximise / close) ask Chrome to change the window state. In kiosk mode some window managers ignore minimise; use *Normal window* mode if yours does.
4. Google may be stricter about sign-in in an automation-controlled Chrome. If a Google login page refuses, sign in once with *Window mode → Normal window*, or use an App-Password-free site such as WhatsApp first.

## Troubleshooting

* If WorkDesk cannot start or attach to Chrome, it shows the exact reason in a red message and writes a log to `~/.local/share/com.workdesk.app/workdesk-chrome.log` (Chrome's own error output is included). Send me that file if the problem persists.
* WorkDesk tries every Chrome / Chromium / Brave / Edge it can find (including Flatpak and Snap installs, each with a profile inside its own sandbox folder) until one starts. Settings → Browser can put a specific one first.
* Test from a terminal: `WORKDESK_WINDOW=window ./WorkDesk-Chrome-v1.0.4-x86_64.AppImage`.

## Fall-back

WorkDesk v1.0 (the WebKit/Tauri build) is kept unchanged in the `WorkDesk-v1.0` folder delivered alongside this one.

## Tests run on this source

* `cargo check` / release build; JavaScript syntax check; Vite build.
* UI regression suite from v1.0 against the new frontend (86 checks pass; two known harness-only noise items).
* Integration test with a real Chromium under Xvfb: local server, cookie/token, RPC round-trips, Notepad storage, DevTools control (open / navigate / back / forward / close site tabs, popup adoption, hover bar → Home, `chrome://` tabs, window state), WebRTC present in website tabs (`RTCPeerConnection`, `getUserMedia`, `RTCRtpSender.getCapabilities`).
* **Not tested here:** live Gmail IMAP/SMTP, WhatsApp / Meet calls against the real services, a physical camera / microphone, Google Chrome stable (Chromium 141 was used), and a real desktop window manager.
