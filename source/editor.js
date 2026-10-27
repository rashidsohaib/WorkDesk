// Rich-text editor controller shared by the compose window and the signature editor.
//
// It builds the formatting toolbar for a contenteditable element and keeps the toolbar state in
// sync with the selection. Everything that goes into the editor from outside (paste, drop, HTML
// source view) passes through the sanitiser, so the editor can never run script.

import { icon } from './icons.js';
import { sanitizeForEditor } from './sanitize.js';

export const FONTS = [
  ['Arial, Helvetica, sans-serif', 'Arial'],
  ['Helvetica, Arial, sans-serif', 'Helvetica'],
  ['Verdana, Geneva, sans-serif', 'Verdana'],
  ['Tahoma, Geneva, sans-serif', 'Tahoma'],
  ['"Trebuchet MS", Helvetica, sans-serif', 'Trebuchet MS'],
  ['Calibri, Carlito, sans-serif', 'Calibri'],
  ['Georgia, serif', 'Georgia'],
  ['"Times New Roman", Times, serif', 'Times New Roman'],
  ['Garamond, "EB Garamond", serif', 'Garamond'],
  ['"Courier New", Courier, monospace', 'Courier New'],
  ['"Lucida Console", Monaco, monospace', 'Lucida Console'],
  ['"Comic Sans MS", cursive', 'Comic Sans MS'],
];
export const SIZES = [8, 9, 10, 11, 12, 13, 14, 16, 18, 20, 24, 28, 32, 36, 48, 72];
const BLOCKS = [
  ['p', 'Normal'],
  ['h1', 'Heading 1'],
  ['h2', 'Heading 2'],
  ['h3', 'Heading 3'],
  ['blockquote', 'Quote'],
  ['pre', 'Preformatted'],
];
const PALETTE = [
  '#000000', '#434343', '#666666', '#999999', '#b7b7b7', '#cccccc', '#efefef', '#ffffff',
  '#980000', '#ff0000', '#ff9900', '#ffff00', '#00ff00', '#00ffff', '#4a86e8', '#9900ff',
  '#e6b8af', '#f4cccc', '#fce5cd', '#fff2cc', '#d9ead3', '#d0e0e3', '#c9daf8', '#d9d2e9',
  '#dd7e6b', '#ea9999', '#f9cb9c', '#ffe599', '#b6d7a8', '#a2c4c9', '#a4c2f4', '#b4a7d6',
  '#cc4125', '#e06666', '#f6b26b', '#ffd966', '#93c47d', '#76a5af', '#6d9eeb', '#8e7cc3',
  '#a61c00', '#cc0000', '#e69138', '#f1c232', '#6aa84f', '#45818e', '#3c78d8', '#674ea7',
  '#5b0f00', '#660000', '#783f04', '#7f6000', '#274e13', '#0c343d', '#1c4587', '#20124d',
];

const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const MAX_IMAGE = 5 * 1024 * 1024;

/**
 * @param {object} o
 * @param {HTMLElement} o.toolbar   container that receives the toolbar buttons
 * @param {HTMLElement} o.editor    the contenteditable element
 * @param {(title:string, fieldsHtml:string, submit?:string)=>Promise<object|null>} o.form  modal form helper
 * @param {()=>void} [o.onChange]
 * @param {(files:File[])=>void} [o.onFiles]  non-image files dropped on the editor (compose: attachments)
 * @param {Array<{icon:string,title:string,run:()=>void}>} [o.extra] extra toolbar buttons
 */
