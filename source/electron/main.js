// WorkDesk Chrome v2: the window, the tab-area website pages and the window buttons live here (Chromium via Electron);
// the mail engine is the Rust backend, started as a child process and used through its local HTTP API.
const { app, BaseWindow, WebContentsView, ipcMain, session, desktopCapturer, shell, Menu, clipboard, dialog, Notification } = require('electron');
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');

app.setName('WorkDesk Chrome');
app.commandLine.appendSwitch('enable-features', 'WebRTCPipeWireCapturer');
app.commandLine.appendSwitch('ozone-platform-hint', 'auto');
app.commandLine.appendSwitch('autoplay-policy', 'no-user-gesture-required');
app.commandLine.appendSwitch('disable-features', 'Translate');

const DATA = path.join(os.homedir(), '.local', 'share', 'com.workdesk.app');
fs.mkdirSync(DATA, { recursive: true });
const LOG = path.join(DATA, 'workdesk-chrome.log');
const log = (...a) => { try { fs.appendFileSync(LOG, `${new Date().toISOString()} ${a.join(' ')}\n`); } catch { /* ignore */ } };
try { if (fs.existsSync(LOG) && fs.statSync(LOG).size > 1e6) fs.renameSync(LOG, LOG + '.old'); } catch { /* ignore */ }

const SITES = 'persist:workdesk-sites';
let win = null, ui = null, backend = null, base = '';
const sites = new Map();       // label -> { view, title }
let activeLabel = null;
let rect = { x: 0, y: 80, w: 800, h: 600 };
let popupN = 0, fullscreenSite = null, quitting = false;

const cleanUA = ua => ua.replace(/ Electron\/[\d.]+/, '').replace(/(Gecko\)) [^ ]+\/[\d.]+ (Chrome\/)/, '$1 $2');
const emit = (name, payload) => { try { if (ui && !ui.webContents.isDestroyed()) ui.webContents.send('wd-event', name, payload); } catch { /* ignore */ } };
const R = v => Math.max(0, Math.round(v));

// ------------------------------------------------------------------ backend
function findBackend() {
  const c = [process.env.WORKDESK_BACKEND, path.join(__dirname, '..', 'bin', 'workdesk-chrome'), path.join(__dirname, 'bin', 'workdesk-chrome'), path.join(process.resourcesPath || '', 'bin', 'workdesk-chrome')].filter(Boolean);
  return c.find(p => { try { fs.accessSync(p, fs.constants.X_OK); return true; } catch { return false; } });
}
function startBackend() {
  return new Promise((resolve, reject) => {
    const bin = findBackend();
    if (!bin) return reject(new Error('The WorkDesk mail engine (bin/workdesk-chrome) was not found next to the app.'));
    log('backend', bin);
    backend = spawn(bin, [], { env: { ...process.env, WORKDESK_SHELL: 'electron', WORKDESK_PORT: process.env.WORKDESK_PORT || '47821' }, stdio: ['pipe', 'pipe', 'pipe'] });
    let buf = '', done = false;
    const timer = setTimeout(() => { if (!done) { done = true; reject(new Error('The mail engine did not start within 20 seconds.')); } }, 20000);
    backend.stdout.on('data', d => {
      buf += d;
      const m = buf.match(/WD_READY (\{.*\})/);
      if (m && !done) { done = true; clearTimeout(timer); try { resolve(JSON.parse(m[1])); } catch (e) { reject(e); } }
    });
    backend.stderr.on('data', d => log('backend:', String(d).trim()));
    backend.on('exit', (code, sig) => {
      log('backend exited', code, sig);
      if (!done) { done = true; clearTimeout(timer); reject(new Error('The mail engine stopped right after starting (code ' + code + '). See ' + LOG)); }
      else if (!quitting) { dialog.showErrorBoxSync('WorkDesk Chrome', 'The mail engine stopped unexpectedly. WorkDesk will close.\n\nLog: ' + LOG); app.quit(); }
    });
  });
}

// ------------------------------------------------------------------ layout
function layout() {
  if (!win || !ui) return;
  const [w, h] = win.getContentSize();
  ui.setBounds({ x: 0, y: 0, width: w, height: h });
  for (const [label, s] of sites) {
    const on = label === activeLabel;
    s.view.setVisible(on);
    if (!on) continue;
    s.view.setBounds(fullscreenSite === label ? { x: 0, y: 0, width: w, height: h } : { x: R(rect.x), y: R(rect.y), width: Math.min(R(rect.w), w), height: Math.min(R(rect.h), h) });
  }
}

