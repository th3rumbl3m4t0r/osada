/* osada: small helpers shared by the app and the public page. Plain
   scripts, no build step; everything is a global. */
'use strict';

function $(sel, root) { return (root || document).querySelector(sel); }
function $$(sel, root) { return Array.from((root || document).querySelectorAll(sel)); }

/* h('div', {class: 'x', onclick: fn, title: 't'}, 'text', child, [more]) */
function h(tag, attrs) {
  const el = document.createElement(tag);
  if (attrs) {
    for (const k in attrs) {
      const v = attrs[k];
      if (v === null || v === undefined || v === false || k === 'value') continue;
      if (k === 'class') el.className = v;
      else if (k === 'text') el.textContent = v;
      else if (k === 'html') el.innerHTML = v;
      else if (k === 'style' && typeof v === 'object') Object.assign(el.style, v);
      else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
      else if (k in el && typeof v !== 'string') el[k] = v;
      else el.setAttribute(k, v === true ? '' : v);
    }
  }
  for (let i = 2; i < arguments.length; i++) add(el, arguments[i]);
  // after the children, so a <select> has its options
  if (attrs && attrs.value !== undefined && attrs.value !== null) el.value = attrs.value;
  return el;
}
function add(el, c) {
  if (c === null || c === undefined || c === false) return;
  if (Array.isArray(c)) c.forEach(x => add(el, x));
  else el.appendChild(c instanceof Node ? c : document.createTextNode(String(c)));
}
function clear(el) { while (el.firstChild) el.removeChild(el.firstChild); return el; }
/* is a key going into a field? (then it is not a shortcut) */
function isTyping(e) {
  const t = e.target;
  return t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t instanceof HTMLSelectElement || (t && t.isContentEditable);
}

/* ---- paths ---- */
function splitPath(p) { return (p || '').split('/').filter(Boolean); }
function encPath(p) { return splitPath(p).map(encodeURIComponent).join('/'); }
function joinPath(a, b) { return '/' + splitPath(a).concat(splitPath(b)).join('/'); }
function parentPath(p) { const s = splitPath(p); s.pop(); return '/' + s.join('/'); }
function baseName(p) { const s = splitPath(p); return s.length ? s[s.length - 1] : ''; }
function extOf(name) { const i = name.lastIndexOf('.'); return i > 0 ? name.slice(i + 1).toLowerCase() : ''; }
function stem(name) { const i = name.lastIndexOf('.'); return i > 0 ? name.slice(0, i) : name; }

const KINDS = {
  video: 'mp4 m4v webm mkv mov ogv',
  audio: 'mp3 m4a aac ogg oga opus flac wav',
  image: 'png jpg jpeg gif webp svg bmp avif ico',
  pdf: 'pdf',
  md: 'md markdown',
  csv: 'csv tsv',
  sheet: 'ods ots xlsx xlsm xltx xltm xls xlt',
  rtf: 'rtf',
  doc: 'odt ott docx docm dotx dotm doc dot',
  slides: 'odp otp odg otg pptx pptm potx potm ppsx ppsm ppt pps pot',
  mail: 'eml msg oft',
  pub: 'pub',
  archive: 'zip jar apk cbz tar tgz gz cbt 7z cb7 rar cbr bz2 tbz2 tbz xz txz',
  // images the browser cannot show itself (img.js): TIFF, Photoshop, the preview in camera raw files
  img: 'tif tiff psd psb 3fr arw cr2 cr3 crw dcr dcs dng erf fff gpr iiq k25 kdc mdc mef mos mrw nef nrw orf pef raf raw rw2 rwl sr2 srf srw x3f',
  book: 'epub',
  cal: 'ics vcs vcf vcard',
  font: 'ttf otf ttc woff woff2',
  iwork: 'pages numbers key',
  text: 'txt log json xml yaml yml toml ini conf cfg sh bash py rs js mjs ts css html htm c h cpp hpp go java kt ' +
        'srt vtt sql diff patch env service gitignore dockerfile makefile',
};
const KIND_OF = {};
for (const k in KINDS) KINDS[k].split(' ').forEach(e => { KIND_OF[e] = k; });
function kindOf(name) { return KIND_OF[extOf(name)] || (extOf(name) === '' && /^(readme|license|makefile|dockerfile)$/i.test(name) ? 'text' : 'other'); }
/* what the in-browser editors handle: of the spreadsheets only ods, of the documents odt (the others
   are shown, and edited as an .ods or .odt made from them) */
