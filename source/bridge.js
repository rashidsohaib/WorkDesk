// WorkDesk Chrome bridge: the same three calls the Tauri build used (invoke / listen / window),
// carried over the local backend: POST /rpc/<command> and a Server-Sent-Events stream.
// If a test harness provides window.__TAURI_INTERNALS__ it is used instead of the network.
const T = window.__TAURI_INTERNALS__;
window.__WORKDESK__ = true;
// Inside the WorkDesk Chrome shell (Electron) the website tabs and window buttons are handled by the shell itself.
const E = window.wdElectron;
const SHELL_CMDS = new Set(['site_open', 'site_show', 'site_close', 'site_action', 'site_state', 'site_probe', 'site_layout', 'win_action', 'app_info', 'webrtc_support']);
const SHELL_EVENTS = new Set(['site-state', 'goto', 'site-new', 'site-closed', 'open-url', 'site-download']);

export async function invoke(cmd, args = {}) {
  if (T) return T.invoke(cmd, args);
  if (E && SHELL_CMDS.has(cmd)) return E.invoke(cmd, args ?? {});
  let r;
  try {
    r = await fetch('/rpc/' + cmd, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-WD': '1' }, body: JSON.stringify(args ?? {}) });
  } catch (e) { throw 'The WorkDesk backend is not reachable (' + (e?.message || e) + ')'; }
  if (!r.ok) throw 'The WorkDesk backend refused the request (HTTP ' + r.status + ')';
  const j = await r.json();
  if (j && 'err' in j) throw j.err;
  return j ? j.ok : null;
}

let es = null;
const subs = new Map();
function stream() {
  if (es) return es;
  es = new EventSource('/events');
  return es;
}

export async function listen(event, cb) {
  if (T) {
    const id = T.transformCallback(cb);
    await T.invoke('plugin:event|listen', { event, target: { kind: 'Any' }, handler: id });
    return () => {};
  }
  if (E && SHELL_EVENTS.has(event)) return E.on(event, p => cb({ event, payload: p }));
  const handler = m => { let payload = null; try { payload = JSON.parse(m.data); } catch { /* ignore */ } cb({ event, payload }); };
  stream().addEventListener(event, handler);
  return () => es && es.removeEventListener(event, handler);
}

// window controls (header buttons) -> Chrome window via the backend
export function getCurrentWindow() {
  const a = action => invoke('win_action', { action });
  return {
    minimize: () => a('minimize'),
    maximize: () => a('maximize'),
    unmaximize: () => a('unmaximize'),
    isMaximized: () => a('is_maximized'),
    close: async () => { try { await a('close'); } catch (e) { window.close(); throw e; } },
  };
}
