/* osada's service worker, /sw.js for all of the site: what this browser keeps for when osada
   at home does not answer. util.js registers it (https, or this machine).
   - /static/ files by version, for good: every file the newest page names is fetched in the
     background when that page comes; versions no kept page names are dropped.
   - The pages (the app's, a link's: the same for every address of their kind) and the JSON
     answers to GETs: home first, the last good answer kept. A page home is slow with (past WAIT)
     comes from the copy unmarked: it is the same page, and its own requests find out about home.
     Slow JSON is waited for (folder sizes, backups over SFTP take their time).
   - A file read whole (or a range that is all of it), up to FILE_MAX: home first, kept up to
     FILES_MAX in all, the least recently used going first. Ranges of a kept file are cut from it.
   What comes from the copies has x-shelf-offline: 1, and so does the 503 for what is not kept.
   Only GETs are answered here. Logging out, or another user, drops the account's copies
   (the pages and /static/ are the same for everybody). */
'use strict';

const V = 1;
const STATIC = 'osada-static-' + V, PAGES = 'osada-pages-' + V, JSONS = 'osada-json-' + V, FILES = 'osada-files-' + V;
const FILES_MAX = 1 << 30, FILE_MAX = 256 << 20;
// a range of a small file (music) gets the whole file fetched aside, for next time
const ASIDE_MAX = 32 << 20;
const WAIT = 6000;

self.addEventListener('install', () => self.skipWaiting());

self.addEventListener('activate', e => e.waitUntil((async () => {
  for (const k of await caches.keys()) if (k.startsWith('osada-') && ![STATIC, PAGES, JSONS, FILES].includes(k)) await caches.delete(k);
  await self.clients.claim();
  // the pages open now came before this worker: keep theirs, and whose they are
  const done = new Set();
  for (const c of await self.clients.matchAll({ type: 'window' })) {
    const u = new URL(c.url), kind = u.pathname.startsWith('/s/') ? 'public' : 'app';
    if (done.has(kind) || u.origin !== location.origin) continue;
    done.add(kind);
    try {
      // whose they are first: the page's /static/ files take a while
      if (kind === 'app') await keepAnswer(false, 'json', '/api/me', await fetch('/api/me', { credentials: 'same-origin' }));
      // the app's page: at /files/home/ (inside osada 0.2, / is its portal)
      await keepPage(kind, await fetch(kind === 'app' ? '/files/home/' : u.pathname, { credentials: 'same-origin' }));
    } catch (x) { /* next time */ }
  }
})()));

self.addEventListener('fetch', e => {
  const req = e.request, url = new URL(req.url);
  if (url.origin !== location.origin) return;
  if (req.method !== 'GET') {
    if (req.method === 'POST' && url.pathname === '/api/logout') e.waitUntil(forget());
    return;
  }
  const kind = classify(req, url);
  if (kind) e.respondWith(answer(e, req, url, kind));
});