function editorOf(name) {
  const k = kindOf(name), e = extOf(name);
  if (k === 'md' || k === 'csv' || k === 'rtf' || k === 'text') return k;
  if (k === 'sheet' && e === 'ods') return k;
  if (k === 'doc' && (e === 'odt' || e === 'ott')) return 'odt';
  if (k === 'slides' && (e === 'odp' || e === 'otp')) return 'odp';
  return null;
}

/* the part that reads a spreadsheet file: sheet (ods, xlsx), xls for the Excel 97-2003 ones */
function sheetPart(name) { return /^(xls|xlt)$/.test(extOf(name)) ? 'xls' : 'sheet'; }
/* the part that reads a document: odt, docx, msdoc (Word 97-2003) */
function docPart(name) { const e = extOf(name); return e === 'doc' || e === 'dot' ? 'msdoc' : e === 'odt' || e === 'ott' ? 'odt' : 'docx'; }
/* the part that reads a presentation: odp, pptx, msppt (PowerPoint 97-2003) */
function slidesPart(name) { const e = extOf(name); return /^(ppt|pps|pot)$/.test(e) ? 'msppt' : /^o[dt][pg]$/.test(e) ? 'odp' : 'pptx'; }

/* ---- formatting ---- */
function fmtSize(n) {
  if (n < 1024) return n + ' B';
  const u = ['KiB', 'MiB', 'GiB', 'TiB'];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return (n < 10 ? n.toFixed(1) : Math.round(n)) + ' ' + u[i];
}
/* at most two digits and a letter: 75G, 2.9T; three with top 999.5: 150G */
function fmtShort(n, top) {
  const u = ['B', 'K', 'M', 'G', 'T', 'P'];
  let i = 0;
  while (n >= (top || 99.5) && i < u.length - 1) { n /= 1024; i++; }
  return (i > 0 && n < 9.95 ? n.toFixed(1) : Math.round(n)) + u[i];
}
function pad2(n) { return (n < 10 ? '0' : '') + n; }
/* ISO 8601 in local time, minutes: 2026-09-27 13:40 */
function fmtTime(ts) {
  if (!ts) return '';
  const d = new Date(ts * 1000);
  return d.getFullYear() + '-' + pad2(d.getMonth() + 1) + '-' + pad2(d.getDate()) + ' ' + pad2(d.getHours()) + ':' + pad2(d.getMinutes());
}

/* ---- server ---- */
class ApiError extends Error {
  constructor(msg, status) { super(msg); this.status = status; }
}
/* JSON API call; state-changing calls carry X-Shelf (the server insists) */
async function api(method, url, body) {
  const o = { method, headers: { 'x-shelf': '1' }, credentials: 'same-origin' };
  if (body !== undefined) { o.headers['content-type'] = 'application/json'; o.body = JSON.stringify(body); }
  let r;
  try { r = await fetch(url, o); } catch (e) { setOffline(true); throw new ApiError(tx('server unreachable'), 0); }
  heard(r);
  let data = null;
  try { data = await r.json(); } catch (e) { /* not JSON */ }
  if (!r.ok) {
    if (r.status === 401 && window.onUnauthorized) window.onUnauthorized();
    throw new ApiError(txErr((data && data.error) || (r.status + ' ' + r.statusText)), r.status);
  }
  return data;
}

/* ---- offline: osada at home does not answer ----
   What this browser kept (sw.js), what the android app kept, and the public edge answer with
   x-shelf-offline: 1 then; a request that gets nowhere says the same. The page shows a bar
   and is read only (offline()), and asks home again every OFF_RETRY until it answers.
   OFF.ping is what it asks, OFF.changed(on) what the page does about it. */
