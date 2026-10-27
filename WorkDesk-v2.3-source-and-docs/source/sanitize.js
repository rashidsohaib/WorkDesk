// HTML sanitiser for received mail. Scripts, active content, forms, event attributes, unsafe CSS
// and non-HTTPS images are removed; a conservative subset of formatting is kept so messages look
// the way the sender designed them.

const ALLOWED = new Set(['P', 'BR', 'DIV', 'SPAN', 'FONT', 'CENTER', 'TABLE', 'THEAD', 'TBODY', 'TFOOT', 'TR', 'TD', 'TH', 'A', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'STRIKE', 'DEL', 'INS', 'MARK', 'UL', 'OL', 'LI', 'BLOCKQUOTE', 'PRE', 'CODE', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'HR', 'SMALL', 'SUB', 'SUP', 'IMG', 'COL', 'COLGROUP', 'CAPTION']);
const DROP = new Set(['SCRIPT', 'STYLE', 'IFRAME', 'OBJECT', 'EMBED', 'FORM', 'INPUT', 'BUTTON', 'SELECT', 'TEXTAREA', 'VIDEO', 'AUDIO', 'SVG', 'MATH', 'LINK', 'META', 'BASE', 'NOSCRIPT', 'TEMPLATE', 'CANVAS']);
const CSS_OK = new Set(['font-family', 'font-size', 'font-weight', 'font-style', 'font-variant', 'text-decoration', 'text-align', 'text-transform', 'color', 'background', 'background-color', 'line-height', 'letter-spacing', 'word-spacing', 'white-space', 'vertical-align', 'margin', 'margin-top', 'margin-right', 'margin-bottom', 'margin-left', 'padding', 'padding-top', 'padding-right', 'padding-bottom', 'padding-left', 'border', 'border-top', 'border-right', 'border-bottom', 'border-left', 'border-collapse', 'border-spacing', 'border-radius', 'border-color', 'border-style', 'border-width', 'width', 'max-width', 'height', 'max-height', 'min-width', 'min-height', 'display', 'list-style-type', 'list-style-position', 'text-indent', 'table-layout', 'float', 'clear', 'box-sizing', 'overflow-wrap', 'word-break']);
const PLAIN_ATTRS = ['align', 'valign', 'bgcolor', 'width', 'height', 'colspan', 'rowspan', 'dir', 'lang', 'role', 'cellpadding', 'cellspacing', 'border', 'color', 'face', 'size'];

