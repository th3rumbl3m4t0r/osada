/* osada web app: router, pages, dialogs. A plain script after util.js,
   viewer.js, csv.js and rtf.js. Pages are {el, title, key(e), save(),
   dirty(), destroy()}; the router swaps them in #app. */
'use strict';

const S = { me: null, page: null, url: '', seq: 0 };

/* ---- urls: /files/<loc>/<path>, /view/…, /edit/…; paths are /a/b ---- */
function encP(p) { const e = encPath(p); return e ? '/' + e : ''; }
/* the office suite (Round 18): what it opens, and its page for a file; back: here */
function officeKind(name) { return !!S.me.office && /^(doc|sheet|slides|rtf|csv)$/.test(kindOf(name)); }
function officeUrl(loc, path, write) {
  return S.me.office + '?' + new URLSearchParams({ loc, path, ...(write ? {} : { ro: '1' }), back: location.pathname + location.search });
}
const U = {
  files: (loc, p) => '/files/' + loc + '/' + encPath(p),
  view: (loc, p) => '/view/' + loc + encP(p),
  edit: (loc, p) => '/edit/' + loc + encP(p),
  raw: (loc, p) => '/api/raw/' + loc + encP(p),
  ls: (loc, p) => '/api/ls/' + loc + encP(p),
  du: (loc, p) => '/api/du/' + loc + encP(p),
  // earlier versions, from the backups (src/versions.rs)
  versions: (loc, p) => '/api/versions/' + loc + encP(p),
  snapLs: (snap, loc, p) => '/api/snap/' + snap + '/ls/' + loc + encP(p),
  snapRaw: (snap, loc, p) => '/api/snap/' + snap + '/raw/' + loc + encP(p),
  version: (snap, loc, p) => '/version/' + snap + '/' + loc + encP(p),
};
function itemUrl(loc, p, dir) { return dir ? U.files(loc, p) : U.view(loc, p); }
function absUrl(u) { return /^https?:\/\//.test(u) ? u : location.origin + u; }
function decodeSegs(rest) {
  return '/' + (rest || '').split('/').filter(Boolean).map(s => { try { return decodeURIComponent(s); } catch (e) { return s; } }).join('/');
}
function checkName(n) {
  if (!n || !n.trim()) return tx('a name is needed');
  if (n.includes('/')) return tx('no slashes in names');
  if (n === '.' || n === '..') return tx('not a usable name');
  return null;
}

/* PUT a body; JSON back, or an ApiError */
async function putRaw(url, body, headers) {
  let r;
  try {
    r = await fetch(url, { method: 'PUT', credentials: 'same-origin', headers: Object.assign({ 'x-shelf': '1' }, headers || {}), body });
  } catch (e) { throw new ApiError(tx('server unreachable'), 0); }
  let data = null;
  try { data = await r.json(); } catch (e) { /* not JSON */ }
  if (!r.ok) {
    if (r.status === 401 && window.onUnauthorized) window.onUnauthorized();
    throw new ApiError(txErr((data && data.error) || (r.status + ' ' + r.statusText)), r.status);
  }
  return data;
}

function setStatus(t) { const el = $('#status'); if (el) el.textContent = t || ''; }
function field(label, input, cls) { return h('div', { class: 'field' + (cls ? ' ' + cls : '') }, h('label', { class: 'lbl', text: label }), input); }
function win(title, body, cls) {
  return h('section', { class: 'win' + (cls ? ' ' + cls : '') },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), title), h('div', { class: 'body' }, body));
}
function btn(text, title, onclick, cls) { return h('button', { text, title: title || null, class: cls || null, onclick }); }
function genPassword() {
  const a = 'abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789';
  return Array.from(crypto.getRandomValues(new Uint32Array(16)), x => a[x % a.length]).join('');
}
function fmtDate(ts) { return ts ? fmtTime(ts).slice(0, 10) : ''; }
/* "home" is a word of the UI; storage keeps its own name */
function locName(x) { return x.loc === 'home' ? tx('home') : x.loc_name; }
/* a storage limit as an admin types it: 50G, 1536G, 500M */
function fmtLimit(n) {
  for (const [u, s] of [['T', 40], ['G', 30], ['M', 20]]) if (n >= 2 ** s && n % 2 ** s === 0) return n / 2 ** s + u;
  return +(n / 2 ** 30).toFixed(2) + 'G';
}

/* ---- router ---- */
// maildot: the webmail on the mail server (S.me.maildot, only with a mail address), in its own tab
// inside osada 0.2 (S.me.portal): its account page, and its admin portal for those with a role
// (S.me.staff); this module's own settings are "admin" at /osada-admin
const NAV = [['files', '/files/home/'], ['shared', '/shared'], ['links', '/links'], ['wiki', '/wiki'], ['maildot', null], ['apptab', '/apptab'], ['settings', '/settings'], ['account', '/account'], ['admin', '/osada-admin'], ['portal', '/admin']];
const ROUTES = [
  [/^\/login$/, () => loginPage()],
  [/^\/files\/([^/]+)(\/.*)?$/, m => filesPage(m[1], decodeSegs(m[2]))],
  [/^\/view\/([^/]+)(\/.*)?$/, m => viewPage(m[1], decodeSegs(m[2]))],
  [/^\/edit\/([^/]+)(\/.*)?$/, (m, parts) => (offline() ? { redirect: U.view(m[1], decodeSegs(m[2])) } : parts.hold('edit').then(x => x.editPage(m[1], decodeSegs(m[2]), parts)))],
  [/^\/version\/([^/]+)\/([^/]+)(\/.*)?$/, (m, parts) => parts.hold('versions').then(x => x.versionPage(m[1], m[2], decodeSegs(m[3])))],
  [/^\/pictures$/, (m, parts) => parts.hold('pictures').then(x => x.picturesPage())],
  [/^\/shared$/, () => sharedPage()],
  [/^\/links$/, () => linksPage()],
  [/^\/apptab$/, () => apptabPage('apptab')],
  [/^\/proxy\/([a-z][a-z0-9-]*)$/, m => apptabPage(m[1])],
  [/^\/settings$/, (m, parts) => parts.hold('settings').then(x => x.settingsPage())],
  // inside osada 0.2 the admin page is one, at /admin (D-U14)
  [/^\/osada-admin$/, (m, parts) => (S.me && S.me.portal ? (location.replace('/admin/files'), new Promise(() => {})) : parts.hold('admin').then(x => x.adminPage()))],
  [/^\/wiki$/, (m, parts) => parts.hold('wiki').then(x => x.wikiList())],
  [/^\/wiki\/new$/, (m, parts) => parts.hold('wiki').then(x => x.wikiEdit(null, parts))],
  [/^\/wiki\/([^/]+)$/, (m, parts) => parts.hold('wiki').then(x => x.wikiPage(decodeURIComponent(m[1]), parts))],
  [/^\/wiki\/([^/]+)\/edit$/, (m, parts) => parts.hold('wiki').then(x => x.wikiEdit(decodeURIComponent(m[1]), parts))],
  [/^\/wiki\/([^/]+)\/history$/, (m, parts) => parts.hold('wiki').then(x => x.wikiHistory(decodeURIComponent(m[1]), parts))],
];

function navigate(url, replace) {
  if (url !== location.pathname + location.search) history[replace ? 'replaceState' : 'pushState'](null, '', url);
  return route();
}

async function loadMe() {
  try { S.me = await api('GET', '/api/me'); } catch (e) { if (e.status === 401) S.me = null; else throw e; }
  // the account's language, once picked, wins over this browser's
  if (S.me && S.me.lang && S.me.lang !== LANG) await setLang(S.me.lang);
  paintTop();
}

/* the language picker in the top bar (i18n.js); logged in, it is the account's default */
window.onLangPick = async code => {
  await setLang(code);
  // and once more who I am: the copies kept for when home is away (sw.js, the app) say this language too
  if (S.me && !offline()) { S.me.lang = code; api('PUT', '/api/me/lang', { lang: code }).then(() => api('GET', '/api/me')).catch(fail); }
  paintTop();
  if (UP.win) upTitle();
  if (S.page && S.page.dirty && S.page.dirty()) toast(tx('the editor switches language when it is opened again'), 'warn');
  else route();
};

