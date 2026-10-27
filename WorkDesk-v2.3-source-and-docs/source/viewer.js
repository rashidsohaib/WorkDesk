// Attachment viewer: images, and PDFs drawn page by page with pdf.js (only the pages near the screen are drawn,
// so a long document opens at once). PDFs can be annotated: pen, highlight, text and scanned signatures.
import * as pdfjs from 'pdfjs-dist/legacy/build/pdf.js';
import workerUrl from 'pdfjs-dist/legacy/build/pdf.worker.min.js?url';
import { drawShapes, hit, move, resizeTo, onHandle, bounds, textSize, loadImage, FONT } from './annot.js';

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

function b64ToBytes(b64) {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
let token = 0;

/** Shared tool settings (the toolbar changes these). */
export const tools = { tool: 'select', ink: '#d93025', hl: '#ffe14d', text: '#111111', width: 2.2, size: 14, pendingSig: null };
export const sigImages = {};          // signature id -> loaded Image
export async function registerSignature(id, dataUrl) { sigImages[id] = await loadImage(dataUrl); }

let hooks = { onChange() {}, onPage() {}, onSelect() {} };
let current = null;                   // { d, pages: [{el, ann, canvas, w, h}], k }
let drag = null;

const shapesOf = (d, i) => (d.ann[i] ||= []);
const snapshot = d => { (d.hist ||= []).push(JSON.stringify(d.ann)); d.redo = []; if (d.hist.length > 80) d.hist.shift(); };
export const canUndo = d => !!d?.hist?.length;
export const canRedo = d => !!d?.redo?.length;
export const hasAnnotations = d => !!d && Object.values(d.ann || {}).some(a => a.length);

function paint(i) {
  if (!current) return;
  const { d, pages, k } = current;
  const pg = pages[i];
  if (!pg) return;
  const dpr = window.devicePixelRatio || 1;
  const ctx = pg.ann.getContext('2d');
  ctx.clearRect(0, 0, pg.ann.width, pg.ann.height);
  drawShapes(ctx, d.ann[i] || [], k * dpr, sigImages, d.sel && d.sel.page === i ? d.sel.shape : null);
}
const paintAll = () => current && current.pages.forEach((_, i) => paint(i));

export function undo(d) {
  if (!canUndo(d)) return;
  (d.redo ||= []).push(JSON.stringify(d.ann));
  d.ann = JSON.parse(d.hist.pop());
  d.sel = null;
  paintAll(); hooks.onChange(d);
}
export function redo(d) {
  if (!canRedo(d)) return;
  d.hist.push(JSON.stringify(d.ann));
  d.ann = JSON.parse(d.redo.pop());
  d.sel = null;
  paintAll(); hooks.onChange(d);
}
export function deleteSelected(d) {
  if (!d?.sel) return false;
  snapshot(d);
  const arr = shapesOf(d, d.sel.page);
  arr.splice(arr.indexOf(d.sel.shape), 1);
  d.sel = null;
  paintAll(); hooks.onChange(d);
  return true;
}
export function setColorOfSelection(d, color) {
  if (!d?.sel || d.sel.shape.type === 'sig') return;
  snapshot(d);
  d.sel.shape.color = color;
  paint(d.sel.page); hooks.onChange(d);
}
export function setTool(t) {
  tools.tool = t;
  if (current) current.host.dataset.tool = t;
  if (t !== 'select' && current?.d) { current.d.sel = null; paintAll(); }
}

function editText(shape, pageIdx, isNew) {
  const { d, pages, k } = current;
  const pg = pages[pageIdx];
  const ta = document.createElement('textarea');
  ta.className = 'ann-text';
  ta.value = shape.text || '';
  ta.style.cssText = `left:${shape.x * k}px;top:${shape.y * k}px;font:${shape.size * k}px/1.25 ${FONT};color:${shape.color};min-width:${Math.max(80, 8 * shape.size) * k / 1.2}px;`;
  shape._edit = true;
  paint(pageIdx);
  pg.el.append(ta);
  const grow = () => { ta.style.height = 'auto'; ta.style.height = ta.scrollHeight + 'px'; ta.style.width = 'auto'; ta.style.width = Math.max(ta.scrollWidth + 6, 80) + 'px'; };
  ta.addEventListener('input', grow);
  let done = false;
  const finish = ok => {
    if (done) return;
    done = true;
    const text = ta.value.replace(/\s+$/, '');
    ta.remove();
    delete shape._edit;
    const arr = shapesOf(d, pageIdx);
    if (!ok || !text) {
      if (isNew) { arr.splice(arr.indexOf(shape), 1); d.hist.pop(); }
      else if (!text && ok) { snapshot(d); arr.splice(arr.indexOf(shape), 1); d.sel = null; }
    } else {
      if (!isNew && shape.text !== text) snapshot(d);
      shape.text = text;
      if (isNew) d.sel = { page: pageIdx, shape };
    }
    paint(pageIdx); hooks.onChange(d);
  };
  ta.addEventListener('blur', () => finish(true));
  ta.addEventListener('keydown', e => { e.stopPropagation(); if (e.key === 'Escape') finish(false); if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) finish(true); });
  setTimeout(() => { ta.focus(); grow(); }, 0);
}

