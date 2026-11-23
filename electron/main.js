// WorkDesk Chrome v2: the window, the tab-area website pages and the window buttons live here (Chromium via Electron);
// the mail engine is the Rust backend, started as a child process and used through its local HTTP API.
const { app, BaseWindow, WebContentsView, ipcMain, session, desktopCapturer, shell, Menu, clipboard, dialog, Notification, net } = require('electron');
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');

app.setName('WorkDesk Chrome');
app.commandLine.appendSwitch('enable-features', 'WebRTCPipeWireCapturer');
app.commandLine.appendSwitch('ozone-platform-hint', 'auto');
app.commandLine.appendSwitch('autoplay-policy', 'no-user-gesture-required');
app.commandLine.appendSwitch('disable-features', 'Translate');
// A call must keep running while its tab is hidden behind another WorkDesk tab: no timer, renderer or occlusion throttling.
app.commandLine.appendSwitch('disable-renderer-backgrounding');
app.commandLine.appendSwitch('disable-background-timer-throttling');
app.commandLine.appendSwitch('disable-backgrounding-occluded-windows');
// Troubleshooting switches, used from a terminal:  WORKDESK_DISABLE_GPU=1 ./WorkDesk-Chrome-*.AppImage
if (process.env.WORKDESK_DISABLE_GPU) app.disableHardwareAcceleration();

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

