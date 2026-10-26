(function () {
  if (window.top !== window || window.__wdBarInit) return;
  window.__wdBarInit = 1;
  var st = { tabs: [], active: '', url: '', canBack: false, canForward: false, loading: false };
  var send = function (o) { try { window.__wdsite(JSON.stringify(o)); } catch (e) {} };
  var host, root, bar, urlIn, tabsEl, hideT, back, fwd, prog;
  function el(tag, css, text) { var e = document.createElement(tag); if (css) e.style.cssText = css; if (text) e.textContent = text; return e; }
  function btn(label, title, fn) {
    var b = el('button', 'all:unset;box-sizing:border-box;min-width:30px;height:28px;padding:0 8px;border-radius:6px;color:#e8eaed;font:600 13px system-ui,sans-serif;display:inline-flex;align-items:center;justify-content:center;cursor:pointer;white-space:nowrap', label);
    b.title = title; b.onmouseenter = function () { b.style.background = 'rgba(255,255,255,.16)'; }; b.onmouseleave = function () { b.style.background = ''; };
    b.onclick = fn; return b;
  }
  function show() { clearTimeout(hideT); bar.style.transform = 'translateY(0)'; }
  function hide(now) { clearTimeout(hideT); hideT = setTimeout(function () { if (root.activeElement !== urlIn) bar.style.transform = 'translateY(-100%)'; }, now ? 0 : 700); }
  function render() {
    if (!bar) return;
    tabsEl.textContent = '';
    st.tabs.forEach(function (t) {
      var on = t.label === st.active;
      var c = el('span', 'display:inline-flex;align-items:center;gap:4px;height:28px;padding:0 6px 0 10px;border-radius:6px;font:13px system-ui,sans-serif;color:#e8eaed;cursor:pointer;max-width:190px;' + (on ? 'background:rgba(255,255,255,.22)' : 'background:rgba(255,255,255,.07)'));
      var n = el('span', 'overflow:hidden;text-overflow:ellipsis;white-space:nowrap', (t.title || 'Website').slice(0, 28));
      var x = el('span', 'opacity:.7;padding:0 4px;font-size:15px;line-height:1', '×'); x.title = 'Close tab';
      x.onclick = function (e) { e.stopPropagation(); send({ a: 'close', label: t.label }); };
      c.onclick = function () { if (!on) send({ a: 'switch', label: t.label }); };
      c.append(n, x); tabsEl.append(c);
    });
    back.style.opacity = st.canBack ? 1 : .35; fwd.style.opacity = st.canForward ? 1 : .35;
    if (root.activeElement !== urlIn) urlIn.value = st.url || '';
    prog.style.opacity = st.loading ? 1 : 0;
  }
  function build() {
    host = document.createElement('div');
    host.style.cssText = 'all:initial;position:fixed;top:0;left:0;right:0;height:0;z-index:2147483647';
    root = host.attachShadow({ mode: 'closed' });
    var zone = el('div', 'position:fixed;top:0;left:0;right:0;height:3px;background:transparent');
    bar = el('div', 'position:fixed;top:0;left:0;right:0;height:42px;box-sizing:border-box;padding:7px 10px;display:flex;align-items:center;gap:6px;background:#202124;box-shadow:0 2px 10px rgba(0,0,0,.4);transform:translateY(-100%);transition:transform .14s ease');
    var home = btn('← WorkDesk', 'Back to WorkDesk (Ctrl+Shift+M)', function () { send({ a: 'home' }); });
    home.style.background = 'rgba(255,255,255,.14)';
    tabsEl = el('span', 'display:inline-flex;gap:4px;overflow:hidden;flex:0 1 auto');
    back = btn('‹', 'Back', function () { send({ a: 'back' }); });
    fwd = btn('›', 'Forward', function () { send({ a: 'forward' }); });
    var rel = btn('↻', 'Reload', function () { send({ a: 'reload' }); });
    urlIn = el('input', 'all:unset;box-sizing:border-box;flex:1 1 auto;min-width:80px;height:28px;padding:0 12px;border-radius:14px;background:#3c4043;color:#e8eaed;font:13px system-ui,sans-serif');
    urlIn.spellcheck = false;
    urlIn.addEventListener('keydown', function (e) { if (e.key === 'Enter') { var u = urlIn.value.trim(); if (u) { if (!/^[a-z]+:\/\//i.test(u)) u = 'https://' + u; send({ a: 'load', url: u }); urlIn.blur(); } } e.stopPropagation(); }, true);
    urlIn.addEventListener('focus', function () { urlIn.select(); show(); });
    urlIn.addEventListener('blur', function () { hide(); });
    prog = el('div', 'position:absolute;left:0;right:0;bottom:0;height:2px;background:#8ab4f8;opacity:0;transition:opacity .2s');
    bar.append(home, tabsEl, back, fwd, rel, urlIn, prog);
    zone.onmouseenter = show; bar.onmouseenter = show; bar.onmouseleave = function () { hide(); };
    root.append(zone, bar);
    document.documentElement.appendChild(host);
    render();
  }
  function whenRoot(cb) {
    if (document.documentElement) return cb();
    var mo = new MutationObserver(function () { if (document.documentElement) { mo.disconnect(); cb(); } });
    mo.observe(document, { childList: true });
  }
  window.__wdBar = { set: function (s) { st = s; if (bar) render(); else whenRoot(function () { if (!host) build(); }); } };
  window.addEventListener('keydown', function (e) { if (e.ctrlKey && e.shiftKey && (e.key === 'M' || e.key === 'm')) { e.preventDefault(); send({ a: 'home' }); } }, true);
  whenRoot(build);
  // the page may replace <html> contents; make sure the bar survives
  document.addEventListener('DOMContentLoaded', function () { if (host && !host.isConnected) document.documentElement.appendChild(host); });
  send({ a: 'hello' });
})();