async function route() {
  const url = location.pathname + location.search;
  if (S.page && url !== S.url && S.page.dirty && S.page.dirty()) {
    if (!(await confirmBox(tx('unsaved changes'), tx('leave this page? the changes are lost.'), tx('leave'), true))) {
      if (location.pathname + location.search !== S.url) history.pushState(null, '', S.url);
      return;
    }
  }
  const seq = ++S.seq;
  closePage();
  S.url = url;
  const path = location.pathname;
  if (!S.me && path !== '/login') {
    try { await loadMe(); } catch (e) { showPage(errorPage(e)); return; }
    if (seq !== S.seq) return;
    if (!S.me) { history.replaceState(null, '', '/login?next=' + encodeURIComponent(url)); S.url = location.pathname + location.search; }
  }
  if (location.pathname === '/login' && S.me) { history.replaceState(null, '', '/files/home/'); S.url = '/files/home/'; }
  if (location.pathname === '/') return navigate('/files/home/', true);
  markNav();
  let fn = null, m = null;
  for (const [re, f] of ROUTES) { m = re.exec(location.pathname); if (m) { fn = f; break; } }
  // the parts (util.js) a page holds go with it, also when making it fails
  let page;
  const parts = holder();
  busy(true);
  try { page = fn ? await fn(m, parts) : errorPage(new Error(tx('no such page'))); } catch (e) { page = errorPage(e); } finally { busy(false); }
  if (seq !== S.seq) { if (page && page.destroy) page.destroy(); parts.done(); return; }
  page.parts = parts;
  if (page.redirect) { parts.done(); return navigate(page.redirect, true); }
  showPage(page);
}

function closePage() {
  if (S.page && S.page.destroy) S.page.destroy();
  if (S.page && S.page.parts) S.page.parts.done();
  S.page = null;
}

function showPage(page) {
  const main = clear($('#app'));
  main.className = page.cls || '';
  main.appendChild(page.el);
  S.page = page;
  document.title = (page.title ? page.title + ' · ' : '') + 'osada';
  setStatus(page.status || '');
  if (page.shown) page.shown();
}

function errorPage(e) {
  const back = S.me ? h('a', { class: 'btn-link', href: '/files/home/', text: tx('home') }) : null;
  return { el: win(tx('error'), [h('p', { class: 'bad', text: e.message || String(e) }), back], 'narrow'), title: tx('error') };
}

function paintTop() {
  const nav = clear($('#nav')), who = clear($('#who'));
  document.body.classList.toggle('anon', !S.me);
  if (!S.me) return;
  // inside osada 0.2 (D-U13): its header, the same on every page: the portal, the apps; admin and the user
  if (S.me.apps) {
    const brand = $('.brand');
    if (brand) brand.hidden = true;
    nav.appendChild(h('a', { href: '/', text: 'portal', class: 'portal', 'data-sec': 'portal' }));
    // the node this page came from (T14): "// home"
    nav.appendChild(h('span', { class: 'nodename dim', title: tx('the node you are connected to'), text: '// ' + (S.me.node_name || S.me.host || '') }));
    for (const a of S.me.apps) {
      if (a.external) nav.appendChild(h('a', { href: a.href, text: a.title, target: '_blank', rel: 'noopener', 'data-sec': a.name, class: 'app' }));
      else nav.appendChild(h('a', { href: a.href, text: a.name === 'files' || a.name === 'shared' || a.name === 'links' || a.name === 'wiki' ? tx(a.name) : a.title,
        'data-sec': a.kind === 'proxy' ? (a.name === 'apptab' ? 'apptab' : 'proxy-' + a.name) : a.name, class: 'app' }));
    }
    // admin at the end of the left side, never hidden
    if (S.me.admin_link) nav.appendChild(h('a', { href: '/admin', text: tx('admin'), class: 'admin' }));
    who.append(h('a', { href: '/account', text: S.me.username, class: 'navlink', title: tx('account') }),
      h('a', { href: '/settings', text: tx('settings'), class: 'navlink', 'data-sec': 'settings' }), btn(tx('log out'), null, logout));
    markNav();
    fitNav(nav, who);
    return;
  }
  for (const [name, href] of NAV) {
    if (name === 'maildot') {
      if (S.me.maildot) nav.appendChild(h('a', { href: S.me.maildot, text: 'maildot', target: '_blank', rel: 'noopener', 'data-sec': name }));
      continue;
    }
    // inside osada 0.2: a tab for every proxy module this account may use (apptab is one)
    if (name === 'apptab' && S.me.proxies) {
      for (const p of S.me.proxies) {
        const href = p.name === 'apptab' ? '/apptab' : '/proxy/' + p.name;
        nav.appendChild(h('a', { href, text: p.title, 'data-sec': p.name === 'apptab' ? 'apptab' : 'proxy-' + p.name }));
      }
      continue;
    }
    if (name === 'account' && !S.me.portal) continue;
    if (name === 'portal' && !S.me.staff) continue;
    if ((name !== 'admin' || S.me.admin) && (name !== 'apptab' || S.me.apptab)) nav.appendChild(h('a', { href, text: tx(name), 'data-sec': name }));
  }
  who.append(h('span', { class: 'dim', text: S.me.username }), btn(tx('log out'), null, logout));
  markNav();
}
/// The header's app links that would come within 20 px of the user's part at the right are not
/// shown (they are on the portal); from the last one back. Again when the window changes.
function fitNav(nav, who) {
  const apps = [...nav.querySelectorAll('a.app')];
  for (const a of apps) a.hidden = false;
  if (!who.children.length) return;
  const room = () => {
    const last = [...nav.children].filter(x => !x.hidden).pop();
    return !last || last.getBoundingClientRect().right + 20 <= who.getBoundingClientRect().left;
  };
  for (let i = apps.length - 1; i >= 0 && !room(); i--) apps[i].hidden = true;
}
window.addEventListener('resize', () => requestAnimationFrame(() => { if (S.me && S.me.apps) fitNav($('#nav'), $('#who')); }));

function markNav() {
  const seg = location.pathname.split('/')[1];
  const sec = seg === 'view' || seg === 'edit' || seg === 'pictures' ? 'files' : seg === 'osada-admin' ? 'admin' : seg === 'proxy' ? 'proxy-' + location.pathname.split('/')[2] : seg;
  for (const a of $$('#nav a')) a.classList.toggle('on', a.dataset.sec === sec);
}

async function logout() {
  if (S.page && S.page.dirty && S.page.dirty() && !(await confirmBox(tx('unsaved changes'), tx('log out and lose the changes?'), tx('log out'), true))) return;
  try { await api('POST', '/api/logout'); } catch (e) { /* gone anyway */ }
  S.me = null;
  closePage();
  paintTop();
  navigate('/login', true);
}

/* home stopped or started answering (util.js): the page again, read only or not; not over
   unsaved changes, a dialog, or something playing */
OFF.changed = on => {
  if (!on) toast(tx('osada at home answers again'));
  if (S.page && !dlgOpen && !(S.page.dirty && S.page.dirty()) && !S.page.stay) route();
};

window.onUnauthorized = () => {
  if (!S.me) return;
  // an editor with changes stays open: the user can log in elsewhere and save again
  if (S.page && S.page.dirty && S.page.dirty()) { toast(tx('logged out: log in again in another tab, then save'), 'err'); return; }
  S.me = null;
  paintTop();
  navigate('/login?next=' + encodeURIComponent(S.url), true);
};

/* ---- login ---- */
function loginPage() {
  // inside osada 0.2: its sign-in page (second factors), which comes back to ?next=
  const signin = document.documentElement.dataset.signin;
  if (signin) { location.replace(signin + location.search); return { el: h('div'), title: tx('log in') }; }
  const q = new URLSearchParams(location.search);
  const next = q.get('next') || '/files/home/';
  // a sign-in for another program (oidc.rs): the server finishes it, after the login
  const oidc = next.startsWith('/oidc/');
  const to = oidc && /^[a-z0-9][a-z0-9._-]{0,31}$/.test(q.get('to') || '') ? q.get('to') : null;
  const user = h('input', { type: 'text', autocomplete: 'username', spellcheck: false, autocapitalize: 'off' });
  const pass = h('input', { type: 'password', autocomplete: 'current-password' });
  const msg = h('div', { class: 'bad login-msg' });
  const go = h('button', { class: 'primary', type: 'submit', text: tx('log in') });
  const form = h('form', { class: 'win login' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), tx('log in')),
    h('div', { class: 'body' }, to ? h('p', { class: 'dim', text: tx('to continue to {name}', { name: to }) }) : null,
      field(tx('user name or UPN'), user), field(tx('password'), pass), msg, h('div', { class: 'row-end' }, go)));
  form.addEventListener('submit', async e => {
    e.preventDefault();
    go.disabled = true;
    msg.textContent = '';
    try {
      await api('POST', '/api/login', { username: user.value.trim(), password: pass.value });
      if (oidc) { location.replace(next); return; }
      await loadMe();
      navigate(next.startsWith('/') && !next.startsWith('//') ? next : '/files/home/', true);
    } catch (err) {
      msg.textContent = err.message;
      pass.select();
    } finally { go.disabled = false; }
  });
  return { el: h('div', { class: 'login-page' }, form), title: tx('log in'), shown: () => user.focus() };
}