function siteState(label) {
  const s = sites.get(label);
  if (!s) return null;
  const c = s.view.webContents;
  const nh = c.navigationHistory;
  return { label, url: c.getURL(), title: c.getTitle() || s.title || '', canBack: nh.canGoBack(), canForward: nh.canGoForward(), loading: c.isLoading() };
}
const pushState = label => { const st = siteState(label); if (st) emit('site-state', st); };

function showSite(label, r) {
  if (r && r.w > 0) rect = { x: r.x, y: r.y, w: r.w, h: r.h };
  activeLabel = label && sites.has(label) ? label : null;
  if (activeLabel) {
    const s = sites.get(activeLabel);
    win.contentView.addChildView(s.view);      // front
    layout();
    s.view.webContents.focus();
  } else layout();
}

function openSite(label, url, ua) {
  if (sites.has(label)) return;
  const view = new WebContentsView({ webPreferences: { partition: SITES, contextIsolation: true, sandbox: true } });
  const c = view.webContents;
  const s = { view, title: '' };
  sites.set(label, s);
  win.contentView.addChildView(view);
  view.setVisible(false);
  c.setUserAgent(ua && /^Mozilla\/5\.0/.test(ua) ? ua : cleanUA(c.getUserAgent()));
  c.setWindowOpenHandler(({ url: u }) => {
    log('popup', u);
    if (/^https?:/i.test(u)) {
      const nl = 'popup-' + (++popupN);
      openSite(nl, u, ua);
      emit('site-new', { label: nl, url: u, title: '' });
      showSite(nl);
    }
    return { action: 'deny' };
  });
  c.on('page-title-updated', (_e, t) => { s.title = t; pushState(label); });
  for (const ev of ['did-start-loading', 'did-stop-loading', 'did-navigate', 'did-navigate-in-page']) c.on(ev, () => pushState(label));
  c.on('did-fail-load', (_e, code, desc, u, main) => { if (main && code !== -3) log('site load failed', label, code, desc, u); });
  c.on('render-process-gone', (_e, d) => log('site renderer gone', label, JSON.stringify(d)));
  c.on('enter-html-full-screen', () => { fullscreenSite = label; win.setFullScreen(true); layout(); });
  c.on('leave-html-full-screen', () => { fullscreenSite = null; win.setFullScreen(false); layout(); });
  c.on('context-menu', (_e, p) => siteMenu(c, p));
  c.on('before-input-event', (e, i) => {
    if (i.type !== 'keyDown') return;
    if (i.control && i.shift && i.key.toLowerCase() === 'i') c.toggleDevTools();
    if (i.key === 'F5') c.reload();
    if (i.alt && i.key === 'ArrowLeft') c.navigationHistory.goBack();
    if (i.alt && i.key === 'ArrowRight') c.navigationHistory.goForward();
  });
  c.loadURL(url).catch(e => { if (!/ERR_ABORTED|-3\)/.test(String(e))) log('loadURL', label, e.message); });
}

function closeSite(label) {
  const s = sites.get(label);
  if (!s) return;
  sites.delete(label);
  try { win.contentView.removeChildView(s.view); s.view.webContents.close(); } catch { /* ignore */ }
  if (activeLabel === label) activeLabel = null;
  layout();
}

function siteMenu(c, p) {
  const t = [];
  if (p.linkURL) t.push({ label: 'Open link in a new WorkDesk tab', click: () => { const nl = 'popup-' + (++popupN); openSite(nl, p.linkURL); emit('site-new', { label: nl, url: p.linkURL, title: '' }); showSite(nl); } }, { label: 'Copy link address', click: () => clipboard.writeText(p.linkURL) }, { type: 'separator' });
  if (p.isEditable) t.push({ role: 'undo' }, { role: 'redo' }, { type: 'separator' }, { role: 'cut' }, { role: 'copy' }, { role: 'paste' }, { role: 'selectAll' });
  else if (p.selectionText) t.push({ role: 'copy' });
  if (!p.isEditable) t.push({ label: 'Back', enabled: c.navigationHistory.canGoBack(), click: () => c.navigationHistory.goBack() }, { label: 'Forward', enabled: c.navigationHistory.canGoForward(), click: () => c.navigationHistory.goForward() }, { label: 'Reload', click: () => c.reload() });
  if (t.length) Menu.buildFromTemplate(t).popup();
}

