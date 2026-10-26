// WorkDesk 0.5 front end.
import { invoke, listen, getCurrentWindow } from './bridge.js';
import { icon, hydrateIcons, setIcon, bookmarkIcon, BOOKMARK_ICONS } from './icons.js';
import { createRichEditor, FONTS, SIZES } from './editor.js';
import { renderDoc } from './viewer.js';
import { createNotepad } from './notes.js';
import { sanitizeEmailHtml, withInlineImages, sanitizeForEditor } from './sanitize.js';
import { $, esc, fmtSize, fmtDate, initial, throttle, toast, openModal, closeModal, askForm, confirmBox, onModal } from './util.js';

const isDesktop = !!window.__WORKDESK__;
const isElectron = !!window.wdElectron;
if (isElectron) document.body.classList.add('electron');

const COLUMNS = {
  select: { label: 'Select', width: 36, fixed: true },
  star: { label: 'Star', width: 36, fixed: true },
  from: { label: 'Sender', width: 190 },
  to: { label: 'Recipient', width: 170 },
  subject: { label: 'Subject', width: 0 },
  date: { label: 'Date', width: 112 },
  attachment: { label: 'Attachments', width: 44, icon: 'attach' },
  folder: { label: 'Folder', width: 120 },
  flags: { label: 'Status', width: 90 },
  size: { label: 'Size', width: 78 },
};
const DEFAULT_COLUMNS = ['select', 'star', 'from', 'subject', 'date', 'attachment'];
const ROLE_ICON = { inbox: 'inbox', starred: 'star', snoozed: 'snooze', important: 'important', sent: 'send', drafts: 'drafts', all: 'allMail', spam: 'spam', trash: 'trash', archive: 'archive' };
const WIDTH_KEY = 'maildesk-column-widths-v2';

const state = {
  account: '',
  folders: [],
  folder: 'INBOX',
  query: '',
  grouped: true,
  page: 1,
  pageSize: 25,
  sort: 'date',
  desc: true,
  items: [],
  total: 0,
  truncated: false,
  synced: true,
  selected: new Set(),
  hidden: new Set(),
  prefs: {},
  bookmarks: [],
  aliases: [],
  signatures: {},
  columns: [...DEFAULT_COLUMNS],
  view: 'mail',
  webTabs: [],
  activeWeb: null,
  thread: null,
  threads: [],
  docs: [],
  notesOpen: false,
  composes: [],
  cur: null,
  contacts: [],
  contactsAt: 0,
  doc: null,
  attachments: [],
  compose: { inReplyTo: '', references: '' },
  syncing: false,
};

