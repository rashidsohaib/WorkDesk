const { contextBridge, ipcRenderer } = require('electron');
const subs = new Map();
ipcRenderer.on('wd-event', (_e, name, payload) => { for (const cb of subs.get(name) || []) { try { cb(payload); } catch (err) { console.error(err); } } });
contextBridge.exposeInMainWorld('wdElectron', {
  invoke: (cmd, args) => ipcRenderer.invoke('wd-invoke', cmd, args).then(r => { if (r && r.err) throw r.err; return r ? r.ok : null; }),
  on: (name, cb) => { if (!subs.has(name)) subs.set(name, new Set()); subs.get(name).add(cb); return () => subs.get(name).delete(cb); },
});