function attach(pageIdx) {
  const { pages } = current;
  const pg = pages[pageIdx];
  const toPt = e => { const r = pg.ann.getBoundingClientRect(); return { x: (e.clientX - r.left) / current.k, y: (e.clientY - r.top) / current.k }; };
  pg.ann.addEventListener('pointerdown', e => {
    if (e.button !== 0) return;
    const { d, k } = current;
    const p = toPt(e);
    const T = tools;
    if (T.tool === 'pen') {
      snapshot(d);
      const s = { type: 'ink', color: T.ink, width: T.width, pts: [[p.x, p.y]] };
      shapesOf(d, pageIdx).push(s);
      drag = { kind: 'draw', s, page: pageIdx };
    } else if (T.tool === 'hl') {
      snapshot(d);
      const s = { type: 'hl', color: T.hl, x: p.x, y: p.y, w: 1, h: 1 };
      shapesOf(d, pageIdx).push(s);
      drag = { kind: 'hl', s, x0: p.x, y0: p.y, page: pageIdx };
    } else if (T.tool === 'text') {
      snapshot(d);
      const s = { type: 'text', color: T.text, size: T.size, x: p.x, y: p.y - T.size * 0.6, text: '' };
      shapesOf(d, pageIdx).push(s);
      editText(s, pageIdx, true);
      e.preventDefault();
      return;
    } else if (T.tool === 'sig') {
      if (!T.pendingSig) return;
      snapshot(d);
      const w = Math.min(180, pg.w * 0.5), h = w * T.pendingSig.ratio;
      const s = { type: 'sig', id: T.pendingSig.id, ratio: T.pendingSig.ratio, x: Math.max(0, Math.min(pg.w - w, p.x - w / 2)), y: Math.max(0, Math.min(pg.h - h, p.y - h / 2)), w, h };
      shapesOf(d, pageIdx).push(s);
      d.sel = { page: pageIdx, shape: s };
      T.pendingSig = null;
      setTool('select'); hooks.onSelect(d);
      paintAll(); hooks.onChange(d);
      return;
    } else {   // select / move / resize
      if (d.sel && d.sel.page === pageIdx && onHandle(d.sel.shape, p.x, p.y, k)) {
        snapshot(d);
        drag = { kind: 'resize', s: d.sel.shape, page: pageIdx };
      } else {
        const arr = shapesOf(d, pageIdx);
        let found = null;
        for (let i = arr.length - 1; i >= 0; i--) if (hit(arr[i], p.x, p.y, 4 / k * 1.5)) { found = arr[i]; break; }
        d.sel = found ? { page: pageIdx, shape: found } : null;
        if (found) { snapshot(d); drag = { kind: 'move', s: found, last: p, page: pageIdx, moved: false }; }
        paintAll(); hooks.onSelect(d);
        if (!found) return;
      }
    }
    pg.ann.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  pg.ann.addEventListener('pointermove', e => {
    if (!drag || drag.page !== pageIdx) return;
    const p = toPt(e);
    if (drag.kind === 'draw') {
      const last = drag.s.pts[drag.s.pts.length - 1];
      if (Math.hypot(p.x - last[0], p.y - last[1]) > 0.5) drag.s.pts.push([p.x, p.y]);
    } else if (drag.kind === 'hl') {
      drag.s.x = Math.min(drag.x0, p.x); drag.s.y = Math.min(drag.y0, p.y);
      drag.s.w = Math.abs(p.x - drag.x0); drag.s.h = Math.abs(p.y - drag.y0);
    } else if (drag.kind === 'move') {
      move(drag.s, p.x - drag.last.x, p.y - drag.last.y); drag.last = p; drag.moved = true;
    } else if (drag.kind === 'resize') resizeTo(drag.s, p.x, p.y);
    paint(pageIdx);
  });
  const end = e => {
    if (!drag) return;
    const dr = drag; drag = null;
    const { d } = current;
    if (dr.kind === 'hl' && (dr.s.w < 3 || dr.s.h < 3)) { const arr = shapesOf(d, dr.page); arr.splice(arr.indexOf(dr.s), 1); d.hist.pop(); }
    if (dr.kind === 'move' && !dr.moved) d.hist.pop();
    paint(dr.page); hooks.onChange(d);
  };
  pg.ann.addEventListener('pointerup', end);
  pg.ann.addEventListener('pointercancel', end);
  pg.ann.addEventListener('dblclick', e => {
    const { d, k } = current;
    const p = toPt(e);
    const s = [...shapesOf(d, pageIdx)].reverse().find(x => x.type === 'text' && hit(x, p.x, p.y, 4 / k));
    if (s) { d.sel = { page: pageIdx, shape: s }; editText(s, pageIdx, false); }
  });
}

document.addEventListener('keydown', e => {
  if (!current || !document.getElementById('docView')?.classList.contains('active')) return;
  if (/^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName)) return;
  const d = current.d;
  if ((e.key === 'Delete' || e.key === 'Backspace') && d.sel) { e.preventDefault(); deleteSelected(d); }
  else if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === 'z') { e.preventDefault(); undo(d); }
  else if ((e.ctrlKey || e.metaKey) && (e.key.toLowerCase() === 'y' || (e.shiftKey && e.key.toLowerCase() === 'z'))) { e.preventDefault(); redo(d); }
  else if (e.key === 'Escape') { if (tools.tool !== 'select') { setTool('select'); hooks.onSelect(d); } else if (d.sel) { d.sel = null; paintAll(); hooks.onSelect(d); } }
});

