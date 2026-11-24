/* osada's office page (Round 18): ?loc=<osada's loc>&path=<the file's path>[&ro=1]. The file is
   fetched from osada (/api/raw, this origin, the signed-in session), opened in ZetaOffice
   (LibreOffice in WebAssembly; office_thread.js runs in its UNO thread) and written back when
   LibreOffice saves it: with If-Match, so that a file changed in osada meanwhile is not lost.
   Saved in another format (LibreOffice's "use ODF format", save as): a new file next to it.
   Derived from zetajs's examples/web-office (MIT, allotropia). */
'use strict';

const q = new URLSearchParams(location.search);
const loc = q.get('loc') || '', readOnly = q.get('ro') === '1';
const segs = (q.get('path') || '').split('/').filter(Boolean);
const name = segs[segs.length - 1] || '';
const dir = segs.slice(0, -1);
const ext = (/\.([A-Za-z0-9]{1,8})$/.exec(name) || [, ''])[1].toLowerCase();
const stem = ext ? name.slice(0, -(ext.length + 1)) : name;
const enc = list => list.map(encodeURIComponent).join('/');
const rawOf = list => '/api/raw/' + loc + (list.length ? '/' + enc(list) : '');
const viewOf = list => '/view/' + loc + (list.length ? '/' + enc(list) : '');
// the file in LibreOffice's file system, and where each file there goes in osada (with its etag)
const FILE = '/tmp/office/file' + (ext ? '.' + ext : '');
const places = { [FILE]: { segs, etag: null } };

const $ = id => document.getElementById(id);
const canvas = $('qtcanvas');
let thrPort = null, modified = false, saving = false;

function say(text, cls) { const m = $('msg'); m.textContent = text || ''; m.className = cls || ''; }
function stopLoading(text, cls) {
  $('loading').querySelector('.spinner').style.display = 'none';
  $('loading-text').textContent = text;
  $('loading-text').className = cls || '';
}

$('name').textContent = name || 'office';
document.title = name ? `${name} · office` : 'office';
// back where it was opened from (the folder or the file's page), else the file's page
const back = q.get('back') || '';
$('back').href = back.startsWith('/') && !back.startsWith('//') ? back : loc && segs.length ? viewOf(segs) : '/';
$('back').onclick = e => { if ((modified || saving) && !confirm('Not saved to osada yet. Leave anyway?')) e.preventDefault(); else modified = saving = false; };

// ZetaOffice's module: its files next to this page
var Module = {
  canvas,
  uno_scripts: ['./zeta.js', './office_thread.js'],
  locateFile: (path, prefix) => (prefix || '') + path,
};

async function fetchFile() {
  if (!loc || !name) throw new Error('Open a document from osada: its page in files, then "open in office".');
  const r = await fetch(rawOf(segs), { credentials: 'same-origin', cache: 'no-store' });
  if (r.status === 401) throw new Error('Sign in to osada first, then open the file again.');
  if (!r.ok) throw new Error(`${name}: ${r.status} ${r.statusText}`);
  places[FILE].etag = r.headers.get('etag');
  return new Uint8Array(await r.arrayBuffer());
}

async function put(list, bytes, how) {
  const url = rawOf(list) + (how === 'new' ? '?mode=new' : '');
  const headers = { 'x-shelf': '1' };
  if (how && how !== 'new') headers['if-match'] = how;
  const r = await fetch(url, { method: 'PUT', credentials: 'same-origin', headers, body: bytes });
  let data = null;
  try { data = await r.json(); } catch (e) { /* not JSON */ }
  return { status: r.status, ok: r.ok, data, error: (data && data.error) || `${r.status} ${r.statusText}` };
}