/* ---- places: home, sftp storage, things shared with me ---- */
async function loadPlaces() {
  const [mounts, shares, groups] = await Promise.all([api('GET', '/api/mounts'), api('GET', '/api/shares'), api('GET', '/api/groups/mine').catch(() => [])]);
  return {
    // the folders of the groups one is in (D-U29)
    groups: groups.map(g => ({ loc: g.loc, name: g.name, title: tx('the folder of the group {group}', { group: g.name }) })),
    mounts: mounts.map(m => ({ loc: m.loc, name: m.name, title: 'sftp: ' + m.username + '@' + m.host + (m.root ? ':' + m.root : '') })),
    shares: shares.with_me.map(s => ({ loc: s.loc, name: s.name, dir: s.dir, title: s.write ? tx('from {user}, can edit', { user: s.user }) : tx('from {user}, read only', { user: s.user }) })),
  };
}

/* room on the storage behind a location, asked at most once a minute: {size, used, free} or null */
const SPACE = new Map();
function spaceOf(loc) {
  const c = SPACE.get(loc);
  if (c && Date.now() - c.t < 60000) return c.p;
  const p = api('GET', '/api/space/' + encodeURIComponent(loc)).catch(() => null);
  SPACE.set(loc, { t: Date.now(), p });
  return p;
}
/* used/size in small print, grey until 80% full; a home with a limit (s.limit) is as big as its limit */
function spaceTag(loc) {
  const el = h('span', { class: 'space' });
  spaceOf(loc).then(s => {
    if (!s || !s.size) return;
    const full = s.limit ? s.used / s.size : s.used / (s.used + s.free || 1), top = s.limit ? 999.5 : undefined;
    el.textContent = fmtShort(s.used, top) + '/' + fmtShort(s.size, top);
    el.classList.toggle('full', full >= 0.8);
    const v = { used: fmtSize(s.used), free: fmtSize(s.free), size: fmtSize(s.size), pc: Math.round(full * 100) };
    el.title = s.limit ? tx('{used} used, {free} free of your limit of {size} ({pc}% full)', v) : tx('{used} used, {free} free of {size} ({pc}% full)', v);
  });
  return el;
}

/* the places window; a click on its titlebar folds it away (remembered),
   metaplaces and all */
function placesBox(places, loc) {
  let here = loc === 'pictures' ? tx('pictures') : tx('home'), hereLoc = loc === 'pictures' ? null : 'home';
  const a = (p, href) => {
    if (p.loc === loc) { here = p.name; hereLoc = p.loc === 'pictures' ? null : p.loc; }
    return h('a', { href, class: 'place' + (p.loc === loc ? ' on' : ''), title: p.title || null },
      h('span', { class: 'pn', text: p.name + (p.dir === false ? '' : '/') }),
      p.loc === 'pictures' ? null : spaceTag(p.loc));
  };
  const kids = [a({ loc: 'home', name: tx('home') }, U.files('home', '/'))];
  if (places && places.mounts.length) {
    kids.push(h('div', { class: 'lbl', text: tx('storage') }));
    places.mounts.forEach(p => kids.push(a(p, U.files(p.loc, '/'))));
  }
  if (places && places.groups && places.groups.length) {
    kids.push(h('div', { class: 'lbl', text: tx('group folders') }));
    places.groups.forEach(p => kids.push(a(p, U.files(p.loc, '/'))));
  }
  if (places && places.shares.length) {
    kids.push(h('div', { class: 'lbl', text: tx('shared with me') }));
    places.shares.forEach(p => kids.push(a(p, p.dir ? U.files(p.loc, '/') : U.view(p.loc, '/'))));
  }
  kids.push(h('div', { class: 'lbl', text: tx('metaplaces') }));
  kids.push(a({ loc: 'pictures', name: tx('pictures'), dir: false, title: tx('every picture here and shared with you, newest first') }, '/pictures'));
  const fold = h('span', { class: 'fold' });
  const box = win([tx('places'), h('span', { class: 'crumbs place-here' }, here, hereLoc && ' ', hereLoc && spaceTag(hereLoc)), h('span', { class: 'spacer' }), fold], kids, 'places');
  const set = shaded => {
    box.classList.toggle('shaded', shaded);
    fold.textContent = shaded ? '[+]' : '[-]';
  };
  set(localStorage.getItem('osada.places') === '0');
  const bar = box.querySelector('.titlebar');
  bar.title = tx('fold / unfold the places');
  bar.addEventListener('click', () => {
    const shaded = !box.classList.contains('shaded');
    set(shaded);
    localStorage.setItem('osada.places', shaded ? '0' : '1');
  });
  return box;
}

function crumbs(loc, rootName, path, last) {
  const segs = splitPath(path);
  const out = [h('a', { href: U.files(loc, '/'), text: loc === 'home' ? tx('home') : rootName })];
  let p = '';
  segs.forEach((s, i) => {
    p += '/' + s;
    out.push(h('span', { class: 'dim', text: ' / ' }));
    out.push(i === segs.length - 1 && !last ? h('span', { class: 'here', text: s }) : h('a', { href: U.files(loc, p), text: s }));
  });
  return h('span', { class: 'crumbs' }, out);
}

/* ---- sorting ---- */
function sortEntries(list, s) {
  const d = s.d ? -1 : 1;
  return list.slice().sort((a, b) => (b.dir - a.dir) ||
    d * ((s.k === 'size' ? a.size - b.size : s.k === 'mtime' ? a.mtime - b.mtime : 0) || nameCmp(a.name, b.name)));
}

/* ---- the file list ---- */
async function filesPage(loc, path) {
  const [ls, places] = await Promise.all([api('GET', U.ls(loc, path)), loadPlaces().catch(() => null)]);
  if (ls.file) return { redirect: U.view(loc, path) };
  // without home: what can be opened here
  const kept = offline() ? await keptHere() : null;
  return new FilesView(loc, path, ls, places, kept).page();
}

class FilesView {
  constructor(loc, path, ls, places, kept) {
    this.loc = loc;
    this.path = path;
    this.ls = ls;
    this.write = ls.write && !offline();
    this.owner = ls.owner && !offline();
    this.kept = kept;
    try { this.sort = JSON.parse(localStorage.getItem('osada.sort')) || { k: 'name', d: false }; } catch (e) { this.sort = { k: 'name', d: false }; }
    this.sel = new Set();
    this.cur = -1;
    this.anchor = -1;
    this.filter = '';
    this.typed = '';
    this.view = [];
    this.rows = [];
    this.build(places);
    this.setEntries(ls.entries);
  }

  page() {
    return {
      el: this.el, cls: 'fill', title: (this.loc === 'home' ? tx('home') : this.ls.root_name) + (this.path === '/' ? '' : this.path),
      key: e => this.key(e),
      onUploaded: (loc, p) => { if (loc === this.loc && p.startsWith(this.path === '/' ? '/' : this.path + '/')) this.soon(); },
      destroy: () => clearTimeout(this.timer),
      shown: () => { this.upd(); this.wrap.focus(); },
    };
  }