/** Draw document `d` ({kind:'pdf'|'image', dataBase64?, src?, scale, fit}) into `host`. Returns {scale, pages}. */
export async function renderDoc(d, scroll, host, h) {
  if (h) hooks = h;
  const my = ++token;
  const again = current && current.d === d;
  const keep = again && scroll.scrollHeight ? scroll.scrollTop / scroll.scrollHeight : 0;
  current = null;
  host.innerHTML = '<div class="loading">Loading…</div>';
  const avail = Math.max(200, scroll.clientWidth - 40);
  try {
    if (d.kind === 'image') {
      const img = new Image();
      img.alt = d.title;
      img.src = d.src;
      await img.decode().catch(() => {});
      if (my !== token) return null;
      const natural = img.naturalWidth || avail;
      if (d.fit) d.scale = Math.min(1, avail / natural);
      img.style.width = Math.round(natural * d.scale) + 'px';
      img.style.height = 'auto';
      host.innerHTML = '';
      host.append(img);
      return { scale: d.scale, pages: 1 };
    }
    if (!d.pdf) d.pdf = await pdfjs.getDocument({ data: b64ToBytes(d.dataBase64) }).promise;
    const pdf = d.pdf;
    d.ann ||= {};
    // page sizes first (cheap), so the scrollbar is right and any page can be reached at once
    const list = await Promise.all(Array.from({ length: pdf.numPages }, (_, i) => pdf.getPage(i + 1)));
    if (my !== token) return null;
    const first = list[0].getViewport({ scale: 1 });
    if (d.fit) d.scale = Math.min(3, avail / first.width);
    const k = d.scale;
    const dpr = window.devicePixelRatio || 1;
    host.innerHTML = '';
    host.dataset.tool = tools.tool;
    const pages = list.map((page, i) => {
      const vp1 = page.getViewport({ scale: 1 });
      const vp = page.getViewport({ scale: k });
      const el = document.createElement('div');
      el.className = 'pdfpage';
      el.dataset.i = String(i);
      el.style.width = Math.floor(vp.width) + 'px';
      el.style.height = Math.floor(vp.height) + 'px';
      const canvas = document.createElement('canvas');
      canvas.className = 'pg';
      const ann = document.createElement('canvas');
      ann.className = 'ann';
      ann.width = Math.floor(vp.width * dpr); ann.height = Math.floor(vp.height * dpr);
      ann.style.width = Math.floor(vp.width) + 'px'; ann.style.height = Math.floor(vp.height) + 'px';
      el.append(canvas, ann);
      host.append(el);
      return { el, canvas, ann, page, vp, w: vp1.width, h: vp1.height, done: false };
    });
    current = { d, pages, k, host, scroll };
    pages.forEach((_, i) => attach(i));
    paintAll();
    const renderPage = async i => {
      const pg = pages[i];
      if (pg.done || pg.busy) return;
      pg.busy = true;
      pg.canvas.width = Math.floor(pg.vp.width * dpr);
      pg.canvas.height = Math.floor(pg.vp.height * dpr);
      pg.canvas.style.width = Math.floor(pg.vp.width) + 'px';
      pg.canvas.style.height = Math.floor(pg.vp.height) + 'px';
      try {
        await pg.page.render({ canvasContext: pg.canvas.getContext('2d'), viewport: pg.vp, transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : null }).promise;
        pg.done = true;
      } catch { /* cancelled by a newer render */ }
      pg.busy = false;
    };
    const io = new IntersectionObserver(es => es.forEach(en => { if (en.isIntersecting && my === token) renderPage(Number(en.target.dataset.i)); }), { root: scroll, rootMargin: '900px 0px' });
    pages.forEach(p => io.observe(p.el));
    d._io?.disconnect(); d._io = io;
    const track = () => {
      const mid = scroll.getBoundingClientRect().top + scroll.clientHeight * 0.35;
      let idx = 0;
      pages.forEach((p, i) => { if (p.el.getBoundingClientRect().top <= mid) idx = i; });
      hooks.onPage(idx + 1, pages.length);
    };
    if (scroll._trk) scroll.removeEventListener('scroll', scroll._trk);
    scroll._trk = track;
    scroll.addEventListener('scroll', track, { passive: true });
    if (keep) scroll.scrollTop = keep * scroll.scrollHeight;
    track();
    hooks.onChange(d);
    return { scale: d.scale, pages: pdf.numPages };
  } catch (e) {
    if (my === token) host.innerHTML = `<div class="loading">Could not display this file: ${String(e?.message || e)}</div>`;
    return null;
  }
}

export function goToPage(n) {
  const p = current?.pages[n - 1];
  if (p) current.scroll.scrollTop = p.el.offsetTop - 8;
}

/** Every page as an image with the annotations on it, for printing. Returns blob URLs. */
export async function pagesForPrint(d, dpi = 150, progress = () => {}) {
  if (d.kind === 'image') return [d.src];
  const out = [];
  const k = dpi / 72;
  for (let i = 0; i < d.pdf.numPages; i++) {
    progress(i + 1, d.pdf.numPages);
    const page = await d.pdf.getPage(i + 1);
    const vp = page.getViewport({ scale: k });
    const c = document.createElement('canvas');
    c.width = Math.floor(vp.width); c.height = Math.floor(vp.height);
    const x = c.getContext('2d');
    x.fillStyle = '#fff'; x.fillRect(0, 0, c.width, c.height);
    await page.render({ canvasContext: x, viewport: vp }).promise;
    drawShapes(x, d.ann[i] || [], k, sigImages);
    out.push(await new Promise(res => c.toBlob(b => res(URL.createObjectURL(b)), 'image/jpeg', 0.92)));
  }
  return out;
}