/* static | page:app | page:public | json | file | kept, or null: not ours */
function classify(req, url) {
  const p = url.pathname;
  // osada 0.2's pages and API (sign-in, account, admin portal) and sites at /web/ are never kept on a device
  if (/^\/(signin|account|admin|static2|web)(\/|$)|^\/api\/(signin|signout|account|admin)(\/|$)/.test(p)) return null;
  if (p === '/api/kept') return 'kept';
  if (p.startsWith('/static/')) return url.searchParams.has('v') ? 'static' : null;
  const pub = /^\/s\/[^/]+\/(ls|info|md|raw)(\/|$)/.exec(p);
  if (pub) return pub[1] === 'raw' ? 'file' : 'json';
  if (/^\/api\/(raw|thumb)\/|^\/api\/wiki\/[^/]+\/media\//.test(p)) return 'file';
  // the phone sync, API keys and backups are not for a page; an export is made fresh, not kept
  if (p.startsWith('/api/')) return /^\/api\/(sync|v1|snap)(\/|$)|^\/api\/wiki\/[^/]+\/export$/.test(p) ? null : 'json';
  if (req.mode !== 'navigate' || /^\/(oidc|\.well-known)\//.test(p) || p === '/sw.js' || p === '/favicon.ico') return null;
  return p.startsWith('/s/') ? 'page:public' : 'page:app';
}

async function answer(e, req, url, kind) {
  if (kind === 'kept') return kept();
  if (kind === 'static') return fromStatic(e, req, url);
  if (kind === 'file') return file(e, req, url);
  return homeFirst(e, req, url, kind);
}

/* not home's answer: the edge's (or the phone's) word that home is away, or the proxy in front failing */
function away(r) {
  return r.headers.has('x-shelf-offline') || ((r.status === 502 || r.status === 504) && !/json/.test(r.headers.get('content-type') || ''));
}
function marked(r, init) {
  const h = new Headers(r.headers);
  h.set('x-shelf-offline', '1');
  return new Response(r.body, Object.assign({ status: r.status, statusText: r.statusText, headers: h }, init));
}
function notKept(json) {
  return json
    ? new Response('{"error":"not kept on this device"}', { status: 503, headers: { 'content-type': 'application/json', 'cache-control': 'no-store', 'x-shelf-offline': '1' } })
    : new Response('not kept on this device\n', { status: 503, headers: { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store', 'x-shelf-offline': '1' } });
}
const keyOf = r => { const u = new URL(r.url); return u.pathname + u.search; };
const at = (name, key) => caches.open(name).then(c => c.match(key, { ignoreVary: true }));

/* ---- /static/: a version never changes ---- */
async function fromStatic(e, req, url) {
  const key = url.pathname + url.search;
  const hit = await at(STATIC, key);
  if (hit) return hit;
  const r = await fetch(req);
  if (r.status === 200 && !away(r)) e.waitUntil(caches.open(STATIC).then(c => c.put(key, r.clone())).catch(() => {}));
  return r;
}

/* ---- pages and JSON: home first ---- */
async function homeFirst(e, req, url, kind) {
  const page = kind.startsWith('page:'), key = page ? '/__' + kind.replace(':', '/') : url.pathname + url.search;
  const home = fetch(req).catch(() => null);
  let timer;
  const first = page ? await Promise.race([home, new Promise(ok => { timer = setTimeout(ok, WAIT, 'late'); })]) : await home;
  clearTimeout(timer);
  const fresh = r => r && !away(r);
  if (first !== 'late' && fresh(first)) { e.waitUntil(keepAnswer(page, kind, key, first.clone())); return first; }
  const copy = await at(page ? PAGES : JSONS, key);
  if (first === 'late') {
    // slow: the copy now, home's answer kept when it comes
    e.waitUntil(home.then(r => fresh(r) && keepAnswer(page, kind, key, r.clone())));
    if (copy) return copy;
    const r = await home;
    return r || Response.error();
  }
  if (copy) return marked(copy);
  // the edge's own copy, or nothing
  return first && first.ok ? first : page ? (first || Response.error()) : notKept(true);
}

async function keepAnswer(page, kind, key, r) {
  if (page) return r.status === 200 ? keepPage(kind.slice(5), r) : null;
  if (r.status === 404 || r.status === 410) return caches.open(JSONS).then(c => c.delete(key));
  if (r.status !== 200 || !/json/.test(r.headers.get('content-type') || '') || away(r)) return null;
  if (key === '/api/me') {
    const me = await r.clone().json().catch(() => null);
    if (me && me.username) await sameUser(me.username);
  }
  // opened only now: another user's copies may just have gone, cache and all
  return (await caches.open(JSONS)).put(key, r);
}

/* a fresh page: kept, and the /static/ files it names fetched */
const DONE = new Set();
async function keepPage(kind, r) {
  if (r.status !== 200 || !/text\/html/.test(r.headers.get('content-type') || '') || away(r)) return;
  const html = await r.clone().text();
  // only osada 0.1's own pages (osada 0.2's portal, account and admin are never kept)
  if (!/<meta name="osada-assets"/.test(html)) return;
  await (await caches.open(PAGES)).put('/__page/' + kind, r);
  const m = /<meta name="osada-assets" content="([^"]*)"/.exec(html);
  if (!m || DONE.has(m[1])) return;
  const c = await caches.open(STATIC);
  // what a kept page names stays
  const want = new Set();
  for (const k of ['app', 'public']) {
    const pg = k === kind ? html : await at(PAGES, '/__page/' + k).then(x => (x ? x.text() : ''));
    for (const u of assetsOf(pg)) want.add(u);
  }
  const have = new Set((await c.keys()).map(keyOf));
  for (const u of have) if (!want.has(u)) await c.delete(u);
  for (const u of assetsOf(html)) {
    if (have.has(u)) continue;
    try {
      const a = await fetch(u, { credentials: 'same-origin' });
      if (a.status !== 200 || away(a)) return;
      await c.put(u, a);
    } catch (x) { return; }   // home went away: the next page goes on
  }
  DONE.add(m[1]);
}
function assetsOf(html) {
  const m = /<meta name="osada-assets" content="([^"]*)"/.exec(html || '');
  return m ? m[1].split(' ').map(kv => kv.split('=')).filter(x => x.length === 2 && x[0] && x[1]).map(([n, v]) => '/static/' + n + '?v=' + v) : [];
}

/* ---- files ---- */
function fileKey(url) {
  const u = new URL(url);
  u.searchParams.delete('dl');
  return u.pathname + u.search;
}

async function file(e, req, url) {
  const key = fileKey(url), range = req.headers.get('range');
  const r = await fetch(req).catch(() => null);
  if (r && !away(r)) {
    const cr = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(r.headers.get('content-range') || '');
    // a copy only of what fits (a clone nobody reads is held in memory), and not of downloads
    const fits = r.headers.has('content-length') && +r.headers.get('content-length') <= FILE_MAX && !url.searchParams.has('dl');
    if (r.status === 200 && fits) e.waitUntil(keepFile(key, r.clone()));
    else if (r.status === 206 && fits && cr && +cr[1] === 0 && +cr[2] + 1 === +cr[3]) e.waitUntil(keepFile(key, r.clone(), true));
    else if (r.status === 206 && cr && +cr[3] <= ASIDE_MAX) e.waitUntil(aside(key, url));
    else if (r.status === 404 || r.status === 410) e.waitUntil(dropFile(key));
    return r;
  }
  const copy = await at(FILES, key);
  if (!copy) return r && r.ok ? r : notKept(false);
  e.waitUntil(touch(key));
  return range ? cut(copy, range) : marked(copy);
}

async function aside(key, url) {
  if (await at(FILES, key)) return;
  const u = new URL(url);
  u.searchParams.delete('dl');
  const r = await fetch(u.href, { credentials: 'same-origin' }).catch(() => null);
  if (r && r.status === 200 && !away(r)) await keepFile(key, r);
}

/* a range of a kept copy */
async function cut(copy, range) {
  const b = await copy.blob(), n = b.size;
  if (!n) return marked(copy);
  const m = /^bytes=(\d*)-(\d*)$/.exec(range.trim());
  let a = -1, z = -1;
  if (m && m[1] !== '') { a = +m[1]; z = m[2] !== '' ? Math.min(+m[2], n - 1) : n - 1; } else if (m && m[2] !== '') { a = Math.max(0, n - +m[2]); z = n - 1; }
  if (a < 0 || a > z) return new Response('', { status: 416, headers: { 'content-range': 'bytes */' + n, 'x-shelf-offline': '1' } });
  const h = new Headers(copy.headers);
  h.set('content-range', 'bytes ' + a + '-' + z + '/' + n);
  h.set('content-length', String(z - a + 1));
  h.set('x-shelf-offline', '1');
  return new Response(b.slice(a, z + 1), { status: 206, headers: h });
}

/* the index of kept files: key -> [size, last used]; written after every change, one at a time */
let IDX = null, writing = Promise.resolve();
async function index() {
  if (IDX) return IDX;
  const c = await caches.open(FILES);
  const r = await c.match('/__index');
  const idx = (r && await r.json().catch(() => null)) || {};
  // copies the index lost (the worker stopped before writing it) count as least recently used
  for (const q of await c.keys()) {
    const k = keyOf(q);
    if (k !== '/__index' && !idx[k]) { const x = await c.match(q); idx[k] = [+(x && x.headers.get('content-length')) || 0, 0]; }
  }
  for (const k of Object.keys(idx)) if (!(await c.match(k))) delete idx[k];
  return (IDX = IDX || idx);
}
function writeIndex() {
  writing = writing.then(async () => (await caches.open(FILES)).put('/__index', new Response(JSON.stringify(IDX || {}), { headers: { 'content-type': 'application/json' } }))).catch(() => {});
  return writing;
}

async function keepFile(key, r, whole) {
  const size = +r.headers.get('content-length');
  if (!(size >= 0) || size > FILE_MAX) return;
  if (whole) {
    // a range that is all of it: kept as the plain answer
    const h = new Headers(r.headers);
    h.delete('content-range');
    r = new Response(await r.blob(), { status: 200, headers: h });
  }
  const c = await caches.open(FILES), idx = await index();
  await c.put(key, r);
  idx[key] = [size, Date.now()];
  let total = Object.values(idx).reduce((a, x) => a + x[0], 0);
  for (const [k, x] of Object.entries(idx).sort((a, b) => a[1][1] - b[1][1])) {
    if (total <= FILES_MAX) break;
    if (k === key) continue;
    await c.delete(k);
    delete idx[k];
    total -= x[0];
  }
  await writeIndex();
}
async function touch(key) {
  const idx = await index();
  if (idx[key]) { idx[key][1] = Date.now(); await writeIndex(); }
}
async function dropFile(key) {
  const idx = await index();
  await (await caches.open(FILES)).delete(key);
  delete idx[key];
  await writeIndex();
}

/* ---- whose copies ---- */
async function sameUser(name) {
  const c = await caches.open(PAGES), r = await c.match('/__user'), was = r ? await r.text() : '';
  if (was === name) return;
  if (was) await forgetFiles();
  await c.put('/__user', new Response(name));
}
async function forgetFiles() {
  await caches.delete(JSONS);
  await caches.delete(FILES);
  IDX = null;
}
async function forget() {
  await forgetFiles();
  await (await caches.open(PAGES)).delete('/__user');
}

/* ---- /api/kept: what can be opened without home ---- */
async function kept() {
  const keys = async name => (await (await caches.open(name)).keys()).map(keyOf);
  const files = (await keys(FILES)).filter(k => k !== '/__index');
  const lists = (await keys(JSONS)).filter(k => /^\/api\/ls\/|^\/s\/[^/]+\/ls(\/|$)/.test(k));
  return new Response(JSON.stringify({ files, lists }), { headers: { 'content-type': 'application/json', 'cache-control': 'no-store', 'x-shelf-offline': '1' } });
}