  build(places) {
    const w = !this.write;
    this.bUp = btn(tx('upload'), tx('upload files (or drop them on the list)'), () => this.pick(false));
    this.bUpDir = btn(tx('upload folder'), tx('upload a folder with everything in it'), () => this.pick(true));
    this.newSel = h('select', { title: tx('create something here') },
      h('option', { value: '', text: tx('new…') }), h('option', { value: 'dir', text: tx('folder') }),
      h('option', { value: 'md', text: tx('markdown (.md)') }), h('option', { value: 'txt', text: tx('text (.txt)') }),
      h('option', { value: 'csv', text: tx('table (.csv)') }), h('option', { value: 'ods', text: tx('spreadsheet (.ods)') }),
      h('option', { value: 'odt', text: tx('document (.odt)') }), h('option', { value: 'rtf', text: tx('document (.rtf)') }),
      h('option', { value: 'odp', text: tx('presentation (.odp)') }));
    this.newSel.addEventListener('change', () => { const v = this.newSel.value; this.newSel.value = ''; if (v) this.create(v).catch(fail); });
    [this.bUp, this.bUpDir, this.newSel].forEach(b => { b.disabled = w; });
    this.bOpen = btn(tx('open'), tx('open (enter)'), () => this.openSel());
    // the office suite (Round 18), straight from the list
    this.bOffice = btn(tx('office'), tx('open it in the office suite in the browser (LibreOffice); saving there writes it back here'), () => this.officeSel());
    this.bOffice.hidden = !S.me.office || offline();
    this.bEdit = btn(tx('edit'), tx('edit in the browser'), () => this.editSel());
    this.bDl = btn(tx('download'), tx('download'), () => this.download());
    this.bRen = btn(tx('rename'), tx('rename (f2)'), () => this.rename().catch(fail));
    this.bMove = btn(tx('move'), tx('move to another folder'), () => this.move().catch(fail));
    this.bDel = btn(tx('delete'), tx('delete (del)'), () => this.remove().catch(fail), 'danger');
    this.bShare = btn(tx('share'), tx('share with users or by link'), () => this.share());
    this.bVer = btn(tx('versions'), null, () => this.versions());
    this.bVer.hidden = !(this.loc === 'home' && S.me.versions) || offline();
    this.filterIn = h('input', { type: 'search', class: 'filter', placeholder: tx('filter (/)'), spellcheck: false });
    this.filterIn.addEventListener('input', () => { this.filter = this.filterIn.value; this.render(); this.paint(); });
    this.filterIn.addEventListener('keydown', e => {
      if (e.key === 'Escape') { this.filterIn.value = ''; this.filter = ''; this.render(); this.paint(); this.wrap.focus(); }
      else if (e.key === 'ArrowDown' || e.key === 'Enter') { e.preventDefault(); this.wrap.focus(); if (this.view.length && this.cur < 0) this.moveTo(0); }
    });
    const bar = h('div', { class: 'toolbar' }, this.bUp, this.bUpDir, this.newSel, h('span', { class: 'sep' }),
      this.bOpen, this.bOffice, this.bEdit, this.bDl, this.bRen, this.bMove, this.bShare, this.bVer, this.bDel, h('span', { class: 'spacer' }), this.filterIn);

    const th = (k, label, cls) => {
      const el = h('th', { class: cls || null, text: tx(label), title: tx('sort') });
      el.addEventListener('click', () => {
        this.sort = { k, d: this.sort.k === k ? !this.sort.d : k !== 'name' };
        localStorage.setItem('osada.sort', JSON.stringify(this.sort));
        this.render();
        this.paint();
      });
      el.dataset.k = k;
      return el;
    };
    this.thead = h('thead', {}, h('tr', {}, th('name', 'name', 'nm'), th('size', 'size', 'sz'), th('mtime', 'modified', 'tm')));
    this.tbody = h('tbody');
    this.empty = h('div', { class: 'empty dim' });
    this.wrap = h('div', { class: 'listwrap', tabindex: '0' }, h('table', { class: 'x11 files' }, this.thead, this.tbody), this.empty);
    this.drop = h('div', { class: 'dropnote', hidden: true }, h('span', { text: tx('drop to upload here') }));

    this.tbody.addEventListener('mousedown', e => { if ((e.shiftKey || e.ctrlKey || e.metaKey) && !e.target.closest('a')) e.preventDefault(); });
    this.tbody.addEventListener('click', e => {
      if (e.target.closest('a')) return;
      const tr = e.target.closest('tr[data-i]');
      if (!tr) return;
      const i = +tr.dataset.i;
      if (i < 0) return;
      if (e.shiftKey && this.anchor >= 0) this.range(this.anchor, i, e.ctrlKey || e.metaKey);
      else if (e.ctrlKey || e.metaKey) { this.toggle(i); this.anchor = i; }
      else { this.sel = new Set([this.view[i].name]); this.anchor = i; }
      this.cur = i;
      this.paint();
    });
    this.tbody.addEventListener('dblclick', e => {
      if (e.target.closest('a')) return;
      const tr = e.target.closest('tr[data-i]');
      if (!tr) return;
      const i = +tr.dataset.i;
      if (i < 0) navigate(U.files(this.loc, parentPath(this.path)));
      else this.open(this.view[i]);
    });
    this.wrap.addEventListener('click', e => {
      if (e.target === this.wrap || e.target === this.empty) { this.sel.clear(); this.paint(); }
    });

    const fw = h('section', { class: 'win filewin' },
      h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), crumbs(this.loc, this.ls.root_name, this.path),
        this.write ? null : h('span', { class: 'tag', text: offline() ? tx('offline') : tx('read only') })),
      bar, this.wrap, this.drop);
    fw.addEventListener('dragover', e => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = this.write ? 'copy' : 'none';
      if (this.write) this.drop.hidden = false;
    });
    this.drop.addEventListener('dragleave', () => { this.drop.hidden = true; });
    fw.addEventListener('drop', e => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      this.drop.hidden = true;
      if (!this.write) { toast(tx('this folder is read only'), 'err'); return; }
      droppedFiles(e.dataTransfer).then(list => upload(this.loc, this.path, list), fail);
    });
    this.el = h('div', { class: 'files-layout' }, placesBox(places, this.loc), fw);
  }

  setEntries(entries, focus) {
    const curName = focus || (this.cur >= 0 && this.view[this.cur] ? this.view[this.cur].name : null);
    // folder sizes from before stand in until the new ones come
    const had = new Map((this.entries || []).filter(e => e.du).map(e => [e.name, e.size]));
    entries.forEach(e => { if (e.dir && had.has(e.name)) { e.size = had.get(e.name); e.du = true; } });
    this.entries = entries;
    this.render();
    const names = new Set(this.view.map(e => e.name));
    this.sel = focus ? new Set([focus]) : new Set([...this.sel].filter(n => names.has(n)));
    this.cur = curName ? this.view.findIndex(e => e.name === curName) : -1;
    this.anchor = this.cur;
    this.paint();
    if (focus && this.rows[this.cur]) this.rows[this.cur].scrollIntoView({ block: 'nearest' });
    this.sizes();
  }

  /* folder sizes come after the list: adding them up can take a moment */
  sizes() {
    if (!this.entries.some(e => e.dir)) return;
    const ask = this.duAsk = (this.duAsk || 0) + 1;
    api('GET', U.du(this.loc, this.path)).then(r => {
      if (ask !== this.duAsk) return;
      for (const e of this.entries) {
        if (e.dir && r.sizes[e.name] !== undefined) { e.size = r.sizes[e.name]; e.du = true; }
      }
      this.render();
      this.paint();
    }, () => {});
  }

  render() {
    const f = this.filter.toLowerCase();
    const curName = this.cur >= 0 && this.view[this.cur] ? this.view[this.cur].name : null;
    this.view = sortEntries(this.entries.filter(e => !f || e.name.toLowerCase().includes(f)), this.sort);
    this.rows = this.view.map((e, i) => {
      const p = joinPath(this.path, e.name);
      const gone = this.kept && !(e.dir ? this.kept.lists.has(U.ls(this.loc, p)) : this.kept.files.has(U.raw(this.loc, p)));
      return h('tr', { 'data-i': i, class: (e.dir ? 'd' : 'f') + (gone ? ' gone' : ''), title: gone ? tx('not kept on this device') : null },
        h('td', { class: 'nm' }, h('a', { href: itemUrl(this.loc, p, e.dir), text: e.name + (e.dir ? '/' : ''), draggable: 'false' }),
          e.shared ? h('span', { class: 'tag', text: tx('shared'), title: tx('shared with users') }) : null,
          e.linked ? h('span', { class: 'tag', text: tx('link'), title: tx('has a public link') }) : null),
        h('td', { class: 'sz', text: e.dir && !e.du ? '' : fmtSize(e.size) }),
        h('td', { class: 'tm', text: fmtTime(e.mtime) }));
    });
    const up = splitPath(this.path).length
      ? [h('tr', { 'data-i': -1, class: 'up' }, h('td', { class: 'nm' }, h('a', { href: U.files(this.loc, parentPath(this.path)), text: '../', title: tx('up (backspace)') })), h('td'), h('td'))]
      : [];
    this.tbody.replaceChildren(...up, ...this.rows);
    this.cur = curName ? this.view.findIndex(e => e.name === curName) : -1;
    for (const t of this.thead.querySelectorAll('th')) {
      t.classList.toggle('sorted', t.dataset.k === this.sort.k);
      t.classList.toggle('desc', t.dataset.k === this.sort.k && this.sort.d);
    }
    this.empty.textContent = this.view.length ? '' : this.entries.length ? tx('nothing matches the filter') : offline() ? tx('empty folder') :
      this.write ? tx('empty folder: drop files here or use upload') : tx('empty folder');
  }

  paint() {
    this.rows.forEach((tr, i) => {
      tr.classList.toggle('sel', this.sel.has(this.view[i].name));
      tr.classList.toggle('cur', i === this.cur);
    });
    this.upd();
  }

  selected() { return this.view.filter(e => this.sel.has(e.name)); }

  upd() {
    const s = this.selected(), one = s.length === 1 ? s[0] : null, w = this.write;
    this.bOpen.disabled = !one;
    this.bOffice.disabled = !(one && !one.dir && officeKind(one.name));
    this.bEdit.disabled = !(one && !one.dir && editorOf(one.name) && w);
    this.bDl.disabled = !s.length || s.some(e => e.dir);
    this.bDl.title = s.some(e => e.dir) ? tx('folders cannot be downloaded yet') : tx('download');
    this.bRen.disabled = !(one && w);
    this.bMove.disabled = !(s.length && w);
    this.bDel.disabled = !(s.length && w);
    this.bShare.disabled = !(one && this.owner);
    this.bVer.disabled = s.length > 1;
    this.bVer.title = one ? tx('earlier versions of {name}, from the backups', { name: one.name }) : tx('earlier versions of this folder, from the backups (deleted files too)');
    const dirs = this.view.filter(e => e.dir).length, files = this.view.length - dirs;
    // folders count once their size is in
    const size = list => fmtSize(list.reduce((a, e) => a + (e.dir && !e.du ? 0 : e.size), 0));
    let t = txn(dirs, '{n} folder', '{n} folders') + ', ' + txn(files, '{n} file', '{n} files') + ', ' + size(this.view);
    if (s.length) t += '  |  ' + txn(s.length, '{n} selected', '{n} selected') + (s.some(e => !e.dir || e.du) ? ', ' + size(s) : '');
    setStatus(t);
  }

  toggle(i) {
    const n = this.view[i].name;
    if (this.sel.has(n)) this.sel.delete(n); else this.sel.add(n);
  }
  range(a, b, add) {
    if (!add) this.sel.clear();
    for (let i = Math.min(a, b); i <= Math.max(a, b); i++) this.sel.add(this.view[i].name);
  }
  moveTo(i, shift, keep) {
    this.cur = i;
    if (shift && this.anchor >= 0) this.range(this.anchor, i, false);
    else if (!keep) { this.sel = new Set([this.view[i].name]); this.anchor = i; }
    this.paint();
    this.rows[i].scrollIntoView({ block: 'nearest' });
  }

  key(e) {
    if (isTyping(e)) return false;
    if (e.target instanceof HTMLButtonElement && (e.key === 'Enter' || e.key === ' ')) return false;
    const n = this.view.length, k = e.key, mod = e.ctrlKey || e.metaKey;
    const step = { ArrowDown: 1, ArrowUp: -1, PageDown: 'pd', PageUp: 'pu', Home: 'h', End: 'e' }[k];
    if (step !== undefined && !e.altKey) {
      if (!n) return true;
      const pg = Math.max(1, Math.floor(this.wrap.clientHeight / 25) - 1);
      let c = this.cur;
      if (step === 1) c = c < 0 ? 0 : c + 1;
      else if (step === -1) c = c < 0 ? n - 1 : c - 1;
      else if (step === 'pd') c = Math.max(c, 0) + pg;
      else if (step === 'pu') c = Math.max(c, 0) - pg;
      else c = step === 'h' ? 0 : n - 1;
      this.moveTo(Math.max(0, Math.min(n - 1, c)), e.shiftKey, mod);
      return true;
    }
    if (k === 'Enter') {
      const s = this.selected();
      if (s.length === 1) this.open(s[0]);
      else if (this.cur >= 0) this.open(this.view[this.cur]);
      return true;
    }
    if (k === 'Backspace' || (k === 'ArrowUp' && e.altKey)) {
      if (splitPath(this.path).length) navigate(U.files(this.loc, parentPath(this.path)));
      return true;
    }
    if (k === 'Delete' && this.write && this.selected().length) { this.remove().catch(fail); return true; }
    if (k === 'F2' && this.write && this.selected().length === 1) { this.rename().catch(fail); return true; }
    if (mod && (k === 'a' || k === 'A')) { this.sel = new Set(this.view.map(x => x.name)); this.paint(); return true; }
    if (k === ' ' && this.cur >= 0) { this.toggle(this.cur); this.anchor = this.cur; this.paint(); return true; }
    if (k === 'Escape') { this.sel.clear(); this.paint(); return true; }
    if (k === '/') { this.filterIn.focus(); this.filterIn.select(); return true; }
    if (k.length === 1 && !mod && !e.altKey && n) {
      // type-ahead: jump to the first name starting with what was typed
      clearTimeout(this.typeTimer);
      this.typed += k.toLowerCase();
      this.typeTimer = setTimeout(() => { this.typed = ''; }, 800);
      const i = this.view.findIndex(x => x.name.toLowerCase().startsWith(this.typed));
      if (i >= 0) this.moveTo(i);
      return true;
    }
    return false;
  }

  soon() { clearTimeout(this.timer); this.timer = setTimeout(() => this.refresh(), 300); }

  async refresh(focus) {
    try {
      const ls = await api('GET', U.ls(this.loc, this.path));
      if (ls.entries) this.setEntries(ls.entries, focus);
    } catch (e) { fail(e); }
  }

  open(e) { navigate(itemUrl(this.loc, joinPath(this.path, e.name), e.dir)); }
  openSel() { const s = this.selected(); if (s.length === 1) this.open(s[0]); }
  editSel() { const s = this.selected(); if (s.length === 1) navigate(U.edit(this.loc, joinPath(this.path, s[0].name))); }
  officeSel() {
    const s = this.selected();
    if (s.length === 1 && !s[0].dir && officeKind(s[0].name)) location.assign(officeUrl(this.loc, joinPath(this.path, s[0].name), this.write));
  }

  pick(dir) {
    const inp = h('input', { type: 'file', multiple: true });
    if (dir) inp.webkitdirectory = true;
    inp.addEventListener('change', () => {
      upload(this.loc, this.path, Array.from(inp.files).map(f => ({ file: f, rel: dir && f.webkitRelativePath ? f.webkitRelativePath : f.name })));
    });
    inp.click();
  }

  download() {
    this.selected().filter(e => !e.dir).forEach((e, i) => setTimeout(() => {
      const a = h('a', { href: U.raw(this.loc, joinPath(this.path, e.name)) + '?dl=1', download: '' });
      document.body.appendChild(a);
      a.click();
      a.remove();
    }, i * 400));
  }

  async rename() {
    const e = this.selected()[0];
    const nn = await dialogAsk(tx('rename'), tx('new name'), e.name, tx('rename'), async v => {
      const bad = checkName(v);
      if (bad) return bad;
      if (v === e.name) return null;
      await api('POST', '/api/mv', { loc: this.loc, moves: [{ from: joinPath(this.path, e.name), to: joinPath(this.path, v) }] });
    });
    if (nn && nn !== e.name) await this.refresh(nn);
  }

  async move() {
    const s = this.selected();
    const dest = await pickFolder(this.loc, this.path, tx('move {what}', { what: s.length === 1 ? s[0].name : txn(s.length, '{n} item', '{n} items') }),
      s.filter(e => e.dir).map(e => joinPath(this.path, e.name)));
    if (dest === null || dest === this.path) return;
    await api('POST', '/api/mv', { loc: this.loc, moves: s.map(e => ({ from: joinPath(this.path, e.name), to: joinPath(dest, e.name) })) });
    toast(tx('moved to {dest}', { dest }));
    this.sel.clear();
    await this.refresh();
  }

  async remove() {
    const s = this.selected();
    const what = s.length !== 1 ? txn(s.length, '{n} item', '{n} items') : s[0].dir ? tx('{name} and everything in it', { name: s[0].name }) : s[0].name;
    if (!(await confirmBox(tx('delete'), tx('delete {what}? this cannot be undone.', { what }), tx('delete'), true))) return;
    await api('POST', '/api/rm', { loc: this.loc, paths: s.map(e => joinPath(this.path, e.name)) });
    this.sel.clear();
    await this.refresh();
  }

  async create(kind) {
    if (kind === 'dir') {
      const n = await dialogAsk(tx('new folder'), tx('name'), tx('new folder'), tx('create'), async v => {
        const bad = checkName(v);
        if (bad) return bad;
        await api('POST', '/api/mkdir', { loc: this.loc, path: joinPath(this.path, v) });
      });
      if (n) await this.refresh(n);
      return;
    }
    const n = await dialogAsk(tx('new file'), tx('name'), tx('untitled') + '.' + kind, tx('create'), async v => {
      const bad = checkName(v);
      if (bad) return bad;
      const name = extOf(v) ? v : v + '.' + kind;
      const body = kind === 'ods' ? await withPart('ods', x => x.OdsOut.blank()) : kind === 'odt' ? await withPart('odt', x => x.ODT.blank())
        : kind === 'odp' ? await withPart('odp', x => x.ODP.blank())
        : kind === 'rtf' ? await withPart('rtf', x => x.RTF.EMPTY)
        : { md: '# ' + stem(name) + '\n\n', txt: '', csv: '' }[kind];
      await putRaw(U.raw(this.loc, joinPath(this.path, name)) + '?mode=new', body);
    });
    if (n) navigate(U.edit(this.loc, joinPath(this.path, extOf(n) ? n : n + '.' + kind)));
  }

  share() {
    const e = this.selected()[0];
    withPart('share', x => x.shareDialog(this.loc, joinPath(this.path, e.name), e.name)).then(() => this.refresh(), fail);
  }

  /* of the one selected, else of this folder */
  versions() {
    const s = this.selected();
    if (s.length > 1) return;
    withPart('versions', x => x.versionsDialog(this.loc, s.length ? joinPath(this.path, s[0].name) : this.path)).then(put => { if (put) this.refresh(); }, fail);
  }
}