export function createRichEditor({ toolbar, editor, form, onChange = () => {}, onFiles, extra = [] }) {
  let saved = null;
  let pendingSize = null;
  let sourceMode = false;
  let sourceBox = null;
  const buttons = new Map(); // command -> button (for the active state)
  let blockSel, fontSel, sizeSel;

  const inEditor = node => node && editor.contains(node.nodeType === 3 ? node.parentNode : node);
  const remember = () => {
    const s = getSelection();
    if (s && s.rangeCount && inEditor(s.anchorNode)) saved = s.getRangeAt(0).cloneRange();
  };
  const restore = () => {
    if (!saved) return;
    const s = getSelection();
    s.removeAllRanges();
    s.addRange(saved);
  };
  const changed = () => {
    remember();
    onChange();
  };

  function run(cmd, value = null) {
    if (sourceMode) return;
    editor.focus();
    restore();
    document.execCommand('styleWithCSS', false, true);
    document.execCommand(cmd, false, value);
    fixSizes();
    changed();
    refresh();
  }

  // execCommand('fontSize') only knows seven abstract sizes; ask for the biggest and rewrite it to
  // the exact pixel size the user picked.
  function fixSizes() {
    if (pendingSize == null) return;
    const hits = editor.querySelectorAll('font[size="7"], span[style*="xxx-large"]');
    hits.forEach(el => {
      if (el.tagName === 'FONT') {
        const span = document.createElement('span');
        span.style.fontSize = pendingSize + 'px';
        span.innerHTML = el.innerHTML;
        el.replaceWith(span);
      } else {
        el.style.fontSize = pendingSize + 'px';
      }
    });
    // nested spans from earlier sizing would override the new size: drop their font-size
    editor.querySelectorAll('span[style*="font-size"]').forEach(el => {
      el.querySelectorAll('span[style*="font-size"]').forEach(inner => inner.style.removeProperty('font-size'));
    });
    if (hits.length) pendingSize = null;
  }

  const applySize = px => {
    pendingSize = px;
    run('fontSize', '7');
  };

  function btn(cmd, iconName, title, handler, group) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'rt-btn';
    b.title = title;
    b.setAttribute('aria-label', title);
    b.innerHTML = icon(iconName);
    b.addEventListener('mousedown', e => e.preventDefault()); // keep the selection in the editor
    b.addEventListener('click', handler);
    group.append(b);
    if (cmd) buttons.set(cmd, b);
    return b;
  }
  const simple = (cmd, iconName, title, group, value) => btn(cmd, iconName, title, () => run(cmd, value), group);
  const newGroup = () => {
    const g = document.createElement('span');
    g.className = 'rt-group';
    toolbar.append(g);
    return g;
  };
  const select = (title, options, group, onPick, cls) => {
    const s = document.createElement('select');
    s.className = 'rt-select ' + cls;
    s.title = title;
    s.setAttribute('aria-label', title);
    s.innerHTML = options.map(([v, l]) => `<option value="${esc(v)}">${esc(l)}</option>`).join('');
    s.addEventListener('change', () => {
      onPick(s.value);
      editor.focus();
    });
    group.append(s);
    return s;
  };

  // ------------------------------------------------------------------ colour popover
  let pop = null;
  const closePop = () => {
    pop?.remove();
    pop = null;
  };
  function colorButton(cmd, iconName, title, group, fallbackCmd) {
    const wrap = document.createElement('span');
    wrap.className = 'rt-colorwrap';
    let current = cmd === 'foreColor' ? '#d93025' : '#ffe599';
    const apply = c => {
      current = c || current;
      swatch.style.background = current;
      if (c === null) {
        if (cmd === 'foreColor') run('foreColor', '#000000');
        else run(document.queryCommandSupported('hiliteColor') ? 'hiliteColor' : fallbackCmd, 'transparent');
        return;
      }
      run(cmd === 'hiliteColor' && !document.queryCommandSupported('hiliteColor') ? fallbackCmd : cmd, c);
    };
    const main = btn(null, iconName, title, () => apply(current), wrap);
    const swatch = document.createElement('span');
    swatch.className = 'rt-swatch';
    swatch.style.background = current;
    main.append(swatch);
    const arrow = document.createElement('button');
    arrow.type = 'button';
    arrow.className = 'rt-btn rt-arrow';
    arrow.title = title + ' — choose colour';
    arrow.setAttribute('aria-label', arrow.title);
    arrow.innerHTML = icon('chevronDown', 'ic-sm');
    arrow.addEventListener('mousedown', e => e.preventDefault());
    arrow.addEventListener('click', e => {
      e.stopPropagation();
      if (pop) return closePop();
      pop = document.createElement('div');
      pop.className = 'rt-pop';
      pop.innerHTML =
        `<div class="rt-palette">${PALETTE.map(c => `<button type="button" class="rt-chip" data-c="${c}" style="background:${c}" title="${c}" aria-label="${c}"></button>`).join('')}</div>` +
        `<div class="rt-pop-row"><button type="button" class="rt-pop-btn" data-none="1">${cmd === 'foreColor' ? 'Automatic' : 'No highlight'}</button>` +
        `<label class="rt-pop-btn">Custom… <input type="color" value="${current.startsWith('#') ? current : '#000000'}" class="rt-custom"></label></div>`;
      // The toolbar scrolls sideways, which would clip a popover placed inside it: show it on the page instead.
      document.body.append(pop);
      const r = arrow.getBoundingClientRect();
      pop.style.position = 'fixed';
      pop.style.top = Math.min(r.bottom + 4, window.innerHeight - 250) + 'px';
      pop.style.left = Math.max(8, Math.min(r.left - 100, window.innerWidth - 252)) + 'px';
      pop.style.zIndex = '150';
      pop.addEventListener('mousedown', ev => {
        if (!ev.target.closest('input')) ev.preventDefault();
      });
      pop.addEventListener('click', ev => {
        const chip = ev.target.closest('[data-c]');
        const none = ev.target.closest('[data-none]');
        if (chip) {
          apply(chip.dataset.c);
          closePop();
        } else if (none) {
          apply(null);
          closePop();
        }
      });
      pop.querySelector('.rt-custom').addEventListener('change', ev => {
        apply(ev.target.value);
        closePop();
      });
    });
    wrap.append(arrow);
    group.append(wrap);
  }
  document.addEventListener('click', e => {
    if (pop && !e.target.closest('.rt-pop')) closePop();
  });
  window.addEventListener('resize', closePop);
  window.addEventListener('keydown', e => { if (e.key === 'Escape') closePop(); });

  // ------------------------------------------------------------------ toolbar
  let g = newGroup();
  simple('undo', 'undo', 'Undo (Ctrl+Z)', g);
  simple('redo', 'redo', 'Redo (Ctrl+Shift+Z)', g);

  g = newGroup();
  blockSel = select('Paragraph style', BLOCKS, g, v => run('formatBlock', `<${v}>`), 'rt-block');
  fontSel = select('Font', FONTS, g, v => run('fontName', v), 'rt-font');
  sizeSel = select('Font size', SIZES.map(s => [String(s), `${s}`]), g, v => applySize(Number(v)), 'rt-size');
  sizeSel.value = '14';

  g = newGroup();
  simple('bold', 'bold', 'Bold (Ctrl+B)', g);
  simple('italic', 'italic', 'Italic (Ctrl+I)', g);
  simple('underline', 'underline', 'Underline (Ctrl+U)', g);
  simple('strikeThrough', 'strike', 'Strikethrough', g);
  simple('subscript', 'subscript', 'Subscript', g);
  simple('superscript', 'superscript', 'Superscript', g);

  g = newGroup();
  colorButton('foreColor', 'textColor', 'Text colour', g);
  colorButton('hiliteColor', 'highlight', 'Highlight colour', g, 'backColor');

  g = newGroup();
  simple('justifyLeft', 'alignLeft', 'Align left', g);
  simple('justifyCenter', 'alignCenter', 'Align centre', g);
  simple('justifyRight', 'alignRight', 'Align right', g);
  simple('justifyFull', 'alignJustify', 'Justify', g);

  g = newGroup();
  simple('insertUnorderedList', 'listBullet', 'Bulleted list', g);
  simple('insertOrderedList', 'listNumber', 'Numbered list', g);
  simple('outdent', 'outdent', 'Decrease indent', g);
  simple('indent', 'indent', 'Increase indent', g);
  btn('blockquote', 'quote', 'Block quote', () => run('formatBlock', isInBlock('blockquote') ? '<p>' : '<blockquote>'), g);

  g = newGroup();
  btn(null, 'link', 'Insert or edit link (Ctrl+K)', insertLink, g);
  simple('unlink', 'unlink', 'Remove link', g);
  const fileInput = document.createElement('input');
  fileInput.type = 'file';
  fileInput.accept = 'image/png,image/jpeg,image/gif,image/webp';
  fileInput.hidden = true;
  fileInput.addEventListener('change', async () => {
    for (const f of fileInput.files) await insertImageFile(f);
    fileInput.value = '';
  });
  btn(null, 'image', 'Insert image', () => fileInput.click(), g).after(fileInput);
  btn(null, 'table', 'Insert table', insertTable, g);
  simple('insertHorizontalRule', 'hr', 'Insert horizontal line', g);

  g = newGroup();
  simple('removeFormat', 'clearFormat', 'Clear formatting', g);
  btn('source', 'source', 'Edit HTML source', toggleSource, g);
  for (const x of extra) btn(null, x.icon, x.title, x.run, g);

  // ------------------------------------------------------------------ commands
  function isInBlock(tag) {
    const s = getSelection();
    let n = s?.anchorNode;
    while (n && n !== editor) {
      if (n.nodeType === 1 && n.tagName.toLowerCase() === tag) return true;
      n = n.parentNode;
    }
    return false;
  }
  const closestAnchor = () => {
    const s = getSelection();
    let n = s?.anchorNode;
    while (n && n !== editor) {
      if (n.nodeType === 1 && n.tagName === 'A') return n;
      n = n.parentNode;
    }
    return null;
  };

  async function insertLink() {
    if (sourceMode) return;
    remember();
    const existing = closestAnchor();
    const selectedText = String(saved || '');
    const values = await form(
      existing ? 'Edit link' : 'Insert link',
      `<label>Web address<input name="url" required placeholder="https://example.com" value="${esc(existing?.getAttribute('href') || '')}"></label>` +
        (existing || selectedText ? '' : '<label>Text to display<input name="text" placeholder="Same as the address"></label>'),
      existing ? 'Update' : 'Insert'
    );
    if (!values) return editor.focus();
    let url = values.url.trim();
    if (!/^(https?:|mailto:)/i.test(url)) url = /@/.test(url) && !/\s/.test(url) ? 'mailto:' + url : 'https://' + url;
    editor.focus();
    restore();
    if (existing) {
      existing.setAttribute('href', url);
    } else if (selectedText) {
      run('createLink', url);
    } else {
      const label = (values.text || '').trim() || url.replace(/^mailto:/i, '');
      run('insertHTML', `<a href="${esc(url)}">${esc(label)}</a>`);
    }
    changed();
  }

  async function insertTable() {
    if (sourceMode) return;
    remember();
    const v = await form(
      'Insert table',
      '<label>Rows<input name="rows" type="number" min="1" max="30" value="3"></label><label>Columns<input name="cols" type="number" min="1" max="12" value="3"></label><label><span><input name="header" type="checkbox" value="1" checked style="width:auto"> First row is a header</span></label>',
      'Insert'
    );
    if (!v) return editor.focus();
    const rows = Math.max(1, Math.min(30, Number(v.rows) || 3));
    const cols = Math.max(1, Math.min(12, Number(v.cols) || 3));
    const cell = (tag, bg) => `<${tag} style="border:1px solid #b0b0b0;padding:6px 8px;min-width:48px;${bg}">&nbsp;</${tag}>`;
    let html = '<table style="border-collapse:collapse;margin:6px 0"><tbody>';
    for (let r = 0; r < rows; r++) {
      html += '<tr>';
      for (let c = 0; c < cols; c++) html += v.header && r === 0 ? cell('th', 'background:#f1f3f4;text-align:left;') : cell('td', '');
      html += '</tr>';
    }
    html += '</tbody></table><div><br></div>';
    run('insertHTML', html);
  }

  function readFile(file) {
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(String(r.result));
      r.onerror = reject;
      r.readAsDataURL(file);
    });
  }
  async function insertImageFile(file) {
    if (!/^image\/(png|jpeg|gif|webp)$/.test(file.type)) return false;
    if (file.size > MAX_IMAGE) {
      alert(`${file.name || 'The image'} is larger than 5 MB. Attach it as a file instead.`);
      return true;
    }
    const url = await readFile(file);
    run('insertHTML', `<img src="${url}" alt="${esc(file.name || '')}" style="max-width:100%;height:auto">`);
    return true;
  }

  function toggleSource() {
    if (!sourceMode) {
      sourceBox = document.createElement('textarea');
      sourceBox.className = 'rt-source';
      sourceBox.spellcheck = false;
      sourceBox.value = editor.innerHTML;
      editor.after(sourceBox);
      editor.hidden = true;
      sourceMode = true;
      toolbar.classList.add('source-on');
      buttons.get('source')?.classList.add('active');
      sourceBox.focus();
    } else {
      editor.innerHTML = sanitizeForEditor(sourceBox.value);
      sourceBox.remove();
      sourceBox = null;
      editor.hidden = false;
      sourceMode = false;
      toolbar.classList.remove('source-on');
      buttons.get('source')?.classList.remove('active');
      editor.focus();
      changed();
    }
  }

  // ------------------------------------------------------------------ state
  function refresh() {
    if (sourceMode) return;
    for (const [cmd, b] of buttons) {
      if (['source', 'unlink', 'removeFormat', 'insertHorizontalRule', 'undo', 'redo', 'indent', 'outdent'].includes(cmd)) continue;
      let on = false;
      try {
        on = cmd === 'blockquote' ? isInBlock('blockquote') : document.queryCommandState(cmd);
      } catch {
        on = false;
      }
      b.classList.toggle('active', !!on);
    }
    const s = getSelection();
    if (!s || !s.rangeCount || !inEditor(s.anchorNode)) return;
    try {
      const block = String(document.queryCommandValue('formatBlock') || 'p').toLowerCase().replace(/[<>]/g, '');
      blockSel.value = BLOCKS.some(([v]) => v === block) ? block : 'p';
    } catch { /* ignore */ }
    const node = s.anchorNode.nodeType === 3 ? s.anchorNode.parentElement : s.anchorNode;
    if (node) {
      const cs = getComputedStyle(node);
      const px = Math.round(parseFloat(cs.fontSize));
      const nearest = SIZES.reduce((a, b) => (Math.abs(b - px) < Math.abs(a - px) ? b : a), SIZES[0]);
      sizeSel.value = String(nearest);
      const first = cs.fontFamily.split(',')[0].replace(/["']/g, '').trim().toLowerCase();
      const match = FONTS.find(([stack]) => stack.split(',')[0].replace(/["']/g, '').trim().toLowerCase() === first);
      fontSel.value = match ? match[0] : FONTS[0][0];
    }
  }

  document.addEventListener('selectionchange', () => {
    const s = getSelection();
    if (s && s.rangeCount && inEditor(s.anchorNode)) {
      remember();
      refresh();
    }
  });
  editor.addEventListener('input', () => {
    fixSizes();
    onChange();
  });
  editor.addEventListener('blur', remember);

  editor.addEventListener('keydown', e => {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.shiftKey && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      insertLink();
    } else if (mod && e.shiftKey && e.key === '7') {
      e.preventDefault();
      run('insertOrderedList');
    } else if (mod && e.shiftKey && e.key === '8') {
      e.preventDefault();
      run('insertUnorderedList');
    } else if (mod && e.key === '\\') {
      e.preventDefault();
      run('removeFormat');
    } else if (e.key === 'Tab' && !mod) {
      const s = getSelection();
      let n = s?.anchorNode;
      let cell = null;
      let li = false;
      while (n && n !== editor) {
        if (n.nodeType === 1 && (n.tagName === 'TD' || n.tagName === 'TH')) cell = cell || n;
        if (n.nodeType === 1 && n.tagName === 'LI') li = true;
        n = n.parentNode;
      }
      if (li) {
        e.preventDefault();
        run(e.shiftKey ? 'outdent' : 'indent');
      } else if (cell) {
        e.preventDefault();
        const cells = [...cell.closest('table').querySelectorAll('td,th')];
        const next = cells[cells.indexOf(cell) + (e.shiftKey ? -1 : 1)];
        if (next) {
          const r = document.createRange();
          r.selectNodeContents(next);
          r.collapse(false);
          s.removeAllRanges();
          s.addRange(r);
        }
      }
    }
  });

  editor.addEventListener('paste', async e => {
    const dt = e.clipboardData;
    if (!dt) return;
    const files = [...dt.files].filter(f => f.type.startsWith('image/'));
    if (files.length) {
      e.preventDefault();
      for (const f of files) await insertImageFile(f);
      return;
    }
    const html = dt.getData('text/html');
    if (html) {
      e.preventDefault();
      document.execCommand('insertHTML', false, sanitizeForEditor(html));
      changed();
    }
  });
  editor.addEventListener('dragover', e => {
    if ([...(e.dataTransfer?.types || [])].includes('Files')) e.preventDefault();
  });
  editor.addEventListener('drop', async e => {
    const files = [...(e.dataTransfer?.files || [])];
    if (!files.length) return;
    e.preventDefault();
    const others = [];
    for (const f of files) if (!(await insertImageFile(f))) others.push(f);
    if (others.length && onFiles) onFiles(others);
  });

  // ---- image selection with corner resize handles
  const host = editor.parentElement;
  if (getComputedStyle(host).position === 'static') host.style.position = 'relative';
  const box = document.createElement('div');
  box.className = 'rt-imgsel';
  box.hidden = true;
  box.innerHTML = ['nw', 'ne', 'sw', 'se'].map(c => `<span class="rt-handle ${c}" data-c="${c}"></span>`).join('') + '<span class="rt-imgsize"></span>';
  host.append(box);
  let selImg = null;
  const placeBox = () => {
    if (!selImg || !editor.contains(selImg)) return deselect();
    const h = host.getBoundingClientRect();
    const r = selImg.getBoundingClientRect();
    Object.assign(box.style, { left: r.left - h.left + 'px', top: r.top - h.top + 'px', width: r.width + 'px', height: r.height + 'px' });
    box.querySelector('.rt-imgsize').textContent = `${Math.round(r.width)} × ${Math.round(r.height)}`;
  };
  function deselect() { selImg = null; box.hidden = true; }
  function selectImg(img) { selImg = img; box.hidden = false; placeBox(); }
  editor.addEventListener('click', e => { if (e.target.tagName === 'IMG') selectImg(e.target); else deselect(); });
  editor.addEventListener('scroll', placeBox);
  editor.addEventListener('keydown', () => { if (selImg) deselect(); });
  window.addEventListener('resize', () => selImg && placeBox());
  document.addEventListener('mousedown', e => { if (selImg && !box.contains(e.target) && e.target !== selImg) deselect(); });
  box.addEventListener('pointerdown', e => {
    const handle = e.target.closest('.rt-handle');
    if (!handle || !selImg) return;
    e.preventDefault();
    handle.setPointerCapture(e.pointerId);
    const img = selImg;
    const startX = e.clientX;
    const startW = img.getBoundingClientRect().width;
    const left = handle.dataset.c.includes('w');
    const maxW = Math.max(60, editor.clientWidth - 40);
    const move = ev => {
      const dx = (ev.clientX - startX) * (left ? -1 : 1);
      const w = Math.round(Math.min(maxW, Math.max(24, startW + dx)));
      img.style.width = w + 'px';
      img.style.height = 'auto';
      img.setAttribute('width', String(w));
      img.removeAttribute('height');
      placeBox();
    };
    const up = () => { handle.removeEventListener('pointermove', move); handle.removeEventListener('pointerup', up); changed(); };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', up);
  });

  return {
    getHTML() {
      if (sourceMode) toggleSource();
      return editor.innerHTML;
    },
    setHTML(html) {
      deselect();
      if (sourceMode) toggleSource();
      editor.innerHTML = html || '';
      saved = null;
    },
    focus: () => editor.focus(),
    run,
    insertHTML: html => run('insertHTML', html),
    refresh,
  };
}
