// Attachment / image viewer: PDFs are drawn with pdf.js, images with a plain <img>.
import * as pdfjs from 'pdfjs-dist/legacy/build/pdf.js';
import workerUrl from 'pdfjs-dist/legacy/build/pdf.worker.min.js?url';

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

const b64ToBytes = b64 => Uint8Array.from(atob(b64), c => c.charCodeAt(0));
let token = 0;

/** Draw document `d` ({kind:'pdf'|'image', dataBase64?, src?, scale, fit}) into `host`. Returns {scale, pages}. */
export async function renderDoc(d, scroll, host) {
  const my = ++token;
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
    if (d.fit) {
      const p1 = await pdf.getPage(1);
      d.scale = Math.min(3, avail / p1.getViewport({ scale: 1 }).width);
    }
    const frag = document.createDocumentFragment();
    const dpr = window.devicePixelRatio || 1;
    for (let n = 1; n <= pdf.numPages; n++) {
      const page = await pdf.getPage(n);
      if (my !== token) return null;
      const vp = page.getViewport({ scale: d.scale });
      const canvas = document.createElement('canvas');
      canvas.width = Math.floor(vp.width * dpr);
      canvas.height = Math.floor(vp.height * dpr);
      canvas.style.width = Math.floor(vp.width) + 'px';
      canvas.style.height = Math.floor(vp.height) + 'px';
      await page.render({ canvasContext: canvas.getContext('2d'), viewport: vp, transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : null }).promise;
      frag.append(canvas);
      if (n === 1) { host.innerHTML = ''; host.append(frag); }
      else host.append(canvas);
    }
    return { scale: d.scale, pages: pdf.numPages };
  } catch (e) {
    if (my === token) host.innerHTML = `<div class="loading">Could not display this file: ${String(e?.message || e)}</div>`;
    return null;
  }
}