/* ask() whose check runs on the server before the dialog closes */
function dialogAsk(title, label, value, okLabel, check) {
  const inp = h('input', { type: 'text', value: value || '', spellcheck: false, class: 'wide' });
  return dialog({
    title, body: [h('label', { class: 'lbl', text: label }), inp],
    buttons: [{ label: tx('cancel'), value: null }, { label: okLabel, primary: true, value: () => inp.value.trim() }],
    select: el => { const v = el.value, i = v.lastIndexOf('.'); el.setSelectionRange(0, i > 0 ? i : v.length); },
    check,
  });
}

/* ---- folder picker for moves ---- */
function pickFolder(loc, start, title, exclude) {
  let cur = start;
  const where = h('div', { class: 'pick-path' });
  const list = h('div', { class: 'pick-list' });
  async function load(p) {
    list.replaceChildren(h('div', { class: 'dim', text: tx('loading…') }));
    try {
      const ls = await api('GET', U.ls(loc, p));
      cur = p;
      where.textContent = (loc === 'home' ? tx('home') : ls.root_name) + (p === '/' ? '/' : p + '/');
      const rows = [];
      if (splitPath(p).length) rows.push(btn('../', tx('up'), () => load(parentPath(p)), 'pick-item'));
      (ls.entries || []).filter(e => e.dir).map(e => e.name).sort(nameCmp).forEach(d => {
        const full = joinPath(p, d);
        const inside = exclude.some(x => full === x || full.startsWith(x + '/'));
        rows.push(h('button', { class: 'pick-item', text: d + '/', disabled: inside, title: inside ? tx('cannot move a folder into itself') : null, onclick: () => load(full) }));
      });
      if (rows.length === (splitPath(p).length ? 1 : 0)) rows.push(h('div', { class: 'dim', text: tx('(no folders in here)') }));
      list.replaceChildren(...rows);
    } catch (e) { list.replaceChildren(h('div', { class: 'bad', text: e.message })); }
  }
  return dialog({
    title, body: [h('label', { class: 'lbl', text: tx('to') }), where, list],
    buttons: [{ label: tx('cancel'), value: null }, { label: tx('move here'), primary: true, value: () => cur }],
    opened: () => load(start),
  });
}

