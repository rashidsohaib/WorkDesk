// Small shared helpers: DOM lookup, escaping, formatting, toasts and the modal dialog.

export const $ = id => document.getElementById(id);

export const esc = s =>
  String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);

export function fmtSize(n) {
  n = Number(n) || 0;
  if (n >= 1048576) return (n / 1048576).toFixed(1) + ' MB';
  if (n >= 1024) return Math.round(n / 1024) + ' KB';
  return n + ' B';
}

/** Today → time, this year → "Oct 22", older → "22 Oct 2024". */
export function fmtDate(value, full = false) {
  const d = new Date(value);
  if (!value || Number.isNaN(d.getTime())) return String(value || '');
  if (full) {
    return new Intl.DateTimeFormat(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' }).format(d);
  }
  const now = new Date();
  if (d.toDateString() === now.toDateString()) {
    return new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }).format(d);
  }
  if (d.getFullYear() === now.getFullYear()) {
    return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(d);
  }
  return new Intl.DateTimeFormat(undefined, { year: 'numeric', month: 'short', day: 'numeric' }).format(d);
}

export const initial = s => (String(s || '?').replace(/^[^\p{L}\p{N}]+/u, '')[0] || '?').toUpperCase();

export function throttle(fn, ms) {
  let last = 0;
  let timer = null;
  return (...args) => {
    const now = Date.now();
    clearTimeout(timer);
    if (now - last >= ms) {
      last = now;
      fn(...args);
    } else {
      timer = setTimeout(() => {
        last = Date.now();
        fn(...args);
      }, ms - (now - last));
    }
  };
}

// ------------------------------------------------------------------------ toasts
export function toast(message, { error = false, action = null, onAction = null, timeout = 4500 } = {}) {
  const el = document.createElement('div');
  el.className = 'toast' + (error ? ' error' : '');
  const span = document.createElement('span');
  span.textContent = message;
  el.append(span);
  const remove = () => el.remove();
  if (action) {
    const b = document.createElement('button');
    b.type = 'button';
    b.textContent = action;
    b.onclick = () => {
      remove();
      onAction?.();
    };
    el.append(b);
  }
  $('toasts').append(el);
  if (timeout) setTimeout(remove, timeout);
  return remove;
}

// ------------------------------------------------------------------------ modal
let modalHooks = { open: () => {}, close: () => {} };
export const onModal = hooks => (modalHooks = hooks);
let closeCurrent = null;

/**
 * Show the modal dialog. `onSubmit(values)` may throw to show an error and keep the dialog open.
 * Resolves with `true` after a successful submit and `false` when cancelled.
 */
export function openModal(title, fields, submitLabel = 'Save', onSubmit = async () => {}, { hideSubmit = false, wide = false } = {}) {
  closeCurrent?.(false);
  return new Promise(resolve => {
    $('modalTitle').textContent = title;
    $('modalFields').innerHTML = fields;
    $('modalError').textContent = '';
    $('modalSubmit').textContent = submitLabel;
    $('modalSubmit').hidden = hideSubmit;
    $('cancelModal').textContent = hideSubmit ? 'Close' : 'Cancel';
    $('modalback').querySelector('.modal').style.width = wide ? 'min(860px,100%)' : '';
    $('modalback').classList.add('show');
    modalHooks.open();
    const close = ok => {
      closeCurrent = null;
      $('modalback').classList.remove('show');
      $('modalFields').innerHTML = '';
      modalHooks.close();
      resolve(ok);
    };
    closeCurrent = close;
    $('cancelModal').onclick = () => close(false);
    $('modalback').onmousedown = e => {
      if (e.target === $('modalback')) close(false);
    };
    $('modalForm').onsubmit = async e => {
      e.preventDefault();
      $('modalSubmit').disabled = true;
      try {
        await onSubmit(Object.fromEntries(new FormData($('modalForm'))));
        close(true);
      } catch (err) {
        $('modalError').textContent = String(err?.message || err || 'Unknown error');
      } finally {
        $('modalSubmit').disabled = false;
      }
    };
    setTimeout(() => $('modalFields').querySelector('input:not([type=hidden]),select,textarea')?.focus(), 0);
  });
}
export const closeModal = () => closeCurrent?.(false);

/** Ask for values. Resolves with the form values, or null when cancelled. */
export function askForm(title, fields, submitLabel = 'OK') {
  return new Promise(resolve => {
    let values = null;
    openModal(title, fields, submitLabel, async v => {
      values = v;
    }).then(() => resolve(values));
  });
}

export function confirmBox(message, okLabel = 'OK', danger = false) {
  return new Promise(resolve => {
    openModal('Please confirm', `<p style="margin:0">${esc(message)}</p>`, okLabel, async () => {}).then(resolve);
    if (danger) $('modalSubmit').classList.add('danger');
    else $('modalSubmit').classList.remove('danger');
  });
}

document.addEventListener('keydown', e => {
  if (e.key === 'Escape' && closeCurrent) closeCurrent(false);
});