const OFF = { on: false, timer: null, bar: null, ping: '/api/me', changed: null };
const OFF_RETRY = 15000;
function offline() { return OFF.on; }
/* a response: from home, or from a copy */
function heard(r) { setOffline(!!r.headers.get('x-shelf-offline')); }
function setOffline(on) {
  if (on === OFF.on) return;
  OFF.on = on;
  document.body.classList.toggle('offline', on);
  if (on && !OFF.bar) {
    OFF.bar = h('div', { id: 'offline', class: 'offline-bar', text: tx('osada at home is not reachable: showing what this device kept, read only') });
    const top = $('#topbar');
    if (top) top.after(OFF.bar); else document.body.prepend(OFF.bar);
  }
  if (OFF.bar) OFF.bar.hidden = !on;
  clearInterval(OFF.timer);
  if (on) OFF.timer = setInterval(askHome, OFF_RETRY);
  // the android app looks for another way home
  if (on && window.OsadaApp && OsadaApp.offline) OsadaApp.offline();
  if (OFF.changed) OFF.changed(on);
}
function askHome() { fetch(OFF.ping, { credentials: 'same-origin', cache: 'no-store' }).then(heard, () => {}); }
addEventListener('online', () => { if (OFF.on) askHome(); });
document.addEventListener('visibilitychange', () => { if (OFF.on && !document.hidden) askHome(); });
/* which files and folders the device kept ({files, lists}: sets of /api/raw/… and /api/ls/… or /s/… urls),
   or null when it cannot tell (no service worker, no app) */
async function keptHere() {
  try {
    const r = await fetch('/api/kept', { credentials: 'same-origin' });
    if (!r.ok || !r.headers.get('x-shelf-offline')) return null;
    const j = await r.json();
    return { files: new Set(j.files || []), lists: new Set(j.lists || []) };
  } catch (e) { return null; }
}

/* this browser keeps what it needs to work without home (sw.js); only on https or this machine.
   The android app keeps its own (no service worker on the home network's plain http). */
if ('serviceWorker' in navigator && window.isSecureContext && !/ osada-android\//.test(navigator.userAgent)) {
  addEventListener('load', () => { navigator.serviceWorker.register('/sw.js').catch(() => {}); });
}

