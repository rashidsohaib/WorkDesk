// WorkDesk icon set: sharp, monochrome, 24 × 24 grid.
//
// Every glyph is drawn with straight, pixel-aligned strokes (2 px, square caps, mitered joins) so
// it stays crisp at 1× and 2× scale, and inherits `currentColor` so it follows the theme.
// An entry is "stroke paths" or "stroke paths|filled paths" (the part after | is drawn solid).

const ICONS = {
  // navigation / app chrome
  mail: 'M3 5h18v14H3z M3 5l9 8 9-8',
  compose: 'M4 20h4L19 9l-4-4L4 16z M13 7l4 4',
  settings: 'M3 7h9 M16 7h5 M12 4h4v6h-4z M3 17h5 M12 17h9 M8 14h4v6H8z',
  globe: 'M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18z M3 12h18 M12 3c-4 3-4 15 0 18 M12 3c4 3 4 15 0 18',
  plus: 'M12 4v16 M4 12h16',
  minus: 'M4 12h16',
  upload: 'M12 17V4 M6 10l6-6 6 6 M4 20h16',
  video: 'M3 6h12v12H3z M15 10l6-3v10l-6-3',
  chrome: 'M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18z M12 8a4 4 0 1 0 0 8a4 4 0 1 0 0-8z M12 8h8 M8.5 14l-4-7 M10 16l4 7',
  clock: 'M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18z M12 7v5l3 3',
  fit: 'M4 9V4h5 M20 9V4h-5 M4 15v5h5 M20 15v5h-5',
  refresh: 'M4 12a8 8 0 0 1 14-5.3 M20 4v5h-5 M20 12a8 8 0 0 1-14 5.3 M4 20v-5h5',
  search: 'M10 3a7 7 0 1 0 0 14a7 7 0 1 0 0-14z M15 15l6 6',
  chevronLeft: 'M15 5l-7 7 7 7',
  chevronRight: 'M9 5l7 7-7 7',
  chevronDown: 'M5 9l7 7 7-7',
  chevronUp: 'M5 15l7-7 7 7',
  first: 'M6 5v14 M18 5l-7 7 7 7',
  last: 'M18 5v14 M6 5l7 7-7 7',
  close: 'M5 5l14 14 M19 5L5 19',
  check: 'M4 12l5 5 11-11',
  back: 'M20 12H4 M10 6l-6 6 6 6',
  forwardNav: 'M4 12h16 M14 6l6 6-6 6',
  minimize: 'M5 19h14',
  maximize: 'M5 5h14v14H5z',
  restore: 'M8 8h11v11H8z M8 5h-3v11h3',
  more: 'M5 12h.01 M12 12h.01 M19 12h.01',
  menu: 'M3 6h18 M3 12h18 M3 18h18',
  pin: 'M9 3h6l-1 6 4 4H6l4-4z M12 13v8',
  sync: 'M4 12a8 8 0 0 1 14-5.3 M20 4v5h-5 M20 12a8 8 0 0 1-14 5.3 M4 20v-5h5',
  offline: 'M3 3l18 18 M8 12a6 6 0 0 1 6-2 M4 9a11 11 0 0 1 5-3 M12 19h.01',

  // mailboxes
  inbox: 'M3 13l3-8h12l3 8v6H3z M3 13h5l1 3h6l1-3h5',
  star: 'M12 3l2.8 5.9 6.2.9-4.5 4.4 1.1 6.3-5.6-3-5.6 3 1.1-6.3L3 9.8l6.2-.9z',
  starFilled: 'M12 3l2.8 5.9 6.2.9-4.5 4.4 1.1 6.3-5.6-3-5.6 3 1.1-6.3L3 9.8l6.2-.9z|M12 3l2.8 5.9 6.2.9-4.5 4.4 1.1 6.3-5.6-3-5.6 3 1.1-6.3L3 9.8l6.2-.9z',
  snooze: 'M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18z M12 7v5l3 3',
  important: 'M3 5h13l5 7-5 7H3z',
  send: 'M21 3L3 11l7 3 3 7z M10 14l11-11',
  drafts: 'M5 3h9l5 5v13H5z M14 3v5h5 M9 14h6 M9 17h4',
  allMail: 'M3 4h18v5H3z M5 9v11h14V9 M9 13h6',
  spam: 'M8 3h8l5 5v8l-5 5H8l-5-5V8z M12 8v5 M12 16v1',
  trash: 'M4 6h16 M9 6V3h6v3 M6 6l1 15h10l1-15 M10 10v7 M14 10v7',
  folder: 'M3 5h7l2 3h9v12H3z',
  label: 'M3 3h9l9 9-9 9-9-9z M8 8h.01',

  // message actions
  archive: 'M3 4h18v5H3z M5 9v11h14V9 M9 13h6',
  markRead: 'M3 10l9-6 9 6v10H3z M3 10l9 6 9-6',
  markUnread: 'M3 7h13v12H3z M3 7l6.5 6L16 7|M17 3h4v4h-4z',
  reply: 'M9 6L3 12l6 6 M3 12h9a9 9 0 0 1 9 9',
  replyAll: 'M7 6L1 12l6 6 M13 6l-6 6 6 6 M7 12h6a8 8 0 0 1 8 8',
  forward: 'M15 6l6 6-6 6 M21 12h-9a9 9 0 0 0-9 9',
  print: 'M6 9V3h12v6 M6 18H3V9h18v9h-3 M6 14h12v7H6z',
  download: 'M12 3v13 M6 11l6 6 6-6 M4 21h16',
  attach: 'M20 11l-8.5 8.5a5 5 0 0 1-7-7L13 4a3.5 3.5 0 0 1 5 5l-8.5 8.5a2 2 0 0 1-3-3L14 7',
  link: 'M10 14a4 4 0 0 0 5.7 0l4-4a4 4 0 0 0-5.7-5.7l-1 1 M14 10a4 4 0 0 0-5.7 0l-4 4a4 4 0 0 0 5.7 5.7l1-1',
  unlink: 'M10 14a4 4 0 0 0 5.7 0l4-4a4 4 0 0 0-5.7-5.7l-1 1 M14 10a4 4 0 0 0-5.7 0l-4 4a4 4 0 0 0 5.7 5.7l1-1 M3 3l18 18',
  external: 'M14 3h7v7 M21 3L11 13 M18 14v7H3V6h7',
  columns: 'M3 4h18v16H3z M9 4v16 M15 4v16',
  select: 'M4 4h16v16H4z M8 12l3 3 5-6',
  conversation: 'M3 4h14v9H8l-5 4z M10 17h6l5 4V10h-2',
  move: 'M3 5h7l2 3h9v12H3z M9 14h8 M14 11l3 3-3 3',
  undo: 'M9 5L4 10l5 5 M4 10h10a6 6 0 0 1 0 12h-3',
  redo: 'M15 5l5 5-5 5 M20 10H10a6 6 0 0 0 0 12h3',
  save: 'M4 3h13l3 3v15H4z M8 3v6h8V3 M8 21v-7h8v7',
  sortAsc: 'M12 19V5 M6 11l6-6 6 6',
  sortDesc: 'M12 5v14 M6 13l6 6 6-6',
  sortNone: 'M8 9l4-4 4 4 M8 15l4 4 4-4',
  image: 'M3 4h18v16H3z M3 17l5-5 4 4 3-3 6 6 M8 8h2v2H8z',
  eye: 'M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12z M10 10h4v4h-4z',

  // rich-text editor
  bold: 'M7 4h6a4 4 0 0 1 0 8H7z M7 12h7a4 4 0 0 1 0 8H7z',
  italic: 'M10 4h8 M6 20h8 M15 4L9 20',
  underline: 'M6 4v8a6 6 0 0 0 12 0V4 M4 21h16',
  strike: 'M4 12h16 M16 7c-.5-2-2.5-3-4.5-3S7.5 5.2 7.5 7c0 2 2 3 4.5 3 M7.5 17c.5 2 2.5 3 4.5 3 2.5 0 4.5-1 4.5-3',
  subscript: 'M3 4l9 11 M12 4L3 15 M15 18h6v2h-6l6 3',
  superscript: 'M3 9l9 11 M12 9L3 20 M15 3h6v2h-6l6 3',
  textColor: 'M6 17l6-13 6 13 M8 12h8|M4 19h16v3H4z',
  highlight: 'M4 15l9-9 5 5-9 9H4z M13 6l2-2 5 5-2 2|M3 21h18v2H3z',
  alignLeft: 'M3 5h18 M3 10h12 M3 15h18 M3 20h12',
  alignCenter: 'M3 5h18 M6 10h12 M3 15h18 M6 20h12',
  alignRight: 'M3 5h18 M9 10h12 M3 15h18 M9 20h12',
  alignJustify: 'M3 5h18 M3 10h18 M3 15h18 M3 20h18',
  listBullet: 'M9 6h12 M9 12h12 M9 18h12 M3 6h.01 M3 12h.01 M3 18h.01',
  listNumber: 'M10 6h11 M10 12h11 M10 18h11 M4 4v5 M3 14h3l-3 3h3 M3 20h3v3',
  indent: 'M3 5h18 M11 10h10 M11 15h10 M3 20h18 M3 9l4 3-4 3',
  outdent: 'M3 5h18 M11 10h10 M11 15h10 M3 20h18 M7 9l-4 3 4 3',
  quote: 'M3 6h7v7H6l-3 4z M14 6h7v7h-4l-3 4z',
  code: 'M4 5h16v14H4z M9 10l-2 2 2 2 M15 10l2 2-2 2',
  source: 'M7 8l-4 4 4 4 M17 8l4 4-4 4 M13 5l-2 14',
  table: 'M3 4h18v16H3z M3 10h18 M3 15h18 M9 4v16 M15 4v16',
  hr: 'M3 12h18 M8 7l4-4 4 4 M8 17l4 4 4-4',
  clearFormat: 'M5 4h13 M12 4L9 17 M14 15l6 6 M20 15l-6 6',
  signature: 'M3 17l4-9 3 6 2-3 3 6 M3 21h18',
  paragraph: 'M9 4h10 M15 4v16 M9 4a4.5 4.5 0 0 0 0 9h6',

  cursor: 'M5 3l14 8-6 2 3 7-3 1-3-7-5 4z',
  pen: 'M4 20l1-5L16 4l4 4L9 19z M14 6l4 4 M4 20h5',
  textbox: 'M5 5h14 M12 5v14 M9 19h6',
  // more bookmark symbols (general and social)
  chat: 'M4 5h16v11H9l-5 4z M8 9h8 M8 12h5',
  camera: 'M3 7h4l2-3h6l2 3h4v13H3z M12 17a4 4 0 1 0 0-8 4 4 0 0 0 0 8',
  play: 'M5 4l14 8-14 8z',
  music: 'M9 18V5l11-2v13 M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0 M20 16a3 3 0 1 1-6 0 3 3 0 0 1 6 0',
  heart: 'M12 21l-8-8a5 5 0 0 1 8-6 5 5 0 0 1 8 6z',
  thumbsup: 'M7 11v9H3v-9z M7 11l4-8a2 2 0 0 1 2 2v4h6l-2 11H7',
  share: 'M18 8a3 3 0 1 0 0-6 3 3 0 0 0 0 6 M6 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6 M18 22a3 3 0 1 0 0-6 3 3 0 0 0 0 6 M8.6 13.5l6.8 4 M15.4 6.5l-6.8 4',
  users: 'M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8 M2 21v-1a6 6 0 0 1 14 0v1 M17 11a3 3 0 1 0 0-6 M22 21v-1a5 5 0 0 0-4-5',
  megaphone: 'M3 10v4h4l8 5V5l-8 5z M18 9a4 4 0 0 1 0 6',
  hash: 'M5 9h15 M4 15h15 M10 3L8 21 M16 3l-2 18',
  atsign: 'M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0 M16 12v1.5a2.5 2.5 0 0 0 5 0V12a9 9 0 1 0-3.5 7',
  news: 'M4 4h13v16H4z M17 8h4v10a2 2 0 0 1-4 0 M7 8h7 M7 12h7 M7 16h4',
  cart: 'M3 4h3l2 12h11l2-9H7 M10 20h.01 M18 20h.01',
  book: 'M4 4h7a3 3 0 0 1 3 3v14a2 2 0 0 0-2-2H4z M20 4h-6a3 3 0 0 0-3 3',
  cloud: 'M7 18a4 4 0 0 1 0-8 5 5 0 0 1 10 1 3.5 3.5 0 0 1 0 7z',
  bell: 'M6 16V10a6 6 0 0 1 12 0v6l2 2H4z M10 21h4',
  phone: 'M5 3h4l2 5-2.5 1.5a11 11 0 0 0 6 6L16 13l5 2v4a2 2 0 0 1-2 2A16 16 0 0 1 3 5a2 2 0 0 1 2-2',
  lock: 'M6 11h12v10H6z M8 11V7a4 4 0 0 1 8 0v4',
  gamepad: 'M6 8h12a4 4 0 0 1 4 4v3a3 3 0 0 1-5 2l-2-2H9l-2 2a3 3 0 0 1-5-2v-3a4 4 0 0 1 4-4 M7 12v3 M5.5 13.5h3 M16 12h.01 M18 14h.01',
  map: 'M12 21s-7-6-7-11a7 7 0 0 1 14 0c0 5-7 11-7 11z M12 12a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5',
  bulb: 'M9 18h6 M10 21h4 M12 3a6 6 0 0 0-4 10c1 1 1 2 1 3h6c0-1 0-2 1-3a6 6 0 0 0-4-10',
  rocket: 'M5 19c0-3 1-4 3-4l-3-3c2-6 8-9 15-9 0 7-3 13-9 15l-3-3c0 2-1 4-4 4z M14 9a1.5 1.5 0 1 0 0-3 1.5 1.5 0 0 0 0 3',

  // bookmark / pin symbols
  calendar: 'M3 5h18v16H3z M3 10h18 M8 3v4 M16 3v4',
  note: 'M5 3h14v18H5z M9 8h6 M9 12h6 M9 16h4',
  briefcase: 'M3 7h18v13H3z M9 7V3h6v4 M3 13h18',
  home: 'M3 11l9-8 9 8 M5 10v11h14V10 M10 21v-6h4v6',
  chart: 'M4 20V4 M4 20h16 M8 16v-5 M13 16V8 M18 16V7',
  wallet: 'M3 6h16v3 M3 6v14h18V9H3 M16 14h.01',
  puzzle: 'M4 8h4V6a2 2 0 0 1 4 0v2h4v4h-2a2 2 0 0 0 0 4h2v4H4z',
  tools: 'M15 3a5 5 0 0 0-4 6L3 17l4 4 8-8a5 5 0 0 0 6-4l-3 3-3-1-1-3z',
};