/* ---- apptab: apps1's apps in a frame (src/apptab.rs) ---- */
async function apptabPage(name) {
  const p = (S.me.proxies || (S.me.apptab ? [{ name: 'apptab', title: 'apptab' }] : [])).find(x => x.name === name);
  if (!p) throw new Error(name === 'apptab' ? tx('apptab is not on for this account') : 'no access to ' + name);
  if (offline()) throw new Error(tx('read only while osada at home is not reachable'));
  const t = await api('POST', '/api/apptab/ticket?name=' + encodeURIComponent(name));
  // at home by the server's address (plain http) apps1 is there itself; over https it comes through osada,
  // the ticket in the frame's address making the pass for its own name
  const lan = location.protocol === 'http:';
  const frame = h('iframe', { class: 'mv-frame', src: lan ? t.lan : t.url, title: p.title, allow: 'clipboard-write; fullscreen' });
  const out = h('a', { class: 'btn-link', href: lan ? t.lan : t.origin + '/', target: '_blank', rel: 'noopener', text: tx('open in a new tab') });
  const el = h('section', { class: 'win viewwin mv-win' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), h('span', { text: p.title }), h('span', { class: 'spacer' }), out),
    frame);
  return { el, cls: 'fill mv', title: p.title };
}

/* ---- uploads: a queue that outlives page changes ---- */
const UP = { items: [], running: 0, win: null, list: null, ask: Promise.resolve() };

function hasFiles(e) { return e.dataTransfer && Array.from(e.dataTransfer.types || []).includes('Files'); }

/* files and folders from a drop, as [{file, rel}] */
async function droppedFiles(dt) {
  // entries have to be taken before the first await
  const entries = Array.from(dt.items || []).filter(i => i.kind === 'file').map(i => i.webkitGetAsEntry && i.webkitGetAsEntry()).filter(Boolean);
  if (!entries.length) return Array.from(dt.files).map(f => ({ file: f, rel: f.name }));
  const out = [];
  async function walk(en, prefix) {
    if (en.isFile) {
      out.push({ file: await new Promise((res, rej) => en.file(res, rej)), rel: prefix + en.name });
    } else if (en.isDirectory) {
      const rd = en.createReader();
      for (;;) {
        const batch = await new Promise((res, rej) => rd.readEntries(res, rej));
        if (!batch.length) break;
        for (const c of batch) await walk(c, prefix + en.name + '/');
      }
    }
  }
  for (const en of entries) await walk(en, '');
  return out;
}

function upload(loc, dir, files) {
  if (!files.length) return;
  if (offline()) { toast(tx('read only while osada at home is not reachable'), 'err'); return; }
  if (!UP.win) {
    UP.list = h('div', { class: 'up-list' });
    UP.title = h('span');
    UP.win = h('div', { class: 'win uploads' },
      h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), UP.title, h('span', { class: 'spacer' }),
        btn(tx('clear'), tx('remove finished'), () => { UP.items = UP.items.filter(i => { if (i.state === 'done' || i.state === 'skip' || i.state === 'err') { i.row.remove(); return false; } return true; }); upTitle(); }),
        btn('x', tx('hide (uploads go on)'), () => { UP.win.hidden = true; })),
      UP.list);
    document.body.appendChild(UP.win);
  }
  UP.win.hidden = false;
  const batch = { policy: null };
  for (const f of files) {
    const it = { loc, path: joinPath(dir, f.rel), file: f.file, mk: f.rel.includes('/'), batch, state: 'wait' };
    it.bar = h('i');
    it.st = h('span', { class: 'up-st dim', text: tx('waiting') });
    it.x = btn('x', tx('cancel'), () => cancelUp(it));
    it.row = h('div', { class: 'up-row' }, h('span', { class: 'up-name', text: f.rel, title: it.path }), h('span', { class: 'up-bar' }, it.bar), it.st, it.x);
    UP.list.appendChild(it.row);
    UP.items.push(it);
  }
  upTitle();
  pump();
}

function upTitle() {
  const left = UP.items.filter(i => i.state === 'wait' || i.state === 'up').length;
  UP.title.textContent = left ? tx('uploading, {n} left', { n: left }) : tx('uploads done');
  const el = $('#up-status');
  if (el) el.textContent = left ? tx('upload: {n} left', { n: left }) : '';
}

function pump() {
  while (UP.running < 2) {
    const it = UP.items.find(i => i.state === 'wait');
    if (!it) break;
    sendUp(it, false);
  }
  upTitle();
}

function cancelUp(it) {
  if (it.state === 'up' && it.xhr) it.xhr.abort();
  else if (it.state === 'wait') { it.state = 'skip'; it.st.textContent = tx('cancelled'); it.x.hidden = true; upTitle(); }
}

function xput(url, body, progress) {
  const x = new XMLHttpRequest();
  const p = new Promise((res, rej) => {
    x.open('PUT', url);
    x.setRequestHeader('x-shelf', '1');
    x.upload.onprogress = e => progress(e.loaded, e.total || body.size);
    x.onload = () => { let j = null; try { j = JSON.parse(x.responseText); } catch (e) { /* none */ } res({ status: x.status, json: j }); };
    x.onerror = () => rej(new Error(tx('network error')));
    x.onabort = () => rej(new Error(tx('cancelled')));
    x.send(body);
  });
  return { x, p };
}

async function sendUp(it, replace) {
  UP.running++;
  it.state = 'up';
  it.st.textContent = '0%';
  // a big file that cannot fit in a home with a limit is not sent at all (the server would read it, then refuse)
  if (!replace && it.loc === 'home' && it.file.size > 32 << 20) {
    const s = await api('GET', '/api/space/home').catch(() => null);
    if (s && s.limit && it.file.size > s.free) {
      UP.running--;
      it.state = 'err';
      it.st.textContent = tx('this does not fit within the storage limit');
      it.st.className = 'up-st bad';
      it.x.hidden = true;
      pump();
      return;
    }
  }
  const q = [];
  if (!replace) q.push('mode=new');
  if (it.mk) q.push('mkdirs=1');
  const { x, p } = xput(U.raw(it.loc, it.path) + (q.length ? '?' + q.join('&') : ''), it.file, (a, b) => {
    const pc = b ? Math.floor(a * 100 / b) : 100;
    it.bar.style.width = pc + '%';
    it.st.textContent = pc + '%';
  });
  it.xhr = x;
  let r = null, err = null;
  try { r = await p; } catch (e) { err = e.message; }
  UP.running--;
  if (r && r.status === 409 && !replace) {
    it.state = 'ask';
    it.st.textContent = tx('exists');
    const how = await conflict(it);
    if (how === 'replace') { sendUp(it, true); return; }
    it.state = 'skip';
    it.st.textContent = tx('skipped');
  } else if (r && r.status >= 200 && r.status < 300) {
    it.state = 'done';
    it.bar.style.width = '100%';
    it.st.textContent = tx('done');
    it.st.className = 'up-st ok';
    if (S.page && S.page.onUploaded) S.page.onUploaded(it.loc, it.path);
  } else {
    it.state = 'err';
    it.st.textContent = err || (r.json && r.json.error && txErr(r.json.error)) || tx('error {status}', { status: r.status });
    it.st.className = 'up-st bad';
    if (r && r.status === 401 && window.onUnauthorized) window.onUnauthorized();
  }
  it.x.hidden = true;
  pump();
}