function escHtml(s) {
  return s.replace(/[&<>"]/g, c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : '&quot;'));
}

/* ---- parts: code loaded when a page or a kind of file needs it ----
   A part is /static/<name>.js (and <name>.css when there is one). Its script calls
   part(name, factory); the factory gets what the parts it builds on (PART_DEPS) export and
   returns its own. hold(name) loads a part and those under it once, and resolves to all
   their exports; release(name) lets go. PART_IDLE after the last hold is let go, the part
   is dropped and its files taken out of the page, so the browser can free them (they stay
   in its cache for the next time). A page holds through a holder(), and lets go of
   everything it held when it closes. */
const PART_DEPS = {
  // kinds of file
  zip: [], cfb: [], xlsx: ['zip'], csv: ['xlsx'], ods: ['csv'], sheet: ['ods'], xls: ['sheet', 'cfb'],
  rich: [], rtf: ['rich'], odt: ['zip', 'rich'], docx: ['zip', 'rich'], msdoc: ['cfb', 'rich'],
  slides: ['rich'], odp: ['zip', 'slides'], pptx: ['zip', 'slides'], msppt: ['cfb', 'slides'],
  mail: [], msg: ['mail', 'cfb', 'rtf'], pub: ['cfb', 'rich'],
  archive: ['zip'], sevenz: ['archive', 'lzma'], rar: ['archive'], lzma: [], bzip2: [], ppmd: [],
  img: [], epub: ['zip'], cal: [], font: [], iwork: ['zip'],
  // pages and dialogs of the app most sessions never open
  edit: [], share: [], versions: [], pictures: ['share'], sftp: [], settings: ['share', 'sftp'], admin: ['sftp'], wiki: [], tree: [],
};
const PART_IDLE = 15000;
const PARTS = new Map();   // name -> {holds, timer, nodes, factory, all, loading}

function partOf(name) {
  let p = PARTS.get(name);
  if (!p) PARTS.set(name, p = { holds: 0, timer: null, nodes: [], factory: null, all: null, loading: null });
  return p;
}
/* what a part's script calls */
function part(name, factory) { partOf(name).factory = factory; }

function partFile(file) {
  return new Promise((ok, no) => {
    const el = file.endsWith('.css') ? h('link', { rel: 'stylesheet', href: assetUrl(file) }) : h('script', { src: assetUrl(file) });
    el.onload = () => ok(el);
    el.onerror = () => { el.remove(); no(new Error(tx('could not load {file}; is the server reachable?', { file }))); };
    document.head.appendChild(el);
  });
}

function loadPart(name) {
  const p = partOf(name);
  if (p.all) return Promise.resolve(p.all);
  if (!p.loading) {
    p.loading = (async () => {
      const deps = PART_DEPS[name] || [];
      // held for as long as this part is loaded
      const below = await Promise.allSettled(deps.map(d => hold(d)));
      const held = deps.filter((d, i) => below[i].status === 'fulfilled');
      try {
        const bad = below.find(r => r.status === 'rejected');
        if (bad) throw bad.reason;
        if (ASSET_V[name + '.css']) p.nodes.push(await partFile(name + '.css'));
        p.nodes.push(await partFile(name + '.js'));
        if (!p.factory) throw new Error(name + '.js did not register');
        const got = Object.assign({}, ...below.map(r => r.value));
        p.all = Object.assign(got, p.factory(got));
        return p.all;
      } catch (e) {
        held.forEach(d => release(d, true));
        p.nodes.splice(0).forEach(n => n.remove());
        p.factory = null;
        throw e;
      } finally {
        p.loading = null;
      }
    })();
  }
  return p.loading;
}

async function hold(name) {
  const p = partOf(name);
  p.holds++;
  clearTimeout(p.timer);
  try { return await loadPart(name); } catch (e) { p.holds--; throw e; }
}
/* `now`: drop it at once when nothing holds it (a part above was dropped after its wait) */
function release(name, now) {
  const p = PARTS.get(name);
  if (!p || p.holds <= 0) return;
  if (--p.holds > 0) return;
  clearTimeout(p.timer);
  if (now) dropPart(name);
  else p.timer = setTimeout(() => dropPart(name), PART_IDLE);
}
function dropPart(name) {
  const p = PARTS.get(name);
  if (!p || p.holds > 0 || p.loading || !p.all) return;
  p.all = null;
  p.factory = null;
  p.nodes.splice(0).forEach(n => n.remove());
  (PART_DEPS[name] || []).forEach(d => release(d, true));
}
/* the parts one page or viewer holds; done() lets go of all of them */
function holder() {
  const held = [];
  let closed = false;
  return {
    async hold(name) {
      const x = await hold(name);
      if (closed) release(name); else held.push(name);
      return x;
    },
    done() { closed = true; held.splice(0).forEach(n => release(n)); },
  };
}
/* a part for one job: held while fn runs */
async function withPart(name, fn) {
  const x = await hold(name);
  try { return await fn(x); } finally { release(name); }
}

/* ---- toast: on, then off; no fade ---- */
let toastTimer = null;
function toast(msg, kind) {
  let el = $('#toast');
  if (!el) {
    el = h('div', { id: 'toast', class: 'win toast' }, h('div', { class: 'titlebar' }), h('div', { class: 'body' }));
    document.body.appendChild(el);
  }
  el.querySelector('.titlebar').textContent = kind === 'err' ? tx('error') : kind === 'warn' ? tx('note') : tx('ok');
  el.querySelector('.titlebar').className = 'titlebar' + (kind === 'err' ? ' bad' : '');
  el.querySelector('.body').textContent = msg;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, kind === 'err' ? 6000 : 2500);
}
function fail(e) { toast(txErr(e && e.message ? e.message : String(e)), 'err'); }

/* ---- dialogs: a .win over a dimmed page, instant; keys go to the topmost one ---- */
let dlgOpen = 0;
const DLGS = [];
function dialog(opts) {
  return new Promise(resolve => {
    const prev = document.activeElement;
    const btns = h('div', { class: 'dlg-buttons' });
    const body = h('div', { class: 'body' }, opts.body || null);
    const win = h('div', { class: 'win dlg' + (opts.wide ? ' wide' : ''), role: 'dialog' },
      h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), opts.title || '', h('span', { class: 'spacer' }),
        h('button', { class: 'dlg-x', text: 'x', title: tx('close (esc)'), onclick: () => done(null) })),
      body, btns);
    const back = h('div', { class: 'dlg-back' }, win);
    let finished = false, checking = false;
    /* check(v) may be async (a server call): the dialog stays open on an error */
    async function done(v) {
      if (finished || checking) return;
      if (v !== null && opts.check) {
        checking = true;
        win.classList.add('busy');
        let err;
        try { err = await opts.check(v); } catch (e) { err = e.message || String(e); }
        checking = false;
        win.classList.remove('busy');
        if (err) { toast(err, 'err'); return; }
      }
      finished = true;
      back.remove();
      DLGS.splice(DLGS.indexOf(back), 1);
      dlgOpen--;
      document.removeEventListener('keydown', key, true);
      if (prev && prev.focus) prev.focus();
      resolve(v);
    }
    (opts.buttons || [{ label: tx('ok'), value: true, primary: true }]).forEach(b => {
      btns.appendChild(h('button', { class: (b.primary ? 'primary' : '') + (b.danger ? ' danger' : ''), text: b.label,
        onclick: () => done(typeof b.value === 'function' ? b.value() : b.value) }));
    });
    function key(e) {
      if (!back.isConnected || DLGS[DLGS.length - 1] !== back) return;
      if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); done(null); }
      else if (e.key === 'Enter' && !(e.target instanceof HTMLTextAreaElement) && !(e.target instanceof HTMLButtonElement)) {
        const p = (opts.buttons || []).find(b => b.primary);
        if (p || !opts.buttons) { e.preventDefault(); done(p ? (typeof p.value === 'function' ? p.value() : p.value) : true); }
      }
    }
    document.addEventListener('keydown', key, true);
    back.addEventListener('pointerdown', e => { if (e.target === back) done(null); });
    document.body.appendChild(back);
    DLGS.push(back);
    dlgOpen++;
    const f = win.querySelector('[autofocus]') || win.querySelector('input, select, textarea') || btns.querySelector('.primary');
    if (f) { f.focus(); if (opts.select) opts.select(f); }
    if (opts.opened) opts.opened(win, done);
  });
}