export const BOOKMARK_ICONS = ['globe', 'calendar', 'folder', 'note', 'briefcase', 'home', 'chart', 'wallet', 'tools', 'star', 'link', 'mail', 'puzzle', 'chat', 'phone', 'video', 'camera', 'image', 'music', 'play', 'heart', 'thumbsup', 'share', 'users', 'megaphone', 'hash', 'atsign', 'news', 'book', 'cart', 'cloud', 'bell', 'lock', 'gamepad', 'map', 'bulb', 'rocket'];

// Bookmarks saved by earlier versions stored an emoji; map them to the nearest symbol.
const LEGACY = { '🌐': 'globe', '📅': 'calendar', '📁': 'folder', '📝': 'note', '💼': 'briefcase', '🏠': 'home', '📊': 'chart', '💰': 'wallet', '🛠️': 'tools', '⭐': 'star', '🔗': 'link' };
export const bookmarkIcon = v => (typeof v === 'string' && v.startsWith('data:image/') ? v : ICONS[v] ? v : LEGACY[v] || 'globe');

export function icon(name, cls = '') {
  if (typeof name === 'string' && name.startsWith('data:image/')) return `<img class="ic favicon ${cls}" src="${name}" alt="" draggable="false">`;
  const def = ICONS[name] || ICONS.more;
  const [stroke, fill] = def.split('|');
  return `<svg class="ic ${cls}" viewBox="0 0 24 24" width="24" height="24" aria-hidden="true" focusable="false"><path d="${stroke}"/>${fill ? `<path class="solid" d="${fill}"/>` : ''}</svg>`;
}

/** Replace every `<i data-icon="name">` below `root` by its SVG. */
export function hydrateIcons(root = document) {
  root.querySelectorAll('i[data-icon]').forEach(el => {
    const holder = document.createElement('span');
    holder.innerHTML = icon(el.dataset.icon, el.className);
    el.replaceWith(holder.firstElementChild);
  });
}

export function setIcon(el, name) {
  if (el) el.innerHTML = icon(name);
}