/* one question at a time; "all" answers the rest of the batch */
function conflict(it) {
  UP.ask = UP.ask.then(async () => {
    if (it.batch.policy) return it.batch.policy;
    const v = await dialog({
      title: tx('already there'), body: h('div', { class: 'dlg-text', text: tx('{name} exists.', { name: it.path }) }),
      buttons: [{ label: tx('skip'), value: 'skip' }, { label: tx('skip all'), value: 'skip-all' },
        { label: tx('replace all'), value: 'replace-all', danger: true }, { label: tx('replace'), value: 'replace', primary: true, danger: true }],
    });
    if (v === 'skip-all' || v === 'replace-all') { it.batch.policy = v.split('-')[0]; return it.batch.policy; }
    return v || 'skip';
  });
  return UP.ask;
}

/* ---- viewing a file ---- */
async function viewPage(loc, path) {
  let ls;
  try { ls = await api('GET', U.ls(loc, path)); } catch (e) {
    // without home a file is also known from its folder's listing
    const up = offline() && splitPath(path).length ? await api('GET', U.ls(loc, parentPath(path))).catch(() => null) : null;
    const f = up && (up.entries || []).find(x => !x.dir && x.name === baseName(path));
    if (!f) throw e;
    ls = { file: f, root_name: up.root_name, write: false };
  }
  if (!ls.file) return { redirect: U.files(loc, path) };
  const name = ls.file.name, dir = parentPath(path), inFolder = splitPath(path).length > 0, write = ls.write && !offline();
  let siblings = null;
  if (inFolder) {
    try { siblings = (await api('GET', U.ls(loc, dir))).entries.filter(e => !e.dir).map(e => e.name); } catch (e) { /* no prev / next */ }
  }
  const v = viewer({
    name, raw: U.raw(loc, path), siblings,
    rawOf: n => U.raw(loc, joinPath(dir, n)),
    // something taken out of the file (an attachment), kept next to it
    save: write ? async (n, bytes) => {
      const bad = checkName(n);
      if (bad) throw new Error(bad);
      try { await putRaw(U.raw(loc, joinPath(dir, n)) + '?mode=new', bytes); } catch (e) {
        if (e.status !== 409) throw e;
        if (!(await confirmBox(tx('already there'), tx('{name} exists. replace it?', { name: n }), tx('replace'), true))) throw new Error(tx('not saved'));
        await putRaw(U.raw(loc, joinPath(dir, n)), bytes);
      }
    } : null,
    // an archive's files, into a folder named after it (a lone .gz: next to it); what is there stays
    extract: write ? async list => {
      const lone = /\.gz$/i.test(name) && !/\.tar\.gz$/i.test(name);
      const into = lone ? dir : joinPath(dir, stem(name).replace(/\.tar$/i, ''));
      let n = 0, there = 0;
      for (const e of list) {
        const segs = e.path.split('/').filter(Boolean);
        if (!segs.length || segs.some(checkName)) continue;
        try { await putRaw(U.raw(loc, joinPath(into, segs.join('/'))) + '?mode=new&mkdirs=1', await e.read()); n++; } catch (x) { if (x.status !== 409) throw x; there++; }
      }
      return { n, kept: there };
    } : null,
    mdOf: text => api('POST', '/api/md', { text }).then(r => r.html),
    md: async () => {
      const r = await fetchText(U.raw(loc, path), TEXT_MAX);
      const res = await api('POST', '/api/md', { text: r.text, img_base: '/api/raw/' + loc + encP(dir) + '/', link_base: '/view/' + loc + encP(dir) + '/' });
      return res.html;
    },
  });
  const go = n => { if (n) navigate(U.view(loc, joinPath(dir, n)), true); };
  const back = inFolder ? U.files(loc, dir) : '/shared';
  // spreadsheets and documents not edited here are edited as an .ods / .odt made from them
  const canEdit = write && editorOf(name), conv = write && !canEdit ? { sheet: 'ods', doc: 'odt', slides: 'odp' }[kindOf(name)] : null;
  let nav = null;
  if (v.prev || v.next) {
    const p = btn(tx('< prev'), tx('previous (left arrow)'), () => go(v.prev)), n = btn(tx('next >'), tx('next (right arrow)'), () => go(v.next));
    p.disabled = !v.prev;
    n.disabled = !v.next;
    nav = [p, n];
  }
  const el = h('section', { class: 'win viewwin' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }),
      inFolder ? crumbs(loc, ls.root_name, path, false) : h('span', { class: 'crumbs' }, h('span', { class: 'here', text: name })),
      h('span', { class: 'spacer' }), nav,
      canEdit ? btn(tx('edit'), tx('edit (e)'), () => navigate(U.edit(loc, path))) : null,
      conv === 'ods' ? btn(tx('edit as .ods'), tx('spreadsheets are edited as .ods: make one from this next to it, and edit that'), () => withPart('edit', x => x.editAsOds(loc, path, name)).catch(fail)) : null,
      conv === 'odt' ? btn(tx('edit as .odt'), tx('documents are edited as .odt: make one from this next to it, and edit that'), () => withPart('edit', x => x.editAsOdt(loc, path, name)).catch(fail)) : null,
      conv === 'odp' ? btn(tx('edit as .odp'), tx('presentations are edited as .odp: make one from this next to it, and edit that'), () => withPart('edit', x => x.editAsOdp(loc, path, name)).catch(fail)) : null,
      // the office suite (Round 18): LibreOffice in the browser, a site of osada 0.2's at /web/<site>/
      S.me.office && officeKind(name) && !offline()
        ? h('a', { class: 'btn-link', href: officeUrl(loc, path, write), text: tx('open in office'), title: tx('open it in the office suite in the browser (LibreOffice); saving there writes it back here') }) : null,
      loc === 'home' && S.me.versions && !offline() ? btn(tx('versions'), tx('earlier versions of {name}, from the backups', { name }),
        () => withPart('versions', x => x.versionsDialog(loc, path)).then(put => { if (put) route(); }, fail)) : null,
      h('a', { class: 'btn-link', href: U.raw(loc, path) + '?dl=1', download: '', text: tx('download') }),
      h('a', { class: 'btn-link', href: back, text: tx('close'), title: tx('close (esc)') })),
    v.el);
  return {
    el, cls: 'fill', title: name,
    status: tx('{size}, modified {time}', { size: fmtSize(ls.file.size), time: fmtTime(ls.file.mtime) }),
    // home coming back does not stop a video
    stay: kindOf(name) === 'video' || kindOf(name) === 'audio',
    destroy: () => v.destroy(),
    key: e => {
      if (isTyping(e) || e.ctrlKey || e.metaKey || e.altKey) return false;
      const media = e.target instanceof HTMLMediaElement;
      if (e.key === 'ArrowLeft' && !media && v.prev) { go(v.prev); return true; }
      if (e.key === 'ArrowRight' && !media && v.next) { go(v.next); return true; }
      if (e.key === 'Escape') { navigate(back); return true; }
      if (e.key === 'e' && canEdit) { navigate(U.edit(loc, path)); return true; }
      return false;
    },
  };
}