/* a .win at x, y (a menu, a filter), gone on esc or a click elsewhere; on narrow screens a sheet at the bottom.
   Gives close(). */
function popup(x, y, win, onclose) {
  const back = h('div', { class: 'pop-back' }, win);
  win.classList.add('pop');
  function close() {
    if (!back.isConnected) return;
    back.remove();
    dlgOpen--;
    document.removeEventListener('keydown', key, true);
    if (onclose) onclose();
  }
  function key(e) { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); } }
  document.addEventListener('keydown', key, true);
  back.addEventListener('pointerdown', e => { if (e.target === back) close(); });
  back.addEventListener('contextmenu', e => { if (e.target === back) { e.preventDefault(); close(); } });
  document.body.appendChild(back);
  dlgOpen++;
  if (innerWidth <= 760) win.classList.add('pop-sheet');
  else {
    const r = win.getBoundingClientRect();
    win.style.left = Math.max(4, Math.min(x, innerWidth - r.width - 4)) + 'px';
    win.style.top = Math.max(4, Math.min(y, innerHeight - r.height - 4)) + 'px';
  }
  return close;
}

function ask(title, label, value, okLabel) {
  const inp = h('input', { type: 'text', value: value || '', spellcheck: false, class: 'wide' });
  return dialog({
    title, body: [h('label', { class: 'lbl', text: label }), inp],
    buttons: [{ label: tx('cancel'), value: null }, { label: okLabel || tx('ok'), primary: true, value: () => inp.value }],
    // select the name without its extension, like a file manager
    select: el => { const v = el.value, i = v.lastIndexOf('.'); el.setSelectionRange(0, i > 0 ? i : v.length); },
  });
}

function confirmBox(title, text, okLabel, danger) {
  return dialog({
    title, body: h('div', { class: 'dlg-text', text }),
    buttons: [{ label: tx('cancel'), value: false }, { label: okLabel || tx('ok'), primary: true, danger, value: true }],
  }).then(v => !!v);
}

/* copy text; works on plain http too */
function copyText(t) {
  if (navigator.clipboard && window.isSecureContext) return navigator.clipboard.writeText(t).then(() => true, () => false);
  const ta = h('textarea', { style: { position: 'fixed', top: '-100px' } });
  ta.value = t;
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try { ok = document.execCommand('copy'); } catch (e) { /* no */ }
  ta.remove();
  return Promise.resolve(ok);
}

/* .srt subtitles to WebVTT, which is what <track> takes */
function srtToVtt(s) {
  return 'WEBVTT\n\n' + s.replace(/^\uFEFF/, '').replace(/\r\n?/g, '\n')
    .replace(/(\d\d:\d\d:\d\d),(\d\d\d)/g, '$1.$2')
    .replace(/^\d+\n(?=\d\d:\d\d)/gm, '');
}

/* stepped busy marker for the status bar */
let busyN = 0;
function busy(on) {
  busyN = Math.max(0, busyN + (on ? 1 : -1));
  const el = $('#busy');
  if (el) el.hidden = busyN === 0;
}
