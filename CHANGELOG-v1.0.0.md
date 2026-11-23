# WorkDesk Chrome v1.0.0

Fork of WorkDesk v1.0 (all features and UI retained).

* The window is now a kiosk-mode Chrome (`--kiosk --app`), controlled over the DevTools Protocol; no WebKitGTK or Tauri.
* Website tabs are real Chrome tabs (WhatsApp, Meet and every bookmark see a genuine Chrome: WebRTC, camera, microphone, calls).
* Hover bar at the top edge of a website tab: back to WorkDesk, switch / close tabs, back / forward / reload, address.
* Camera, microphone, notifications, clipboard and screen-share permissions are pre-granted for opened sites.
* Pop-ups from sites become website tabs; "Open in Chrome" opens a tab in the same window.
* New setting: Window mode (Kiosk / Normal window). Single-instance: a second launch raises the first.
* Local backend server on 127.0.0.1 with per-run secret token; the AppImage is a single static-ish binary (OpenSSL bundled).
* Call check (Settings → Calls) now reports on Chrome, camera and audio.
* Data folders, keyring entry and preferences are unchanged from v1.0.