/* ---- shared with me / by me ---- */
/* what someone of a federated instance shares (Round 22): browsed through this instance, view only */
function fedBrowse(s) {
  const box = h('div', { class: 'stack fed-browse' });
  const raw = (path, dl) => '/api/federated/' + s.id + '/raw?path=' + encodeURIComponent(path) + (dl ? '&dl=1' : '');
  async function show(path) {
    box.replaceChildren(h('div', { class: 'dim', text: tx('asking {instance}…', { instance: s.instance }) }));
    let r;
    try { r = await api('GET', '/api/federated/' + s.id + '/ls?path=' + encodeURIComponent(path)); } catch (e) { box.replaceChildren(h('div', { class: 'bad', text: e.message })); return; }
    const up = path ? path.split('/').slice(0, -1).join('/') : null;
    const rows = (r.entries || []).map(e => {
      const p = (path ? path + '/' : '') + e.name;
      return h('tr', {}, h('td', {}, e.dir ? h('a', { href: '#', text: e.name + '/', onclick: ev => { ev.preventDefault(); show(p); } })
        : h('a', { href: raw(p, false), target: '_blank', rel: 'noopener', text: e.name })),
        h('td', { class: 'num dim', text: e.dir ? '' : fmtSize(e.size) }),
        h('td', { class: 'act' }, e.dir ? null : h('a', { class: 'btn', href: raw(p, true), text: tx('download') })));
    });
    box.replaceChildren(
      h('div', { class: 'row' }, h('b', { text: s.name + (path ? '/' + path : '') }), up !== null ? btn(tx('up'), null, () => show(up)) : null),
      r.file ? h('div', { class: 'row' }, h('a', { href: raw('', false), target: '_blank', rel: 'noopener', text: tx('open') }), h('a', { class: 'btn', href: raw('', true), text: tx('download') }), h('span', { class: 'dim', text: fmtSize(r.file.size) }))
        : rows.length ? h('table', { class: 'x11' }, h('tbody', {}, rows)) : h('div', { class: 'dim', text: tx('empty') }));
  }
  return dialog({ title: tx('{name}, from {user}', { name: s.name, user: s.from }), wide: true, body: box,
    buttons: [{ label: tx('close'), value: true, primary: true }], opened: () => show('') });
}

async function sharedPage() {
  const [d, fed] = await Promise.all([api('GET', '/api/shares'), api('GET', '/api/federated').catch(() => ({ incoming: [], outgoing: [], peers: [] }))]);
  const box = h('div', { class: 'stack' });
  const withMe = d.with_me.map(s => h('tr', {},
    h('td', {}, h('a', { href: s.dir ? U.files(s.loc, '/') : U.view(s.loc, '/'), text: s.name + (s.dir ? '/' : '') })),
    h('td', { text: s.user }), h('td', { class: s.write ? 'ok' : 'dim', text: s.write ? tx('can edit') : tx('read only') }),
    h('td', { class: 'dim', text: fmtDate(s.created) }),
    h('td', { class: 'act' }, offline() ? null : btn(tx('remove'), tx('drop it from your list'), async () => {
      if (await confirmBox(tx('remove'), tx('remove {name} from your list? {user} keeps it.', { name: s.name, user: s.user }), tx('remove'))) api('DELETE', '/api/shares/' + s.id).then(() => navigate(location.pathname, true), fail);
    }))));
  const byMe = d.by_me.map(s => h('tr', {},
    h('td', {}, h('a', { href: s.dir ? U.files(s.loc, s.path) : U.view(s.loc, s.path), text: s.name + (s.dir ? '/' : '') })),
    h('td', { class: 'dim', text: locName(s) + ':' + parentPath(s.path) }),
    h('td', { text: s.user }), h('td', { class: s.write ? 'ok' : 'dim', text: s.write ? tx('can edit') : tx('read only') }),
    h('td', { class: 'dim', text: fmtDate(s.created) }),
    h('td', { class: 'act' }, offline() ? null : btn(tx('stop'), tx('stop sharing'), () => api('DELETE', '/api/shares/' + s.id).then(() => navigate(location.pathname, true), fail)))));
  const table = (heads, rows, none) => rows.length
    ? h('table', { class: 'x11' }, h('thead', {}, h('tr', {}, heads.map(t => h('th', { text: tx(t) })))), h('tbody', {}, rows))
    : h('div', { class: 'dim', text: none });
  // with people of federated instances (Round 22): view only
  const fedIn = fed.incoming.map(s => h('tr', { class: 'fed' },
    h('td', {}, h('a', { href: '#', text: s.name + (s.dir ? '/' : ''), onclick: e => { e.preventDefault(); fedBrowse(s); } })),
    h('td', {}, h('span', { title: s.from_display, text: s.from })), h('td', { class: 'dim', text: tx('read only') }),
    h('td', { class: 'dim', text: fmtDate(s.created) }),
    h('td', { class: 'act' }, btn(tx('remove'), tx('drop it from your list'), async () => {
      if (await confirmBox(tx('remove'), tx('remove {name} from your list? {user} keeps it.', { name: s.name, user: s.from }), tx('remove'))) api('DELETE', '/api/federated/' + s.id).then(() => navigate(location.pathname, true), fail);
    }))));
  const fedOut = fed.outgoing.map(s => h('tr', { class: 'fed' },
    h('td', {}, h('a', { href: s.dir ? U.files(s.loc, s.path) : U.view(s.loc, s.path), text: s.name + (s.dir ? '/' : '') })),
    h('td', { class: 'dim', text: locName(s) + ':' + parentPath(s.path) }),
    h('td', { text: s.upn }), h('td', { class: 'dim', text: tx('read only') }),
    h('td', { class: 'dim', text: fmtDate(s.created) }),
    h('td', { class: 'act' }, offline() ? null : btn(tx('stop'), tx('stop sharing'), () => api('DELETE', '/api/fedshares/' + s.id).then(() => navigate(location.pathname, true), fail)))));
  box.append(
    win(tx('shared with me'), table(['name', 'from', 'access', 'since', ''], [...withMe, ...fedIn], tx('nothing yet'))),
    win(tx('shared by me'), table(['name', 'in', 'with', 'access', 'since', ''], [...byMe, ...fedOut], tx('nothing: use share in the file list'))));
  return { el: box, title: tx('shared') };
}

async function linksPage() {
  const links = await api('GET', '/api/links');
  const rows = links.map(l => {
    const url = absUrl(l.url);
    const expired = l.expires && l.expires * 1000 < Date.now();
    return h('tr', {},
      h('td', {}, h('a', { href: l.dir ? U.files(l.loc, l.path) : U.view(l.loc, l.path), text: l.name + (l.dir ? '/' : '') })),
      h('td', { class: 'dim', text: locName(l) + ':' + parentPath(l.path) }),
      h('td', { class: 'url' }, h('a', { href: url, target: '_blank', rel: 'noopener', text: url })),
      h('td', { class: expired ? 'bad' : 'dim', text: l.expires ? (expired ? tx('expired {date}', { date: fmtDate(l.expires) }) : fmtDate(l.expires)) : tx('never') }),
      h('td', { class: 'num', text: l.hits }),
      h('td', { class: 'dim', text: fmtDate(l.created) }),
      h('td', { class: 'act' }, btn(tx('copy'), null, () => copyText(url).then(ok => toast(ok ? tx('copied') : tx('copy failed'), ok ? '' : 'err'))),
        offline() ? null : btn(tx('remove'), tx('the link stops working'), () => api('DELETE', '/api/links/' + l.id).then(() => navigate(location.pathname, true), fail))));
  });
  const body = rows.length
    ? h('table', { class: 'x11' }, h('thead', {}, h('tr', {}, ['item', 'in', 'link', 'expires', 'opened', 'created', ''].map(t => h('th', { text: tx(t) })))), h('tbody', {}, rows))
    : h('div', { class: 'dim', text: tx('no links: use share in the file list') });
  return { el: h('div', { class: 'stack' }, win(tx('public links'), body)), title: tx('links') };
}

/* ---- wiring ---- */
document.addEventListener('click', e => {
  if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  const a = e.target.closest('a');
  if (!a || a.target || a.hasAttribute('download') || a.origin !== location.origin) return;
  if (!/^\/(files|view|edit|version|pictures|shared|links|wiki|settings|osada-admin|login)(\/|$)/.test(a.pathname)) return;
  e.preventDefault();
  // a reference to a wiki article nobody wrote yet: its name goes along, to start it with
  const name = a.classList.contains('missing') && a.pathname.startsWith('/wiki/') ? (a.search ? '&' : '?') + 'title=' + encodeURIComponent(a.textContent.trim()) : '';
  navigate(a.pathname + a.search + name);
});
// a page may take its own history steps (the slideshow's open / close)
window.addEventListener('popstate', () => { if (!(S.page && S.page.pop && S.page.pop())) route(); });
document.addEventListener('keydown', e => {
  if ((e.ctrlKey || e.metaKey) && !e.altKey && (e.key === 's' || e.key === 'S')) {
    e.preventDefault();   // never the browser's "save page"
    if (S.page && S.page.save && !dlgOpen) S.page.save();
  }
}, true);
document.addEventListener('keydown', e => {
  if (dlgOpen || e.defaultPrevented || !S.page || !S.page.key) return;
  if (S.page.key(e)) e.preventDefault();
});
// files dropped outside a drop zone must not replace the app
document.addEventListener('dragover', e => { if (hasFiles(e)) e.preventDefault(); });
document.addEventListener('drop', e => { if (hasFiles(e)) e.preventDefault(); });
window.addEventListener('beforeunload', e => {
  if ((S.page && S.page.dirty && S.page.dirty()) || UP.items.some(i => i.state === 'up' || i.state === 'wait')) { e.preventDefault(); e.returnValue = ''; }
});
// the texts of the language in use first (i18n.js)
LANG_READY.then(route);