/// LibreOffice saved `url` (a file:// of its own): to osada.
async function stored(url) {
  const p = decodeURIComponent(url.replace(/^file:\/\//, ''));
  let place = places[p];
  if (!place) {
    // saved under another name or format: a new file next to the one opened
    const base = p.split('/').pop();
    const fresh = base.startsWith('file.') ? stem + base.slice(4) : base;
    place = places[p] = { segs: [...dir, fresh], etag: null, fresh: true };
  }
  const bytes = FS.readFile(p);
  const label = place.segs[place.segs.length - 1];
  saving = true;
  say(`saving ${label}…`, 'dim');
  try {
    let r = await put(place.segs, bytes, place.fresh ? 'new' : place.etag || '*');
    if (r.status === 409 && place.fresh) {
      if (!confirm(`${label} is in osada already. Replace it?`)) { say(`${label}: not saved`, 'warn'); return; }
      r = await put(place.segs, bytes, '*');
    } else if (r.status === 412) {
      if (confirm(`${label} changed in osada since it was opened here. Replace it with this version anyway?\n(Cancel keeps both: this version is saved next to it as a copy.)`)) {
        r = await put(place.segs, bytes, '*');
      } else {
        const copy = `${stem} (office ${new Date().toISOString().slice(0, 16).replace('T', ' ').replace(':', '.')})${ext ? '.' + ext : ''}`;
        r = await put([...dir, copy], bytes, 'new');
        if (r.ok) { say(`saved as ${copy}, next to ${label}`, 'warn'); return; }
      }
    }
    if (!r.ok) { say(`${label}: not saved: ${r.error}`, 'bad'); return; }
    place.etag = (r.data && r.data.etag) || null;
    place.fresh = false;
    modified = false;
    say(`saved to osada at ${new Date().toLocaleTimeString()}${place.segs !== segs && label !== name ? ` (as ${label})` : ''}`, 'ok');
  } catch (e) {
    say(`${label}: not saved: ${e.message}`, 'bad');
  } finally {
    saving = false;
  }
}

async function onThread(e, file) {
  const d = e.data;
  switch (d.cmd) {
    case 'thr_running': {
      let bytes;
      try { bytes = await file; } catch (err) { stopLoading(err.message, 'bad'); return; }
      try { FS.mkdir('/tmp/office'); } catch (err) { /* there */ }
      FS.writeFile(FILE, bytes);
      $('loading-text').textContent = `opening ${name}…`;
      thrPort.postMessage({ cmd: 'open', url: 'file://' + FILE, ro: readOnly });
      break;
    }
    case 'ui_ready':
      // the embedded window takes the canvas's size
      window.dispatchEvent(new Event('resize'));
      setTimeout(() => {
        $('loading').style.display = 'none';
        canvas.style.visibility = 'visible';
        $('save').disabled = readOnly;
        say(readOnly ? 'read only: you may not change it in osada' : '', readOnly ? 'warn' : 'dim');
        canvas.focus();
      }, 1000);
      break;
    case 'modified':
      modified = !!d.on;
      if (modified && !saving) say('changed, not saved', 'warn');
      break;
    case 'stored':
      stored(d.url);
      break;
    case 'error':
      if ($('loading').style.display !== 'none' && d.what === 'open') stopLoading(`LibreOffice cannot open ${name}: ${d.message}`, 'bad');
      else say(`${d.what}: ${d.message}`, 'bad');
      break;
    default:
      console.log('office: from the thread', d);
  }
}

$('save').onclick = () => { if (thrPort && !readOnly) thrPort.postMessage({ cmd: 'save' }); };

/* ---- share (as osada's file list does): users of this server, public links ---- */
function el(tag, attrs, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k.startsWith('on')) e.addEventListener(k.slice(2), v); else if (k === 'text') e.textContent = v; else e.setAttribute(k, v === true ? '' : v);
  }
  for (const k of kids.flat()) if (k != null && k !== false) e.append(k);
  return e;
}
async function call(method, url, body) {
  const r = await fetch(url, { method, credentials: 'same-origin', headers: { 'x-shelf': '1', ...(body ? { 'content-type': 'application/json' } : {}) }, body: body ? JSON.stringify(body) : undefined });
  let data = null;
  try { data = await r.json(); } catch (e) { /* none */ }
  if (!r.ok) throw new Error((data && data.error) || `${r.status} ${r.statusText}`);
  return data;
}
const day = t => t ? new Date(t * 1000).toLocaleDateString() : '';
function shareDialog() {
  const path = '/' + segs.join('/');
  const qs = '?loc=' + encodeURIComponent(loc) + '&path=' + encodeURIComponent(path);
  const body = el('div', { class: 'body' }, el('div', { class: 'dim', text: 'loading…' }));
  const close = () => { back.remove(); document.removeEventListener('keydown', esc, true); };
  const esc = e => { if (e.key === 'Escape') { e.preventDefault(); close(); } };
  const win = el('div', { class: 'win dlg', role: 'dialog' },
    el('div', { class: 'titlebar' }, el('span', { class: 'grip', text: '::' }), `share ${name}`, el('span', { class: 'spacer' }), el('button', { class: 'dlg-x', text: 'x', title: 'close (esc)', onclick: close })),
    body, el('div', { class: 'dlg-buttons' }, el('button', { class: 'primary', text: 'close', onclick: close })));
  const back = el('div', { class: 'dlg-back', onclick: e => { if (e.target === back) close(); } }, win);
  document.body.append(back);
  document.addEventListener('keydown', esc, true);
  const fail = e => say(e.message, 'bad');
  // Round 21, as osada's own share window: the search on top (this server, federated instances,
  // public e-mail: a view-only link mailed from your mailbox), then who has it and the links
  const find = el('input', { type: 'search', class: 'share-find', placeholder: 'a name, or anyone\'s address: user@domain', autocomplete: 'off', spellcheck: 'false' });
  const results = el('div', { class: 'share-results', hidden: true });
  const below = el('div', {}, el('div', { class: 'dim', text: 'loading…' }));
  body.replaceChildren(el('div', { class: 'row' }, find, el('button', { class: 'primary', text: 'search', title: 'this server, then federated instances, then public e-mail', onclick: () => search() })), results, below);
  let item = null, asked = 0, timer = null;
  const lbl = t => el('span', { class: 'lbl', text: t });
  async function search() {
    const text = find.value.trim();
    if (text.length < 2) { results.hidden = true; return; }
    const mine = ++asked;
    let r;
    try { r = await call('GET', '/api/people?q=' + encodeURIComponent(text)); } catch (e) { fail(e); return; }
    if (mine !== asked) return;
    const taken = new Set(((item && item.shares) || []).map(x => x.user));
    const local = r.local.map(p => {
      const w = el('input', { type: 'checkbox', title: 'can change it' });
      return el('tr', { class: 'person' }, el('td', {}, el('b', { text: p.display })), el('td', { class: 'mono', text: p.upn }),
        el('td', {}, p.me || taken.has(p.user) ? null : el('label', {}, w, ' can edit')),
        el('td', { class: 'act' }, p.me ? el('span', { class: 'dim', text: '(you)' }) : taken.has(p.user) ? el('span', { class: 'ok', text: 'shared' })
          : el('button', { class: 'primary', text: 'share', onclick: () => call('POST', '/api/shares', { loc, path, user: p.user, write: w.checked }).then(() => { say(`shared with ${p.upn}`, 'ok'); load().then(search); }, fail) })));
    });
    const parts = [lbl('1. on this server'), local.length ? el('table', { class: 'x11' }, el('tbody', {}, local)) : el('div', { class: 'dim', text: 'nobody here matches' }),
      lbl('2. federated instances'), r.federated.length ? el('table', { class: 'x11' }, el('tbody', {}, r.federated.map(p => el('tr', { class: 'person fed' }, el('td', {}, el('b', { text: p.display || p.upn })), el('td', { class: 'mono', text: p.upn }),
        el('td', { class: 'dim', text: `on ${p.instance}, view only` }),
        el('td', { class: 'act' }, ((item && item.federated) || []).some(f => f.upn === p.upn) ? el('span', { class: 'ok', text: 'shared' })
          : el('button', { class: 'primary', text: 'share', onclick: () => call('POST', '/api/shares', { loc, path, user: p.upn, write: false }).then(() => { say(`shared with ${p.upn}`, 'ok'); load().then(search); }, fail) }))))))
        : el('div', { class: 'dim', text: r.federation === 'none' ? 'none set up' : r.federation === 'unreachable' ? 'their instance does not answer' : 'nobody there matches' })];
    if (r.email) {
      const e = r.email;
      parts.push(lbl('3. public e-mail'));
      if (!e.can_send) parts.push(el('div', { class: 'bad', text: `${e.address}: cannot mail a link to it: ${e.why_not}` }));
      else {
        const days = el('input', { type: 'number', min: 1, max: 3650, placeholder: 'never', class: 'num' });
        parts.push(el('div', { class: 'dim', text: `${e.address} is nobody here: they get a link to open it, view only, mailed from your mailbox.` }),
          el('div', { class: 'row' }, el('span', { class: 'dim', text: 'expires after' }), days, el('span', { class: 'dim', text: 'days' }),
            el('button', { class: 'primary', text: 'send link', onclick: ev => {
              ev.target.disabled = true;
              call('POST', '/api/links', { loc, path, days: days.value ? +days.value : null, send_to: e.address })
                .then(() => { say(`public link sent to ${e.address}`, 'ok'); find.value = ''; results.hidden = true; load(); }, err => { fail(err); ev.target.disabled = false; });
            } })));
      }
    } else if (text.includes('@') && !local.length) parts.push(el('div', { class: 'dim', text: 'not an e-mail address' }));
    results.replaceChildren(...parts);
    results.hidden = false;
  }
  find.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(search, 250); });
  find.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); clearTimeout(timer); search(); } });
  setTimeout(() => find.focus(), 0);
  async function load() {
    const it = item = await call('GET', '/api/shares/item' + qs);
    const users = it.shares.map(x => {
      const w = el('input', { type: 'checkbox', checked: x.write, title: 'can change it' });
      w.addEventListener('change', () => call('POST', '/api/shares', { loc, path, user: x.user, write: w.checked }).then(load, fail));
      return el('tr', {}, el('td', {}, el('b', { text: x.display || x.user })), el('td', { class: 'mono', text: x.upn || x.user }), el('td', {}, el('label', {}, w, ' can edit')), el('td', { class: 'dim', text: day(x.created) }),
        el('td', { class: 'act' }, el('button', { text: 'remove', title: `stop sharing with ${x.upn || x.user}`, onclick: () => call('DELETE', '/api/shares/' + x.id).then(load, fail) })));
    });
    // people of federated instances (Round 22): view only
    users.push(...(it.federated || []).map(f => el('tr', { class: 'fed' }, el('td', {}, el('b', { text: f.upn })), el('td', { class: 'dim', text: `on ${f.instance}` }), el('td', { class: 'dim', text: 'view only' }), el('td', { class: 'dim', text: day(f.created) }),
      el('td', { class: 'act' }, el('button', { text: 'remove', title: `stop sharing with ${f.upn}`, onclick: () => call('DELETE', '/api/fedshares/' + f.id).then(load, fail) })))));
    const linkRow = l => {
      const url = new URL(l.url, location.origin).href;
      const expired = l.expires && l.expires * 1000 < Date.now();
      return el('tr', {}, l.sent_to ? el('td', {}, 'public link sent to ', el('b', { class: 'mono', text: l.sent_to })) : null,
        el('td', { class: 'url' }, el('a', { href: url, target: '_blank', rel: 'noopener', text: url })),
        el('td', { class: expired ? 'bad' : 'dim', text: l.expires ? (expired ? `expired ${day(l.expires)}` : `until ${day(l.expires)}`) : 'no expiry' }),
        el('td', { class: 'dim', text: `opened ${l.hits} time${l.hits === 1 ? '' : 's'}` }),
        el('td', { class: 'act' }, el('button', { text: 'copy', onclick: () => navigator.clipboard.writeText(url).then(() => say('link copied', 'ok'), () => say(url, 'dim')) }),
          ' ', el('button', { class: 'danger', text: 'remove', title: 'the link stops working', onclick: () => call('DELETE', '/api/links/' + l.id).then(load, fail) })));
    };
    const sent = it.links.filter(l => l.sent_to).map(linkRow), links = it.links.filter(l => !l.sent_to).map(linkRow);
    const days = el('input', { type: 'number', min: 1, max: 3650, placeholder: 'never', class: 'num' });
    below.replaceChildren(
      lbl('shared with'),
      users.length || sent.length ? el('table', { class: 'x11' }, el('tbody', {}, users, sent)) : el('div', { class: 'dim', text: 'not shared with anyone' }),
      lbl('links: anyone with the link can open it, no login'),
      links.length ? el('table', { class: 'x11' }, el('tbody', {}, links)) : el('div', { class: 'dim', text: 'no links' }),
      el('div', { class: 'row' }, el('span', { class: 'dim', text: 'expires after' }), days, el('span', { class: 'dim', text: 'days' }),
        el('button', { class: 'primary', text: 'create link', onclick: async () => {
          try {
            const r = await call('POST', '/api/links', { loc, path, days: days.value ? +days.value : null });
            const url = new URL(r.url, location.origin).href;
            await navigator.clipboard.writeText(url).then(() => say('link made and copied', 'ok'), () => say('link made', 'ok'));
            load();
          } catch (e) { fail(e); }
        } })));
  }
  load().catch(e => below.replaceChildren(el('div', { class: 'bad', text: e.message })));
}
$('share').onclick = shareDialog;
// sharing is the owner's (as in the file list)
if (loc && segs.length && !q.get('ro')) {
  call('GET', '/api/ls/' + loc + (dir.length ? '/' + enc(dir) : '')).then(v => { $('share').hidden = !v.owner; }, () => {});
}
window.addEventListener('beforeunload', e => { if (modified || saving) { e.preventDefault(); e.returnValue = ''; } });
// the wheel scrolls the document, not the page
canvas.addEventListener('wheel', e => e.preventDefault(), { passive: false });
// Qt learns of a changed zoom from a second resize
let lastRatio = window.devicePixelRatio;
window.addEventListener('resize', () => setTimeout(() => {
  if (lastRatio && lastRatio !== window.devicePixelRatio) { lastRatio = false; window.dispatchEvent(new Event('resize')); }
  else if (!lastRatio) lastRatio = window.devicePixelRatio;
}, 100));

if (!window.crossOriginIsolated) {
  stopLoading('This page needs the headers Cross-Origin-Opener-Policy: same-origin and Cross-Origin-Embedder-Policy: require-corp (admin → web → its site → headers).', 'bad');
} else if (!loc || !name) {
  stopLoading('Open a document from osada: its page in files, then "open in office".', 'warn');
} else {
  const file = fetchFile();
  file.catch(() => {});
  const s = document.createElement('script');
  s.src = 'soffice.js';
  s.onload = () => Module.uno_main.then(port => { thrPort = port; port.onmessage = e => onThread(e, file); });
  s.onerror = () => stopLoading('LibreOffice\'s files are not on this site (soffice.js)', 'bad');
  document.body.appendChild(s);
}
