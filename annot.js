// Annotation shapes for the PDF viewer. Everything is stored in PDF points (1/72 inch, top-left origin of the
// displayed page), so it does not depend on the zoom level. `k` below is "pixels per point".
import { PDFDocument, degrees } from 'pdf-lib';

export const FONT = '"Helvetica Neue", Helvetica, Arial, sans-serif';
let measureCtx = null;
const mctx = () => (measureCtx ||= document.createElement('canvas').getContext('2d'));

export function textSize(s) {
  const c = mctx();
  c.font = `${s.size}px ${FONT}`;
  const lines = String(s.text || '').split('\n');
  const w = Math.max(6, ...lines.map(l => c.measureText(l).width));
  return { w, h: lines.length * s.size * 1.25 };
}

export function bounds(s) {
  if (s.type === 'ink') {
    const xs = s.pts.map(p => p[0]), ys = s.pts.map(p => p[1]);
    const pad = s.width / 2 + 2;
    return { x: Math.min(...xs) - pad, y: Math.min(...ys) - pad, w: Math.max(...xs) - Math.min(...xs) + 2 * pad, h: Math.max(...ys) - Math.min(...ys) + 2 * pad };
  }
  if (s.type === 'text') { const t = textSize(s); return { x: s.x, y: s.y, w: t.w, h: t.h }; }
  return { x: s.x, y: s.y, w: s.w, h: s.h };   // hl, sig
}

function distToSeg(px, py, a, b) {
  const dx = b[0] - a[0], dy = b[1] - a[1];
  const t = dx || dy ? Math.max(0, Math.min(1, ((px - a[0]) * dx + (py - a[1]) * dy) / (dx * dx + dy * dy))) : 0;
  return Math.hypot(px - (a[0] + t * dx), py - (a[1] + t * dy));
}

/** Is the point (in points) on the shape? `tol` is a tolerance in points. */
export function hit(s, x, y, tol = 4) {
  if (s.type === 'ink') {
    for (let i = 1; i < s.pts.length; i++) if (distToSeg(x, y, s.pts[i - 1], s.pts[i]) <= s.width / 2 + tol) return true;
    return s.pts.length === 1 && Math.hypot(x - s.pts[0][0], y - s.pts[0][1]) <= s.width / 2 + tol;
  }
  const b = bounds(s);
  return x >= b.x - tol && x <= b.x + b.w + tol && y >= b.y - tol && y <= b.y + b.h + tol;
}

export function move(s, dx, dy) {
  if (s.type === 'ink') s.pts = s.pts.map(p => [p[0] + dx, p[1] + dy]);
  else { s.x += dx; s.y += dy; }
}

/** Resize from the bottom-right handle to (x, y). */
export function resizeTo(s, x, y) {
  if (s.type === 'sig') { const w = Math.max(20, x - s.x); s.w = w; s.h = w * s.ratio; }
  else if (s.type === 'hl') { s.w = Math.max(4, x - s.x); s.h = Math.max(4, y - s.y); }
  else if (s.type === 'text') { s.size = Math.max(6, Math.min(96, (y - s.y) / (String(s.text || '').split('\n').length * 1.25))); }
}

/** Draw `shapes` on `ctx`. `images` maps signature id -> loaded Image. */
export function drawShapes(ctx, shapes, k, images, selected = null) {
  for (const s of shapes) {
    if (s._edit) continue;
    ctx.save();
    if (s.type === 'ink') {
      ctx.strokeStyle = s.color; ctx.lineWidth = s.width * k; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
      ctx.beginPath();
      const p = s.pts;
      ctx.moveTo(p[0][0] * k, p[0][1] * k);
      if (p.length === 1) ctx.lineTo(p[0][0] * k + 0.01, p[0][1] * k);
      for (let i = 1; i < p.length - 1; i++) ctx.quadraticCurveTo(p[i][0] * k, p[i][1] * k, ((p[i][0] + p[i + 1][0]) / 2) * k, ((p[i][1] + p[i + 1][1]) / 2) * k);
      if (p.length > 1) ctx.lineTo(p[p.length - 1][0] * k, p[p.length - 1][1] * k);
      ctx.stroke();
    } else if (s.type === 'hl') {
      ctx.globalAlpha = 0.4; ctx.fillStyle = s.color; ctx.fillRect(s.x * k, s.y * k, s.w * k, s.h * k);
    } else if (s.type === 'text') {
      ctx.fillStyle = s.color; ctx.font = `${s.size * k}px ${FONT}`; ctx.textBaseline = 'top';
      String(s.text || '').split('\n').forEach((l, i) => ctx.fillText(l, s.x * k, (s.y + i * s.size * 1.25) * k));
    } else if (s.type === 'sig') {
      const im = images[s.id];
      if (im && im.complete) ctx.drawImage(im, s.x * k, s.y * k, s.w * k, s.h * k);
    }
    ctx.restore();
    if (selected === s) {
      const b = bounds(s);
      ctx.save();
      ctx.strokeStyle = '#2f6fed'; ctx.lineWidth = 1.2; ctx.setLineDash([5, 3]);
      ctx.strokeRect(b.x * k - 2, b.y * k - 2, b.w * k + 4, b.h * k + 4);
      if (s.type !== 'ink') { ctx.setLineDash([]); ctx.fillStyle = '#fff'; ctx.fillRect((b.x + b.w) * k - 1, (b.y + b.h) * k - 1, 10, 10); ctx.strokeRect((b.x + b.w) * k - 1, (b.y + b.h) * k - 1, 10, 10); }
      ctx.restore();
    }
  }
}