export function safeStyle(value) {
  const out = [];
  for (const part of String(value || '').split(';')) {
    const at = part.indexOf(':');
    if (at < 1) continue;
    const key = part.slice(0, at).trim().toLowerCase();
    const val = part.slice(at + 1).trim();
    if (!CSS_OK.has(key) || !val || /url\s*\(|expression|javascript:|@import|behavior\s*:/i.test(val) || /[<>]/.test(val)) continue;
    out.push(`${key}:${val}`);
  }
  return out.join(';');
}

function cleanNode(node, opts) {
  const out = document.createDocumentFragment();
  for (const child of [...node.childNodes]) {
    if (child.nodeType === Node.TEXT_NODE) {
      out.append(document.createTextNode(child.nodeValue || ''));
      continue;
    }
    if (child.nodeType !== Node.ELEMENT_NODE) continue;
    const tag = child.tagName;
    if (!ALLOWED.has(tag)) {
      if (!DROP.has(tag)) out.append(cleanNode(child, opts)); // unknown wrapper: keep its content
      continue;
    }
    const el = document.createElement(tag.toLowerCase());
    const style = safeStyle(child.getAttribute('style'));
    if (style) el.setAttribute('style', style);
    for (const a of ['class', 'id']) {
      const v = child.getAttribute(a) || '';
      if (v && /^[a-zA-Z0-9_ .:-]{1,200}$/.test(v)) el.setAttribute(a, v);
    }
    for (const a of PLAIN_ATTRS) {
      const v = child.getAttribute(a) || '';
      if (v && /^[a-zA-Z0-9 _#%.,-]{1,80}$/.test(v)) el.setAttribute(a, v);
    }
    if (child.getAttribute('data-maildesk-signature') === 'yes') el.setAttribute('data-maildesk-signature', 'yes');
    if (tag === 'A') {
      const href = child.getAttribute('href') || '';
      if (/^https?:\/\//i.test(href) || /^mailto:/i.test(href)) {
        el.setAttribute('data-mail-link', href);
        el.setAttribute('title', href);
        el.style.textDecoration = 'underline';
        if (!el.style.color) el.style.color = 'var(--accent)';
      }
    }
    if (tag === 'IMG') {
      const src = child.getAttribute('src') || '';
      const okSrc = (/^https:\/\//i.test(src) && !opts.blockRemoteImages) || /^data:image\/(png|jpeg|gif|webp);base64,/i.test(src);
      if (!okSrc) {
        if (/^https:\/\//i.test(src)) opts.blocked = (opts.blocked || 0) + 1;
        continue;
      }
      el.setAttribute('src', src);
      el.setAttribute('loading', 'lazy');
      el.setAttribute('referrerpolicy', 'no-referrer');
      el.setAttribute('alt', child.getAttribute('alt') || '');
      el.style.maxWidth = '100%';
      el.style.height = el.style.height || 'auto';
      out.append(el);
      continue;
    }
    el.append(cleanNode(child, opts));
    out.append(el);
  }
  return out;
}

/** Scope the message's own <style> rules to the message container so they cannot restyle the app. */
function scopedRules(parsed) {
  let rules = '';
  for (const sheet of parsed.querySelectorAll('style')) {
    const css = sheet.textContent || '';
    for (const match of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
      const selectors = match[1].trim();
      if (!selectors || selectors.startsWith('@') || /url\s*\(|expression|javascript:|behavior\s*:/i.test(match[2])) continue;
      const declarations = safeStyle(match[2]);
      if (!declarations) continue;
      const safe = selectors
        .split(',')
        .map(s => s.trim())
        .filter(s => /^[a-zA-Z0-9 .#:_\-[\]="'*>+~]+$/.test(s) && !/:has\s*\(/i.test(s))
        .map(s => {
          s = s.replace(/\b(html|body)\b/gi, '.mail-html');
          return s.startsWith('.mail-html') ? s : `.mail-html ${s}`;
        });
      if (safe.length) rules += `${safe.join(',')}{${declarations}}`;
    }
  }
  return rules;
}

/**
 * @param {string} source untrusted HTML
 * @param {{blockRemoteImages?: boolean, noStyleSheet?: boolean}} [options]
 * @returns {{html: string, blocked: number}}
 */
export function sanitizeEmailHtml(source, options = {}) {
  const parsed = new DOMParser().parseFromString(String(source || ''), 'text/html');
  const opts = { ...options, blocked: 0 };
  const root = document.createElement('div');
  root.className = 'mail-html';
  root.append(cleanNode(parsed.body, opts));
  if (!options.noStyleSheet) {
    const rules = scopedRules(parsed);
    if (rules) {
      const style = document.createElement('style');
      style.textContent = rules;
      root.prepend(style);
    }
  }
  return { html: root.outerHTML, blocked: opts.blocked };
}

/** Replace cid: references with the inline images that came with the message. */
export function withInlineImages(html, attachments) {
  let out = String(html || '');
  for (const a of attachments || []) {
    const id = String(a.contentId || '').replace(/^<|>$/g, '');
    const type = String(a.contentType || '').toLowerCase();
    const data = String(a.dataBase64 || '');
    if (!id || !/^image\/(png|jpeg|gif|webp)$/.test(type) || !/^[A-Za-z0-9+/=\r\n]+$/.test(data)) continue;
    const escaped = id.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    out = out.replace(new RegExp('cid:' + escaped, 'gi'), `data:${type};base64,${data.replace(/\s/g, '')}`);
  }
  return out;
}

/** Sanitise pasted / inserted content for the compose editors (keeps formatting, drops everything active). */
export function sanitizeForEditor(html) {
  const parsed = new DOMParser().parseFromString(String(html || ''), 'text/html');
  const holder = document.createElement('div');
  holder.append(cleanNode(parsed.body, { noStyleSheet: true }));
  holder.querySelectorAll('a[data-mail-link]').forEach(a => {
    a.setAttribute('href', a.getAttribute('data-mail-link'));
    a.removeAttribute('data-mail-link');
    a.removeAttribute('title');
  });
  return holder.innerHTML;
}