// ------------------------------------------------------------------ site icons / print
async function grab(u, max, ms = 7000) {
  const r = await net.fetch(u, { signal: AbortSignal.timeout(ms), redirect: 'follow', headers: { 'User-Agent': cleanUA(session.defaultSession.getUserAgent()) } });
  if (!r.ok) throw new Error('HTTP ' + r.status);
  const b = Buffer.from(await r.arrayBuffer());
  if (b.length > max) throw new Error('too large');
  return { b, type: (r.headers.get('content-type') || '').split(';')[0].trim().toLowerCase() };
}
const iconData = (b, type, u) => {
  let t = type;
  if (!/^image\//.test(t)) t = /\.svg(\?|$)/i.test(u) ? 'image/svg+xml' : /\.png(\?|$)/i.test(u) ? 'image/png' : /\.ico(\?|$)/i.test(u) ? 'image/x-icon' : '';
  if (!t || b.length < 20) throw new Error('not an image');
  return 'data:' + t + ';base64,' + b.toString('base64');
};
async function fetchFavicon(pageUrl) {
  const base = new URL(pageUrl);
  if (!/^https?:$/.test(base.protocol)) throw new Error('Not a web address');
  const cands = [];
  try {
    const { b } = await grab(base.href, 600000);
    const html = b.toString('utf8', 0, 200000);
    for (const m of html.matchAll(/<link\b[^>]*>/gi)) {
      const tag = m[0];
      if (!/rel\s*=\s*["'][^"']*icon/i.test(tag)) continue;
      const h = /href\s*=\s*["']([^"']+)["']/i.exec(tag);
      if (!h) continue;
      const sz = /sizes\s*=\s*["'](\d+)/i.exec(tag);
      try { cands.push({ u: new URL(h[1], base).href, w: sz ? +sz[1] : 0, apple: /apple-touch/i.test(tag) }); } catch { /* skip */ }
    }
  } catch { /* fall back to /favicon.ico */ }
  cands.sort((a, b) => (Math.abs((a.w || 32) - 64) - Math.abs((b.w || 32) - 64)));
  cands.push({ u: new URL('/favicon.ico', base).href });
  for (const c of cands.slice(0, 5)) {
    try { const { b, type } = await grab(c.u, 150000); return iconData(b, type, c.u); } catch { /* next */ }
  }
  return '';
}
const printUi = () => new Promise((res, rej) => ui.webContents.print({ silent: false, printBackground: true }, (ok, why) => (ok || /cancel/i.test(why || '') ? res(ok) : rej(new Error(why || 'print failed')))));

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

// `creation` is given when the page itself opened this tab with window.open(): the new page keeps its link to the
// opener (window.opener, postMessage, window.close), which Google Chat / Meet calls and sign-in pop-ups rely on.

const WDDIAG = `(() => { try {
  if (window.__wdd) return; window.__wdd = 1;
  // Electron cannot open Document Picture-in-Picture windows. Hide the API so Google Chat / Meet use their normal in-page call view.
  try { delete window.documentPictureInPicture; if ('documentPictureInPicture' in window) Object.defineProperty(window, 'documentPictureInPicture', { value: undefined, configurable: true }); } catch (e) {}
  try { delete window.DocumentPictureInPicture; delete window.DocumentPictureInPictureEvent; } catch (e) {}
  const L = (...a) => { try { console.debug('WDDIAG ' + location.hostname + ' ' + a.join(' ')); } catch (e) {} };
  const md = navigator.mediaDevices;
  if (md && md.getUserMedia) {
    const g = md.getUserMedia.bind(md);
    md.getUserMedia = async c => { L('getUserMedia request', JSON.stringify(c)); try { const s = await g(c); L('getUserMedia ok', s.getTracks().map(t => t.kind + ':' + t.readyState + ':' + t.label).join('|')); return s; } catch (e) { L('getUserMedia FAILED', e.name, e.message); throw e; } };
  }
  if (md && md.enumerateDevices) {
    const en = md.enumerateDevices.bind(md);
    md.enumerateDevices = async () => { const d = await en(); L('enumerateDevices', d.map(x => x.kind + ':' + (x.label ? 'named' : 'unnamed')).join(',')); return d; };
  }
  if (window.RTCPeerConnection) {
    const P = window.RTCPeerConnection;
    window.RTCPeerConnection = function (...a) {
      const pc = new P(...a);
      L('RTCPeerConnection created', JSON.stringify((a[0] && a[0].iceServers || []).map(s => [].concat(s.urls).join('+'))).slice(0, 200));
      pc.addEventListener('iceconnectionstatechange', () => L('ice', pc.iceConnectionState));
      pc.addEventListener('connectionstatechange', () => L('connection', pc.connectionState));
      pc.addEventListener('icecandidateerror', e => L('icecandidateerror', e.errorCode, e.errorText, e.url));
      pc.addEventListener('signalingstatechange', () => L('signaling', pc.signalingState));
      return pc;
    };
    window.RTCPeerConnection.prototype = P.prototype;
    Object.setPrototypeOf(window.RTCPeerConnection, P);
  }
  if (window.AudioContext) { const A = window.AudioContext; window.AudioContext = function (...a) { try { const c = new A(...a); L('AudioContext', c.state, c.sampleRate); return c; } catch (e) { L('AudioContext FAILED', e.message); throw e; } }; window.AudioContext.prototype = A.prototype; }
  window.addEventListener('unhandledrejection', e => { const r = e.reason; L('unhandledrejection', (r && (r.name + ': ' + r.message)) || String(r)); });
} catch (e) {} })();`;

function openSite(label, url, ua, creation) {
  if (sites.has(label)) return null;
  const view = new WebContentsView(creation || { webPreferences: { partition: SITES, contextIsolation: true, sandbox: true } });
  const c = view.webContents;
  const s = { view, title: '' };
  sites.set(label, s);
  win.contentView.addChildView(view);
  view.setVisible(false);
  c.setUserAgent(ua && /^Mozilla\/5\.0/.test(ua) ? ua : cleanUA(c.getUserAgent()));
  // Electron reports the browser as "Chromium" only. Google Chat / Meet calls check for "Google Chrome", so the client hints say so too.
  const hints = () => {
    try {
      const full = process.versions.chrome, major = full.split('.')[0];
      if (!c.debugger.isAttached()) c.debugger.attach('1.3');
      c.debugger.sendCommand('Emulation.setUserAgentOverride', {
        userAgent: c.getUserAgent(),
        userAgentMetadata: {
          brands: [{ brand: 'Not-A.Brand', version: '24' }, { brand: 'Chromium', version: major }, { brand: 'Google Chrome', version: major }],
          fullVersionList: [{ brand: 'Not-A.Brand', version: '24.0.0.0' }, { brand: 'Chromium', version: full }, { brand: 'Google Chrome', version: full }],
          platform: 'Linux', platformVersion: os.release().split('-')[0], architecture: 'x86', model: '', mobile: false, bitness: '64', wow64: false,
        },
      }).catch(e => log('client hints failed', e.message));
    } catch (e) { log('client hints failed', e.message); }
  };
  hints();
  // Call troubleshooting: failed network requests and browser-level warnings of the page and its frames (e.g. the call frame).
  let probeReady = Promise.resolve();
  try {
    const dbg = c.debugger;
    let n = 0;
    const note = (...a) => { if (n++ < 300) log('diag', label, ...a); };
    dbg.on('message', (_e, method, p, sid) => {
      try {
        if (method === 'Network.loadingFailed' && !p.canceled) note('request failed', p.errorText, p.blockedReason || '', (p.type || ''), 'id=' + p.requestId);
        else if (method === 'Network.requestWillBeSent' && /meet\.google\.com\/(call|_\/)|\/\$rpc\/|clients\d?\.google|signaling|webrtc|stun|turn/i.test(p.request.url)) note('request', p.request.method, p.request.url.slice(0, 140));
        else if (method === 'Runtime.consoleAPICalled' && p.args && p.args[0] && String(p.args[0].value || '').startsWith('WDDIAG')) note(p.args.map(a => a.value).join(' ').slice(6, 400));
        else if (method === 'Network.webSocketClosed') note('websocket closed', p.requestId);
        else if (method === 'Log.entryAdded' && p.entry.level !== 'verbose' && p.entry.level !== 'info') note('log', p.entry.level, p.entry.source, String(p.entry.text).slice(0, 240), String(p.entry.url || '').slice(0, 80));
        else if (method === 'Runtime.exceptionThrown') note('exception', String((p.exceptionDetails.exception && p.exceptionDetails.exception.description) || p.exceptionDetails.text).slice(0, 300));
        else if (method === 'Target.attachedToTarget') {
          note('frame attached', p.targetInfo.type, String(p.targetInfo.url).slice(0, 100));
          for (const m of ['Network.enable', 'Log.enable', 'Runtime.enable']) dbg.sendCommand(m, {}, p.sessionId).catch(() => {});
          dbg.sendCommand('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, p.sessionId).catch(() => {});
          dbg.sendCommand('Page.enable', {}, p.sessionId).then(() => dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', { source: WDDIAG, runImmediately: true }, p.sessionId)).catch(e => note('probe install failed', e.message)).finally(() => dbg.sendCommand('Runtime.runIfWaitingForDebugger', {}, p.sessionId).catch(() => {}));
        }
      } catch { /* ignore */ }
    });
    for (const m of ['Network.enable', 'Log.enable', 'Runtime.enable']) dbg.sendCommand(m).catch(() => {});
    dbg.sendCommand('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }).catch(() => {});
    const t0 = Date.now();
    probeReady = dbg.sendCommand('Page.enable').then(() => dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', { source: WDDIAG, runImmediately: true })).then(() => note('probe ready', (Date.now() - t0) + 'ms')).catch(e => note('probe install failed', e.message));
  } catch (e) { log('diag setup failed', e.message); }
  // Backup for the top frame: hide Document PiP as soon as the DOM exists (Chat checks it when the call button is clicked).
  c.on('dom-ready', () => { c.executeJavaScript("try{delete window.documentPictureInPicture;if('documentPictureInPicture' in window)Object.defineProperty(window,'documentPictureInPicture',{value:undefined,configurable:true})}catch(e){}").catch(() => {}); });
  c.setWindowOpenHandler(({ url: u, frameName, features, disposition }) => {
    log('popup', u, '| name=' + (frameName || ''), '| features=' + (features || ''), '| disposition=' + disposition, '| from=' + label);
    if (!/^(https?:|about:blank$)/i.test(u)) return { action: 'deny' };
    // Google Chat / Meet open the call as a Picture-in-Picture window (disposition "other"). It is a small floating
    // window that must be a real window, so it is allowed as one (the page controls its content and size).
    // Document Picture-in-Picture (disposition "other") is what Google Chat / Meet use to pop a call out. Electron cannot
    // create that window: the request would wait forever and the call would never start. It is refused straight away so
    // the page falls back to showing the call inside its own tab.
    if (disposition === 'other') { log('picture-in-picture refused (call stays in the tab)'); return { action: 'deny' }; }
    return {
      action: 'allow',
      outlivesOpener: true,
      createWindow: opts => {
        try {
          log('popup createWindow', 'disposition=' + disposition, 'opts=' + JSON.stringify({ w: opts && opts.width, h: opts && opts.height, wp: !!(opts && opts.webContents), keys: Object.keys(opts || {}).slice(0, 12) }));
          const nl = 'popup-' + (++popupN);
          const wc = openSite(nl, u, ua, opts);
          if (!wc) { log('popup createWindow: no contents for', nl); return null; }
          emit('site-new', { label: nl, url: u, title: '' });
          showSite(nl);
          return wc;
        } catch (e) { log('popup createWindow FAILED', e && e.stack || e); throw e; }
      },
    };
  });
  // the page closed itself (window.close(), e.g. a finished sign-in or call pop-up)
  const gone = () => { if (sites.get(label) === s && !quitting) { emit('site-closed', { label }); closeSite(label); } };
  c.on('close', gone);
  c.on('did-create-window', (w, d) => log('window created', d && d.url, d && d.disposition));
  c.on('destroyed', gone);
  c.on('page-favicon-updated', async (_e, favs) => {
    const u = (favs || []).filter(f => /^https?:/i.test(f)).pop();
    if (!u) return;
    try { const { b, type } = await grab(u, 150000); emit('site-favicon', { url: c.getURL(), data: iconData(b, type, u) }); } catch { /* keep the generic icon */ }
  });
  c.on('page-title-updated', (_e, t) => { s.title = t; pushState(label); });
  for (const ev of ['did-start-loading', 'did-stop-loading', 'did-navigate', 'did-navigate-in-page']) c.on(ev, () => pushState(label));
  c.on('did-fail-load', (_e, code, desc, u, main) => { if (main && code !== -3) log('site load failed', label, code, desc, u); });
  c.on('console-message', (_e, level, msg, line, src) => {
    // errors of any kind from call-related sites, plus anything that mentions media or calls (rate limited per tab)
    const host = (() => { try { return new URL(c.getURL()).hostname; } catch { return ''; } })();
    s.logN = (s.logN || 0);
    const callish = /(^|\.)(meet|chat|hangouts|voice|calendar|mail)\.google\.com$|(^|\.)whatsapp\.com$/.test(host);
    if (s.logN < 200 && ((level >= 2 && (callish || /webrtc|peerconnection|getusermedia|ice |dtls|rtp|codec|mediastream|notallowed|notfound|overconstrained|datachannel/i.test(msg))))) { s.logN++; log('site console', label, host, msg.slice(0, 300), '@', String(src || '').slice(-60) + ':' + line); }
  });
  c.on('did-navigate', (_e, u, code) => log('navigate', label, u.slice(0, 200), code));
  c.on('did-frame-navigate', (_e, u, code, _t, isMain) => { if (isMain) log('navigate-frame', label, u.slice(0, 200)); });
  c.on('unresponsive', () => log('unresponsive', label));
  c.on('certificate-error', (_e, u, err) => log('certificate error', label, u.slice(0, 120), err));
  c.on('media-started-playing', () => log('media started', label));
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
  if (!creation) Promise.race([probeReady, new Promise(r => setTimeout(r, 800))]).then(() => c.loadURL(url)).catch(e => { if (!/ERR_ABORTED|-3\)/.test(String(e))) log('loadURL', label, e.message); });
  return c;
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
  print_page: () => printUi(),
  fetch_favicon: a => fetchFavicon(String(a.url || '')),
  fetch_image: async a => {
    const u = String(a.url || '');
    if (!/^https?:\/\//i.test(u)) throw new Error('Not a web address');
    const { b, type } = await grab(u, 25 * 1024 * 1024, 20000);
    return { dataBase64: b.toString('base64'), type };
  },
};
ipcMain.handle('wd-invoke', async (_e, cmd, args) => {
  const h = handlers[cmd];
  if (!h) return { err: 'Unknown shell command: ' + cmd };
  try { return { ok: await h(args || {}) }; } catch (e) { return { err: String(e && e.message || e) }; }
});

// ------------------------------------------------------------------ window
function stateFile() { return path.join(app.getPath('userData'), 'window.json'); }
function loadBounds() { try { return JSON.parse(fs.readFileSync(stateFile(), 'utf8')); } catch { return {}; } }

app.on('child-process-gone', (_e, d) => log('child process gone', d.type, d.reason, 'code', d.exitCode, d.name || ''));
async function start() {
  let info;
  try { info = await startBackend(); } catch (e) { log('start failed', e.message); dialog.showErrorBoxSync('WorkDesk Chrome', e.message); return app.quit(); }
  base = `http://127.0.0.1:${info.port}`;

  const ses = session.fromPartition(SITES);
  ses.setUserAgent(cleanUA(ses.getUserAgent()));
  ses.setPermissionRequestHandler((_wc, perm, cb, det) => { log('permission request', perm, (det && det.requestingUrl || '').slice(0, 80), (det && det.mediaTypes || '')); cb(true); });
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