// ------------------------------------------------------------------ commands from the page
const probeScript = `JSON.stringify({ ua: navigator.userAgent, rtcPeerConnection: typeof RTCPeerConnection, mediaDevices: !!(navigator.mediaDevices && navigator.mediaDevices.getUserMedia), secure: window.isSecureContext, getCapabilities: typeof (window.RTCRtpSender && RTCRtpSender.getCapabilities) })`;

async function webrtcLines() {
  const L = [];
  L.push('OK|Chromium engine ' + process.versions.chrome + ' (Electron ' + process.versions.electron + ') runs every website tab, as Chrome does.');
  try {
    const d = await ui.webContents.executeJavaScript('navigator.mediaDevices.enumerateDevices().then(a => a.map(x => x.kind))');
    const n = k => d.filter(x => x === k).length;
    L.push((n('audioinput') ? 'OK' : 'FAIL') + '|Microphones found: ' + n('audioinput'));
    L.push((n('videoinput') ? 'OK' : 'INFO') + '|Cameras found: ' + n('videoinput'));
    L.push((n('audiooutput') ? 'OK' : 'INFO') + '|Speakers found: ' + n('audiooutput'));
  } catch (e) { L.push('INFO|Could not list audio/video devices: ' + e.message); }
  L.push('INFO|Camera, microphone, notifications and screen sharing are allowed for the sites you open.');
  return L;
}