const store = {
  get: k => { try { return localStorage.getItem(k); } catch { return null; } },
  set: (k, v) => { try { localStorage.setItem(k, v); } catch { /* storage unavailable */ } },
};
const call = (cmd, args = {}) => invoke(cmd, args);
const roleOf = name => state.folders.find(f => f.name === name)?.role || (name === 'INBOX' ? 'inbox' : null);
const folderDisplay = name => state.folders.find(f => f.name === name)?.display || (name === 'INBOX' ? 'Inbox' : String(name || '').replace(/^\[(Gmail|Google Mail)\]\//, ''));
const identities = () => [state.account, ...state.aliases].filter((v, i, a) => v && a.indexOf(v) === i);

// ======================================================================== views
function setView(view) {
  if (state.view === 'notes' && view !== 'notes') notepad.leave();
  if (view === 'notes') { state.notesOpen = true; notepad.open(); }
  if (state.view === 'compose' && view !== 'compose') snapCompose();
  if (view === 'web' && state.view !== 'web') state.prevView = state.view;
  state.view = view;
  document.querySelectorAll('.view').forEach(v => v.classList.toggle('active', v.id === view + 'View'));
  document.querySelectorAll('.spaceitem').forEach(b => b.classList.remove('active'));
  if (view === 'mail') $('mailSpace').classList.add('active');
  if (view === 'settings') $('settingsBtn').classList.add('active');
  if (view === 'notes') $('notesSpace').classList.add('active');
  if (view !== 'web') suspendSites();
  showFrames();
  renderTabs();
  renderPins();
}

// ===================================================================== prefs
async function loadPrefs() {
  if (isDesktop) {
    try { state.prefs = (await call('load_preferences')) || {}; } catch (e) { console.error(e); state.prefs = {}; }
  }
  const p = state.prefs;
  // v0.5 makes conversation view the default everywhere: older builds saved `false` as the default.
  if (!p.conversation_default_v5) {
    p.conversations_enabled = true;
    p.conversation_default_v5 = true;
  }
  state.grouped = p.conversations_enabled !== false;
  state.account = p.identity || '';
  state.pageSize = Number(p.page_size || 25);
  const cols = (p.columns || DEFAULT_COLUMNS).filter(k => COLUMNS[k]);
  state.columns = [...new Set(['select', ...cols.filter(k => k !== 'select')])];
  state.bookmarks = (p.bookmarks || []).filter(b => b && b.url).filter(b => !(/^(Google Drive|Google Calendar|Google Keep)$/i.test(String(b.title || '')) && /(drive|calendar|keep)\.google\.com/i.test(String(b.url || ''))));
  state.aliases = p.aliases || [];
  state.signatures = p.signatures || {};
  if (p.signature && state.account && !(state.account in state.signatures)) {
    state.signatures[state.account] = `<p>${esc(p.signature).replace(/\r?\n/g, '<br>')}</p>`;
  }
  $('themeSelect').value = p.theme || 'light';
  $('pageSize').value = String(state.pageSize);
  $('displayName').value = p.display_name || '';
  $('aliasesText').value = state.aliases.join('\n');
  $('undoSeconds').value = String(p.undo_seconds ?? 5);
  $('conversationSetting').checked = state.grouped;
  $('remoteImages').checked = p.remote_images !== false;
  $('checkMinutes').value = String(p.check_minutes ?? 5);
  themeColor = p.theme_color || '';
  themeTint = !!p.theme_tint;
  $('themeTint').checked = themeTint;
  const cf = p.compose_font || {};
  $('composeFont').value = cf.family || '';
  $('composeSize').value = String(cf.size || 14);
  $('composeLine').value = cf.line || '';
  $('composeColor').value = cf.color || '#000000';
  $('composeColor').dataset.auto = cf.color ? '' : '1';
  scheduleMailCheck();
  applyTheme();
  syncConvButton();
  renderPins();
}

// ---- compose defaults and periodic mail check
for (const [v, label] of FONTS) $('composeFont').insertAdjacentHTML('beforeend', `<option value="${esc(v)}">${esc(label)}</option>`);
$('composeSize').innerHTML = SIZES.map(n => `<option value="${n}">${n} px</option>`).join('');
$('composeColor').oninput = () => { $('composeColor').dataset.auto = ''; };
$('composeColorReset').onclick = () => { $('composeColor').dataset.auto = '1'; $('composeColor').value = '#000000'; };

function composeCss() {
  const cf = state.prefs.compose_font || {};
  const parts = [`font-family:${cf.family || FONTS[0][0]}`, `font-size:${cf.size || 14}px`];
  if (cf.color) parts.push(`color:${cf.color}`);
  if (cf.line) parts.push(`line-height:${cf.line}`);
  return parts.join(';');
}
function applyComposeDefaults() {
  const cf = state.prefs.compose_font || {};
  const st = $('editor').style;
  st.fontFamily = cf.family || FONTS[0][0];
  st.fontSize = (cf.size || 14) + 'px';
  st.color = cf.color || '';
  st.lineHeight = cf.line || '';
}

let mailTimer = null;
function scheduleMailCheck() {
  clearInterval(mailTimer);
  mailTimer = null;
  const m = Number(state.prefs.check_minutes ?? 5);
  if (!(m > 0) || !isDesktop) return;
  mailTimer = setInterval(() => {
    if (!state.account || state.syncing || document.querySelector('.modalback.show')) return;
    call('start_sync', { username: state.account, priority: state.folder }).catch(() => {});
  }, m * 60000);
}

async function savePrefs() {
  const p = state.prefs;
  Object.assign(p, {
    identity: state.account || null,
    theme: $('themeSelect').value,
    page_size: state.pageSize,
    columns: state.columns,
    conversations_enabled: state.grouped,
    conversation_default_v5: true,
    bookmarks: state.bookmarks,
    aliases: state.aliases,
    signatures: state.signatures,
    display_name: $('displayName').value,
    undo_seconds: Number($('undoSeconds').value),
    remote_images: $('remoteImages').checked,
    theme_color: themeColor,
    theme_tint: themeTint,
    check_minutes: Math.min(1440, Math.max(0, Math.round(Number($('checkMinutes').value) || 0))),
    compose_font: {
      family: $('composeFont').value || FONTS[0][0],
      size: Number($('composeSize').value) || 14,
      line: $('composeLine').value || '',
      color: $('composeColor').dataset.auto ? '' : $('composeColor').value,
    },
  });
  if (isDesktop) await call('save_preferences', { preferences: p });
}

function applyTheme() {
  document.body.classList.toggle('dark', $('themeSelect').value === 'dark');
  applyThemeColor();
}

// ---- theme colour: any colour from the palette tints accents (and optionally the header and rail)
let themeColor = '';
let themeTint = false;
const PALETTE = ['#3b64d8', '#2563eb', '#0ea5e9', '#06b6d4', '#14b8a6', '#10b981', '#16a34a', '#65a30d', '#ca8a04', '#f59e0b', '#ea580c', '#dc2626',
  '#e11d48', '#db2777', '#c026d3', '#9333ea', '#7c3aed', '#4f46e5', '#334155', '#475569', '#78716c', '#57534e', '#0f766e', '#1e3a8a'];
const hexToRgb = h => { const m = /^#?([0-9a-f]{6})$/i.exec(h); if (!m) return null; const n = parseInt(m[1], 16); return [n >> 16, (n >> 8) & 255, n & 255]; };
const rgbToHex = r => '#' + r.map(v => Math.round(Math.max(0, Math.min(255, v))).toString(16).padStart(2, '0')).join('');
const mix = (a, b, t) => a.map((v, i) => v * (1 - t) + b[i] * t);
const luminance = ([r, g, b]) => { const f = v => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }; return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b); };
function applyThemeColor() {
  const st = document.body.style;
  const names = ['--accent', '--accentbg', '--selected', '--hover', '--chrome-bg', '--chrome-fg'];
  const rgb = hexToRgb(themeColor);
  document.body.classList.toggle('tinted', !!rgb && themeTint);
  if (!rgb) { names.forEach(n => st.removeProperty(n)); return; }
  const dark = document.body.classList.contains('dark');
  const panel = dark ? [26, 32, 43] : [255, 255, 255];
  const accent = dark ? mix(rgb, [255, 255, 255], 0.3) : rgb;
  st.setProperty('--accent', rgbToHex(accent));
  st.setProperty('--accentbg', rgbToHex(mix(accent, panel, dark ? 0.78 : 0.9)));
  st.setProperty('--selected', rgbToHex(mix(accent, panel, dark ? 0.7 : 0.86)));
  st.setProperty('--hover', rgbToHex(mix(accent, panel, dark ? 0.86 : 0.95)));
  const chrome = dark ? mix(rgb, [0, 0, 0], 0.45) : rgb;
  st.setProperty('--chrome-bg', rgbToHex(chrome));
  st.setProperty('--chrome-fg', luminance(chrome) > 0.45 ? '#111827' : '#ffffff');
}
function renderPalette() {
  const cur = (themeColor || '').toLowerCase();
  $('palette').innerHTML = PALETTE.map(c => `<button type="button" style="background:${c}" data-c="${c}" aria-label="Colour ${c}" aria-pressed="${c === cur}" title="${c}"></button>`).join('');
  if (hexToRgb(themeColor)) $('themeColorCustom').value = themeColor;
}
async function setThemeColor(c, tint = themeTint) {
  themeColor = c; themeTint = tint;
  $('themeTint').checked = tint;
  applyThemeColor(); renderPalette();
  try { await savePrefs(); } catch (e) { console.error(e); }
}
$('palette').addEventListener('click', e => { const b = e.target.closest('[data-c]'); if (b) setThemeColor(b.dataset.c); });
$('themeColorCustom').addEventListener('change', e => setThemeColor(e.target.value));
$('themeTint').onchange = e => setThemeColor(themeColor, e.target.checked);
$('themeColorReset').onclick = () => setThemeColor('', false);
$('checkCalls').onclick = async () => {
  let lines = [];
  try { lines = await call('webrtc_support'); } catch (e) { lines = ['FAIL|' + String(e?.message || e)]; }
  // Ask a live website tab (WhatsApp first) what its engine really supports.
  const tab = state.webTabs.find(t => /whatsapp/i.test(t.url || '')) || state.webTabs[0];
  if (tab) {
    try {
      const p = JSON.parse(await call('site_probe', { label: labelOf(tab.id) }));
      if (p.error) lines.push('FAIL|Page probe error: ' + p.error);
      else {
        lines.push('INFO|Tab “' + (tab.title || tab.url) + '” reports: ' + p.ua);
        lines.push((p.rtcPeerConnection === 'function' ? 'OK' : 'FAIL') + '|RTCPeerConnection in the page: ' + p.rtcPeerConnection);
        lines.push((p.mediaDevices ? 'OK' : 'FAIL') + '|getUserMedia (camera / microphone) in the page: ' + p.mediaDevices);
        lines.push((p.secure ? 'OK' : 'FAIL') + '|Secure context: ' + p.secure);
        lines.push((p.getCapabilities ? 'OK' : 'FAIL') + '|RTCRtpSender.getCapabilities: ' + p.getCapabilities);
      }
    } catch (e) { lines.push('INFO|Could not probe the open website tab: ' + String(e?.message || e)); }
  } else lines.push('INFO|Open WhatsApp Web in a tab first, then run this check again to test the page itself.');
  const mark = { OK: 'ok', FAIL: 'bad', INFO: 'info' };
  const anyBad = lines.some(l => l.startsWith('FAIL'));
  openModal('Call support on this computer',
    lines.map(l => { const [k, ...t] = l.split('|'); return `<div class="itemrow"><b style="min-width:44px;color:${k === 'OK' ? 'var(--green)' : k === 'FAIL' ? 'var(--danger)' : 'var(--muted)'}">${esc(k === 'INFO' ? '·' : k)}</b><div class="meta"><span style="white-space:normal">${esc(t.join('|'))}</span></div></div>`; }).join('') +
    `<p class="sub" style="margin:10px 0 0">${anyBad ? 'Fix the items marked FAIL (or use the Chrome button in the website bar for calls), then restart WorkDesk.' : 'Everything needed for calls was found. If WhatsApp still refuses, use the Chrome button in the website bar.'}</p>`,
    'Close', async () => {}, { hideSubmit: true });
};

// ======================================================================= tabs
function renderTabs() {
  const host = $('tabs');
  host.innerHTML = '';
  const mail = document.createElement('button');
  mail.className = 'tab' + (!['web', 'thread', 'doc', 'compose', 'notes'].includes(state.view) ? ' active' : '');
  mail.innerHTML = `${icon('mail', 'ic-sm')}<span class="tabtitle">Mail</span>`;
  mail.onclick = () => setView('mail');
  host.append(mail);
  for (const t of state.threads) {
    const el = document.createElement('div');
    el.className = 'tab' + (state.view === 'thread' && state.thread === t ? ' active' : '');
    el.tabIndex = 0;
    el.title = t.title;
    el.innerHTML = `${icon('mail', 'ic-sm')}<span class="tabtitle">${esc(t.title)}</span><button class="tabclose" type="button" title="Close tab" aria-label="Close ${esc(t.title)}">${icon('close')}</button>`;
    el.onclick = e => {
      if (e.target.closest('.tabclose')) closeThread(t);
      else activateThread(t);
    };
    host.append(el);
  }
  if (state.notesOpen) {
    const el = document.createElement('div');
    el.className = 'tab' + (state.view === 'notes' ? ' active' : '');
    el.tabIndex = 0;
    el.title = 'Notepad';
    el.innerHTML = `${icon('note', 'ic-sm')}<span class="tabtitle">Notepad</span><button class="tabclose" type="button" title="Close tab" aria-label="Close Notepad">${icon('close')}</button>`;
    el.onclick = e => {
      if (e.target.closest('.tabclose')) { notepad.leave(); state.notesOpen = false; if (state.view === 'notes') setView('mail'); else renderTabs(); }
      else setView('notes');
    };
    host.append(el);
  }
  for (const c of state.composes) {
    const el = document.createElement('div');
    const live = state.view === 'compose' && state.cur === c;
    const title = (live ? $('subjectField').value : c.subject).trim() || c.title;
    el.className = 'tab' + (live ? ' active' : '');
    el.tabIndex = 0;
    el.title = title;
    el.innerHTML = `${icon('compose', 'ic-sm')}<span class="tabtitle">${esc(title)}</span><button class="tabclose" type="button" title="Close tab" aria-label="Close ${esc(title)}">${icon('close')}</button>`;
    el.onclick = e => { if (e.target.closest('.tabclose')) closeComposeTab(c); else activateCompose(c); };
    host.append(el);
  }
  for (const d of state.docs) {
    const el = document.createElement('div');
    el.className = 'tab' + (state.view === 'doc' && state.doc === d ? ' active' : '');
    el.tabIndex = 0;
    el.title = d.title;
    el.innerHTML = `${icon(d.kind === 'pdf' ? 'note' : 'image', 'ic-sm')}<span class="tabtitle">${esc(d.title)}</span><button class="tabclose" type="button" title="Close tab" aria-label="Close ${esc(d.title)}">${icon('close')}</button>`;
    el.onclick = e => { if (e.target.closest('.tabclose')) closeDoc(d); else activateDoc(d); };
    host.append(el);
  }
  for (const t of state.webTabs) {
    const el = document.createElement('div');
    el.className = 'tab' + (state.view === 'web' && state.activeWeb === t.id ? ' active' : '');
    el.tabIndex = 0;
    el.title = t.url;
    el.innerHTML = `${icon(t.icon || 'globe', 'ic-sm')}<span class="tabtitle">${esc(t.title)}</span><button class="tabclose" type="button" title="Close tab" aria-label="Close ${esc(t.title)}">${icon('close')}</button>`;
    el.onclick = e => {
      if (e.target.closest('.tabclose')) closeWeb(t.id);
      else activateWeb(t.id);
    };
    host.append(el);
  }
}

// ================================================================== bookmarks
function renderPins() {
  const rail = $('pinned');
  rail.innerHTML = '';
  state.bookmarks.forEach((b, i) => {
    if (!b.pinned) return;
    const el = document.createElement('button');
    el.className = 'spaceitem pinned' + (state.view === 'web' && state.webTabs.find(t => t.id === state.activeWeb)?.url === b.url ? ' active' : '');
    el.dataset.tip = b.title + ' — right-click to edit';
    el.setAttribute('aria-label', b.title);
    el.innerHTML = icon(bookmarkIcon(b.icon));
    el.onclick = () => openWeb(b);
    el.oncontextmenu = e => { e.preventDefault(); bookmarkModal(i); };
    rail.append(el);
  });
  renderSettingsLists();
  const list = $('bookmarkList');
  list.innerHTML = '';
  state.bookmarks.forEach((b, i) => {
    const el = document.createElement('div');
    el.className = 'nav';
    el.title = b.url;
    el.innerHTML = `${icon(bookmarkIcon(b.icon), 'ic-sm')}<span class="name">${esc(b.title)}</span>`;
    el.onclick = () => openWeb(b);
    el.oncontextmenu = e => { e.preventDefault(); bookmarkModal(i); };
    list.append(el);
  });
  if (!state.bookmarks.length) list.innerHTML = '<div class="sub">No bookmarks yet. Pinned sites also appear in the left rail.</div>';
}

async function bookmarkModal(edit = -1) {
  const b = state.bookmarks[edit] || { title: '', url: '', pinned: true, icon: 'globe' };
  const cur = bookmarkIcon(b.icon);
  const icons = BOOKMARK_ICONS.map(n => `<label title="${n}"><input type="radio" name="icon" value="${n}" ${n === cur ? 'checked' : ''}>${icon(n)}</label>`).join('');
  await openModal(
    edit < 0 ? 'Add bookmark' : 'Edit bookmark',
    `<label>Title<input name="title" required value="${esc(b.title)}"></label>
     <label>URL<input name="url" type="url" required placeholder="https://example.com" value="${esc(b.url)}"></label>
     <div>Icon</div><div class="iconlist">${icons}</div>
     <label class="inline"><input name="pinned" type="checkbox" value="yes" ${b.pinned ? 'checked' : ''}> Pin to the left rail</label>`,
    'Save',
    async d => {
      if (!/^https?:\/\//i.test(d.url)) throw new Error('The address must start with http:// or https://');
      const item = { title: d.title.trim(), url: d.url.trim(), icon: d.icon || 'globe', pinned: d.pinned === 'yes' };
      if (edit < 0) state.bookmarks.push(item); else state.bookmarks[edit] = item;
      await savePrefs();
      renderPins();
    }
  );
}

function renderSettingsLists() {
  const row = (ic, title, meta, pinned, attr, i, extra = '') => `<div class="itemrow">${icon(ic)}<div class="meta"><b>${esc(title)}</b><span>${esc(meta)}</span></div>${pinned ? '<span class="pinned-badge">Pinned</span>' : ''}${extra}<button type="button" class="iconbtn" data-${attr}-edit="${i}" title="Modify" aria-label="Modify ${esc(title)}">${icon('compose')}</button><button type="button" class="iconbtn" data-${attr}-del="${i}" title="Remove" aria-label="Remove ${esc(title)}">${icon('trash')}</button></div>`;
  $('bookmarkSettings').innerHTML = state.bookmarks.map((b, i) => row(bookmarkIcon(b.icon), b.title, b.url, b.pinned, 'bm', i)).join('') || '<div class="sub" style="padding:10px 0">No bookmarks yet.</div>';
}

$('addBookmarkSettings').onclick = () => bookmarkModal();
$('bookmarkSettings').addEventListener('click', async e => {
  const ed = e.target.closest('[data-bm-edit]');
  const del = e.target.closest('[data-bm-del]');
  if (ed) return bookmarkModal(+ed.dataset.bmEdit);
  if (del) {
    const i = +del.dataset.bmDel;
    if (await confirmBox(`Remove the bookmark “${state.bookmarks[i].title}”?`, 'Remove', true)) { state.bookmarks.splice(i, 1); await savePrefs(); renderPins(); }
  }
});

function manageBookmarks() {
  const rows = state.bookmarks.map((b, i) => `<div class="settingsrow"><label>${esc(b.title)}<span class="hint">${esc(b.url)}</span></label><button type="button" class="iconbtn" data-edit="${i}" title="Edit" aria-label="Edit">${icon('compose')}</button><button type="button" class="iconbtn" data-remove="${i}" title="Remove" aria-label="Remove">${icon('trash')}</button></div>`).join('');
  openModal('Manage bookmarks', rows || '<p>No bookmarks yet.</p>', 'Done', async () => {}, { hideSubmit: true });
}

// =================================================================== website tabs
// Each website tab is a real tab of the kiosk Chrome window (see backend/src/chrome.rs); this page just drives it.
const labelOf = id => id.replace(/[^a-zA-Z0-9_-]/g, '-').slice(0, 60);
const nextFrame = () => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
const viewportRect = () => { const r = $('webViewport').getBoundingClientRect(); return { x: r.left, y: r.top, w: r.width, h: r.height }; };

function openWeb(b) {
  const url = String(b.url || '').trim();
  if (!/^https?:\/\//i.test(url)) { toast('Only http and https addresses can open in tabs.', { error: true }); return; }
  let tab = state.webTabs.find(t => t.url0 === url);
  if (!tab) {
    let host = url;
    try { host = new URL(url).hostname; } catch { /* keep url */ }
    tab = { id: 'site-' + Date.now().toString(36) + Math.random().toString(36).slice(2, 5), url, url0: url, title: b.title || host, fixedTitle: !!b.title, icon: bookmarkIcon(b.icon || 'globe'), opened: false, embed: false, canBack: false, canForward: false, loading: false };
    state.webTabs.push(tab);
  }
  activateWeb(tab.id);
}

const FRAME_ALLOW = 'camera *; microphone *; display-capture *; clipboard-read *; clipboard-write *; autoplay *; fullscreen *; geolocation *; encrypted-media *; picture-in-picture *; screen-wake-lock *; web-share *; publickey-credentials-get *';
function showFrames() {
  const tab = state.webTabs.find(t => t.id === state.activeWeb);
  const on = state.view === 'web' && tab?.embed;
  document.querySelectorAll('#webViewport iframe').forEach(f => f.classList.toggle('on', on && f.dataset.label === labelOf(tab.id)));
  $('webMessage').style.display = on && tab.opened ? 'none' : '';
}
function frameFor(tab) {
  const label = labelOf(tab.id);
  let f = [...document.querySelectorAll('#webViewport iframe')].find(x => x.dataset.label === label);
  if (!f) {
    f = document.createElement('iframe');
    f.dataset.label = label;
    f.name = 'wd-' + label;
    f.allow = FRAME_ALLOW;
    f.allowFullscreen = true;
    f.referrerPolicy = 'strict-origin-when-cross-origin';
    f.addEventListener('load', () => { tab.loading = false; if (state.activeWeb === tab.id) syncWebBar(tab); });
    $('webViewport').append(f);
  }
  return f;
}

async function activateWeb(id) {
  const tab = state.webTabs.find(t => t.id === id);
  if (!tab) return;
  state.activeWeb = id;
  setView('web');
  $('webUrl').value = tab.url;
  $('webMessage').textContent = 'Opening website…';
  syncWebBar(tab);
  if (!isDesktop) { $('webMessage').innerHTML = 'Website tabs need the desktop app.'; return; }
  try {
    if (tab.embed) {
      // The website lives inside this very window, in the area under the tab strip.
      if (!tab.opened) {
        await call('site_open', { label: labelOf(tab.id), url: tab.url, embed: true, ua: null });
        tab.opened = true;
        tab.loading = true;
        frameFor(tab).src = tab.url;
      }
      showFrames();
      syncWebBar(tab);
      return;
    }
    // A real Chromium page placed over the area under the tab strip
    if (!tab.opened) {
      await call('site_open', { label: labelOf(tab.id), url: tab.url, embed: false, ua: null });
      tab.opened = true;
    }
    await nextFrame();
    await call('site_show', { label: labelOf(tab.id), rect: viewportRect() });
    $('webMessage').textContent = '';
  } catch (e) {
    $('webMessage').innerHTML = `Could not open this website: ${esc(e?.message || e)}<br><br><button class="primary" id="webFallback">Open in the system browser</button>`;
    $('webFallback').onclick = () => openExternal(tab.url);
  }
}

async function closeWeb(id) {
  const tab = state.webTabs.find(t => t.id === id);
  if (!tab) return;
  if (isDesktop && tab.opened) call('site_close', { label: labelOf(tab.id) }).catch(() => {});
  document.querySelectorAll('#webViewport iframe').forEach(f => { if (f.dataset.label === labelOf(tab.id)) f.remove(); });
  const i = state.webTabs.indexOf(tab);
  state.webTabs.splice(i, 1);
  if (state.activeWeb === id) {
    const next = state.webTabs[i] || state.webTabs[i - 1];
    state.activeWeb = next?.id || null;
    if (next) activateWeb(next.id); else setView('mail');
  } else renderTabs();
}

function suspendSites() {
  if (isDesktop && state.webTabs.some(t => t.opened && !t.embed)) call('site_show', { label: null }).catch(() => {});
}
function resumeSites() {
  const tab = state.webTabs.find(t => t.id === state.activeWeb);
  if (state.view === 'web' && tab?.opened && !tab.embed) nextFrame().then(() => call('site_show', { label: labelOf(tab.id), rect: viewportRect() })).catch(() => {});
}
onModal({ open: suspendSites, close: resumeSites });

function syncWebBar(tab) {
  $('webBack').disabled = !tab?.canBack;
  $('webForward').disabled = !tab?.canForward;
  $('webProgress').classList.toggle('on', !!tab?.loading);
}

function openExternal(url) {
  call('open_external', { url, browser: null }).catch(e => toast(String(e?.message || e), { error: true }));
}

async function webAction(action, url = null) {
  const tab = state.webTabs.find(t => t.id === state.activeWeb);
  if (!tab?.opened) return;
  try { await call('site_action', { label: labelOf(tab.id), action, url }); } catch (e) { toast(String(e?.message || e), { error: true }); }
}

// events from the website tabs' hover bar (Chrome side)
function onGoto(p) {
  if (p?.web) {
    const t = state.webTabs.find(x => labelOf(x.id) === p.web);
    if (t) activateWeb(t.id);
  } else if (state.view === 'web') setView(state.prevView && state.prevView !== 'web' ? state.prevView : 'mail');
}
function onSiteNew(p) {
  if (!p?.label || state.webTabs.some(t => labelOf(t.id) === p.label)) return;
  let host = p.url || '';
  try { host = new URL(p.url).hostname || host; } catch { /* keep */ }
  const tab = { id: p.label, url: p.url || '', url0: '', title: p.title || host || 'Website', fixedTitle: false, icon: bookmarkIcon('globe'), opened: true, embed: false, canBack: false, canForward: false, loading: true };
  state.webTabs.push(tab);
  state.activeWeb = tab.id;
  setView('web');
  $('webUrl').value = tab.url;
  $('webMessage').textContent = '';
  syncWebBar(tab);
}

function onSiteState(p) {
  const tab = state.webTabs.find(t => labelOf(t.id) === p.label);
  if (!tab) return;
  if (tab.embed && /^chrome-error:/.test(p.url || '')) {
    if (!tab.errShown) { tab.errShown = true; toast('This website could not be shown inside WorkDesk. Use the Chrome button in the address bar to open it in a separate Chrome tab, or choose “Separate Chrome tab” under Settings → Website tabs.', { error: true, timeout: 12000 }); }
    return;
  }
  tab.errShown = false;
  Object.assign(tab, { url: p.url || tab.url, canBack: p.canBack, canForward: p.canForward, loading: p.loading });
  if (!tab.fixedTitle && p.title) tab.title = p.title;
  if (state.activeWeb === tab.id) {
    if (document.activeElement !== $('webUrl')) $('webUrl').value = tab.url;
    syncWebBar(tab);
  }
  renderTabs();
}

// ================================================================ folders & sync
function renderFolders() {
  const host = $('folders');
  const rail = $('railFolders');
  host.innerHTML = '';
  rail.innerHTML = '';
  const list = state.folders.filter(f => f.selectable);
  if (!list.length) host.innerHTML = '<div class="sub" style="padding:8px 10px">Connect Gmail to see your mailboxes.</div>';
  let labelsHeader = false;
  for (const f of list) {
    if (!f.role && !labelsHeader) {
      labelsHeader = true;
      const h = document.createElement('div');
      h.className = 'sect';
      h.textContent = 'Labels';
      host.append(h);
    }
    const showCount = f.unread > 0 && !['all', 'trash', 'sent'].includes(f.role);
    const el = document.createElement('div');
    el.className = 'nav' + (state.folder === f.name && !state.query ? ' active' : '');
    el.dataset.folder = f.name;
    el.title = f.display;
    el.innerHTML = `${icon(ROLE_ICON[f.role] || 'label', 'ic-sm')}<span class="name">${esc(f.display)}</span>${showCount ? `<span class="count">${f.unread}</span>` : ''}`;
    host.append(el);
    const b = document.createElement('button');
    b.className = 'spaceitem' + (state.folder === f.name && !state.query ? ' active' : '');
    b.dataset.tip = f.display;
    b.dataset.folder = f.name;
    b.setAttribute('aria-label', f.display);
    b.innerHTML = icon(ROLE_ICON[f.role] || 'label');
    rail.append(b);
  }
  $('accountBtn').querySelector('.name').textContent = state.account ? state.account : 'Connect Gmail';
  updateActionStates();
}

async function reloadFolders(refresh = false) {
  if (!state.account) return;
  try {
    const r = await call('list_folders', { username: state.account, refresh });
    state.folders = r.folders || [];
    renderFolders();
    if (r.offline) setSync('off', 'Offline — showing cached mail');
  } catch (e) {
    setSync('err', 'Could not list folders');
    console.error(e);
  }
}

function setSync(kind, text) {
  $('syncPill').querySelector('.dot').className = 'dot' + (kind ? ' ' + kind : '');
  $('syncText').textContent = text;
  $('syncPill').title = text;
}

async function selectFolder(name) {
  state.folder = name;
  state.query = '';
  $('search').value = '';
  $('searchClear').hidden = true;
  state.page = 1;
  state.selected.clear();
  state.hidden.clear();
  setView('mail');
  renderFolders();
  await loadList();
  const f = state.folders.find(x => x.name === name);
  if (f && !f.synced && !state.syncing) syncCurrent(false);
}

async function syncCurrent(manual) {
  if (!state.account) return;
  try {
    if (manual) setSync('busy', 'Syncing…');
    await call('sync_folder_now', { username: state.account, folder: state.folder });
    await reloadFolders();
    if (!state.query) await loadList(); else if (manual) { await loadList(true); }
    if (manual) setSync('', 'Up to date');
  } catch (e) {
    setSync('err', 'Sync failed');
    if (manual) toast('Sync failed: ' + String(e?.message || e), { error: true });
  }
  if (manual && !state.syncing) call('start_sync', { username: state.account, priority: state.folder }).catch(() => {});
}

const refreshListSoon = throttle(() => { if (state.view === 'mail' && !state.query) loadList(); reloadFolders(); }, 1500);
let errorShown = false;

function onSync(ev) {
  switch (ev.phase) {
    case 'folders':
      state.syncing = true;
      reloadFolders();
      break;
    case 'headers':
      state.syncing = true;
      setSync('busy', `Downloading headers · ${folderDisplay(ev.folder)} ${ev.done.toLocaleString()} / ${ev.total.toLocaleString()}`);
      if (ev.folder === state.folder) refreshListSoon();
      break;
    case 'folder-done':
      if (ev.folder === state.folder) { state.synced = true; if (!state.query) loadList(); }
      reloadFolders();
      break;
    case 'error':
      if (ev.folder) console.warn('sync error', ev.folder, ev.message);
      if (!ev.folder) { setSync('err', 'Sync stopped'); state.syncing = false; }
      if (!errorShown) { errorShown = true; toast(`Sync problem${ev.folder ? ' in ' + folderDisplay(ev.folder) : ''}: ${ev.message || 'unknown error'}`, { error: true, timeout: 8000 }); }
      break;
    case 'finished':
      state.syncing = false;
      setSync('', 'All headers downloaded');
      reloadFolders();
      if (!state.query && state.view === 'mail') loadList();
      break;
    default:
  }
}

// ==================================================================== list view
let listSeq = 0;
async function loadList(fresh = false) {
  const seq = ++listSeq;
  if (!state.account) { state.items = []; renderList(); return; }
  const searching = !!state.query;
  $('mailView').setAttribute('aria-busy', 'true');
  if (searching) $('subTitle').textContent = 'Searching Gmail…';
  try {
    const base = { username: state.account, grouped: state.grouped, sort: state.sort, desc: state.desc, page: state.page, pageSize: state.pageSize };
    const res = searching
      ? await call('search_mail', { ...base, query: state.query, fresh })
      : await call('list_page', { ...base, folder: state.folder });
    if (seq !== listSeq) return;
    state.items = res.items || [];
    state.total = res.total || 0;
    state.truncated = !!res.truncated;
    state.synced = res.synced !== false;
    const pages = Math.max(1, Math.ceil(state.total / state.pageSize));
    if (state.page > pages && state.total) { state.page = pages; return loadList(); }
    const keys = new Set(state.items.map(i => i.key));
    for (const k of [...state.selected]) if (!keys.has(k)) state.selected.delete(k);
    renderList();
  } catch (e) {
    if (seq !== listSeq) return;
    state.items = [];
    renderList(String(e?.message || e));
  } finally {
    if (seq === listSeq) $('mailView').removeAttribute('aria-busy');
  }
}

function loadWidths() { try { return JSON.parse(store.get(WIDTH_KEY) || '{}'); } catch { return {}; } }

function renderList(errorText = '') {
  const searching = !!state.query;
  $('folderTitle').textContent = searching ? 'Search results' : folderDisplay(state.folder);
  const banner = $('banner');
  if (searching) {
    banner.hidden = false;
    banner.className = 'banner' + (state.truncated ? ' warn' : '');
    banner.innerHTML = `${icon('search', 'ic-sm')}<span>Gmail search for <b>${esc(state.query)}</b> across All Mail, Trash and Spam — ${state.total.toLocaleString()} ${state.grouped ? 'conversation' : 'message'}${state.total === 1 ? '' : 's'}${state.truncated ? '. Very large result: showing the newest matches only — refine the search to narrow it.' : ''}</span>`;
  } else if (!state.synced && state.account && !state.items.length && !errorText) {
    banner.hidden = false;
    banner.className = 'banner';
    banner.innerHTML = `${icon('sync', 'ic-sm')}<span>Downloading message headers for this folder — they appear here as they arrive.</span>`;
  } else banner.hidden = true;

  // columns
  const widths = loadWidths();
  const keys = state.columns;
  $('cols').innerHTML = keys.map(k => { const w = widths[k] || COLUMNS[k].width; return `<col data-col="${k}"${w ? ` style="width:${w}px"` : ''}>`; }).join('');
  $('tableHead').innerHTML = keys.map(k => {
    const c = COLUMNS[k];
    if (k === 'select') return '<th></th>';
    if (k === 'star') return `<th title="Starred">${icon('star', 'ic-sm')}</th>`;
    const sorted = state.sort === k;
    const label = c.icon ? icon(c.icon, 'ic-sm') : esc(c.label);
    return `<th data-col="${k}" class="${sorted ? 'sorted' : ''}"><button class="sortbtn" data-sort="${k}" title="Sort by ${esc(c.label)}">${label}${icon(sorted ? (state.desc ? 'sortDesc' : 'sortAsc') : 'sortNone')}</button><span class="grip" data-grip="${k}" title="Drag to resize"></span></th>`;
  }).join('');

  const items = state.items.filter(i => !state.hidden.has(i.key));
  if (!items.length) {
    const msg = errorText || (!state.account ? 'Connect your Gmail account to load mail.' : searching ? 'No conversations match this search.' : state.synced ? 'This folder is empty.' : 'Downloading headers…');
    $('mailRows').innerHTML = `<tr><td class="empty" colspan="${keys.length}">${icon(errorText ? 'spam' : searching ? 'search' : 'inbox')}<div>${esc(msg)}</div></td></tr>`;
  } else {
    $('mailRows').innerHTML = items.map(i => rowHtml(i, keys)).join('');
  }
  const total = state.total;
  const pages = Math.max(1, Math.ceil(total / state.pageSize));
  const start = total ? (state.page - 1) * state.pageSize + 1 : 0;
  $('pageInfo').textContent = `${start.toLocaleString()}–${Math.min(state.page * state.pageSize, total).toLocaleString()} of ${total.toLocaleString()}`;
  $('firstPage').disabled = $('prevPage').disabled = state.page <= 1;
  $('nextPage').disabled = $('lastPage').disabled = state.page >= pages;
  $('subTitle').textContent = searching ? '' : state.account ? `${state.total.toLocaleString()} ${state.grouped ? 'conversations' : 'messages'}` : '';
  syncConvButton();
  updateSelectState();
}

function rowHtml(i, keys) {
  const unread = i.unread > 0;
  const cells = keys.map(k => {
    switch (k) {
      case 'select': return `<td><input type="checkbox" class="pick rowcheck" data-key="${esc(i.key)}" ${state.selected.has(i.key) ? 'checked' : ''} aria-label="Select"></td>`;
      case 'star': return `<td><button class="starbtn ${i.flagged ? 'on' : ''}" data-star="${esc(i.key)}" title="${i.flagged ? 'Unstar' : 'Star'}" aria-label="Toggle star">${icon(i.flagged ? 'starFilled' : 'star')}</button></td>`;
      case 'from': return `<td title="${esc(i.from)}">${esc(roleOf(state.folder) === 'sent' || roleOf(state.folder) === 'drafts' ? (i.to || i.from) : i.from)}</td>`;
      case 'to': return `<td title="${esc(i.to)}">${esc(i.to)}</td>`;
      case 'subject': {
        const chip = state.query && roleOf(i.folder) && ['trash', 'spam'].includes(roleOf(i.folder)) ? `<span class="chip">${esc(folderDisplay(i.folder))}</span>` : '';
        const cnt = state.grouped && i.count > 1 ? `<span class="cnt">${i.count}</span>` : '';
        return `<td data-col="subject" title="${esc(i.subject)}">${chip}${esc(i.subject || '(no subject)')}${cnt}</td>`;
      }
      case 'date': return `<td data-col="date" title="${esc(fmtDate(i.date, true))}">${esc(fmtDate(i.date))}</td>`;
      case 'attachment': return `<td>${i.hasAttachment ? icon('attach', 'ic-sm') : ''}</td>`;
      case 'folder': return `<td>${esc((i.folders || [i.folder]).map(folderDisplay).join(', '))}</td>`;
      case 'flags': return `<td>${unread ? (i.unread > 1 ? i.unread + ' unread' : 'Unread') : 'Read'}${i.flagged ? ', Starred' : ''}</td>`;
      case 'size': return `<td>${esc(fmtSize(i.size))}</td>`;
      default: return '<td></td>';
    }
  }).join('');
  return `<tr class="${unread ? 'unread' : ''} ${state.selected.has(i.key) ? 'selected' : ''} ${state.thread?.item.key === i.key ? 'current' : ''}" data-key="${esc(i.key)}">${cells}</tr>`;
}

const itemByKey = key => state.items.find(i => i.key === key);
const selectedItems = () => state.items.filter(i => state.selected.has(i.key) && !state.hidden.has(i.key));

function updateSelectState() {
  const visible = state.items.filter(i => !state.hidden.has(i.key));
  const n = visible.filter(i => state.selected.has(i.key)).length;
  const all = $('selectAll');
  all.checked = visible.length > 0 && n === visible.length;
  all.indeterminate = n > 0 && n < visible.length;
  updateActionStates();
}

function updateActionStates() {
  const role = roleOf(state.folder);
  const searching = !!state.query;
  const some = selectedItems().length > 0;
  for (const id of ['archive', 'delete', 'spam', 'markRead', 'markUnread', 'starSelected']) $(id).disabled = !some;
  if (some) $('archive').disabled = searching ? false : role !== 'inbox';
  $('archive').title = role === 'inbox' || searching ? 'Archive' : 'Archive (only available in the Inbox)';
  const inTrash = role === 'trash' && !searching;
  $('delete').title = inTrash ? 'Delete forever' : 'Move to Trash';
  setIcon($('delete'), 'trash');
  $('spam').title = role === 'spam' && !searching ? 'Not spam' : 'Report spam';
}

// list interactions
$('mailRows').addEventListener('click', e => {
  const tr = e.target.closest('tr[data-key]');
  if (!tr) return;
  const key = tr.dataset.key;
  const star = e.target.closest('[data-star]');
  if (star) { toggleStar([itemByKey(key)]); return; }
  if (e.target.closest('input.rowcheck')) {
    e.target.checked ? state.selected.add(key) : state.selected.delete(key);
    tr.classList.toggle('selected', e.target.checked);
    updateSelectState();
    return;
  }
  const item = itemByKey(key);
  if (item) openConversation(item);
});

$('tableHead').addEventListener('click', e => {
  const b = e.target.closest('[data-sort]');
  if (!b) return;
  const k = b.dataset.sort;
  if (state.sort === k) state.desc = !state.desc; else { state.sort = k; state.desc = k === 'date' || k === 'size'; }
  state.page = 1;
  loadList();
});

$('tableHead').addEventListener('pointerdown', e => {
  const grip = e.target.closest('[data-grip]');
  if (!grip) return;
  e.preventDefault();
  e.stopPropagation();
  const key = grip.dataset.grip;
  const col = $('cols').querySelector(`col[data-col="${key}"]`);
  const th = grip.closest('th');
  const startX = e.clientX;
  const startW = th.getBoundingClientRect().width;
  grip.classList.add('dragging');
  grip.setPointerCapture?.(e.pointerId);
  const move = ev => { col.style.width = Math.max(40, Math.min(900, startW + ev.clientX - startX)) + 'px'; };
  const up = () => {
    grip.classList.remove('dragging');
    window.removeEventListener('pointermove', move);
    const w = loadWidths();
    w[key] = Math.round(parseFloat(col.style.width));
    store.set(WIDTH_KEY, JSON.stringify(w));
  };
  window.addEventListener('pointermove', move);
  window.addEventListener('pointerup', up, { once: true });
});

$('selectAll').addEventListener('change', e => {
  const visible = state.items.filter(i => !state.hidden.has(i.key));
  for (const i of visible) e.target.checked ? state.selected.add(i.key) : state.selected.delete(i.key);
  renderList();
});

$('prevPage').onclick = () => { if (state.page > 1) { state.page--; loadList(); } };
$('nextPage').onclick = () => { if (state.page < Math.ceil(state.total / state.pageSize)) { state.page++; loadList(); } };
$('firstPage').onclick = () => { if (state.page !== 1) { state.page = 1; loadList(); } };
$('lastPage').onclick = () => { const p = Math.max(1, Math.ceil(state.total / state.pageSize)); if (state.page !== p) { state.page = p; loadList(); } };

function syncConvButton() {
  $('convToggle').setAttribute('aria-pressed', String(state.grouped));
  $('convToggle').title = state.grouped ? 'Conversation view is on — click to list single messages' : 'Conversation view is off — click to group messages into conversations';
  $('conversationSetting').checked = state.grouped;
}
async function setGrouped(on) {
  state.grouped = on;
  state.page = 1;
  state.selected.clear();
  syncConvButton();
  await savePrefs().catch(() => {});
  loadList();
}
$('convToggle').onclick = () => setGrouped(!state.grouped);
$('conversationSetting').onchange = e => setGrouped(e.target.checked);

$('columnsBtn').onclick = () => {
  const opts = Object.entries(COLUMNS).filter(([k]) => k !== 'select').map(([k, c]) => `<label class="inline"><input type="checkbox" name="col_${k}" value="1" ${state.columns.includes(k) ? 'checked' : ''}> ${esc(c.label)}</label>`).join('');
  const sel = `<label class="inline"><input type="checkbox" name="col_select" value="1" ${state.columns.includes('select') ? 'checked' : ''}> Select checkbox</label>`;
  openModal('Choose table columns', sel + opts, 'Apply', async d => {
    const chosen = Object.keys(COLUMNS).filter(k => d['col_' + k]);
    state.columns = chosen.includes('subject') ? chosen : [...chosen, 'subject'];
    state.columns.sort((a, b) => Object.keys(COLUMNS).indexOf(a) - Object.keys(COLUMNS).indexOf(b));
    await savePrefs();
    renderList();
  });
};

// ===================================================================== actions
function undoDelay() { return Math.max(0, Math.min(30, Number(state.prefs.undo_seconds ?? 5))); }

/** Hide the items at once, run the server action after the undo window. */
function queueAction(items, label, run) {
  const keys = items.map(i => i.key);
  const finish = async () => {
    try { await run(); } catch (e) { toast('Could not complete the action: ' + String(e?.message || e), { error: true, timeout: 8000 }); }
    keys.forEach(k => { state.hidden.delete(k); state.selected.delete(k); });
    await Promise.all([loadList(true), reloadFolders()]);
  };
  keys.forEach(k => state.hidden.add(k));
  renderList();
  const delay = undoDelay();
  if (!delay) return finish();
  let cancelled = false;
  const timer = setTimeout(() => { if (!cancelled) { remove(); finish(); } }, delay * 1000);
  const remove = toast(`${label} · ${items.length} ${state.grouped ? 'conversation' : 'message'}${items.length === 1 ? '' : 's'}`, {
    action: 'Undo', timeout: 0,
    onAction: () => { cancelled = true; clearTimeout(timer); keys.forEach(k => state.hidden.delete(k)); renderList(); },
  });
}

const refsOf = items => items.flatMap(i => i.refs || []);

function applyAction(kind, items) {
  items = items.filter(Boolean);
  if (!items.length) return;
  const role = roleOf(state.folder);
  const searching = !!state.query;
  const u = state.account;
  if (kind === 'archive') return queueAction(items, 'Archived', () => call('move_messages', { username: u, refs: refsOf(items), target: 'archive' }));
  if (kind === 'trash') {
    if (role === 'trash' && !searching) {
      return confirmBox(`Permanently delete ${items.length} ${items.length === 1 ? 'item' : 'items'}? This cannot be undone.`, 'Delete forever', true).then(ok => ok && queueAction(items, 'Deleted forever', () => call('delete_forever', { username: u, refs: refsOf(items) })));
    }
    return queueAction(items, 'Moved to Trash', () => call('move_messages', { username: u, refs: refsOf(items), target: 'trash' }));
  }
  if (kind === 'spam') {
    if (role === 'spam' && !searching) return queueAction(items, 'Moved to Inbox', () => call('move_messages', { username: u, refs: refsOf(items), target: 'inbox' }));
    return queueAction(items, 'Reported as spam', () => call('move_messages', { username: u, refs: refsOf(items), target: 'spam' }));
  }
  const flagJob = async (flag, enabled) => {
    try {
      await call('set_flag', { username: u, refs: refsOf(items), flag, enabled });
      await Promise.all([loadList(true), reloadFolders()]);
    } catch (e) { toast('Could not update the messages: ' + String(e?.message || e), { error: true }); }
  };
  if (kind === 'read') return flagJob('Seen', true);
  if (kind === 'unread') return flagJob('Seen', false);
}

function toggleStar(items) {
  items = items.filter(Boolean);
  if (!items.length) return;
  const on = !items.every(i => i.flagged);
  call('set_flag', { username: state.account, refs: refsOf(items), flag: 'Flagged', enabled: on })
    .then(() => Promise.all([loadList(true), reloadFolders()]))
    .catch(e => toast('Could not update the star: ' + String(e?.message || e), { error: true }));
}

$('archive').onclick = () => applyAction('archive', selectedItems());
$('delete').onclick = () => applyAction('trash', selectedItems());
$('spam').onclick = () => applyAction('spam', selectedItems());
$('markRead').onclick = () => applyAction('read', selectedItems());
$('markUnread').onclick = () => applyAction('unread', selectedItems());
$('starSelected').onclick = () => toggleStar(selectedItems());

// ================================================================ conversation view
function threadRefsForRemoval() {
  const t = state.thread;
  const item = t.item;
  const role = roleOf(item.folder);
  if (['all', 'trash', 'spam'].includes(role)) return item.refs;
  // Moving the All Mail copy trashes the whole Gmail message (every label), for every message of the thread.
  const allCopies = t.rows.filter(r => roleOf(r.folder) === 'all').map(r => ({ folder: r.folder, uid: r.uid }));
  return allCopies.length ? allCopies : item.refs;
}

function openConversation(item) {
  let t = state.threads.find(x => x.item.key === item.key && x.grouped === state.grouped);
  if (!t) {
    t = { id: 'thr-' + Date.now().toString(36), item, title: item.subject || '(no subject)', rows: [], details: new Map(), open: new Set(), allOpen: false, loaded: false, grouped: state.grouped };
    state.threads.push(t);
  }
  return activateThread(t);
}

async function activateThread(t) {
  state.thread = t;
  setView('thread');
  setIcon($('tStar'), t.item.flagged ? 'starFilled' : 'star');
  setIcon($('tExpand'), t.allOpen ? 'chevronUp' : 'chevronDown');
  $('threadSubject').textContent = t.title;
  $('threadScroll').scrollTop = 0;
  if (t.loaded) return renderThread();
  $('threadMessages').innerHTML = '<div class="loading">Loading conversation…</div>';
  const item = t.item;
  try {
    const rows = await call('thread_messages', { username: state.account, threadId: t.grouped ? item.threadId || null : null, folder: item.folder, uid: item.uid });
    if (!rows.length) throw new Error('This conversation is no longer on the server.');
    t.rows = rows;
    t.loaded = true;
    t.title = rows[0].subject || item.subject || '(no subject)';
    const unread = rows.filter(r => !r.flags.some(f => f.toLowerCase() === 'seen'));
    (unread.length ? unread : [rows[rows.length - 1]]).forEach(r => t.open.add(r.uid + '@' + r.folder));
    t.open.add(rows[rows.length - 1].uid + '@' + rows[rows.length - 1].folder);
    renderTabs();
    if (state.thread === t && state.view === 'thread') {
      $('threadSubject').textContent = t.title;
      renderThread();
    }
  } catch (e) {
    if (state.thread === t) $('threadMessages').innerHTML = `<div class="loading">${esc(e?.message || e)}</div>`;
  }
}

/** Close a conversation tab; shows the neighbouring tab, or the list when none is left. */
function closeThread(t, refresh = true) {
  const i = state.threads.indexOf(t);
  if (i < 0) return;
  state.threads.splice(i, 1);
  const wasShown = state.view === 'thread' && state.thread === t;
  if (state.thread === t) state.thread = null;
  if (wasShown) {
    const next = state.threads[i] || state.threads[i - 1];
    if (next) activateThread(next); else setView('mail');
  } else renderTabs();
  if (refresh) Promise.all([loadList(true), reloadFolders()]);
}

const rowId = r => r.uid + '@' + r.folder;

function renderThread() {
  const t = state.thread;
  if (!t) return;
  $('threadMessages').innerHTML = t.rows.map(r => {
    const open = t.open.has(rowId(r));
    const unread = !r.flags.some(f => f.toLowerCase() === 'seen');
    return `<div class="msgcard ${open ? 'open' : ''} ${unread ? 'unread' : ''}" data-id="${esc(rowId(r))}" style="margin-bottom:10px">
      <div class="msghead" data-toggle="${esc(rowId(r))}"><div class="msgavatar">${esc(initial(r.from))}</div>
        <div class="msgwho"><b>${esc(r.from)}</b> <span class="sub">&lt;${esc(r.email)}&gt;</span><div class="sub">to ${esc(r.to || 'me')}</div></div>
        ${r.hasAttachment ? icon('attach', 'ic-sm') : ''}<span class="msgdate" title="${esc(fmtDate(r.date, true))}">${esc(fmtDate(r.date, true))}</span></div>
      ${open ? `<div class="msgbody" data-body="${esc(rowId(r))}"><div class="loading">Loading message…</div></div><div class="msgfoot" data-foot="${esc(rowId(r))}"></div>` : ''}
    </div>`;
  }).join('');
  for (const r of t.rows) if (t.open.has(rowId(r))) fillBody(r);
}

async function getDetail(r) {
  const t = state.thread;
  const id = rowId(r);
  if (t.details.has(id)) return t.details.get(id);
  const d = await call('get_message', { username: state.account, folder: r.folder, uid: r.uid });
  t.details.set(id, d);
  return d;
}

async function fillBody(r, forceImages = false, plain = false) {
  const id = rowId(r);
  const host = document.querySelector(`[data-body="${CSS.escape(id)}"]`);
  if (!host) return;
  try {
    const d = await getDetail(r);
    if (!state.thread || !document.body.contains(host)) return;
    if (!r.flags.some(f => f.toLowerCase() === 'seen')) r.flags = [...r.flags, 'Seen'];
    host.closest('.msgcard')?.classList.remove('unread');
    let html = '';
    let blocked = 0;
    if (d.html && d.html.trim() && !plain) {
      const res = sanitizeEmailHtml(withInlineImages(d.html, d.attachments), { blockRemoteImages: !forceImages && state.prefs.remote_images === false });
      html = res.html;
      blocked = res.blocked;
    } else {
      html = `<pre class="plain">${esc(d.text || '(This message has no text.)')}</pre>`;
    }
    const extras = [];
    if (blocked) extras.push(`<div class="banner warn" style="margin-bottom:8px">${icon('image', 'ic-sm')}<span>${blocked} remote image${blocked === 1 ? '' : 's'} blocked.</span><button type="button" data-images="${esc(id)}">Load images</button></div>`);
    if (d.html && d.html.trim()) extras.push(`<div style="text-align:right;margin-bottom:6px"><button type="button" class="btnlabel" data-plain="${esc(id)}" data-mode="${plain ? 'html' : 'plain'}">${icon('source', 'ic-sm')} ${plain ? 'Show formatted' : 'Show plain text'}</button></div>`);
    host.innerHTML = extras.join('') + html;
    const foot = document.querySelector(`[data-foot="${CSS.escape(id)}"]`);
    if (foot) {
      const atts = (d.attachments || []).filter(a => !a.inline || !(d.html || '').includes('cid:' + a.contentId));
      foot.innerHTML = atts.map(a => `<button type="button" class="attach" data-att="${a.index}" data-row="${esc(id)}" title="Open or save">${icon('download', 'ic-sm')}${esc(a.filename)} · ${esc(fmtSize(a.size))}</button>`).join('') +
        `<span class="push"></span><button type="button" class="btnlabel" data-reply="${esc(id)}">${icon('reply', 'ic-sm')} Reply</button><button type="button" class="btnlabel" data-fwd="${esc(id)}">${icon('forward', 'ic-sm')} Forward</button>`;
    }
  } catch (e) {
    host.innerHTML = `<div class="loading">Could not load this message: ${esc(e?.message || e)}</div>`;
  }
}

$('threadMessages').addEventListener('click', async e => {
  const t = state.thread;
  if (!t) return;
  const link = e.target.closest('[data-mail-link]');
  if (link) {
    e.preventDefault();
    const url = link.getAttribute('data-mail-link') || '';
    if (/^https?:\/\//i.test(url)) openWeb({ url });
    else if (/^mailto:/i.test(url)) startCompose({ to: decodeURIComponent(url.slice(7).split('?')[0]) });
    return;
  }
  const inl = e.target.closest('.msgbody img');
  if (inl && inl.src) {
    e.preventDefault();
    const m = /^data:([^;,]+);base64,(.*)$/s.exec(inl.src);
    const ext = (m?.[1] || 'image/png').split('/')[1].replace('jpeg', 'jpg').replace(/\+.*/, '');
    return openDoc({ kind: 'image', title: inl.alt || 'Image', filename: (inl.alt && /\.\w{2,4}$/.test(inl.alt) ? inl.alt : 'image.' + ext), src: inl.src, dataBase64: m ? m[2] : '' });
  }
  const find = id => t.rows.find(r => rowId(r) === id);
  const img = e.target.closest('[data-images]');
  if (img) return fillBody(find(img.dataset.images), true);
  const plainBtn = e.target.closest('[data-plain]');
  if (plainBtn) return fillBody(find(plainBtn.dataset.plain), false, plainBtn.dataset.mode === 'plain');
  const att = e.target.closest('[data-att]');
  if (att) return openAttachment(find(att.dataset.row), Number(att.dataset.att), att);
  const rp = e.target.closest('[data-reply]');
  if (rp) return replyTo(find(rp.dataset.reply), false);
  const fw = e.target.closest('[data-fwd]');
  if (fw) return forwardOf(find(fw.dataset.fwd));
  const head = e.target.closest('[data-toggle]');
  if (head) {
    const id = head.dataset.toggle;
    const card = head.closest('.msgcard');
    if (t.open.has(id) && card.classList.contains('open')) t.open.delete(id); else t.open.add(id);
    renderThread();
  }
});

const isPdf = a => /pdf/i.test(a.contentType || '') || /\.pdf$/i.test(a.filename || '');
const isImg = a => /^image\/(png|jpe?g|gif|webp|bmp|svg\+xml)$/i.test(a.contentType || '');

async function openAttachment(row, index, btn) {
  if (btn) btn.disabled = true;
  try {
    const a = await call('get_attachment', { username: state.account, folder: row.folder, uid: row.uid, index });
    if (isPdf(a)) openDoc({ kind: 'pdf', title: a.filename, filename: a.filename, dataBase64: a.dataBase64 });
    else if (isImg(a)) openDoc({ kind: 'image', title: a.filename, filename: a.filename, dataBase64: a.dataBase64, src: `data:${a.contentType};base64,${a.dataBase64}` });
    else {
      const p = await call('save_attachment', { filename: a.filename, dataBase64: a.dataBase64 });
      toast('Saved to ' + p);
    }
  } catch (e) { toast('Could not fetch the attachment: ' + String(e?.message || e), { error: true }); }
  finally { if (btn) btn.disabled = false; }
}

// ---- viewer tabs (PDF and image)
function openDoc(o) {
  const key = o.filename + ':' + (o.dataBase64 || o.src || '').length + ':' + (o.dataBase64 || o.src || '').slice(-40);
  let d = state.docs.find(x => x.key === key);
  if (!d) { d = { id: 'doc-' + Date.now().toString(36), key, scale: 1, fit: true, ...o }; state.docs.push(d); }
  return activateDoc(d);
}
async function activateDoc(d) {
  state.doc = d;
  setView('doc');
  $('docTitle').textContent = d.title;
  $('docSave').hidden = !d.dataBase64;
  await drawDoc();
}
async function drawDoc() {
  const d = state.doc;
  if (!d) return;
  const r = await renderDoc(d, $('docScroll'), $('docPages'));
  if (!r || state.doc !== d) return;
  $('docZoomLabel').textContent = Math.round(d.scale * 100) + '%';
  $('docPageInfo').textContent = d.kind === 'pdf' ? `${r.pages} page${r.pages === 1 ? '' : 's'}` : '';
}
function closeDoc(d) {
  const i = state.docs.indexOf(d);
  if (i < 0) return;
  state.docs.splice(i, 1);
  const wasShown = state.view === 'doc' && state.doc === d;
  if (state.doc === d) state.doc = null;
  if (wasShown) {
    const next = state.docs[i] || state.docs[i - 1];
    if (next) activateDoc(next); else if (state.thread) activateThread(state.thread); else setView('mail');
  } else renderTabs();
}
const zoomDoc = f => { const d = state.doc; if (!d) return; d.fit = false; d.scale = Math.min(5, Math.max(0.25, d.scale * f)); drawDoc(); };
$('docZoomIn').onclick = () => zoomDoc(1.25);
$('docZoomOut').onclick = () => zoomDoc(0.8);
$('docFit').onclick = () => { if (state.doc) { state.doc.fit = true; drawDoc(); } };
$('docSave').onclick = async () => {
  const d = state.doc;
  if (!d?.dataBase64) return;
  try { toast('Saved to ' + await call('save_attachment', { filename: d.filename, dataBase64: d.dataBase64 })); }
  catch (e) { toast(String(e?.message || e), { error: true }); }
};
window.addEventListener('resize', throttle(() => { if (state.view === 'doc' && state.doc?.fit) drawDoc(); }, 300));

async function leaveThread() {
  setView('mail');
  await Promise.all([loadList(true), reloadFolders()]);
}
$('backMail').onclick = leaveThread;
$('tArchive').onclick = () => { const t = state.thread; if (!t) return; if (roleOf(t.item.folder) !== 'inbox' && !state.query) return toast('Archive is only available for Inbox conversations.'); applyAction('archive', [t.item]); closeThread(t, false); };
$('tTrash').onclick = () => { const t = state.thread; if (!t) return; const item = { ...t.item, refs: threadRefsForRemoval() }; applyAction('trash', [item]); closeThread(t, false); };
$('tSpam').onclick = () => { const t = state.thread; if (!t) return; const item = { ...t.item, refs: threadRefsForRemoval() }; applyAction('spam', [item]); closeThread(t, false); };
$('tUnread').onclick = async () => { const t = state.thread; if (!t) return; const last = t.rows[t.rows.length - 1]; await call('set_flag', { username: state.account, refs: [{ folder: last.folder, uid: last.uid }], flag: 'Seen', enabled: false }).catch(e => toast(String(e), { error: true })); closeThread(t); };
$('tStar').onclick = () => { const t = state.thread; if (!t) return; toggleStar([t.item]); t.item.flagged = !t.item.flagged; setIcon($('tStar'), t.item.flagged ? 'starFilled' : 'star'); };
$('tReply').onclick = () => state.thread && replyTo(state.thread.rows.at(-1), false);
$('tReplyAll').onclick = () => state.thread && replyTo(state.thread.rows.at(-1), true);
$('tForward').onclick = () => state.thread && forwardOf(state.thread.rows.at(-1));
$('tPrint').onclick = () => window.print();
$('tExpand').onclick = () => {
  const t = state.thread; if (!t) return;
  t.allOpen = !t.allOpen;
  t.open = new Set(t.allOpen ? t.rows.map(rowId) : [rowId(t.rows.at(-1))]);
  setIcon($('tExpand'), t.allOpen ? 'chevronUp' : 'chevronDown');
  renderThread();
};

// ======================================================================= compose
let composeEditor;
let signatureEditor;

function signatureHtml(identity) {
  const html = state.signatures[identity];
  return html && html.replace(/<[^>]*>|&nbsp;|\s/g, '') ? `<div data-maildesk-signature="yes" style="margin-top:16px">${html}</div>` : '';
}

function updateComposeSignature() {
  const ed = $('editor');
  const block = signatureHtml($('fromIdentity').value || state.account);
  const old = ed.querySelector('[data-maildesk-signature]');
  if (old) {
    if (block) old.outerHTML = block; else old.remove();
  } else if (block) {
    const quote = ed.querySelector('[data-maildesk-quote]');
    if (quote) quote.insertAdjacentHTML('beforebegin', block); else ed.insertAdjacentHTML('beforeend', block);
  }
}

function fillIdentities(preferred) {
  const items = identities();
  for (const id of ['fromIdentity', 'signatureIdentity']) {
    const sel = $(id);
    const cur = sel.value;
    sel.innerHTML = items.map(v => `<option value="${esc(v)}">${esc(v)}</option>`).join('');
    sel.value = items.includes(cur) ? cur : state.account;
  }
  if (preferred && items.includes(preferred)) $('fromIdentity').value = preferred;
}

// ---- compose tabs: every message is a tab; the single compose form is loaded from / saved to the tab's snapshot
function snapCompose() {
  const c = state.cur;
  if (!c || !composeEditor) return;
  Object.assign(c, {
    identity: $('fromIdentity').value, to: $('toField').value, cc: $('ccField').value, bcc: $('bccField').value,
    subject: $('subjectField').value, html: composeEditor.getHTML(), showCc: !$('ccRow').hidden, showBcc: !$('bccRow').hidden,
    attachments: state.attachments, meta: state.compose, status: $('composeStatus').textContent,
  });
}

function loadCompose(c) {
  fillIdentities(c.identity);
  $('toField').value = c.to; $('ccField').value = c.cc; $('bccField').value = c.bcc;
  $('ccRow').hidden = !c.showCc; $('bccRow').hidden = !c.showBcc;
  $('subjectField').value = c.subject;
  $('composeTitle').textContent = c.title;
  state.compose = c.meta;
  state.attachments = c.attachments;
  renderAttachments();
  $('composeStatus').textContent = c.status || '';
  applyComposeDefaults();
  composeEditor.setHTML(c.html);
}

function composeBlank(c) {
  const probe = document.createElement('div');
  probe.innerHTML = c.html || '';
  probe.querySelectorAll('[data-maildesk-signature]').forEach(n => n.remove());
  return !c.to.trim() && !c.cc.trim() && !c.bcc.trim() && !c.subject.trim() && !c.attachments.length && !probe.textContent.trim() && !probe.querySelector('img');
}

function startCompose(o = {}) {
  if (!state.account) return accountModal();
  if (state.view === 'compose') snapCompose();
  const c = {
    id: 'cmp-' + Date.now().toString(36) + Math.random().toString(36).slice(2, 5), title: o.title || 'New message',
    identity: o.identity, to: o.to || '', cc: o.cc || '', bcc: '', subject: o.subject || '',
    html: `<div><br></div>${o.quote || ''}`, showCc: !!o.cc, showBcc: false,
    attachments: o.attachments || [], meta: { inReplyTo: o.inReplyTo || '', references: o.references || '' },
    draft: null, status: '',
  };
  state.composes.push(c);
  state.cur = c;
  loadCompose(c);
  updateComposeSignature();
  snapCompose();
  setView('compose');
  ($('toField').value ? $('editor') : $('toField')).focus();
  if ($('toField').value) { const r = document.createRange(); r.setStart($('editor'), 0); r.collapse(true); getSelection().removeAllRanges(); getSelection().addRange(r); }
  call('warm_connections', { username: state.account }).catch(() => {});
  loadContacts();
}

function activateCompose(c) {
  if (state.view === 'compose') snapCompose();
  state.cur = c;
  loadCompose(c);
  setView('compose');
}

/** Remove a compose tab and show the neighbouring tab (or the list). */
function dropCompose(c) {
  const i = state.composes.indexOf(c);
  if (i < 0) return;
  const wasShown = state.view === 'compose' && state.cur === c;
  state.composes.splice(i, 1);
  if (state.cur === c) state.cur = null;
  if (wasShown) {
    const next = state.composes[i] || state.composes[i - 1];
    if (next) activateCompose(next); else setView('mail');
  } else renderTabs();
}

async function closeComposeTab(c) {
  if (state.cur === c && state.view === 'compose') snapCompose();
  if (!composeBlank(c) && !(await confirmBox('Discard this message?', 'Discard', true))) return;
  dropCompose(c);
}

const addrList = s => String(s || '').split(',').map(x => x.trim()).filter(Boolean);

function quoteBlock(d, kind) {
  const who = `${esc(d.from)} &lt;${esc(d.email)}&gt;`;
  const inner = d.html && d.html.trim() ? sanitizeForEditor(d.html) : `<div>${esc(d.text).replace(/\n/g, '<br>')}</div>`;
  if (kind === 'forward') {
    return `<div data-maildesk-quote="yes"><br>---------- Forwarded message ----------<br>From: ${who}<br>Date: ${esc(fmtDate(d.date, true))}<br>Subject: ${esc(d.subject)}<br>To: ${esc(d.to)}<br><br>${inner}</div>`;
  }
  return `<div data-maildesk-quote="yes"><br>On ${esc(fmtDate(d.date, true))}, ${who} wrote:<blockquote style="margin:0 0 0 .8ex;border-left:2px solid #ccc;padding-left:1ex">${inner}</blockquote></div>`;
}

async function replyTo(row, all) {
  if (!row) return;
  try {
    const d = await getDetail(row);
    const mine = identities().map(x => x.toLowerCase());
    const fromMe = mine.includes(String(d.email).toLowerCase());
    const to = fromMe ? addrList(d.to) : [d.replyTo || d.email];
    let cc = [];
    if (all) cc = [...addrList(d.to), ...addrList(d.cc)].filter(a => !mine.includes(a.toLowerCase()) && !to.map(x => x.toLowerCase()).includes(a.toLowerCase()));
    const recipients = [...addrList(d.to), ...addrList(d.cc)].map(a => a.toLowerCase());
    const identity = identities().find(i => recipients.includes(i.toLowerCase()));
    startCompose({
      title: all ? 'Reply all' : 'Reply', to: to.join(', '), cc: [...new Set(cc)].join(', '),
      subject: /^re:/i.test(d.subject) ? d.subject : 'Re: ' + d.subject,
      inReplyTo: d.messageId, references: [d.references, d.messageId].filter(Boolean).join(' '),
      quote: quoteBlock(d, 'reply'), identity,
    });
  } catch (e) { toast('Could not prepare the reply: ' + String(e?.message || e), { error: true }); }
}

async function forwardOf(row) {
  if (!row) return;
  try {
    const d = await getDetail(row);
    const atts = [];
    for (const a of (d.attachments || []).filter(x => !x.inline)) {
      const full = await call('get_attachment', { username: state.account, folder: row.folder, uid: row.uid, index: a.index });
      atts.push({ filename: full.filename, contentType: full.contentType, dataBase64: full.dataBase64, size: full.size });
    }
    startCompose({ title: 'Forward', subject: /^fwd?:/i.test(d.subject) ? d.subject : 'Fwd: ' + d.subject, quote: quoteBlock(d, 'forward'), attachments: atts });
  } catch (e) { toast('Could not prepare the forward: ' + String(e?.message || e), { error: true }); }
}

function renderAttachments() {
  $('attachmentList').innerHTML = state.attachments.map((a, i) => `<span class="tag">${icon('attach', 'ic-sm')}${esc(a.filename)} · ${esc(fmtSize(a.size))} <button type="button" data-rm="${i}" title="Remove" aria-label="Remove ${esc(a.filename)}">${icon('close')}</button></span>`).join('');
}
$('attachmentList').onclick = e => { const b = e.target.closest('[data-rm]'); if (b) { state.attachments.splice(+b.dataset.rm, 1); renderAttachments(); } };

async function addFiles(files) {
  for (const f of files) {
    const total = state.attachments.reduce((n, a) => n + a.size, 0) + f.size;
    if (f.size > 25 * 1024 * 1024 || total > 25 * 1024 * 1024) { toast(`${f.name} would exceed Gmail's 25 MB limit and was skipped.`, { error: true }); continue; }
    const url = await new Promise((res, rej) => { const r = new FileReader(); r.onload = () => res(String(r.result)); r.onerror = rej; r.readAsDataURL(f); });
    state.attachments.push({ filename: f.name, contentType: f.type || 'application/octet-stream', dataBase64: url.split(',')[1] || '', size: f.size });
  }
  renderAttachments();
}
$('attachBtn').onclick = () => $('fileInput').click();
$('fileInput').onchange = async e => { await addFiles([...e.target.files]); e.target.value = ''; };
$('showCc').onclick = () => { $('ccRow').hidden = false; $('ccField').focus(); };
$('showBcc').onclick = () => { $('bccRow').hidden = false; $('bccField').focus(); };
$('fromIdentity').onchange = updateComposeSignature;

function buildRequest() {
  const html = `<div style="${composeCss()}">${composeEditor.getHTML()}</div>`;
  return {
    username: state.account,
    fromEmail: $('fromIdentity').value || state.account,
    fromName: $('displayName').value || null,
    to: $('toField').value,
    cc: $('ccField').value || null,
    bcc: $('bccField').value || null,
    subject: $('subjectField').value,
    text: $('editor').innerText,
    html,
    inReplyTo: state.compose.inReplyTo || null,
    references: state.compose.references || null,
    attachments: state.attachments.map(({ filename, contentType, dataBase64 }) => ({ filename, contentType, dataBase64 })),
  };
}

function checkRecipients() {
  if (!$('toField').value.trim() && !$('ccField').value.trim() && !$('bccField').value.trim()) { toast('Enter at least one recipient.', { error: true }); return false; }
  return true;
}

// Send in the background: the compose tab closes at once and the result arrives as a toast.
$('sendBtn').onclick = async () => {
  if (!checkRecipients()) return;
  if (!$('subjectField').value.trim() && !(await confirmBox('Send this message without a subject?', 'Send anyway'))) return;
  const c = state.cur;
  snapCompose();
  const req = buildRequest();
  const subject = req.subject || '(no subject)';
  dropCompose(c);
  const done = toast(`Sending “${subject}”…`, { timeout: 0 });
  try {
    await call('send_email', { req });
    done();
    toast('Message sent');
    setTimeout(() => syncCurrent(false), 2500);
  } catch (e) {
    done();
    toast('Could not send “' + subject + '”: ' + String(e?.message || e), { error: true, timeout: 0, action: 'Reopen', onAction: () => { state.composes.push(c); activateCompose(c); } });
  }
};

$('saveDraft').onclick = async () => {
  const c = state.cur;
  $('saveDraft').disabled = true;
  $('composeStatus').textContent = 'Saving draft…';
  try {
    const r = await call('save_draft', { req: buildRequest(), replaceUid: c.draft?.uid ?? null, replaceValidity: c.draft?.validity ?? null });
    c.draft = { uid: r.uid, validity: r.validity };
    if (state.cur === c) $('composeStatus').textContent = r.message;
  } catch (e) { if (state.cur === c) $('composeStatus').textContent = 'Could not save draft: ' + String(e?.message || e); }
  finally { $('saveDraft').disabled = false; }
};
$('discardDraft').onclick = async () => { if (await confirmBox('Discard this message?', 'Discard', true)) dropCompose(state.cur); };
$('backCompose').onclick = () => setView('mail');
$('subjectField').addEventListener('input', throttle(renderTabs, 400));

// ---- schedule send
const pad2 = n => String(n).padStart(2, '0');
const localInput = d => `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}T${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
const fmtWhen = d => new Intl.DateTimeFormat(undefined, { weekday: 'short', day: 'numeric', month: 'short', hour: 'numeric', minute: '2-digit' }).format(d);
function schedulePresets() {
  const at = (days, h) => { const d = new Date(); d.setDate(d.getDate() + days); d.setHours(h, 0, 0, 0); return d; };
  const list = [];
  const now = new Date();
  if (now.getHours() < 17) list.push(['Later today', at(0, now.getHours() < 12 ? 15 : 18)]);
  list.push(['Tomorrow morning', at(1, 8)], ['Tomorrow afternoon', at(1, 13)]);
  const mon = new Date(); mon.setDate(mon.getDate() + ((8 - mon.getDay()) % 7 || 7)); mon.setHours(8, 0, 0, 0);
  list.push(['Monday morning', mon]);
  return list;
}

$('scheduleBtn').onclick = async () => {
  if (!checkRecipients()) return;
  const c = state.cur;
  snapCompose();
  const presets = schedulePresets();
  const soon = new Date(Date.now() + 3600e3);
  const opts = presets.map(([label, d], i) => `<label class="schedopt"><input type="radio" name="when" value="${d.getTime()}" ${i === 0 ? 'checked' : ''}> ${esc(label)}<small>${esc(fmtWhen(d))}</small></label>`).join('');
  const ok = await openModal('Schedule send',
    `${opts}<label class="schedopt"><input type="radio" name="when" value="custom"> Pick date and time<input type="datetime-local" name="custom" value="${localInput(soon)}" min="${localInput(new Date())}" style="margin-left:auto"></label>
     <p class="sub" style="margin:8px 0 0">The message is sent from this computer at the chosen time, so WorkDesk must be running then. If it was closed, the message goes out the next time you open it.</p>
     <p style="margin:6px 0 0"><button type="button" class="btnlabel" id="openScheduled">View scheduled messages</button></p>`,
    'Schedule',
    async d => {
      const t = d.when === 'custom' ? new Date(d.custom).getTime() : Number(d.when);
      if (!Number.isFinite(t) || t < Date.now() + 30000) throw new Error('Choose a time in the future.');
      await call('schedule_send', { req: buildRequest(), sendAt: t });
      toast('Scheduled for ' + fmtWhen(new Date(t)));
      dropCompose(c);
    });
  return ok;
};
document.addEventListener('click', e => { if (e.target.closest('#openScheduled')) { closeModal(); scheduledModal(); } });
$('viewScheduled').onclick = () => scheduledModal();

async function scheduledModal() {
  const draw = async () => {
    const items = await call('list_scheduled').catch(() => []);
    $('modalFields').innerHTML = items.length ? items.map(i => `<div class="itemrow" data-sid="${esc(i.id)}"><div class="meta"><b>${esc(i.subject || '(no subject)')}</b><span>To ${esc(i.to)} · ${esc(fmtWhen(new Date(i.sendAt)))}${i.error ? ' · failed: ' + esc(i.error) : ''}</span></div><button type="button" class="btnlabel" data-snow="${esc(i.id)}">${i.error ? 'Retry' : 'Send now'}</button><button type="button" class="btnlabel" data-scancel="${esc(i.id)}">Cancel</button></div>`).join('') : '<div class="sub">Nothing is scheduled.</div>';
  };
  const p = openModal('Scheduled messages', '', 'Close', async () => {}, { hideSubmit: true });
  await draw();
  const host = $('modalFields');
  host.onclick = async e => {
    const n = e.target.closest('[data-snow]'); const x = e.target.closest('[data-scancel]');
    try {
      if (n) { await call('send_scheduled_now', { id: n.dataset.snow }); toast('Sending shortly…'); }
      if (x) await call('cancel_scheduled', { id: x.dataset.scancel });
    } catch (err) { toast(String(err?.message || err), { error: true }); }
    if (n || x) draw();
  };
  await p;
}

// ---- contacts: suggestions from synced headers + imported address books
async function loadContacts(force = false) {
  if (!state.account || (!force && Date.now() - state.contactsAt < 600000)) return state.contacts;
  state.contactsAt = Date.now();
  try {
    const mined = await call('list_contacts', { username: state.account });
    const seen = new Map();
    for (const c of [...(state.prefs.contacts_imported || []).map(c => ({ ...c, score: 3 })), ...mined]) {
      const k = c.email.toLowerCase();
      const old = seen.get(k);
      if (!old) seen.set(k, { ...c });
      else { old.score += c.score; if (!old.name && c.name) old.name = c.name; }
    }
    state.contacts = [...seen.values()].sort((a, b) => b.score - a.score);
    $('contactsInfo').textContent = `${state.contacts.length.toLocaleString()} contacts available for suggestions (${(state.prefs.contacts_imported || []).length.toLocaleString()} imported). Addresses are collected from the mail headers you have synced; you can also import a Google Contacts CSV or a vCard file.`;
  } catch (e) { console.error(e); }
  return state.contacts;
}

let acBox = null, acItems = [], acIndex = 0, acInput = null;
function hideAc() { acBox?.remove(); acBox = null; acItems = []; }
function acToken(input) {
  const v = input.value, pos = input.selectionStart ?? v.length;
  const start = Math.max(v.lastIndexOf(',', pos - 1), v.lastIndexOf(';', pos - 1)) + 1;
  return { start, end: pos, text: v.slice(start, pos).trim() };
}
function showAc(input) {
  const t = acToken(input).text.toLowerCase();
  if (t.length < 1) return hideAc();
  const have = new Set(input.value.toLowerCase().match(/[^\s<>,;"]+@[^\s<>,;"]+/g) || []);
  acItems = state.contacts.filter(c => !have.has(c.email.toLowerCase()) && (c.email.toLowerCase().includes(t) || (c.name || '').toLowerCase().split(/\s+/).some(w => w.startsWith(t)) || (c.name || '').toLowerCase().startsWith(t))).slice(0, 8);
  if (!acItems.length) return hideAc();
  acInput = input; acIndex = 0;
  if (!acBox) { acBox = document.createElement('div'); acBox.className = 'acbox'; document.body.append(acBox); }
  const r = input.getBoundingClientRect();
  Object.assign(acBox.style, { left: r.left + 'px', top: r.bottom + 4 + 'px' });
  acBox.innerHTML = acItems.map((c, i) => `<div data-i="${i}" class="${i === 0 ? 'on' : ''}"><b>${esc(c.name || c.email)}</b>${c.name ? `<span>${esc(c.email)}</span>` : ''}</div>`).join('');
  acBox.onmousedown = e => { const d = e.target.closest('[data-i]'); if (d) { e.preventDefault(); pickAc(+d.dataset.i); } };
}
function pickAc(i) {
  const c = acItems[i];
  if (!c || !acInput) return;
  const { start, end } = acToken(acInput);
  const v = acInput.value;
  const label = c.name && !/[",<>;]/.test(c.name) ? `${c.name} <${c.email}>` : c.email;
  const before = v.slice(0, start).replace(/\s+$/, '');
  acInput.value = (before ? before + ' ' : '') + label + ', ' + v.slice(end).replace(/^\s*[,;]?\s*/, '');
  acInput.focus();
  hideAc();
}
for (const id of ['toField', 'ccField', 'bccField']) {
  const input = $(id);
  input.addEventListener('input', () => showAc(input));
  input.addEventListener('blur', () => setTimeout(hideAc, 120));
  input.addEventListener('keydown', e => {
    if (!acBox) return;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      acIndex = (acIndex + (e.key === 'ArrowDown' ? 1 : acItems.length - 1)) % acItems.length;
      [...acBox.children].forEach((n, i) => n.classList.toggle('on', i === acIndex));
    } else if (e.key === 'Enter' || e.key === 'Tab') { e.preventDefault(); pickAc(acIndex); }
    else if (e.key === 'Escape') { e.stopPropagation(); hideAc(); }
  });
}

function parseCsv(text) {
  const rows = []; let row = [], cur = '', q = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (q) { if (ch === '"') { if (text[i + 1] === '"') { cur += '"'; i++; } else q = false; } else cur += ch; }
    else if (ch === '"') q = true;
    else if (ch === ',') { row.push(cur); cur = ''; }
    else if (ch === '\n' || ch === '\r') { if (ch === '\r' && text[i + 1] === '\n') i++; row.push(cur); cur = ''; if (row.some(x => x)) rows.push(row); row = []; }
    else cur += ch;
  }
  row.push(cur); if (row.some(x => x)) rows.push(row);
  return rows;
}
function parseContactsFile(name, text) {
  const out = [];
  const add = (n, e) => { e = String(e || '').trim(); if (/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(e)) out.push({ name: String(n || '').trim(), email: e.toLowerCase() }); };
  if (/\.vcf$/i.test(name) || /^BEGIN:VCARD/im.test(text)) {
    for (const card of text.split(/BEGIN:VCARD/i).slice(1)) {
      const fn = /^FN[^:]*:(.*)$/im.exec(card)?.[1] || '';
      for (const m of card.matchAll(/^(?:item\d+\.)?EMAIL[^:]*:(.*)$/gim)) add(fn, m[1]);
    }
  } else {
    const rows = parseCsv(text.replace(/^\uFEFF/, ''));
    const head = (rows.shift() || []).map(h => h.trim().toLowerCase());
    const col = re => head.map((h, i) => (re.test(h) ? i : -1)).filter(i => i >= 0);
    const nameCols = col(/^(name|display name|full name)$/);
    const parts = ['first name', 'middle name', 'last name'].map(k => head.indexOf(k)).filter(i => i >= 0);
    const mailCols = col(/^(e-?mail( \d+)? - value|e-?mail( address)?|e-?mail \d+ address|primary email)$/);
    for (const r of rows) {
      const nm = (nameCols.length && r[nameCols[0]]) || parts.map(i => r[i]).filter(Boolean).join(' ');
      for (const i of mailCols) for (const e of String(r[i] || '').split(':::')) add(nm, e);
    }
  }
  return out;
}
$('importContacts').onclick = () => $('contactsFile').click();
$('contactsFile').onchange = async e => {
  const f = e.target.files[0];
  e.target.value = '';
  if (!f) return;
  try {
    const found = parseContactsFile(f.name, await f.text());
    if (!found.length) return toast('No email addresses were found in that file.', { error: true });
    const map = new Map((state.prefs.contacts_imported || []).map(c => [c.email, c]));
    for (const c of found) map.set(c.email, c);
    state.prefs.contacts_imported = [...map.values()].slice(0, 5000);
    await savePrefs();
    await loadContacts(true);
    toast(`Imported ${found.length.toLocaleString()} contacts.`);
  } catch (err) { toast('Could not read that file: ' + String(err?.message || err), { error: true }); }
};
$('refreshContacts').onclick = async () => { await loadContacts(true); toast(`${state.contacts.length.toLocaleString()} contacts available.`); };

// ====================================================================== search
async function runSearch(fresh = true) {
  const q = $('search').value.trim();
  $('searchClear').hidden = !q;
  state.page = 1;
  state.selected.clear();
  state.query = q;
  if (!state.account) return accountModal();
  setView('mail');
  renderFolders();
  await loadList(fresh);
}
$('searchForm').addEventListener('submit', e => { e.preventDefault(); runSearch(true); });
$('search').addEventListener('input', () => { $('searchClear').hidden = !$('search').value; });
$('searchClear').onclick = () => { $('search').value = ''; $('search').focus(); if (state.query) runSearch(); else $('searchClear').hidden = true; };

// ====================================================================== account
function accountModal() {
  if (!isDesktop) return openModal('Desktop app required', '<p>Gmail connectivity is available in the WorkDesk Chrome desktop app.</p>', 'OK');
  openModal('Connect Gmail',
    `<p class="sub">Use your Gmail address and a Google App Password (not your normal password). 2-Step Verification must be on.</p>
     <label>Email address<input name="username" type="email" required placeholder="you@gmail.com" value="${esc(state.account)}"></label>
     <label>Google App Password<input name="password" type="password" required autocomplete="new-password" placeholder="16-character App Password"></label>
     <p class="sub">The password is stored in the operating-system keyring, never in preferences.</p>`,
    'Connect',
    async d => {
      const u = d.username.trim().toLowerCase();
      await call('save_credentials', { username: u, appPassword: d.password });
      const switched = u !== state.account;
      state.account = u;
      await savePrefs();
      await call('test_connection', { username: u });
      if (switched) await call('clear_cache', { username: u }).catch(() => {});
      connect();
    });
}
$('accountBtn').onclick = accountModal;
$('connectBtn').onclick = accountModal;
$('disconnectBtn').onclick = async () => {
  if (!state.account || !(await confirmBox('Remove the saved App Password and the local header cache?', 'Forget account', true))) return;
  await call('delete_credentials', { username: state.account }).catch(() => {});
  state.account = '';
  state.items = []; state.folders = []; state.folder = 'INBOX'; state.query = ''; state.threads = []; state.thread = null; state.composes = []; state.cur = null; state.contacts = []; state.contactsAt = 0;
  await savePrefs();
  renderFolders(); renderList(); setSync('off', 'Not connected');
  $('settingsEmail').textContent = 'Not connected'; $('avatar').textContent = '?';
};
$('rebuildCache').onclick = async () => {
  if (!state.account) return;
  await call('clear_cache', { username: state.account });
  state.items = []; renderList();
  setSync('busy', 'Rebuilding cache…');
  call('start_sync', { username: state.account, priority: state.folder });
  toast('Header cache cleared — downloading again.');
};

async function connect() {
  if (!state.account) { renderFolders(); renderList(); setSync('off', 'Not connected'); return; }
  $('settingsEmail').textContent = state.account;
  $('avatar').textContent = initial(state.account);
  fillIdentities();
  loadSignatureEditor();
  setSync('busy', 'Connecting…');
  await reloadFolders();
  if (!state.folders.length) return;
  if (!state.folders.some(f => f.name === state.folder)) state.folder = state.folders.find(f => f.role === 'inbox')?.name || 'INBOX';
  renderFolders();
  loadList();
  state.syncing = true;
  call('start_sync', { username: state.account, priority: state.folder })
    .then(started => { if (!started) return; setSync('busy', 'Synchronising headers…'); })
    .catch(e => { state.syncing = false; setSync('err', 'Sync failed'); toast(String(e?.message || e), { error: true }); });
}

// ===================================================================== settings
function loadSignatureEditor() {
  const sel = $('signatureIdentity');
  if (!sel.value) sel.value = state.account;
  signatureEditor.setHTML(state.signatures[sel.value] || '');
}
function saveCurrentSignature() {
  const id = $('signatureIdentity').value;
  if (id) state.signatures[id] = signatureEditor.getHTML();
}
$('signatureIdentity').onchange = () => { loadSignatureEditor(); };

$('saveSettings').onclick = async () => {
  saveCurrentSignature();
  state.aliases = $('aliasesText').value.split(/[\n,;]/).map(x => x.trim().toLowerCase()).filter(x => x && x.includes('@') && x !== state.account);
  state.pageSize = Number($('pageSize').value);
  state.grouped = $('conversationSetting').checked;
  applyTheme();
  try { await savePrefs(); scheduleMailCheck(); applyComposeDefaults(); toast('Preferences saved.'); } catch (e) { toast('Could not save preferences: ' + String(e), { error: true }); }
  fillIdentities();
  state.page = 1;
  syncConvButton();
  loadList();
};
$('themeSelect').onchange = applyTheme;
$('settingsBtn').onclick = () => { setView('settings'); loadSignatureEditor(); };
$('backSettings').onclick = () => setView('mail');

// ================================================================= window chrome
document.querySelectorAll('[data-win]').forEach(b => b.onclick = async () => {
  if (!isDesktop) return;
  const w = getCurrentWindow();
  try {
    if (b.dataset.win === 'minimize') await w.minimize();
    else if (b.dataset.win === 'maximize') await (await w.isMaximized() ? w.unmaximize() : w.maximize());
    else await w.close();
  } catch (e) { console.error(e); toast('Window control failed: ' + String(e?.message || e), { error: true, timeout: 8000 }); }
});

function setFoldersCollapsed(on) {
  $('app').classList.toggle('folder-collapsed', on);
  store.set('maildesk-folder-collapsed', on ? '1' : '0');
  setIcon($('collapseFolders'), on ? 'chevronRight' : 'chevronLeft');
  $('railToggle').setAttribute('aria-expanded', String(!on));
  $('railToggle').dataset.tip = on ? 'Show the folder pane' : 'Hide the folder pane';
  if (state.view === 'web') relayoutWeb();
}
$('collapseFolders').onclick = () => setFoldersCollapsed(!$('app').classList.contains('folder-collapsed'));
$('railCompose').onclick = () => startCompose();
$('railToggle').onclick = () => setFoldersCollapsed(!$('app').classList.contains('folder-collapsed'));
$('folders').addEventListener('click', e => { const n = e.target.closest('[data-folder]'); if (n) selectFolder(n.dataset.folder); });
$('railFolders').addEventListener('click', e => { const n = e.target.closest('[data-folder]'); if (n) selectFolder(n.dataset.folder); });
$('folders').addEventListener('contextmenu', async e => {
  const n = e.target.closest('[data-folder]');
  if (!n || roleOf(n.dataset.folder) || n.dataset.folder === 'INBOX') return;
  e.preventDefault();
  if (await confirmBox(`Delete the label “${folderDisplay(n.dataset.folder)}”? Messages stay in All Mail.`, 'Delete label', true)) {
    try { await call('delete_folder', { username: state.account, folder: n.dataset.folder }); if (state.folder === n.dataset.folder) state.folder = 'INBOX'; await reloadFolders(); loadList(); }
    catch (err) { toast('Could not delete the label: ' + String(err?.message || err), { error: true }); }
  }
});
$('addFolder').onclick = async () => {
  if (!state.account) return accountModal();
  const v = await askForm('New label', '<label>Name<input name="name" required maxlength="120"></label>', 'Create');
  if (!v) return;
  try { await call('create_folder', { username: state.account, folder: v.name }); await reloadFolders(); } catch (e) { toast('Could not create the label: ' + String(e?.message || e), { error: true }); }
};
$('composeBtn').onclick = () => startCompose();
$('mailSpace').onclick = () => setView('mail');
$('addBookmark').onclick = () => bookmarkModal();
$('bookmarkManage').onclick = manageBookmarks;
$('modalFields').addEventListener('click', async e => {
  const b = e.target.closest('[data-edit],[data-remove]');
  if (!b) return;
  if (b.dataset.remove !== undefined) { state.bookmarks.splice(+b.dataset.remove, 1); await savePrefs(); renderPins(); manageBookmarks(); }
  else { const i = +b.dataset.edit; closeModal(); bookmarkModal(i); }
});
$('refresh').onclick = async () => {
  if (!state.account) return accountModal();
  $('refresh').classList.add('spinning');
  $('refresh').disabled = true;
  try { await syncCurrent(true); } finally { $('refresh').classList.remove('spinning'); $('refresh').disabled = false; }
};

// website bar
$('webBack').onclick = () => webAction('back');
$('webForward').onclick = () => webAction('forward');
$('webReload').onclick = () => webAction('reload');
$('webBackMail').onclick = () => setView('mail');
$('webExternal').onclick = () => { const t = state.webTabs.find(x => x.id === state.activeWeb); if (t) openExternal(t.url); };
$('webForm').addEventListener('submit', e => {
  e.preventDefault();
  let u = $('webUrl').value.trim();
  if (!u) return;
  if (!/^https?:\/\//i.test(u)) u = 'https://' + u;
  webAction('load', u);
});
function relayoutWeb() {
  const tab = state.webTabs.find(t => t.id === state.activeWeb);
  if (isElectron && state.view === 'web' && tab?.opened && !tab.embed && !document.querySelector('.modalback.show')) call('site_layout', viewportRect()).catch(() => {});
}
new ResizeObserver(() => relayoutWeb()).observe($('webViewport'));
window.addEventListener('resize', relayoutWeb);

// right-click the empty workspace for quick settings
document.addEventListener('keydown', e => {
  if (e.key === 'Escape' && state.view === 'thread' && !document.querySelector('.modalback.show')) leaveThread();
});

// Back on the WorkDesk Chrome tab after looking at a website in its own Chrome tab: leave the placeholder "website" view.
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState !== 'visible' || state.view !== 'web') return;
  const tab = state.webTabs.find(t => t.id === state.activeWeb);
  if (tab && !tab.embed) setView(state.prevView && state.prevView !== 'web' ? state.prevView : 'mail');
});

// ==================================================================== start up
const notepad = createNotepad({ call });
$('notesSpace').onclick = () => setView('notes');

// ---- resizable folder pane
const SIDE_MIN = 180, SIDE_MAX = 520, SIDE_DEFAULT = 236;
function setSideWidth(px, persist = true) {
  const w = Math.round(Math.min(SIDE_MAX, Math.max(SIDE_MIN, px)));
  $('app').style.setProperty('--side-w', w + 'px');
  if (persist) store.set('workdesk-folder-width', String(w));
  return w;
}
(() => {
  const saved = Number(store.get('workdesk-folder-width'));
  if (saved) setSideWidth(saved, false);
  const bar = $('sideResize');
  bar.addEventListener('pointerdown', e => {
    e.preventDefault();
    bar.setPointerCapture(e.pointerId);
    bar.classList.add('drag');
    document.body.classList.add('resizing');
    const left = $('app').getBoundingClientRect().left + 56;
    const move = ev => setSideWidth(ev.clientX - left, false);
    const up = ev => {
      bar.removeEventListener('pointermove', move);
      bar.removeEventListener('pointerup', up);
      bar.classList.remove('drag');
      document.body.classList.remove('resizing');
      setSideWidth(ev.clientX - left);
      if (state.view === 'web') relayoutWeb();
    };
    bar.addEventListener('pointermove', move);
    bar.addEventListener('pointerup', up);
  });
  bar.addEventListener('dblclick', () => { setSideWidth(SIDE_DEFAULT); if (state.view === 'web') relayoutWeb(); });
  bar.tabIndex = 0;
  bar.addEventListener('keydown', e => {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    e.preventDefault();
    const cur = $('sidebar').getBoundingClientRect().width;
    setSideWidth(cur + (e.key === 'ArrowRight' ? 16 : -16));
  });
})();

async function start() {
  hydrateIcons(document);
  composeEditor = createRichEditor({
    toolbar: $('composeToolbar'), editor: $('editor'), form: askForm, onFiles: files => addFiles(files),
    extra: [{ icon: 'signature', title: 'Insert signature', run: () => { const s = signatureHtml($('fromIdentity').value || state.account); if (!s) toast('This sender has no signature yet — add one in Settings.'); else { $('editor').querySelector('[data-maildesk-signature]')?.remove(); composeEditor.insertHTML(s); } } }],
  });
  notepad.init();
  renderPalette();
  signatureEditor = createRichEditor({ toolbar: $('signatureToolbar'), editor: $('signatureEditor'), form: askForm, onChange: saveCurrentSignature });
  setFoldersCollapsed(store.get('maildesk-folder-collapsed') === '1');
  renderTabs();
  renderPins();
  renderFolders();
  renderList();
  if (!isDesktop) {
    setSync('off', 'Desktop runtime required');
    $('mailRows').innerHTML = '<tr><td class="empty" colspan="6">Run WorkDesk Chrome to connect to Gmail.</td></tr>';
    await loadPrefs();
    return;
  }
  await listen('sync', e => onSync(e.payload));
  await listen('scheduled', e => {
    const p = e.payload;
    if (p.ok) toast(`Scheduled message “${p.subject || '(no subject)'}” was sent.`);
    else toast(`Scheduled message “${p.subject || '(no subject)'}” failed: ${p.error}`, { error: true, timeout: 0, action: 'View', onAction: scheduledModal });
  });
  await listen('site-state', e => onSiteState(e.payload));
  await listen('goto', e => onGoto(e.payload));
  await listen('site-new', e => onSiteNew(e.payload));
  await listen('open-url', e => { if (e.payload?.url) openWeb({ url: e.payload.url, title: '' }); });
  await listen('site-download', e => { const p = e.payload || {}; toast(p.ok ? 'Downloaded ' + p.name + ' to your Downloads folder' : 'Download failed: ' + (p.name || ''), { error: !p.ok }); });
  await listen('site-closed', e => { const t = state.webTabs.find(x => labelOf(x.id) === e.payload?.label); if (t) { t.opened = false; closeWeb(t.id); } });
  (async () => {
    // Chrome opens this page before the backend has finished attaching to it, so look again for a while before complaining.
    for (let i = 0; i < 40; i++) {
      let info = null;
      try { info = await call('app_info'); } catch { /* retry */ }
      if (info?.chrome) { document.body.classList.toggle('native-window', !info.kiosk); return; }
      if (info?.failed) { toast(info.error, { error: true, timeout: 0 }); return; }
      await new Promise(r => setTimeout(r, 1000));
    }
  })();
  await loadPrefs();
  if (state.account) connect(); else { setSync('off', 'Not connected'); renderList(); }
}
start().catch(e => { console.error(e); toast('Start-up problem: ' + String(e?.message || e), { error: true, timeout: 0 }); });