/** Is (x, y) on the resize handle of `s`? (the handle is drawn about 10 px wide at the bottom-right corner) */
export function onHandle(s, x, y, k) {
  if (!s || s.type === 'ink') return false;
  const b = bounds(s);
  return Math.abs(x - (b.x + b.w + 4 / k)) < 9 / k && Math.abs(y - (b.y + b.h + 4 / k)) < 9 / k;
}

export function loadImage(src) {
  return new Promise(res => { const im = new Image(); im.onload = () => res(im); im.onerror = () => res(im); im.src = src; });
}

const dataUrlToBytes = u => { const b = atob(u.split(',')[1]); const a = new Uint8Array(b.length); for (let i = 0; i < b.length; i++) a[i] = b.charCodeAt(i); return a; };

/** Paint every page's annotations onto the original PDF and return the new file's bytes. */
export async function annotatedPdf(originalBytes, pdfjsDoc, ann, images) {
  const doc = await PDFDocument.load(originalBytes, { ignoreEncryption: true });
  const SCALE = 3;
  for (const key of Object.keys(ann)) {
    const shapes = ann[key];
    const n = Number(key);
    if (!shapes || !shapes.length || n >= doc.getPageCount()) continue;
    const page = await pdfjsDoc.getPage(n + 1);
    const vp = page.getViewport({ scale: 1 });
    const canvas = document.createElement('canvas');
    canvas.width = Math.ceil(vp.width * SCALE); canvas.height = Math.ceil(vp.height * SCALE);
    drawShapes(canvas.getContext('2d'), shapes, SCALE, images);
    const png = await doc.embedPng(dataUrlToBytes(canvas.toDataURL('image/png')));
    // where the bottom-left and bottom-right corners of the displayed page lie in the PDF's own coordinates
    const bl = vp.convertToPdfPoint(0, vp.height), br = vp.convertToPdfPoint(vp.width, vp.height);
    const angle = Math.atan2(br[1] - bl[1], br[0] - bl[0]) * 180 / Math.PI;
    doc.getPage(n).drawImage(png, { x: bl[0], y: bl[1], width: vp.width, height: vp.height, rotate: degrees(angle) });
  }
  return doc.save();
}

/** Remove a scanned signature's paper background and crop it. Returns a PNG data URL. */
export async function prepareSignature(file, removeBackground = true) {
  const url = URL.createObjectURL(file);
  const im = await loadImage(url);
  const maxW = 700;
  const sc = Math.min(1, maxW / (im.naturalWidth || maxW));
  const w = Math.max(1, Math.round(im.naturalWidth * sc)), h = Math.max(1, Math.round(im.naturalHeight * sc));
  const c = document.createElement('canvas'); c.width = w; c.height = h;
  const x = c.getContext('2d', { willReadFrequently: true });
  x.drawImage(im, 0, 0, w, h);
  URL.revokeObjectURL(url);
  const data = x.getImageData(0, 0, w, h);
  const px = data.data;
  let x0 = w, y0 = h, x1 = 0, y1 = 0;
  for (let i = 0; i < px.length; i += 4) {
    if (removeBackground) {
      const L = 0.299 * px[i] + 0.587 * px[i + 1] + 0.114 * px[i + 2];
      const a = L >= 215 ? 0 : L <= 130 ? 255 : Math.round(((215 - L) / 85) * 255);
      px[i + 3] = Math.min(px[i + 3], a);
    }
    if (px[i + 3] > 24) { const p = i / 4, X = p % w, Y = (p / w) | 0; if (X < x0) x0 = X; if (X > x1) x1 = X; if (Y < y0) y0 = Y; if (Y > y1) y1 = Y; }
  }
  if (x1 <= x0 || y1 <= y0) { x0 = 0; y0 = 0; x1 = w - 1; y1 = h - 1; }
  x.putImageData(data, 0, 0);
  const pad = 4;
  x0 = Math.max(0, x0 - pad); y0 = Math.max(0, y0 - pad); x1 = Math.min(w - 1, x1 + pad); y1 = Math.min(h - 1, y1 + pad);
  const out = document.createElement('canvas'); out.width = x1 - x0 + 1; out.height = y1 - y0 + 1;
  out.getContext('2d').drawImage(c, x0, y0, out.width, out.height, 0, 0, out.width, out.height);
  return { dataUrl: out.toDataURL('image/png'), ratio: out.height / out.width };
}