const handlers = {
  site_open: a => { openSite(a.label, a.url, a.ua); return null; },
  site_show: a => { showSite(a.label || null, a.rect); return null; },
  site_layout: a => { if (a && a.w > 0) { rect = { x: a.x, y: a.y, w: a.w, h: a.h }; layout(); } return null; },
  site_close: a => { closeSite(a.label); return null; },
  site_state: a => siteState(a.label),
  site_probe: async a => { const s = sites.get(a.label); if (!s) return JSON.stringify({ error: 'tab not open' }); try { return await s.view.webContents.executeJavaScript(probeScript); } catch (e) { return JSON.stringify({ error: e.message }); } },
  site_action: a => {
    const s = sites.get(a.label); if (!s) throw 'This website tab is not open.';
    const c = s.view.webContents;
    if (a.action === 'back') c.navigationHistory.goBack();
    else if (a.action === 'forward') c.navigationHistory.goForward();
    else if (a.action === 'reload') c.reload();
    else if (a.action === 'stop') c.stop();
    else if (a.action === 'load' && /^https?:\/\//i.test(a.url || '')) c.loadURL(a.url).catch(() => {});
    return null;
  },
  win_action: a => {
    switch (a.action) {
      case 'minimize': win.minimize(); return null;
      case 'maximize': win.maximize(); return null;
      case 'unmaximize': win.unmaximize(); return null;
      case 'is_maximized': return win.isMaximized();
      case 'close': win.close(); return null;
      default: throw 'Unknown window action: ' + a.action;
    }
  },
  app_info: () => ({ chrome: true, version: app.getVersion(), error: '', log: LOG, kiosk: true, failed: false }),
  webrtc_support: () => webrtcLines(),
};
ipcMain.handle('wd-invoke', async (_e, cmd, args) => {
  const h = handlers[cmd];
  if (!h) return { err: 'Unknown shell command: ' + cmd };
  try { return { ok: await h(args || {}) }; } catch (e) { return { err: String(e && e.message || e) }; }
});

// ------------------------------------------------------------------ window
function stateFile() { return path.join(app.getPath('userData'), 'window.json'); }
function loadBounds() { try { return JSON.parse(fs.readFileSync(stateFile(), 'utf8')); } catch { return {}; } }

async function start() {
  let info;
  try { info = await startBackend(); } catch (e) { log('start failed', e.message); dialog.showErrorBoxSync('WorkDesk Chrome', e.message); return app.quit(); }
  base = `http://127.0.0.1:${info.port}`;

  const ses = session.fromPartition(SITES);
  ses.setUserAgent(cleanUA(ses.getUserAgent()));
  ses.setPermissionRequestHandler((_wc, _perm, cb) => cb(true));
  ses.setPermissionCheckHandler(() => true);
  ses.setDisplayMediaRequestHandler(async (_req, cb) => {
    try { const s = await desktopCapturer.getSources({ types: ['screen', 'window'] }); cb(s.length ? { video: s[0], audio: 'loopback' } : {}); } catch (e) { log('display media', e.message); cb({}); }
  }, { useSystemPicker: true });
  ses.on('will-download', (_e, item) => {
    const dir = app.getPath('downloads');
    let name = item.getFilename() || 'download', dest = path.join(dir, name), n = 1;
    const ext = path.extname(name), stem = path.basename(name, ext);
    while (fs.existsSync(dest)) dest = path.join(dir, `${stem} (${n++})${ext}`);
    item.setSavePath(dest);
    item.once('done', (_ev, st) => emit('site-download', { name: path.basename(dest), path: dest, ok: st === 'completed' }));
  });

  const b = loadBounds();
  win = new BaseWindow({ width: b.width || 1360, height: b.height || 860, x: b.x, y: b.y, minWidth: 900, minHeight: 560, frame: false, show: false, backgroundColor: '#f4f6fb', title: 'WorkDesk — Design By Rashid', icon: path.join(__dirname, 'icon.png') });
  ui = new WebContentsView({ webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true, sandbox: true, spellcheck: true, backgroundThrottling: false } });
  win.contentView.addChildView(ui);
  const uc = ui.webContents;
  uc.setZoomFactor(1);
  uc.setVisualZoomLevelLimits(1, 1);
  uc.on('zoom-changed', () => uc.setZoomFactor(1));
  const toTab = u => { if (/^https?:\/\//i.test(u)) emit('open-url', { url: u }); };
  uc.setWindowOpenHandler(({ url }) => { toTab(url); return { action: 'deny' }; });
  uc.on('will-navigate', (e, url) => { if (!url.startsWith(base)) { e.preventDefault(); toTab(url); } });
  uc.on('context-menu', (_e, p) => {
    const t = [];
    if (p.isEditable) { for (const s of p.dictionarySuggestions || []) t.push({ label: s, click: () => uc.replaceMisspelling(s) }); if (t.length) t.push({ type: 'separator' }); t.push({ role: 'undo' }, { role: 'redo' }, { type: 'separator' }, { role: 'cut' }, { role: 'copy' }, { role: 'paste' }, { role: 'selectAll' }); }
    else if (p.selectionText) t.push({ role: 'copy' });
    if (p.linkURL && /^https?:/i.test(p.linkURL)) t.push({ label: 'Copy link address', click: () => clipboard.writeText(p.linkURL) });
    if (t.length) Menu.buildFromTemplate(t).popup();
  });
  uc.on('before-input-event', (e, i) => { if (i.type === 'keyDown' && i.control && i.shift && i.key.toLowerCase() === 'i') uc.toggleDevTools(); });
  uc.on('render-process-gone', (_e, d) => log('ui renderer gone', JSON.stringify(d)));
  uc.loadURL(info.url).catch(e => log('ui load', e.message));

  win.on('resize', layout);
  win.on('maximize', layout);
  win.on('unmaximize', layout);
  win.on('enter-full-screen', layout);
  win.on('leave-full-screen', layout);
  win.on('close', () => { try { if (!win.isMaximized() && !win.isFullScreen()) fs.writeFileSync(stateFile(), JSON.stringify(win.getBounds())); } catch { /* ignore */ } });
  win.on('closed', () => { quitting = true; try { backend && backend.kill(); } catch { /* ignore */ } app.quit(); });
  win.show();
  layout();
  uc.once('did-finish-load', () => layout());
  if (process.env.WORKDESK_DEBUG) uc.openDevTools({ mode: 'detach' });
}

const lock = app.requestSingleInstanceLock();
if (!lock) app.quit();
else {
  app.on('second-instance', () => { if (win) { if (win.isMinimized()) win.restore(); win.focus(); } });
  app.whenReady().then(start);
  app.on('before-quit', () => { quitting = true; try { backend && backend.kill(); } catch { /* ignore */ } });
}
