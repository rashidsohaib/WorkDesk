// WorkDesk Notepad — a built-in port of the "Rashid-Thunderbird-Notepad" extension.
// Many notes, rich text, find, backup/import (same JSON format as the extension), Markdown export.
import { $, esc, toast, openModal, confirmBox, askForm } from './util.js';
import { icon, setIcon } from './icons.js';
import { createRichEditor } from './editor.js';

const SIZES = [12, 13, 14, 15, 16, 18, 20, 22, 24];

export function createNotepad({ call, onTitle = () => {} }) {
  const data = { notes: {}, active: '', settings: { fontSize: 15, mdFolder: 'WorkDesk Notes', mdAuto: false } };
  let editorApi = null;
  let loaded = false;
  let saveTimer = null;
  let dirty = false;           // changed since last save to disk
  let mdDirty = false;         // changed since last .md export
  let bkDirty = false;         // changed since last JSON backup
  let findMarks = [], findIdx = 0;

  const el = () => $('notesEditor');

  // ------------------------------------------------------------------ persistence
  async function load() {
    if (loaded) return;
    try {
      const raw = await call('notes_load');
      if (raw && raw.trim()) {
        const j = JSON.parse(raw);
        if (j && j.notes) { data.notes = j.notes; data.active = j.active || ''; Object.assign(data.settings, j.settings || {}); }
        else if (j && typeof j === 'object') data.notes = j; // bare extension backup
      }
    } catch (e) { toast('Could not read your notes: ' + String(e?.message || e), { error: true }); }
    if (!Object.keys(data.notes).length) data.notes['Note 1'] = { content: '', updatedAt: Date.now() };
    if (!data.notes[data.active]) data.active = Object.keys(data.notes)[0];
    loaded = true;
  }

  function cleanHtml() {
    const h = editorApi.getHTML();
    return h.replace(/<mark[^>]*data-find[^>]*>([\s\S]*?)<\/mark>/g, '$1');
  }

  function pullEditor() {
    if (!loaded || !data.notes[data.active]) return;
    const html = cleanHtml();
    const n = data.notes[data.active];
    if (n.content !== html) { n.content = html; n.updatedAt = Date.now(); }
  }

  async function flush() {
    clearTimeout(saveTimer);
    if (!dirty) return;
    pullEditor();
    dirty = false;
    try {
      await call('notes_save', { data: JSON.stringify({ notes: data.notes, active: data.active, settings: data.settings }) });
      $('notesSaved').textContent = 'Saved';
      setTimeout(() => { if ($('notesSaved').textContent === 'Saved') $('notesSaved').textContent = ''; }, 1500);
    } catch (e) { dirty = true; $('notesSaved').textContent = 'Not saved'; toast('Could not save notes: ' + String(e?.message || e), { error: true }); }
  }

  function changed() {
    dirty = true; mdDirty = true; bkDirty = true;
    $('notesSaved').textContent = 'Saving…';
    clearTimeout(saveTimer);
    saveTimer = setTimeout(flush, 600);
    wordCount();
  }

  // ------------------------------------------------------------------ list & notes
  function renderList() {
    const names = Object.keys(data.notes).sort((a, b) => (data.notes[b].updatedAt || 0) - (data.notes[a].updatedAt || 0));
    $('notesList').innerHTML = names.map(n => `<div class="note-item ${n === data.active ? 'active' : ''}" data-note="${esc(n)}" tabindex="0"><span class="note-name">${esc(n)}</span><span class="note-actions"><button type="button" data-rename="${esc(n)}" title="Rename" aria-label="Rename ${esc(n)}">${icon('compose', 'ic-sm')}</button><button type="button" data-del="${esc(n)}" title="Delete" aria-label="Delete ${esc(n)}">${icon('trash', 'ic-sm')}</button></span></div>`).join('');
    $('notesName').textContent = data.active;
    onTitle(data.active);
  }

  function wordCount() {
    const t = (el().innerText || '').trim();
    $('notesWords').textContent = (t ? t.split(/\s+/).length : 0) + ' words';
  }

  function show() {
    const n = data.notes[data.active];
    editorApi.setHTML(n?.content || '');
    closeFind();
    wordCount();
    renderList();
  }

  async function switchNote(name) {
    if (name === data.active || !data.notes[name]) return;
    pullEditor();
    data.active = name;
    dirty = true;
    show();
    flush();
  }

  async function newNote() {
    pullEditor();
    let name = 'New Note', i = 2;
    while (data.notes[name]) name = 'New Note ' + i++;
    data.notes[name] = { content: '', updatedAt: Date.now() };
    data.active = name;
    dirty = true; mdDirty = true; bkDirty = true;
    show(); flush();
    el().focus();
  }

  async function renameNote(name) {
    const v = await askForm('Rename note', `<label>Note name<input name="n" value="${esc(name)}" required></label>`, 'Rename');
    const nn = v?.n?.trim();
    if (!nn || nn === name) return;
    if (data.notes[nn]) return toast('A note with that name already exists.', { error: true });
    pullEditor();
    data.notes[nn] = data.notes[name];
    delete data.notes[name];
    if (data.active === name) data.active = nn;
    dirty = true; mdDirty = true; bkDirty = true;
    renderList(); flush();
  }

  async function deleteNote(name) {
    if (!(await confirmBox(`Delete the note “${name}”? This cannot be undone.`, 'Delete', true))) return;
    delete data.notes[name];
    if (!Object.keys(data.notes).length) data.notes['Note 1'] = { content: '', updatedAt: Date.now() };
    if (data.active === name) data.active = Object.keys(data.notes)[0];
    dirty = true; mdDirty = true; bkDirty = true;
    show(); flush();
  }

  // ------------------------------------------------------------------ find
  function clearMarks() {
    el().querySelectorAll('mark[data-find]').forEach(m => m.replaceWith(document.createTextNode(m.textContent)));
    el().normalize();
    findMarks = [];
  }
  function runFind() {
    clearMarks();
    const q = $('notesFindInput').value.trim();
    if (!q) { $('notesFindCount').textContent = ''; return; }
    const re = new RegExp(q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'gi');
    const walker = document.createTreeWalker(el(), NodeFilter.SHOW_TEXT);
    const nodes = [];
    for (let n; (n = walker.nextNode());) nodes.push(n);
    for (const tn of nodes) {
      const text = tn.nodeValue;
      re.lastIndex = 0;
      if (!re.test(text)) continue;
      re.lastIndex = 0;
      const frag = document.createDocumentFragment();
      let last = 0;
      for (let m; (m = re.exec(text));) {
        frag.append(text.slice(last, m.index));
        const mk = document.createElement('mark');
        mk.dataset.find = '1';
        mk.textContent = m[0];
        frag.append(mk);
        last = re.lastIndex;
        if (m[0] === '') re.lastIndex++;
      }
      frag.append(text.slice(last));
      tn.replaceWith(frag);
    }
    findMarks = [...el().querySelectorAll('mark[data-find]')];
    findIdx = 0;
    $('notesFindCount').textContent = findMarks.length ? `1 / ${findMarks.length}` : 'No results';
    focusMark();
  }
  function focusMark() {
    findMarks.forEach(m => m.classList.remove('cur'));
    const m = findMarks[findIdx];
    if (m) { m.classList.add('cur'); m.scrollIntoView({ block: 'nearest' }); }
  }
  function stepFind(d) {
    if (!findMarks.length) return;
    findIdx = (findIdx + d + findMarks.length) % findMarks.length;
    $('notesFindCount').textContent = `${findIdx + 1} / ${findMarks.length}`;
    focusMark();
  }
  function openFind() { $('notesFind').hidden = false; $('notesFindInput').focus(); $('notesFindInput').select(); }
  function closeFind() { $('notesFind').hidden = true; clearMarks(); $('notesFindCount').textContent = ''; }

  // ------------------------------------------------------------------ export / import
  function nodeToMd(node) {
    if (node.nodeType === 3) return node.textContent;
    if (node.nodeType !== 1) return '';
    const tag = node.tagName.toLowerCase();
    const inner = () => [...node.childNodes].map(nodeToMd).join('');
    const li = ordered => [...node.children].filter(c => c.tagName === 'LI').map((c, i) => `${ordered ? i + 1 + '.' : '-'} ${[...c.childNodes].map(nodeToMd).join('').trim()}`).join('\n') + '\n\n';
    switch (tag) {
      case 'h1': case 'h2': case 'h3': case 'h4': case 'h5': case 'h6': return '#'.repeat(+tag[1]) + ' ' + inner().trim() + '\n\n';
      case 'p': case 'div': { const t = inner().trim(); return t ? t + '\n\n' : ''; }
      case 'br': return '\n';
      case 'strong': case 'b': return `**${inner()}**`;
      case 'em': case 'i': return `*${inner()}*`;
      case 'u': return `<u>${inner()}</u>`;
      case 's': case 'strike': case 'del': return `~~${inner()}~~`;
      case 'code': return '`' + inner() + '`';
      case 'pre': return '```\n' + node.textContent.trim() + '\n```\n\n';
      case 'blockquote': return inner().trim().split('\n').map(l => '> ' + l).join('\n') + '\n\n';
      case 'hr': return '---\n\n';
      case 'a': return `[${inner()}](${node.getAttribute('href') || ''})`;
      case 'img': return `![${node.getAttribute('alt') || ''}](${node.getAttribute('src') || ''})`;
      case 'ul': return li(false);
      case 'ol': return li(true);
      case 'li': return '';
      case 'table': {
        const rows = [...node.querySelectorAll('tr')];
        if (!rows.length) return '';
        const md = rows.map(r => '| ' + [...r.querySelectorAll('th,td')].map(c => c.textContent.trim().replace(/\|/g, '\\|')).join(' | ') + ' |');
        md.splice(1, 0, '|' + ' --- |'.repeat(rows[0].querySelectorAll('th,td').length));
        return md.join('\n') + '\n\n';
      }
      default: return inner();
    }
  }
  const htmlToMd = html => { const d = document.createElement('div'); d.innerHTML = html || ''; return nodeToMd(d).replace(/\n{3,}/g, '\n\n').trim() + '\n'; };

  function mdToHtml(md) {
    const e = s => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
    return e(md || '')
      .split(/\n{2,}/).map(block => {
        let b = block.trim();
        if (!b) return '';
        const h = /^(#{1,6}) (.+)$/.exec(b);
        if (h) return `<h${h[1].length}>${h[2]}</h${h[1].length}>`;
        if (/^---+$/.test(b)) return '<hr>';
        if (/^([-*] .+(\n|$))+$/.test(b)) return '<ul>' + b.split('\n').map(l => `<li>${l.replace(/^[-*] /, '')}</li>`).join('') + '</ul>';
        if (/^(\d+\. .+(\n|$))+$/.test(b)) return '<ol>' + b.split('\n').map(l => `<li>${l.replace(/^\d+\. /, '')}</li>`).join('') + '</ol>';
        if (/^&gt; /.test(b)) return '<blockquote>' + b.replace(/^&gt; ?/gm, '').replace(/\n/g, '<br>') + '</blockquote>';
        return '<p>' + b.replace(/\n/g, '<br>') + '</p>';
      }).join('')
      .replace(/\*\*\*(.+?)\*\*\*/g, '<strong><em>$1</em></strong>').replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>').replace(/\*(.+?)\*/g, '<em>$1</em>')
      .replace(/~~(.+?)~~/g, '<del>$1</del>').replace(/`([^`\n]+)`/g, '<code>$1</code>')
      .replace(/!\[([^\]]*)\]\((data:image\/[^)]+|https?:[^)]+)\)/g, '<img alt="$1" src="$2">')
      .replace(/\[([^\]]+)\]\((https?:[^)]+|mailto:[^)]+)\)/g, '<a href="$2">$1</a>');
  }

  const b64 = text => { const bytes = new TextEncoder().encode(text); let s = ''; bytes.forEach(b => (s += String.fromCharCode(b))); return btoa(s); };

  async function exportMd() {
    pullEditor();
    const v = await askForm('Save notes as Markdown files',
      `<label>Folder name<input name="folder" value="${esc(data.settings.mdFolder)}" required spellcheck="false"></label>
       <p class="sub" style="margin:6px 0">Each note becomes a <b>.md</b> file in your Downloads folder, inside this folder.</p>
       <label class="inline"><input type="checkbox" name="auto" value="yes" ${data.settings.mdAuto ? 'checked' : ''}> Keep the .md files up to date automatically (every 5 minutes after changes)</label>`, 'Save files');
    if (!v) return;
    data.settings.mdFolder = v.folder.trim() || 'WorkDesk Notes';
    data.settings.mdAuto = v.auto === 'yes';
    dirty = true;
    await writeMd(true);
    flush();
  }
  async function writeMd(verbose) {
    pullEditor();
    const files = Object.entries(data.notes).map(([name, n]) => ({ name, content: htmlToMd(n.content) }));
    try {
      const dir = await call('notes_export', { subfolder: data.settings.mdFolder, files });
      mdDirty = false;
      $('notesSaved').textContent = 'Markdown saved';
      if (verbose) toast(`Saved ${files.length} note${files.length === 1 ? '' : 's'} to ${dir}`);
    } catch (e) { if (verbose) toast('Could not save the files: ' + String(e?.message || e), { error: true }); }
  }
  setInterval(() => { if (loaded && data.settings.mdAuto && mdDirty) writeMd(false); }, 300000);

  // Backup: ~/Downloads/WorkDesk Notes/workdesk-notes-backup.json (the previous one becomes .json.bak)
  async function backupJson(verbose = true) {
    pullEditor();
    try {
      const path = await call('notes_backup', { data: JSON.stringify(data.notes, null, 2) });
      bkDirty = false;
      $('notesBackupStatus').textContent = 'Backed up ' + new Date().toLocaleTimeString();
      if (verbose) toast('Backup saved to ' + path);
    } catch (e) {
      $('notesBackupStatus').textContent = 'Backup failed';
      if (verbose) toast('Backup failed: ' + String(e?.message || e), { error: true });
    }
  }
  setInterval(() => { if (loaded && bkDirty) backupJson(false); }, 60000);

  async function importFiles(files) {
    let count = 0;
    pullEditor();
    for (const f of files) {
      const text = await f.text();
      if (/\.json$/i.test(f.name)) {
        let j;
        try { j = JSON.parse(text); } catch { toast(`${f.name} is not valid JSON.`, { error: true }); continue; }
        const src = j && j.notes ? j.notes : j;
        for (const [name, n] of Object.entries(src || {})) {
          if (!n || typeof n.content !== 'string') continue;
          let nn = name;
          while (data.notes[nn] && data.notes[nn].content !== n.content) nn = nn + ' (imported)';
          data.notes[nn] = { content: n.content, updatedAt: n.updatedAt || Date.now() };
          count++;
        }
      } else {
        const name = f.name.replace(/\.(md|markdown|txt|html?)$/i, '') || 'Imported note';
        const html = /\.html?$/i.test(f.name) ? (/<body[^>]*>([\s\S]*)<\/body>/i.exec(text)?.[1] || text) : mdToHtml(text);
        let nn = name;
        while (data.notes[nn]) nn += ' (imported)';
        data.notes[nn] = { content: html, updatedAt: Date.now() };
        count++;
      }
    }
    dirty = true; mdDirty = true; bkDirty = true;
    show(); flush();
    toast(count ? `Imported ${count} note${count === 1 ? '' : 's'}.` : 'Nothing could be imported.', { error: !count });
  }

  // ------------------------------------------------------------------ wiring
  function init() {
    editorApi = createRichEditor({ toolbar: $('notesToolbar'), editor: el(), form: askForm, onChange: changed });
    $('notesNew').onclick = newNote;
    $('notesList').addEventListener('click', e => {
      const r = e.target.closest('[data-rename]'), d = e.target.closest('[data-del]'), n = e.target.closest('[data-note]');
      if (r) return renameNote(r.dataset.rename);
      if (d) return deleteNote(d.dataset.del);
      if (n) switchNote(n.dataset.note);
    });
    $('notesFindBtn').onclick = openFind;
    $('notesFindClose').onclick = closeFind;
    $('notesFindNext').onclick = () => stepFind(1);
    $('notesFindPrev').onclick = () => stepFind(-1);
    $('notesFindInput').addEventListener('input', runFind);
    $('notesFindInput').addEventListener('keydown', e => { if (e.key === 'Enter') stepFind(e.shiftKey ? -1 : 1); if (e.key === 'Escape') closeFind(); });
    $('notesView').addEventListener('keydown', e => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') { e.preventDefault(); openFind(); } });
    $('notesExportMd').onclick = exportMd;
    $('notesBackup').onclick = () => backupJson(true);
    $('notesImport').onclick = () => $('notesFile').click();
    $('notesFile').onchange = async e => { const f = [...e.target.files]; e.target.value = ''; if (f.length) await importFiles(f); };
    const sel = $('notesFontSize');
    sel.innerHTML = SIZES.map(n => `<option value="${n}">${n} px</option>`).join('');
    sel.onchange = () => { data.settings.fontSize = +sel.value; el().style.fontSize = sel.value + 'px'; dirty = true; flush(); };
    window.addEventListener('beforeunload', () => { if (dirty) flush(); });
  }

  return {
    init,
    async open() {
      await load();
      $('notesFontSize').value = String(data.settings.fontSize);
      el().style.fontSize = data.settings.fontSize + 'px';
      show();
    },
    flush,
    leave() { pullEditor(); if (dirty) flush(); if (loaded && bkDirty) backupJson(false); },
  };
}
