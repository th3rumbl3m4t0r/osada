// osada 0.2: the account and admin pages. One module and its tables
// (grid.js), no framework: pages are built with DOM calls (never HTML from
// data) and talk to /api. Every table is a grid (filter, quick filters,
// sorting, columns); everything picked from what users make is a picker.

import { el as h, grid, modal, picker } from './grid.js';

const $app = document.getElementById('app');
const $nav = document.getElementById('nav');
const $who = document.getElementById('who');
const $status = document.getElementById('status');

const DOMAINS = ['directory', 'files', 'wiki', 'links', 'mail', 'chat', 'calendar', 'contacts'];
const RULE_DOMAINS = ['ledger', ...DOMAINS];
const ROLES = ['voter', 'idp', 'store', 'blob', 'edge-http', 'edge-mail', 'cache', 'public-cache', 'worker', 'modhost', 'media'];
// the modules a node can have (Round 14): a tag on a node is its rule for the module's domain
// (on: may, off: never) and the roles the module needs, set with it; the rest of the roles are advanced
const MODULES = [
  { tag: 'idp', label: 'IdP', domain: 'directory', roles: ['idp', 'store'] },
  { tag: 'files', label: 'files', domain: 'files', roles: ['store', 'blob'] },
  { tag: 'mail', label: 'mail', domain: 'mail', roles: ['edge-mail'] },
  { tag: 'web', label: 'web', roles: ['edge-http'] },
  { tag: 'chat', label: 'chat', domain: 'chat', roles: ['store'] },
  { tag: 'wiki', label: 'wiki', domain: 'wiki', roles: ['store'] },
  { tag: 'links', label: 'links', domain: 'links', roles: ['store'] },
  { tag: 'cache', label: 'cache', roles: ['cache'] },
  { tag: 'public-cache', label: 'public cache', roles: ['public-cache'] },
];
const MANAGED_ROLES = ['idp', 'store', 'blob', 'edge-mail', 'edge-http', 'cache', 'public-cache'];
function hasModule(roles, rules, m) {
  return m.roles.every(r => roles.has(r)) && (!m.domain || (rules[m.domain] || 'may') !== 'never');
}
// a module on or off: its rule, and the managed roles the modules left on need (others kept)
function setModule(roles, rules, m, on) {
  if (m.domain) { if (on) { if (rules[m.domain] === 'never') delete rules[m.domain]; } else rules[m.domain] = 'never'; }
  const onNow = MODULES.filter(x => x === m ? on : hasModule(roles, rules, x));
  const need = new Set(onNow.flatMap(x => x.roles));
  for (const r of MANAGED_ROLES) { if (need.has(r)) roles.add(r); else roles.delete(r); }
}
function modulesOf(n) {
  return MODULES.filter(m => hasModule(new Set(n.roles), n.rules || {}, m));
}
const LOGS = ['auth', 'admin', 'access', 'http', 'node', 'ops'];
// how the settings name a log (access: files' reads and changes)
const LOG_LABEL = { access: 'files', http: 'http requests' };
const LEVELS = ['off', 'minimal', 'standard', 'detailed', 'debug'];
const EVENTS = ['sign_in', 'sign_in_failed', 'sign_out', 'session_ended', 'password_changed', 'user_created', 'user_updated',
  'group_created', 'member_added', 'member_removed', 'role_granted', 'role_revoked', 'node_control_changed', 'settings_changed', 'handover_requested'];
const PERM_LABEL = {
  'cluster-manage': 'cluster', 'ou-manage': 'OUs', 'ou-security': 'OU security', 'directory-read': 'read directory',
  'directory-manage': 'manage directory', 'password-set': 'passwords', 'account-lock': 'lock accounts',
  'roles-grant': 'grant roles', 'logs-read': 'read logs', 'data-read': 'read data (eDiscovery)',
};

function win(title, ...body) {
  return h('section', { class: 'win' }, h('div', { class: 'titlebar' }, title), h('div', { class: 'body' }, ...body));
}

function status(text, kind) {
  $status.textContent = text || '';
  $status.className = kind || '';
}

function when(ms) {
  return ms ? new Date(ms).toLocaleString() : '';
}

function age(ms) {
  let v = Math.max(0, (Date.now() - ms) / 1000);
  for (const [n, name] of [[60, 'sec.'], [60, 'min.'], [24, 'hours'], [7, 'days'], [4.35, 'weeks'], [12, 'months'], [Infinity, 'years']]) {
    if (v < n) return `${Math.floor(v)} ${name}`;
    v /= n;
  }
  return '';
}

function short(id) {
  return h('code', { title: id }, String(id).slice(0, 8));
}

// home's state as the page sees it (D-U39): the page comes from the edge at once; what needs
// home comes by the API, and a request that takes long shows "waiting for home", an edge's
// 503 with x-shelf-offline "home unreachable" (until a request gets through again)
let waiting = 0, slow = false, homeAway = false;
// the node that answers, by its name (node control's): from the page at once, then the session's
const PAGE_NODE = (document.querySelector('meta[name="osada-node"]') || {}).content || 'home';
const homeName = () => (me && me.node_name) || PAGE_NODE;
function homeState() {
  let el = document.getElementById('home-state');
  if (!el) { el = h('div', { id: 'home-state', class: 'home-state', role: 'status', hidden: true }); document.body.append(el); }
  if (homeAway) { el.textContent = `${homeName()} cannot be reached · trying again`; el.className = 'home-state bad'; el.hidden = false; }
  else if (slow && waiting > 0) { el.textContent = `waiting for ${homeName()}…`; el.className = 'home-state'; el.hidden = false; }
  else el.hidden = true;
}

async function api(method, path, body) {
  const opt = { method, headers: { 'x-shelf': '1' }, credentials: 'same-origin' };
  if (body !== undefined) {
    opt.headers['content-type'] = 'application/json';
    opt.body = JSON.stringify(body);
  }
  waiting++;
  const t = setTimeout(() => { slow = true; homeState(); }, 700);
  let r;
  try { r = await fetch(path, opt); }
  finally { waiting--; clearTimeout(t); if (!waiting) slow = false; }
  homeAway = r.status === 503 && r.headers.get('x-shelf-offline') === '1';
  homeState();
  let data = {};
  try { data = await r.json(); } catch (e) { /* no body */ }
  if (!r.ok) {
    const err = new Error(data.error || r.statusText);
    err.status = r.status;
    throw err;
  }
  return data;
}

function select(options, current, onchange, attrs) {
  return h('select', { ...(attrs || {}), onchange: e => onchange(e.target.value) },
    options.map(([value, label, disabled]) => h('option', { value, selected: value === current, disabled }, label)));
}

// a choice of several (Round 14): what is chosen, comma separated, and a button that opens every
// option on a row of its own with a tick box
function multiPick(options, chosen, onchange, attrs) {
  const text = h('span', { class: 'mpick-text' });
  const panel = h('div', { class: 'mpick-panel', hidden: true });
  // the open panel sits above everything (fixed to the window, under the button, or above it when
  // there is no room below), so a table's edge never cuts it off
  const btn = h('button', { type: 'button', class: 'mpick-btn', title: 'choose', onclick: e => {
    e.stopPropagation();
    const was = panel.hidden;
    for (const x of document.querySelectorAll('.mpick-panel')) x.hidden = true;
    if (!was) return;
    panel.hidden = false;
    const r = btn.getBoundingClientRect(), ph = panel.offsetHeight, pw = panel.offsetWidth;
    panel.style.left = `${Math.max(4, Math.min(r.left, window.innerWidth - pw - 8))}px`;
    panel.style.top = `${r.bottom + ph + 4 > window.innerHeight && r.top > ph + 4 ? r.top - ph - 2 : r.bottom + 2}px`;
  } }, '▾');
  const wrap = h('span', { class: 'mpick', ...(attrs || {}) }, text, btn, panel);
  const draw = () => {
    const shown = options.filter(([v]) => chosen.has(v)).map(([, l]) => l);
    text.textContent = shown.length ? shown.join(', ') : '—';
    text.className = shown.length ? 'mpick-text' : 'mpick-text dim';
    panel.replaceChildren(...options.map(([v, l, title]) => h('label', { class: 'mpick-row', title: title || '' },
      check(chosen.has(v), on => { if (on) chosen.add(v); else chosen.delete(v); draw(); onchange(v, on, new Set(chosen)); }, { 'data-value': v }), l)));
  };
  draw();
  return wrap;
}
document.addEventListener('mousedown', e => {
  for (const x of document.querySelectorAll('.mpick-panel')) if (!x.hidden && !x.parentNode.contains(e.target)) x.hidden = true;
});
// a panel stays where it was opened: the page or a table scrolling closes it
window.addEventListener('scroll', e => {
  for (const x of document.querySelectorAll('.mpick-panel')) if (!x.hidden && !x.contains(e.target)) x.hidden = true;
}, true);
window.addEventListener('resize', () => { for (const x of document.querySelectorAll('.mpick-panel')) x.hidden = true; });

function check(on, onchange, attrs) {
  return h('input', { ...(attrs || {}), type: 'checkbox', checked: !!on, onchange: e => onchange(e.target.checked) });
}

function confirmModal(title, text, action) {
  const m = modal(title, h('p', {}, text), [
    h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
    h('button', { type: 'button', class: 'primary', onclick: async () => { m.close(); await action(); } }, 'yes'),
  ]);
}

let me = null;
// inside the drop-in upgrade of osada 0.1: its pages (files, wiki, …) are the rest of the site
const FILES = document.documentElement.dataset.files === '1';

// where to go after signing in (?next=, from the files pages or another program's sign-in): only this site
function nextUrl() {
  const n = new URLSearchParams(location.search).get('next');
  return n && n.startsWith('/') && !n.startsWith('//') && !n.startsWith('/\\') ? n : null;
}

// osada 0.1's view of the account (its /api/me): the apps, storage, its admin flag
let mine = null;

async function route() {
  // the first time: the header's place at once, the page waits for home's answer
  if (!me && !$nav.childElementCount) $nav.append(h('a', { href: '/', class: 'portal' }, 'osada'), h('span', { class: 'nodename dim' }, '// …'));
  try { me = await api('GET', '/api/account'); } catch (e) {
    me = null;
    if (homeAway) {
      $app.replaceChildren(win('osada', h('p', {}, `${homeName()} cannot be reached right now; this page tries again every 10 seconds. Public links whose files the edges keep still work.`)));
      setTimeout(route, 10000);
      return;
    }
  }
  mine = null;
  if (me && FILES) { try { mine = await api('GET', '/api/me'); } catch (e) { mine = null; } }
  topbar();
  $app.replaceChildren();
  if (!me) return signInPage();
  if (location.pathname === '/signin') {
    const n = nextUrl();
    // a full load: an OIDC sign-in goes on at the server, the files pages are another app
    if (n) { location.replace(n); return; }
    return go('/', true);
  }
  if (location.pathname.startsWith('/admin')) {
    if (!me.staff) return go('/', true);
    return adminPage();
  }
  chatPoll();
  if (location.pathname === '/account') return accountPage();
  if (location.pathname === '/chat') return chatPage();
  if (location.pathname === '/contacts') return contactsPage();
  if (location.pathname === '/calendar') return calendarPage();
  return portalPage();
}

function go(path, replace) {
  if (FILES && !/^\/(account|admin|signin|chat|contacts|calendar)?(\/|$|\?)/.test(path)) { location.assign(path); return; }
  history[replace ? 'replaceState' : 'pushState'](null, '', path);
  route();
}

window.addEventListener('popstate', route);
document.addEventListener('click', e => {
  const a = e.target.closest('a[data-go]');
  if (a) { e.preventDefault(); go(a.getAttribute('href')); }
});

/// The header (D-U13), as osada 0.1's pages draw it too: the portal at the left, the apps, then at the
/// right admin and the user.
function topbar() {
  $nav.replaceChildren();
  $who.replaceChildren();
  if (!me) return;
  const at = location.pathname;
  $nav.append(h('a', { href: '/', 'data-go': true, class: 'portal' + (at === '/' ? ' on' : '') }, 'osada'),
    // the instance (Round 24: "osada // <instance>"); the nodes this page came through in its title (T14)
    h('span', { class: 'nodename dim', title: me.via ? `you are connected to node ${me.via_name || me.via}, which relays to node ${me.node_name || me.node}, which answers` : `you are connected to node ${me.node_name || me.node}, which answers` },
      '// ' + (me.instance || me.node_name || me.node)));
  const apps = (mine && mine.apps) || [];
  for (const a of apps) {
    $nav.append(a.external ? h('a', { href: a.href, target: '_blank', rel: 'noopener', class: 'app' }, a.title)
      : a.name === 'chat' ? h('a', { href: '/chat', 'data-go': true, class: 'app chatlink' + (at === '/chat' ? ' on' : '') }, 'chat')
      : a.name === 'contacts' || a.name === 'calendar' ? h('a', { href: a.href, 'data-go': true, class: 'app' + (at === a.href ? ' on' : '') }, a.title)
      : h('a', { href: a.href, class: 'app' }, a.title));
  }
  // chat is built in (D-U42): there without osada 0.1's pages too
  if (!apps.some(a => a.name === 'chat')) $nav.append(h('a', { href: '/chat', 'data-go': true, class: 'app chatlink' + (at === '/chat' ? ' on' : '') }, 'chat'));
  // contacts and calendar (Round 15): osada's pages over the mail server's
  for (const [path, label] of [['/contacts', 'contacts'], ['/calendar', 'calendar']]) {
    if (!apps.some(a => a.name === label)) $nav.append(h('a', { href: path, 'data-go': true, class: 'app' + (at === path ? ' on' : '') }, label));
  }
  // admin at the end of the left side, never hidden
  if (me.staff || (mine && mine.admin_link)) $nav.append(h('a', { href: '/admin', 'data-go': true, class: 'admin' + (at.startsWith('/admin') ? ' on' : '') }, 'admin'));
  $who.append(
    h('a', { href: '/account', 'data-go': true, class: 'navlink' + (at === '/account' ? ' on' : ''), title: `signed in at node ${me.node_name || me.node}` }, me.user.name),
    FILES ? h('a', { href: '/settings', class: 'navlink' }, 'settings') : null,
    h('button', { type: 'button', onclick: async () => { await api('POST', '/api/signout').catch(() => {}); go('/signin', true); } }, 'sign out'),
  );
  fitNav($nav, $who);
  chatBadges();
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

window.addEventListener('resize', () => requestAnimationFrame(() => fitNav($nav, $who)));

// ---- the portal: what this account may use (D-U12) -------------------------

// the apps' icons (T7), drawn as maildot's mail icon is: an x11 window, a
// light pictogram with a green stroke, the green dot of "mail."
const ICON_PICTO = {
  mail: `<rect x="22" y="40" width="84" height="58" fill="none" class="l" stroke-width="6"/><polyline points="22,40 64,72 106,40" fill="none" class="g" stroke-width="6"/>`,
  files: `<path d="M22 44h28l8 9h48v45H22z" fill="none" class="l" stroke-width="6" stroke-linejoin="miter"/><line x1="22" y1="64" x2="106" y2="64" class="g" stroke-width="6"/>`,
  shared: `<circle cx="48" cy="56" r="11" fill="none" class="l" stroke-width="6"/><path d="M26 98c2-16 12-24 22-24s20 8 22 24" fill="none" class="l" stroke-width="6"/><circle cx="82" cy="56" r="11" fill="none" class="g" stroke-width="6"/><path d="M68 76c4-2 9-2 14-2 10 0 20 8 22 24" fill="none" class="g" stroke-width="6"/>`,
  links: `<rect x="22" y="56" width="46" height="26" rx="13" fill="none" class="l" stroke-width="6"/><rect x="60" y="56" width="46" height="26" rx="13" fill="none" class="g" stroke-width="6"/>`,
  wiki: `<rect x="34" y="36" width="60" height="66" fill="none" class="l" stroke-width="6"/><line x1="44" y1="54" x2="84" y2="54" class="g" stroke-width="6"/><line x1="44" y1="68" x2="84" y2="68" class="l" stroke-width="5"/><line x1="44" y1="82" x2="72" y2="82" class="l" stroke-width="5"/>`,
  proxy: `<rect x="24" y="40" width="80" height="58" fill="none" class="l" stroke-width="6"/><polyline points="40,69 86,69" fill="none" class="g" stroke-width="6"/><polyline points="74,57 86,69 74,81" fill="none" class="g" stroke-width="6"/>`,
  account: `<circle cx="64" cy="56" r="15" fill="none" class="l" stroke-width="6"/><path d="M34 102c3-18 15-28 30-28s27 10 30 28" fill="none" class="l" stroke-width="6"/><line x1="52" y1="102" x2="76" y2="102" class="g" stroke-width="6"/>`,
  admin: `<line x1="26" y1="52" x2="102" y2="52" class="l" stroke-width="5"/><line x1="26" y1="72" x2="102" y2="72" class="l" stroke-width="5"/><line x1="26" y1="92" x2="102" y2="92" class="l" stroke-width="5"/><rect x="42" y="44" width="14" height="16" class="gf"/><rect x="74" y="64" width="14" height="16" class="gf"/><rect x="54" y="84" width="14" height="16" class="gf"/>`,
  chat: `<path d="M24 40h80v46H56l-18 14V86H24z" fill="none" class="l" stroke-width="6" stroke-linejoin="miter"/><line x1="40" y1="60" x2="88" y2="60" class="g" stroke-width="6"/>`,
  contacts: `<rect x="30" y="34" width="68" height="66" fill="none" class="l" stroke-width="6"/><circle cx="64" cy="58" r="10" fill="none" class="g" stroke-width="6"/><path d="M46 90c3-10 10-14 18-14s15 4 18 14" fill="none" class="g" stroke-width="6"/>`,
  calendar: `<rect x="26" y="38" width="76" height="62" fill="none" class="l" stroke-width="6"/><line x1="26" y1="54" x2="102" y2="54" class="l" stroke-width="6"/><rect x="40" y="64" width="12" height="12" class="gf"/><rect x="58" y="64" width="12" height="12" class="gf"/><rect x="76" y="80" width="12" height="12" class="gf"/>`,
};
function appIcon(kind) {
  const picto = ICON_PICTO[kind] || ICON_PICTO.proxy;
  const span = h('span', { class: 'appicon', 'aria-hidden': 'true' });
  // static markup of this file only, never data
  // the theme's colours (night or day, and the accent): .appicon rules in app.css
  span.innerHTML = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="56" height="56"><rect class="frame" x="4" y="4" width="120" height="120" stroke-width="4"/><rect class="bar" x="6" y="6" width="116" height="18"/><rect class="grp" x="13" y="12" width="5" height="5"/><rect class="grp" x="21" y="12" width="5" height="5"/>${picto}<rect class="gf" x="98" y="102" width="14" height="14"/></svg>`;
  return span;
}

function portalPage() {
  const cards = [];
  const card = (icon, title, href, external, ...body) => {
    const link = !href ? h('span', {}, title) : external ? h('a', { href, target: '_blank', rel: 'noopener' }, title + ' ↗')
      : /^\/(account|admin|chat|contacts|calendar)/.test(href) ? h('a', { href, 'data-go': true }, title) : h('a', { href }, title);
    const pic = !href ? appIcon(icon) : h('a', external ? { href, target: '_blank', rel: 'noopener', tabindex: '-1' } : /^\/(account|admin|chat|contacts|calendar)/.test(href) ? { href, 'data-go': true, tabindex: '-1' } : { href, tabindex: '-1' }, appIcon(icon));
    const s = h('section', { class: 'win card' }, h('div', { class: 'titlebar' }, link), h('div', { class: 'body' }, pic, h('div', { class: 'text' }, ...body)));
    cards.push(s);
  };
  const bar = (used, limit) => h('div', { class: 'meter' }, h('progress', { max: limit, value: Math.min(used, limit) }),
    h('span', { class: used >= limit * 0.8 ? 'warn' : 'dim' }, ` ${fmtBytes(used)} of ${fmtBytes(limit)}`));
  for (const a of (mine && mine.apps) || []) {
    const extra = [];
    if (a.name === 'files' && mine.storage) {
      const s = mine.storage;
      if (s.limit != null && s.used != null) extra.push(bar(s.used, s.limit));
      else if (s.used != null) extra.push(h('p', { class: 'dim' }, `${fmtBytes(s.used)} in your home, no limit`));
      else extra.push(h('p', { class: 'dim' }, s.limit != null ? `storage limit ${fmtBytes(s.limit)}` : 'no storage limit'));
    }
    // T6: the mail server's own pages for the account
    if (a.name === 'mail' && a.account) extra.push(h('p', { class: 'dim' }, 'Passwords for mail programs, encryption of stored mail: ',
      h('a', { href: a.account, target: '_blank', rel: 'noopener', id: 'mail-account' }, 'the mail server\'s account pages ↗')));
    if (a.name === 'chat') extra.push(h('p', { id: 'chat-card-unread', class: 'dim' }));
    card(ICON_PICTO[a.name] ? a.name : a.kind === 'proxy' ? 'proxy' : a.name, a.title, a.href, a.external, h('p', {}, a.about), ...extra);
  }
  for (const [name, about] of [['contacts', 'Everyone: the people here, your address books, whom you mailed with; one search over all of them.'], ['calendar', 'Your calendars on the mail server: the month, the day, new events. Phones and maildot sync them by CalDAV.']]) {
    if (!((mine && mine.apps) || []).some(a => a.name === name)) card(name, name, '/' + name, false, h('p', {}, about));
  }
  if (!((mine && mine.apps) || []).some(a => a.name === 'chat')) {
    card('chat', 'chat', '/chat', false, h('p', {}, 'Rooms and direct messages with the people here.'), h('p', { id: 'chat-card-unread', class: 'dim' }));
  }
  const keys = me.fido2 ? me.fido2.length : 0;
  card('account', 'account', '/account', false,
    h('p', {}, 'Your password, second factors, sessions.'),
    h('dl', { class: 'kv' },
      h('dt', {}, 'second factors'), h('dd', {}, [me.totp ? 'TOTP' : null, keys ? `${keys} security key${keys > 1 ? 's' : ''}` : null].filter(Boolean).join(', ') || h('span', { class: 'warn' }, 'none')),
      h('dt', {}, 'sessions'), h('dd', {}, String(me.sessions.length)),
      h('dt', {}, 'OU'), h('dd', {}, me.ou_path)));
  if (me.staff || (mine && mine.admin_link)) card('admin', 'admin', '/admin', false, h('p', {}, 'Nodes, modules, OUs, users and groups, logs; the settings of each module (files, proxies, sign-in for apps).'));
  if (!FILES) cards.unshift(h('p', { class: 'dim' }, 'This node runs no apps of its own yet.'));
  $app.append(h('div', { class: 'portal' }, cards));
  chatBadges();
}

// ---- chat (§12.1, D-U42) ----------------------------------------------------
// Rooms and direct messages. The page keeps one long poll open (/api/chat/wait, raw fetch: it
// is not "waiting for home"); when the version moves it reloads the room list (unread counts
// for the header and the portal) and what is new in the open room. Typing is said at most every
// 3 s and shown for 6 s; it is never written anywhere.

const chat = { v: 0, rooms: [], polling: false, open: null, items: [], head: 0, read: 0, more: false, firstUnread: 0, typingAt: 0, typingTimer: 0, data: null };

function chatUnread() {
  return chat.rooms.reduce((n, r) => n + (r.unread || 0), 0);
}

// the header's chat link and the page title carry the unread count
function chatBadges() {
  const n = chatUnread();
  for (const a of $nav.querySelectorAll('a.chatlink')) a.textContent = n ? `chat (${n})` : 'chat';
  const base = document.title.replace(/^\(\d+\) /, '');
  document.title = n ? `(${n}) ${base}` : base;
  const c = document.getElementById('chat-card-unread');
  if (c) c.textContent = n ? `${n} unread message${n > 1 ? 's' : ''}` : 'Nothing unread.';
}

async function chatPoll() {
  if (chat.polling) return;
  chat.polling = true;
  let fails = 0;
  try {
    while (me) {
      try {
        const r = await fetch('/api/chat/wait?v=' + chat.v, { headers: { 'x-shelf': '1' }, credentials: 'same-origin' });
        if (r.status === 401) break;
        if (!r.ok) throw new Error(String(r.status));
        const d = await r.json();
        fails = 0;
        if (d.v !== chat.v) { chat.v = d.v; await chatRefresh(); }
      } catch (e) {
        fails++;
        await new Promise(res => setTimeout(res, Math.min(30000, 1000 * 2 ** Math.min(fails, 5))));
      }
    }
  } finally { chat.polling = false; }
}

async function chatRefresh() {
  try { chat.rooms = (await api('GET', '/api/chat/rooms')).rooms; } catch (e) { return; }
  chatBadges();
  if (location.pathname !== '/chat') return;
  chatDrawRooms();
  if (chat.open) await chatLoadNew();
}

function chatTime(ms) {
  const d = new Date(ms), now = new Date();
  const t = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  return d.toDateString() === now.toDateString() ? t : `${d.toLocaleDateString()} ${t}`;
}

// text with its web addresses as links (built as nodes, never HTML)
function chatText(text) {
  const out = [];
  let last = 0;
  for (const m of text.matchAll(/https?:\/\/[^\s<>"']+[^\s<>"'.,;:!?)\]]/g)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    out.push(h('a', { href: m[0], target: '_blank', rel: 'noopener noreferrer' }, m[0]));
    last = m.index + m[0].length;
  }
  out.push(text.slice(last));
  return out;
}

function chatUrl(id) {
  return id ? `/chat?room=${encodeURIComponent(id)}` : '/chat';
}

function chatPage() {
  const params = new URLSearchParams(location.search);
  chat.open = params.get('room') || null;
  const list = h('div', { id: 'chat-rooms', class: 'chat-rooms' });
  const pane = h('section', { id: 'chat-pane', class: 'win chat-pane' });
  const side = h('section', { class: 'win chat-side' }, h('div', { class: 'titlebar' }, 'chat'),
    h('div', { class: 'body' },
      h('div', { class: 'row' },
        h('button', { type: 'button', onclick: chatNewRoom }, 'new room'),
        h('button', { type: 'button', onclick: chatNewDm }, 'message someone')),
      list));
  $app.append(h('div', { class: 'chat' }, side, pane));
  chatDrawRooms();
  api('GET', '/api/chat/rooms').then(d => { chat.rooms = d.rooms; chatBadges(); chatDrawRooms(); }).catch(e => status(e.message, 'bad'));
  if (chat.open) chatOpen(chat.open, true);
  else chatDrawPane();
}

function chatDrawRooms() {
  const list = document.getElementById('chat-rooms');
  if (!list) return;
  if (!chat.rooms.length) {
    list.replaceChildren(h('p', { class: 'dim' }, 'No rooms yet: make one, or message someone.'));
    return;
  }
  list.replaceChildren(...chat.rooms.map(r => h('a', {
    class: 'chat-room-item' + (r.id === chat.open ? ' on' : '') + (r.unread ? ' unread' : ''), href: chatUrl(r.id),
    onclick: e => { e.preventDefault(); chatOpen(r.id); },
  },
  h('div', { class: 'top' }, h('span', { class: 'n' }, r.kind === 'dm' ? '@ ' + r.name : '# ' + r.name),
    r.unread ? h('span', { class: 'badge' }, String(r.unread)) : null,
    r.last ? h('span', { class: 't dim' }, chatTime(r.last.time)) : null),
  h('div', { class: 'last dim' }, r.typing && r.typing.length ? `${r.typing.join(', ')} typing…`
    : r.last ? `${r.kind === 'dm' && r.last.author === me.user.id ? 'you: ' : r.kind === 'dm' ? '' : r.last.author_display + ': '}${r.last.text}` : (r.topic || 'no messages yet')))));
}

async function chatOpen(id, replace) {
  if (chat.open !== id || !replace) history[replace ? 'replaceState' : 'pushState'](null, '', chatUrl(id));
  chat.open = id;
  chat.items = []; chat.data = null;
  chatDrawRooms();
  chatDrawPane();
  let d;
  try { d = await api('GET', `/api/chat/rooms/${id}?limit=60`); } catch (e) {
    if (chat.open === id) document.getElementById('chat-pane').replaceChildren(h('div', { class: 'titlebar' }, 'chat'), h('div', { class: 'body' }, h('p', { class: 'bad' }, e.message)));
    return;
  }
  if (chat.open !== id) return;
  chat.data = d; chat.items = d.items; chat.head = d.head; chat.read = d.read; chat.more = d.more;
  chat.firstUnread = (d.items.find(i => i.kind === 'say' && i.seq > d.read && i.author !== me.user.id) || {}).seq || 0;
  chatDrawPane();
  chatScroll(true);
  chatMarkRead();
}

async function chatLoadNew() {
  const id = chat.open;
  if (!id || !chat.data) return;
  for (let i = 0; i < 5; i++) {
    let d;
    try { d = await api('GET', `/api/chat/rooms/${id}?after=${chat.head}`); } catch (e) { return; }
    if (chat.open !== id) return;
    const log = document.getElementById('chat-log');
    const atEnd = !log || log.scrollHeight - log.scrollTop - log.clientHeight < 60;
    const deleted = new Set(d.deleted);
    chat.items = chat.items.filter(it => !deleted.has(it.seq));
    for (const c of d.changed) { const it = chat.items.find(x => x.seq === c.seq); if (it) { it.text = c.text; it.edited = c.edited; } }
    chat.items.push(...d.items.filter(it => !deleted.has(it.seq)));
    chat.head = d.head;
    Object.assign(chat.data, { name: d.name, topic: d.topic, members: d.members, owner: d.owner, typing: d.typing });
    chatDrawPane(true);
    chatScroll(atEnd);
    if (!d.more) break;
  }
  chatMarkRead();
}

async function chatOlder() {
  const id = chat.open;
  const first = chat.items[0];
  if (!first) return;
  const d = await api('GET', `/api/chat/rooms/${id}?before=${first.seq}&limit=60`);
  if (chat.open !== id) return;
  const log = document.getElementById('chat-log');
  const from = log ? log.scrollHeight - log.scrollTop : 0;
  chat.items = [...d.items, ...chat.items];
  chat.more = d.more;
  chatDrawPane(true);
  const l2 = document.getElementById('chat-log');
  if (l2) l2.scrollTop = l2.scrollHeight - from;
}

function chatScroll(toEnd) {
  const log = document.getElementById('chat-log');
  if (log && toEnd) log.scrollTop = log.scrollHeight;
}

// read up to the last message, when the page is seen
async function chatMarkRead() {
  const id = chat.open;
  const last = [...chat.items].reverse().find(i => i.kind === 'say');
  if (!id || !last || last.seq <= chat.read || document.visibilityState !== 'visible' || !document.hasFocus()) return;
  chat.read = last.seq;
  const r = chat.rooms.find(x => x.id === id);
  if (r) { r.unread = 0; chatBadges(); chatDrawRooms(); }
  try { await api('POST', `/api/chat/rooms/${id}/read`, { seq: last.seq }); } catch (e) { /* next time */ }
}
window.addEventListener('focus', () => { if (location.pathname === '/chat') chatMarkRead(); });
document.addEventListener('visibilitychange', () => { if (location.pathname === '/chat') chatMarkRead(); });

async function chatAct(body) {
  try { return await api('POST', `/api/chat/rooms/${chat.open}/do`, body); } catch (e) { status(e.message, 'bad'); throw e; }
}

function chatItem(it, prev) {
  if (it.kind === 'event') {
    const who = it.author === me.user.id ? 'you' : it.author_display;
    const text = {
      create: chat.data && chat.data.kind === 'dm' ? `${who} started this conversation` : `${who} made the room${it.name ? ' ' + it.name : ''}${it.users && it.users.length ? ' with ' + it.users.join(', ') : ''}`,
      rename: `${who} renamed the room to ${it.name}`,
      topic: it.topic ? `${who} set the topic: ${it.topic}` : `${who} cleared the topic`,
      add: `${who} added ${(it.users || []).join(', ')}`,
      remove: `${who} took ${(it.users || []).join(', ')} out`,
      leave: `${who} left`,
    }[it.event] || `${who}: ${it.event}`;
    return h('div', { class: 'chat-event' }, h('span', { class: 't' }, chatTime(it.time)), ' ', text);
  }
  const mine = it.author === me.user.id;
  const same = prev && prev.kind === 'say' && prev.author === it.author && it.time - prev.time < 5 * 60 * 1000;
  const acts = h('span', { class: 'acts' },
    mine ? h('a', { title: 'edit', onclick: () => chatEdit(it) }, 'edit') : null,
    mine || (chat.data && chat.data.owner) ? h('a', { title: 'delete', onclick: () => confirmModal('delete the message', 'Delete this message for everyone?', () => chatAct({ op: 'delete', seq: it.seq }).catch(() => {})) }, 'delete') : null);
  return h('div', { class: 'chat-msg' + (same ? ' cont' : '') + (mine ? ' mine' : ''), 'data-seq': it.seq },
    same ? null : h('div', { class: 'head' }, h('span', { class: 'who' }, it.author_display), ' ', h('span', { class: 't', title: when(it.time) }, chatTime(it.time))),
    h('div', { class: 'text' }, chatText(it.text), it.edited ? h('span', { class: 'dim edited', title: when(it.edited) }, ' (edited)') : null, acts));
}

function chatEdit(it) {
  const ta = h('textarea', { rows: 4, style: 'width: 100%; box-sizing: border-box' });
  ta.value = it.text;
  const m = modal('edit the message', ta, [
    h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
    h('button', { type: 'button', class: 'primary', onclick: async () => { try { await chatAct({ op: 'edit', seq: it.seq, text: ta.value }); m.close(); } catch (e) { /* shown */ } } }, 'save'),
  ]);
  ta.focus();
}

function chatDrawPane(keep) {
  const pane = document.getElementById('chat-pane');
  if (!pane) return;
  const d = chat.data;
  if (!chat.open || !d) {
    pane.removeAttribute('data-room');
    pane.replaceChildren(h('div', { class: 'titlebar' }, 'chat'), h('div', { class: 'body' },
      h('p', { class: 'dim' }, chat.open ? 'loading…' : 'Pick a room at the left, make one, or message someone.')));
    return;
  }
  // the first time for this room: the frame and the box to write in, kept from then on (its
  // text and focus survive what comes in)
  if (pane.dataset.room !== chat.open) {
    pane.dataset.room = chat.open;
    const ta = h('textarea', { id: 'chat-compose', rows: 2, 'aria-label': 'message' });
    ta.addEventListener('keydown', e => { if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); chatSend(ta); } });
    ta.addEventListener('input', () => chatTyping(ta));
    pane.replaceChildren(h('div', { class: 'titlebar', id: 'chat-title' }),
      h('div', { class: 'body' },
        h('div', { id: 'chat-head', class: 'chat-head row' }),
        h('div', { id: 'chat-log', class: 'chat-log' }),
        h('div', { id: 'chat-typing', class: 'chat-typing' }),
        h('div', { class: 'chat-compose' }, ta, h('button', { type: 'button', class: 'primary', onclick: () => chatSend(ta) }, 'send'))));
    if (!keep) ta.focus();
  }
  document.getElementById('chat-title').textContent = d.kind === 'dm' ? `@ ${d.name}` : `# ${d.name}`;
  document.getElementById('chat-compose').placeholder = d.kind === 'dm' ? `message ${d.name}` : `message # ${d.name}`;
  const people = h('a', { class: 'people', onclick: chatPeople, title: 'who is in the room' }, `${d.members.length} ${d.members.length === 1 ? 'person' : 'people'}`);
  document.getElementById('chat-head').replaceChildren(
    d.kind === 'dm' ? h('span', { class: 'dim' }, `direct messages with ${d.name} (${d.with})`) : h('span', { class: 'dim topic' }, d.topic || 'no topic'),
    h('span', { class: 'spacer' }), people);
  const items = [];
  if (chat.more) items.push(h('div', { class: 'chat-older' }, h('button', { type: 'button', onclick: () => chatOlder().catch(e => status(e.message, 'bad')) }, 'older messages')));
  let prev = null, day = '';
  for (const it of chat.items) {
    const dd = new Date(it.time).toDateString();
    if (dd !== day) { items.push(h('div', { class: 'chat-day' }, new Date(it.time).toLocaleDateString([], { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' }))); day = dd; prev = null; }
    if (chat.firstUnread && it.seq === chat.firstUnread) items.push(h('div', { class: 'chat-new' }, 'new'));
    items.push(chatItem(it, prev));
    prev = it;
  }
  if (!chat.items.length) items.push(h('p', { class: 'dim' }, 'No messages yet.'));
  document.getElementById('chat-log').replaceChildren(...items);
  const typing = (d.typing || []).length ? `${d.typing.join(', ')} ${d.typing.length > 1 ? 'are' : 'is'} typing…` : '';
  document.getElementById('chat-typing').textContent = typing;
  clearTimeout(chat.typingTimer);
  if (typing) chat.typingTimer = setTimeout(() => { if (chat.data) { chat.data.typing = []; const t = document.getElementById('chat-typing'); if (t) t.textContent = ''; } }, 6500);
}

async function chatSend(ta) {
  const text = ta.value;
  if (!text.trim()) return;
  ta.disabled = true;
  try {
    await chatAct({ op: 'say', text });
    ta.value = '';
    chat.typingAt = 0;
    chat.firstUnread = 0;
  } catch (e) { /* shown */ }
  ta.disabled = false;
  ta.focus();
  await chatLoadNew();
  chatScroll(true);
}

function chatTyping(ta) {
  if (!ta.value.trim() || Date.now() - chat.typingAt < 3000 || !chat.open) return;
  chat.typingAt = Date.now();
  api('POST', `/api/chat/rooms/${chat.open}/typing`).catch(() => {});
}

// someone to write to or add: type a part of a name, pick from up to 15
function peoplePick(onpick, placeholder) {
  const input = h('input', { type: 'text', placeholder: placeholder || 'name', autocomplete: 'off' });
  const dd = h('div', { class: 'dd', hidden: true });
  let t = 0;
  const look = async () => {
    let d;
    try { d = await api('GET', '/api/chat/people?q=' + encodeURIComponent(input.value)); } catch (e) { return; }
    const list = d.people.filter(p => p.id !== me.user.id);
    dd.replaceChildren(...list.map(p => h('div', { class: 'opt', onmousedown: e => { e.preventDefault(); input.value = ''; dd.hidden = true; onpick(p); } }, p.display ? `${p.display} (${p.name})` : p.name)));
    dd.hidden = !list.length;
  };
  input.addEventListener('input', () => { clearTimeout(t); t = setTimeout(look, 250); });
  input.addEventListener('focus', look);
  input.addEventListener('blur', () => setTimeout(() => { dd.hidden = true; }, 150));
  return h('span', { class: 'refpick' }, input, dd);
}

function chatNewRoom() {
  const name = h('input', { type: 'text', placeholder: 'the room\'s name', maxlength: 80 });
  const chosen = new Map();
  const chips = h('div', { class: 'row' });
  const draw = () => chips.replaceChildren(...[...chosen.values()].map(p => h('span', { class: 'chip' }, p.display || p.name, h('a', { onclick: () => { chosen.delete(p.name); draw(); } }, '✕'))));
  const msg = h('p', { class: 'msg bad' });
  const m = modal('new room', h('div', { class: 'stack' }, h('label', {}, 'name'), name, h('label', {}, 'people'), peoplePick(p => { chosen.set(p.name, p); draw(); }, 'add someone'), chips, msg), [
    h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
    h('button', { type: 'button', class: 'primary', onclick: async () => {
      try {
        const d = await api('POST', '/api/chat/rooms', { name: name.value, members: [...chosen.keys()] });
        m.close();
        await chatRefresh();
        chatOpen(d.id);
      } catch (e) { msg.textContent = e.message; }
    } }, 'make the room'),
  ]);
  name.focus();
}

function chatNewDm() {
  const msg = h('p', { class: 'msg bad' });
  const m = modal('message someone', h('div', { class: 'stack' }, peoplePick(async p => {
    try {
      const d = await api('POST', '/api/chat/dm', { user: p.name });
      m.close();
      await chatRefresh();
      chatOpen(d.id);
    } catch (e) { msg.textContent = e.message; }
  }, 'who?'), msg), [h('button', { type: 'button', onclick: () => m.close() }, 'cancel')]);
  const i = m.node.querySelector('input');
  if (i) i.focus();
}

function chatPeople() {
  const d = chat.data;
  if (!d) return;
  const room = d.kind !== 'dm';
  const msg = h('p', { class: 'msg bad' });
  const act = async body => { try { await chatAct(body); m.close(); } catch (e) { msg.textContent = e.message; } };
  const rows = d.members.map(p => h('div', { class: 'row' },
    h('span', {}, p.display, ' ', h('span', { class: 'dim' }, `(${p.name})`)), p.role === 'owner' ? h('span', { class: 'pill on' }, 'owner') : null,
    room && d.owner && p.id !== me.user.id ? h('a', { class: 'bad', onclick: () => act({ op: 'remove', user: p.name }) }, 'take out') : null));
  const name = h('input', { type: 'text', value: d.name, maxlength: 80 });
  const topic = h('input', { type: 'text', value: d.topic, maxlength: 300, style: 'width: 100%' });
  const body = h('div', { class: 'stack' }, rows,
    room ? h('div', { class: 'row' }, peoplePick(p => act({ op: 'add', users: [p.name] }), 'add someone')) : null,
    room && d.owner ? h('div', { class: 'stack' },
      h('label', {}, 'name'), h('div', { class: 'row' }, name, h('button', { type: 'button', onclick: () => act({ op: 'rename', name: name.value }) }, 'rename')),
      h('label', {}, 'topic'), h('div', { class: 'row' }, topic, h('button', { type: 'button', onclick: () => act({ op: 'topic', topic: topic.value }) }, 'set'))) : null,
    msg);
  const m = modal(room ? `# ${d.name}` : `@ ${d.name}`, body, [
    room ? h('button', { type: 'button', class: 'bad', onclick: () => confirmModal('leave the room', `Leave # ${d.name}? Someone in it can add you again.`, async () => {
      try { await chatAct({ op: 'leave' }); m.close(); chat.open = null; chat.data = null; await chatRefresh(); go('/chat', true); } catch (e) { /* shown */ }
    }) }, 'leave') : null,
    h('button', { type: 'button', onclick: () => m.close() }, 'close'),
  ]);
}

// ---- contacts and calendar (Round 15): the person's, from the mail server, with osada's people ----

const pimEsc = s => String(s || '').toLowerCase();

function contactsPage() {
  const search = h('input', { type: 'search', id: 'contacts-q', placeholder: 'search everyone: name, address, phone, company', autocomplete: 'off', style: 'width:100%' });
  const body = h('div', { id: 'contacts-groups' }, h('p', { class: 'dim' }, 'loading…'));
  const detail = h('section', { class: 'win contact-detail', id: 'contact-detail' }, h('div', { class: 'titlebar' }, 'contact'), h('div', { class: 'body' }, h('p', { class: 'dim' }, 'Pick someone.')));
  $app.append(h('div', { class: 'contacts' }, win('contacts', search, body), detail));
  let data = null;
  const show = c => {
    const rows = [];
    for (const e of c.emails || []) rows.push(h('div', {}, h('a', { href: `mailto:${e}` }, e)));
    for (const p of c.phones || []) rows.push(h('div', {}, h('a', { href: `tel:${p.replace(/\s+/g, '')}` }, p)));
    detail.replaceChildren(h('div', { class: 'titlebar' }, c.title), h('div', { class: 'body' },
      h('p', { class: 'dim' }, c.group), c.sub ? h('p', {}, c.sub) : null, ...rows,
      h('div', { class: 'row' },
        c.emails && c.emails[0] ? h('a', { class: 'button', href: `mailto:${c.emails[0]}` }, 'write mail') : null,
        c.osada && c.osada !== me.user.name ? h('button', { type: 'button', onclick: async () => {
          try { const d = await api('POST', '/api/chat/dm', { user: c.osada }); go(`/chat?room=${d.id}`); } catch (e) { status(e.message, 'bad'); }
        } }, 'message in chat') : null)));
  };
  const draw = () => {
    if (!data) return;
    const q = pimEsc(search.value.trim());
    const hit = (...xs) => !q || xs.some(x => pimEsc(x).includes(q));
    const groups = [
      ['people here', (data.people || []).filter(p => hit(p.display, p.name, p.email)).map(p => ({ title: p.display, sub: p.name, addr: p.email || '', emails: p.email ? [p.email] : [], osada: p.name, group: 'people here' }))],
      ['address book', (data.book || []).filter(c => hit(c.name, c.org, ...(c.emails || []), ...(c.phones || []))).map(c => ({ title: c.name || (c.emails || [])[0] || '(no name)', sub: [c.org, (c.books || []).join(', ')].filter(Boolean).join(' · '), emails: c.emails, phones: c.phones, group: 'address book' }))],
      ['mailed with', (data.mailed || []).filter(m => hit(m.name, m.email)).map(m => ({ title: m.name || m.email, sub: `${m.name ? m.email + ' · ' : ''}${m.count} message${m.count > 1 ? 's' : ''}, last ${m.last ? new Date(m.last).toLocaleDateString() : '?'}`, emails: [m.email], group: 'mailed with' }))],
    ];
    body.replaceChildren(...(data.error ? [h('p', { class: 'warn' }, `the mail server: ${data.error}`)] : []), ...(data.note ? [h('p', { class: 'dim' }, data.note)] : []),
      ...groups.map(([name, list]) => h('div', { class: 'contact-group', 'data-group': name },
        h('div', { class: 'contact-group-head' }, `${name} (${list.length})`),
        list.length ? list.slice(0, 500).map(c => h('a', { class: 'contact-item', onclick: () => show(c) }, h('span', { class: 'n' }, c.title), c.sub ? h('span', { class: 'dim' }, ` ${c.sub}`) : null,
          c.addr ? h('span', { class: 'addr' }, ` ${c.addr}`) : null))
          : h('div', { class: 'dim' }, q ? 'nobody matches' : 'nobody'))));
  };
  search.addEventListener('input', draw);
  api('GET', '/api/pim/contacts').then(d => { data = d; draw(); }).catch(e => body.replaceChildren(h('p', { class: 'bad' }, e.message)));
  search.focus();
}

let calMonth = null;
function calendarPage() {
  const now = new Date();
  if (!calMonth) calMonth = new Date(now.getFullYear(), now.getMonth(), 1);
  const head = h('div', { class: 'row' });
  const grid = h('div', { class: 'cal-grid', id: 'cal-grid' });
  const day = h('div', { id: 'cal-day' });
  const pane = h('section', { class: 'win' }, h('div', { class: 'titlebar' }, 'calendar'), h('div', { class: 'body' }, head, grid));
  const side = h('section', { class: 'win' }, h('div', { class: 'titlebar' }, 'the day'), h('div', { class: 'body' }, day));
  $app.append(h('div', { class: 'calendar' }, pane, side));
  let data = { calendars: [], events: [] }, picked = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const ymd = d => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  const startOf = () => { const s = new Date(calMonth); const wd = (s.getDay() + 6) % 7; s.setDate(s.getDate() - wd); return s; };
  const color = id => { const c = (data.calendars || []).find(x => x.id === id); return (c && c.color) || 'var(--accent)'; };
  const eventsOn = d => (data.events || []).filter(e => (e.start || '').slice(0, 10) === ymd(d));
  const timeOf = e => e.showWithoutTime ? 'all day' : (e.start || '').slice(11, 16);
  const drawDay = () => {
    const list = eventsOn(picked);
    day.replaceChildren(h('p', {}, h('b', {}, picked.toLocaleDateString([], { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' }))),
      ...(list.length ? list.map(e => h('div', { class: 'cal-event' }, h('span', { class: 'bar', style: `background:${color(Object.keys(e.calendarIds || {})[0])}` }),
        h('span', { class: 't' }, timeOf(e)), ' ', h('b', {}, e.title || '(no title)'),
        e.locations ? h('span', { class: 'dim' }, ' · ' + Object.values(e.locations).map(l => l.name).filter(Boolean).join(', ')) : null,
        e.description ? h('div', { class: 'dim' }, e.description) : null)) : [h('p', { class: 'dim' }, 'nothing')]),
      h('div', { class: 'row' }, h('button', { type: 'button', id: 'cal-new', onclick: () => newEvent(picked) }, 'new event')));
  };
  const drawGrid = () => {
    head.replaceChildren(h('button', { type: 'button', onclick: () => { calMonth = new Date(calMonth.getFullYear(), calMonth.getMonth() - 1, 1); load(); } }, '<'),
      h('button', { type: 'button', onclick: () => { calMonth = new Date(now.getFullYear(), now.getMonth(), 1); picked = new Date(now.getFullYear(), now.getMonth(), now.getDate()); load(); } }, 'today'),
      h('button', { type: 'button', onclick: () => { calMonth = new Date(calMonth.getFullYear(), calMonth.getMonth() + 1, 1); load(); } }, '>'),
      h('b', { id: 'cal-title' }, calMonth.toLocaleDateString([], { month: 'long', year: 'numeric' })),
      h('span', { class: 'dim' }, (data.calendars || []).map(c => c.name).join(', ')));
    const s = startOf(), cells = [];
    for (const n of ['Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa', 'Su']) cells.push(h('div', { class: 'cal-wd' }, n));
    for (let i = 0; i < 42; i++) {
      const d = new Date(s); d.setDate(s.getDate() + i);
      const evs = eventsOn(d);
      cells.push(h('div', { class: 'cal-cell' + (d.getMonth() !== calMonth.getMonth() ? ' out' : '') + (ymd(d) === ymd(now) ? ' today' : '') + (ymd(d) === ymd(picked) ? ' on' : ''), 'data-day': ymd(d),
        onclick: () => { picked = d; drawGrid(); drawDay(); } },
        h('div', { class: 'cal-n' }, String(d.getDate())),
        ...evs.slice(0, 3).map(e => h('div', { class: 'cal-chip', style: `border-left-color:${color(Object.keys(e.calendarIds || {})[0])}` }, e.title || '(no title)')),
        evs.length > 3 ? h('div', { class: 'dim' }, `+${evs.length - 3}`) : null));
    }
    grid.replaceChildren(...cells);
  };
  const load = async () => {
    const s = startOf(), e = new Date(s); e.setDate(s.getDate() + 42);
    drawGrid();
    try { data = await api('GET', `/api/pim/calendar?from=${encodeURIComponent(s.toISOString().slice(0, 19) + 'Z')}&to=${encodeURIComponent(e.toISOString().slice(0, 19) + 'Z')}`); }
    catch (err) { day.replaceChildren(h('p', { class: 'bad' }, err.message)); return; }
    drawGrid(); drawDay();
  };
  const newEvent = d => {
    const cals = data.calendars || [];
    if (!cals.length) { status('no calendar yet on the mail server', 'bad'); return; }
    let cal = cals[0].id, allDay = false;
    const title = h('input', { placeholder: 'title', style: 'width:100%' });
    const date = h('input', { type: 'date', value: ymd(d) });
    const time = h('input', { type: 'time', value: '10:00' });
    const len = h('input', { type: 'number', min: 5, step: 5, value: 60, style: 'width:6em' });
    const loc = h('input', { placeholder: 'where (optional)', style: 'width:100%' });
    const notes = h('textarea', { rows: 3, placeholder: 'notes (optional)', style: 'width:100%' });
    const m = modal('a new event', h('div', { class: 'stack' }, title,
      h('div', { class: 'row' }, date, time, len, ' min', h('label', { class: 'row' }, check(false, v => { allDay = v; time.disabled = v; len.disabled = v; }), 'all day')),
      h('div', { class: 'row' }, 'calendar ', select(cals.map(c => [c.id, c.name]), cal, v => { cal = v; })), loc, notes), [
      h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
      h('button', { type: 'button', class: 'primary', onclick: async () => {
        if (!title.value.trim()) { title.focus(); return; }
        const body = { calendar: cal, title: title.value, all_day: allDay, start: `${date.value}T${allDay ? '00:00' : time.value}:00`, duration: allDay ? 'P1D' : `PT${Math.max(5, +len.value || 60)}M`,
          time_zone: Intl.DateTimeFormat().resolvedOptions().timeZone, location: loc.value, description: notes.value };
        try { await api('POST', '/api/pim/calendar/event', body); m.close(); status('saved', 'ok'); load(); } catch (e) { status(e.message, 'bad'); }
      } }, 'save'),
    ], { cls: 'pickmodal' });
    title.focus();
  };
  load();
}

// ---- sign in --------------------------------------------------------------

function signInPage() {
  const msg = h('div', { class: 'msg bad' });
  const form = h('form', { class: 'stack', onsubmit: async e => {
    e.preventDefault();
    msg.textContent = '';
    const f = new FormData(form);
    try {
      const r = await api('POST', '/api/signin', { name: f.get('name'), password: f.get('password') });
      if (r.next) return secondStep(r);
      route();
    } catch (err) { msg.textContent = err.message; }
  } },
    h('label', { class: 'lbl', for: 'si-name' }, 'user name or UPN'),
    h('input', { id: 'si-name', name: 'name', autocomplete: 'username', required: true, autofocus: true, spellcheck: false, autocapitalize: 'off' }),
    h('label', { class: 'lbl', for: 'si-pw' }, 'password'),
    h('input', { id: 'si-pw', name: 'password', type: 'password', autocomplete: 'current-password', required: true }),
    h('button', { class: 'primary', type: 'submit' }, 'sign in'),
    msg,
  );
  // another program's sign-in (OIDC) waits for this one
  const to = new URLSearchParams(location.search).get('to');
  const s = win('sign in', form,
    nextUrl() && /^[a-z0-9][a-z0-9._-]{0,31}$/.test(to || '') ? h('p', {}, `to continue to ${to}`) : null,
    h('p', { class: 'dim' }, 'You sign in at this node only if it keeps your account\'s secrets; it tells you where to go otherwise.'));
  s.classList.add('signin');
  $app.append(s);
  // the program names who signs in (OIDC login_hint): filled in, on to the password
  const hint = (new URLSearchParams(location.search).get('hint') || '').trim();
  if (hint && hint.length <= 128) {
    form.querySelector('#si-name').value = hint;
    setTimeout(() => form.querySelector('#si-pw').focus());
  }
}

/// The QR code and the key of a TOTP secret being set up.
function totpShow(e) {
  return h('div', { class: 'totp' },
    e.qr ? h('img', { class: 'qr', alt: 'QR code for an authenticator app', src: 'data:image/svg+xml;base64,' + btoa(e.qr) }) : null,
    h('div', {}, h('span', { class: 'lbl' }, 'key '), h('code', { class: 'totp-key' }, e.secret.replace(/(.{4})/g, '$1 ').trim())));
}

// ---- FIDO2 (WebAuthn): the browser asks the key; the node checks (webauthn.rs) ----

const b64u = buf => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const unb64u = s => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((s.length + 3) % 4)), c => c.charCodeAt(0));

/// A new key: navigator.credentials.create for `e` (challenge, user, exclude).
async function fido2Create(e) {
  if (!window.PublicKeyCredential) throw new Error('this browser has no WebAuthn here (it needs https and a name, not an address)');
  const c = await navigator.credentials.create({ publicKey: {
    challenge: unb64u(e.challenge), rp: { id: location.hostname, name: 'osada' },
    user: { id: new TextEncoder().encode(e.user.id), name: e.user.name, displayName: e.user.name },
    pubKeyCredParams: [{ type: 'public-key', alg: -7 }, { type: 'public-key', alg: -8 }],
    excludeCredentials: (e.exclude || []).map(id => ({ type: 'public-key', id: unb64u(id) })),
    attestation: 'none', authenticatorSelection: { userVerification: 'preferred', residentKey: 'discouraged' }, timeout: 120000,
  } });
  return { client: b64u(c.response.clientDataJSON), attestation: b64u(c.response.attestationObject) };
}

/// A signature with one of the account's keys (`e.allow`).
async function fido2Get(e) {
  if (!window.PublicKeyCredential) throw new Error('this browser has no WebAuthn here (it needs https and a name, not an address)');
  const c = await navigator.credentials.get({ publicKey: {
    challenge: unb64u(e.challenge), rpId: location.hostname, userVerification: 'preferred', timeout: 120000,
    allowCredentials: e.allow.map(id => ({ type: 'public-key', id: unb64u(id) })),
  } });
  return { id: b64u(c.rawId), client: b64u(c.response.clientDataJSON), authenticator: b64u(c.response.authenticatorData), signature: b64u(c.response.signature) };
}

/// After the password, FIDO2: a key of the account's, or its first one (tier 0).
function fido2Step(r) {
  const msg = h('div', { class: 'msg bad' });
  const enrol = r.next === 'enrol_fido2';
  const go = h('button', { type: 'button', class: 'primary', id: 'si-fido2', onclick: async () => {
    msg.textContent = '';
    go.disabled = true;
    try {
      const answer = enrol ? await fido2Create(r.enrol) : await fido2Get(r.enrol);
      await api('POST', '/api/signin/fido2', { ticket: r.ticket, ...answer });
      route();
    } catch (err) {
      msg.textContent = err.message;
      if (/start again|ran out/.test(err.message)) setTimeout(route, 1500); else go.disabled = false;
    }
  } }, enrol ? 'register a key and sign in' : 'use my security key');
  const s = win('security key (FIDO2)',
    h('p', {}, enrol
      ? 'Your account is in a tier-0 OU: it signs in with a security key or passkey, and has none yet. Register one now (touch it when it blinks).'
      : 'Touch your security key (or use your passkey) when the browser asks.'),
    go, msg,
    r.enrol && r.enrol.or_totp ? h('p', {}, h('a', { href: '#', onclick: ev => { ev.preventDefault(); secondStep({ ...r, next: 'totp' }); } }, 'use a TOTP code instead')) : null);
  s.classList.add('signin');
  $app.replaceChildren(s);
  go.focus();
}

/// After the password: the TOTP code, or setting TOTP up first.
function secondStep(r) {
  if (r.next === 'fido2' || r.next === 'enrol_fido2') return fido2Step(r);
  const msg = h('div', { class: 'msg bad' });
  const code = h('input', { id: 'si-code', name: 'code', inputmode: 'numeric', autocomplete: 'one-time-code', required: true });
  const form = h('form', { class: 'stack', onsubmit: async e => {
    e.preventDefault();
    msg.textContent = '';
    try { await api('POST', '/api/signin/totp', { ticket: r.ticket, code: code.value }); route(); }
    catch (err) { msg.textContent = err.message; if (err.message.includes('start again')) setTimeout(route, 1500); }
  } },
    h('label', { class: 'lbl', for: 'si-code' }, 'code from your authenticator app'), code,
    h('button', { class: 'primary', type: 'submit' }, r.next === 'enrol_totp' ? 'set up and sign in' : 'sign in'), msg);
  const s = win('second factor',
    r.next === 'enrol_totp' ? [h('p', {}, 'Your OU asks for a second factor and this account has none yet. Scan this with an authenticator app, or type the key in, then give the code it shows.'), totpShow(r.enrol)] : null,
    form);
  s.classList.add('signin');
  $app.replaceChildren(s);
  code.focus();
}

// ---- account --------------------------------------------------------------

function accountPage() {
  const u = me.user;
  const sessions = grid({
    id: 'my-sessions',
    columns: [
      { key: 'sid', label: 'session', cell: s => h('span', {}, short(s.sid), s.sid === me.sid ? h('span', { class: 'pill on' }, ' this one') : null) },
      { key: 'started', label: 'started', type: 'date' },
      { key: 'expires', label: 'ends', type: 'date' },
      { key: 'addr', label: 'from' },
      { key: 'end', label: '', value: () => '', cell: s => s.sid === me.sid ? null : h('button', { type: 'button', onclick: async () => {
        await api('POST', '/api/account/end-session', { sid: s.sid }).catch(err => status(err.message, 'bad'));
        route();
      } }, 'end') },
    ],
    rows: me.sessions,
    sort: 'started', dir: 'desc',
  });
  const caution = me.caution ? h('p', { class: 'caution' },
    `Sessions last ${me.session_days} days, longer than 90 % of the ${me.auth_retention_days} days the authentication log is kept. ` +
    'Once its entries are gone, a session can no longer be traced back to the sign-in it came from.') : null;
  const pwMsg = h('div', { class: 'msg' });
  const pw = h('form', { class: 'stack', onsubmit: async e => {
    e.preventDefault();
    const f = new FormData(pw);
    if (f.get('new') !== f.get('again')) { pwMsg.className = 'msg bad'; pwMsg.textContent = 'the new passwords differ'; return; }
    try {
      await api('POST', '/api/account/password', { old: f.get('old'), new: f.get('new') });
      pwMsg.className = 'msg ok'; pwMsg.textContent = 'changed'; pw.reset();
    } catch (err) { pwMsg.className = 'msg bad'; pwMsg.textContent = err.message; }
  } },
    h('label', { class: 'lbl' }, 'current password'), h('input', { name: 'old', type: 'password', autocomplete: 'current-password', required: true }),
    h('label', { class: 'lbl' }, 'new password'), h('input', { name: 'new', type: 'password', autocomplete: 'new-password', required: true, minlength: 8 }),
    h('label', { class: 'lbl' }, 'again'), h('input', { name: 'again', type: 'password', autocomplete: 'new-password', required: true }),
    h('button', { class: 'primary', type: 'submit' }, 'change'), pwMsg);

  const tf = h('div', {});
  const tmsg = h('div', { class: 'msg' });
  const bad = err => { tmsg.className = 'msg bad'; tmsg.textContent = err.message; };
  if (me.totp === null) tf.append(h('p', { class: 'dim' }, 'This node does not keep your secrets.'));
  else if (me.totp) {
    const code = h('input', { inputmode: 'numeric', placeholder: 'a code from the app', autocomplete: 'one-time-code' });
    tf.append(h('p', {}, 'TOTP is on.', me.rule.second_factor ? ` ${me.rule.second_factor} asks for a second factor, so it stays.` : ''),
      me.rule.second_factor ? '' : h('div', { class: 'row' }, code, h('button', { type: 'button', class: 'danger', onclick: async () => {
        try { await api('POST', '/api/account/totp/remove', { code: code.value }); route(); } catch (err) { bad(err); }
      } }, 'take off')));
  } else {
    tf.append(h('p', {}, 'No TOTP yet.'), h('button', { type: 'button', class: 'primary', onclick: async () => {
      try {
        const e = await api('POST', '/api/account/totp/begin');
        const code = h('input', { inputmode: 'numeric', placeholder: 'the code it shows', autocomplete: 'one-time-code' });
        tf.replaceChildren(h('p', {}, 'Scan this with an authenticator app, or type the key in, then give the code it shows.'), totpShow(e),
          h('div', { class: 'row' }, code, h('button', { type: 'button', class: 'primary', onclick: async () => {
            try { await api('POST', '/api/account/totp/confirm', { code: code.value }); route(); } catch (err) { bad(err); }
          } }, 'keep it')));
      } catch (err) { bad(err); }
    } }, 'set up TOTP'));
  }
  $app.append(h('div', { class: 'cols' },
    win('account', h('dl', { class: 'kv' },
      h('dt', {}, 'user'), h('dd', {}, u.name),
      me.upn ? [h('dt', {}, 'UPN'), h('dd', { title: 'you sign in with it or the user name; the share window and other instances know you by it' }, h('code', {}, me.upn))] : null,
      h('dt', {}, 'name'), h('dd', {}, u.display || '—'),
      h('dt', {}, 'OU'), h('dd', {}, me.ou_path),
      h('dt', {}, 'id (sub)'), h('dd', {}, h('code', {}, u.id)),
      h('dt', {}, 'signed in at'), h('dd', {}, me.node_name || me.node),
      h('dt', {}, 'this session'), h('dd', {}, h('code', {}, me.sid)),
    )),
    win('sign-in rule', h('dl', { class: 'kv' },
      h('dt', {}, 'tier 0'), h('dd', {}, me.rule.tier0 ? `on, from ${me.rule.tier0}: FIDO2 only` : 'off'),
      h('dt', {}, 'second factor'), h('dd', {}, me.rule.second_factor ? `required, from ${me.rule.second_factor}` : 'not required'),
      h('dt', {}, 'secrets kept on'), h('dd', {}, me.secrets_on.join(', ')),
    ), h('p', { class: 'dim' }, 'Only the nodes that keep your secrets can sign you in, and only their signatures count for you.')),
    win('password', pw),
    win('second factor', tf, tmsg, h('p', { class: 'dim' }, 'TOTP: six digits from an authenticator app.')),
    win('security keys (FIDO2)', keysBox(), h('p', { class: 'dim' }, 'A security key or passkey signs you in with your password; in a tier-0 OU it is the only second factor taken. It works over https at a name (not at an address).')),
    mailPasswordsWin(),
  ), win(`sessions on node ${me.node_name || me.node}`, caution, sessions.node));
}

/// Passwords for mail programs (Round 24): the mail server signs people in through osada, so
/// IMAP, SMTP, CalDAV and CardDAV programs use app passwords the account makes here (the
/// mail server makes the secret; it is shown once). Hidden when there is no mail server, no
/// mailbox, or this node has no mail part (the answer is not the list then).
function mailPasswordsWin() {
  const box = h('div', { class: 'stack' });
  const msg = h('div', { class: 'msg' });
  const w = win('mail programs', box, msg);
  w.id = 'mail-passwords';
  w.hidden = true;
  const bad = err => { msg.className = 'msg bad'; msg.textContent = err.message; };
  const show = async fresh => {
    let d;
    try { d = await api('GET', '/api/account/mail-passwords'); } catch (err) {
      if (err.status === 502 || err.status === 400) { w.hidden = false; box.replaceChildren(); bad(err); }
      return;
    }
    if (!d || d.available !== true || !Array.isArray(d.list)) return;
    w.hidden = false;
    const name = h('input', { placeholder: 'its name (phone, laptop)', maxlength: 80 });
    const make = async () => {
      try {
        const r = await api('POST', '/api/account/mail-passwords', { description: name.value });
        msg.className = 'msg'; msg.textContent = '';
        await show(r);
      } catch (err) { bad(err); }
    };
    name.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); make(); } });
    const secret = fresh ? h('div', { class: 'msg ok fresh-secret' },
      `The password for “${fresh.description}”, shown only now: type or paste it into the program. `,
      h('code', { class: 'secret' }, fresh.secret), ' ',
      h('button', { type: 'button', onclick: () => navigator.clipboard && navigator.clipboard.writeText(fresh.secret) }, 'copy')) : null;
    const host = d.host || '—';
    box.replaceChildren(
      h('p', { class: 'dim' }, 'Mail programs (a phone, Thunderbird, Outlook) do not sign in with your osada password: give each its own app password, and revoke it when the program is gone.'),
      secret,
      h('dl', { class: 'kv' },
        h('dt', {}, 'user name'), h('dd', {}, h('code', {}, d.login || '—')),
        h('dt', {}, 'IMAP'), h('dd', {}, h('code', {}, host), ', port 993, SSL/TLS'),
        h('dt', {}, 'SMTP'), h('dd', {}, h('code', {}, host), ', port 465 SSL/TLS, or 587 STARTTLS'),
        h('dt', {}, 'calendar, contacts'), h('dd', {}, h('code', {}, `https://${host}/`), ' (CalDAV, CardDAV)')),
      d.list.length ? h('ul', { class: 'keys' }, d.list.map(p => h('li', { class: 'row' },
        h('b', {}, p.description || 'app password'),
        h('span', { class: 'dim' }, p.created ? ' made ' + new Date(p.created).toLocaleDateString() : ''),
        h('button', { type: 'button', class: 'danger', onclick: () => confirmModal('revoke the app password', `Revoke “${p.description}”? Mail programs using it are refused from then on.`, async () => {
          try { await api('DELETE', '/api/account/mail-passwords/' + encodeURIComponent(p.id)); msg.textContent = ''; await show(); } catch (err) { bad(err); }
        }) }, 'revoke')))) : h('p', {}, 'No app passwords yet.'),
      h('div', { class: 'row' }, name, h('button', { type: 'button', class: 'primary', id: 'make-mail-password', onclick: make }, 'make one')));
  };
  show();
  return w;
}

/// The account's FIDO2 keys: listed, added, taken off.
function keysBox() {
  const box = h('div', { class: 'stack' });
  const kmsg = h('div', { class: 'msg' });
  const bad = err => { kmsg.className = 'msg bad'; kmsg.textContent = err.message; };
  if (me.fido2 === null || me.fido2 === undefined) { box.append(h('p', { class: 'dim' }, 'This node does not keep your secrets.')); return box; }
  box.append(me.fido2.length ? h('ul', { class: 'keys' }, me.fido2.map(k => h('li', { class: 'row' },
    h('b', {}, k.name || 'security key'), h('span', { class: 'dim' }, k.added ? ' added ' + new Date(k.added).toLocaleDateString() : ''),
    h('button', { type: 'button', class: 'danger', onclick: () => confirmModal('take the key off', `Take “${k.name}” off this account?`, async () => {
      try { await api('POST', '/api/account/fido2/remove', { id: k.id }); route(); } catch (err) { bad(err); }
    }) }, 'take off')))) : h('p', {}, 'No keys yet.'));
  const name = h('input', { placeholder: 'its name (my YubiKey)', maxlength: 60 });
  box.append(h('div', { class: 'row' }, name, h('button', { type: 'button', class: 'primary', id: 'add-key', onclick: async () => {
    try {
      const e = await api('POST', '/api/account/fido2/begin');
      const answer = await fido2Create(e);
      await api('POST', '/api/account/fido2/confirm', { ...answer, name: name.value });
      route();
    } catch (err) { bad(err); }
  } }, 'add a key')), kmsg);
  return box;
}

// ---- admin ----------------------------------------------------------------
//
// /admin/<tab>: nodes, ous, collections, users, logs; /admin/users/<id> and
// /admin/groups/<id> are cards. Node control is edited as a draft and saved
// as a whole; the node checks each changed part against the actor's roles.

const admin = { ov: null, draft: null, barHost: null };

function pathOf(id) {
  const d = admin.draft;
  if (!d.ous[id]) return `(OU ${id})`;
  const names = [];
  let c = Number(id), guard = 0;
  while (d.ous[c] && d.ous[c].parent !== null && d.ous[c].parent !== undefined && guard++ < 64) {
    names.unshift(d.ous[c].name);
    c = d.ous[c].parent;
  }
  return '/' + names.join('/');
}

function ouInfo(id) {
  return admin.ov.ous.find(o => o.id === Number(id));
}

function dirty() {
  return JSON.stringify(admin.draft) !== JSON.stringify(admin.ov.placement);
}

/// The save bar, without redrawing the page (filters and inputs stay).
function markDirty(error) {
  if (!admin.barHost) return;
  admin.barHost.replaceChildren(dirty() ? h('div', { class: 'bar-save' },
    h('span', { class: 'warn' }, 'node control changed here, not saved yet'),
    h('button', { class: 'primary', type: 'button', onclick: savePlacement }, 'save'),
    h('button', { type: 'button', onclick: () => { admin.draft = structuredClone(admin.ov.placement); drawAdmin(); } }, 'discard'),
    error ? h('span', { class: 'bad' }, error) : null) : '');
}

async function loadOverview() {
  admin.ov = await api('GET', '/api/admin/overview');
  admin.draft = structuredClone(admin.ov.placement);
}

async function adminPage() {
  // another tab keeps what is not saved yet (Round 14: a module on a node here, its masters there)
  if (admin.ov && admin.draft && dirty()) { drawAdmin(); return; }
  try { await loadOverview(); } catch (err) {
    $app.append(win('admin', h('p', { class: 'bad' }, err.message)));
    return;
  }
  drawAdmin();
}

async function reloadAdmin(delay) {
  if (delay) await new Promise(r => setTimeout(r, delay));
  await loadOverview();
  drawAdmin();
}

async function savePlacement() {
  try {
    await api('POST', '/api/admin/placement', admin.draft);
    status('node control saved', 'ok');
    await reloadAdmin(400);
  } catch (err) {
    status(err.message, 'bad');
    markDirty(err.message);
  }
}

function adminRoute() {
  const parts = location.pathname.split('/').filter(Boolean);
  return { tab: parts[1] || (admin.ov.cluster_admin ? 'nodes' : 'users'), id: parts[2] || null };
}

function drawAdmin() {
  if (nodeMapCy) { try { nodeMapCy.destroy(); } catch (e) { /* gone */ } nodeMapCy = null; }
  const ov = admin.ov;
  const { tab, id } = adminRoute();
  // a tab per module with settings of its own (D-U14): osada 0.1's, for those who run the cluster
  const moduleTabs = FILES && ov.cluster_admin ? ['files', 'proxies', 'apps', 'federation'] : [];
  // web: every administrator; the server shows each the sites of the OUs they manage (D-U28)
  // the setup first (RBAC hides it later), then a tab per module, then the people and the logs (Round 14)
  const tabs = [...(ov.cluster_admin ? ['nodes'] : []), 'idp', ...(ov.cluster_admin ? ['files'] : []), 'mail', 'web', ...(ov.cluster_admin ? ['chat', 'wiki', 'links', 'dns'] : []),
    'users', ...(ov.logs_read || ov.cluster_admin ? ['logs'] : []), ...(ov.cluster_admin ? ['modules', 'collections'] : []), ...moduleTabs.filter(t => t !== 'files')];
  const head = h('div', { class: 'row' },
    h('span', {}, 'connected to ', h('b', {}, nodeLabel(ov.via || ov.me)), ov.via ? [' → ', h('b', {}, nodeLabel(ov.me)), ' (answers)'] : null),
    h('span', { class: 'dim' }, ov.holds_cluster_log ? '· the IdP master of / (holds the cluster log)' : `· the IdP master of / is ${nodeLabel(ov.voter)}; changes go there`),
    h('span', { class: 'dim' }, `· cluster log at entry ${ov.cluster_log.applied}, ${ov.cluster_log.head}`),
    h('span', { class: 'spacer' }),
    h('button', { type: 'button', onclick: () => reloadAdmin() }, 'reload'));
  admin.barHost = h('div', {});
  const problems = ov.problems.length ? h('p', { class: 'bad' }, 'Node control has problems: ' + ov.problems.join('; ')) : null;
  const nav = h('div', { class: 'tabs' }, tabs.map(t => h('a', { href: `/admin/${t}`, 'data-go': true, class: (t === tab || (t === 'users' && tab === 'groups') || (t === 'idp' && tab === 'ous')) ? 'on' : null, 'data-tab': t },
    t === 'idp' ? 'IdP' : t === 'apps' ? 'sign-in for apps' : t)));
  let body;
  if (tab === 'users' && id) body = userCard(id);
  else if (tab === 'groups' && id) body = groupCard(id);
  else body = ({ nodes: nodesTab, modules: modulesTab, web: webTab, dns: dnsTab, idp: ousTab, ous: ousTab, collections: collectionsTab, users: usersTab, logs: logsTab,
    mail: () => moduleTab('mail', 'mail', mailTab()),
    files: () => moduleTab('files', 'files', ...(FILES && ov.cluster_admin ? [filesTab()] : [])),
    // calendar and contacts are mail's (Stalwart keeps them; the owner, Round 15): no tabs of their own
    chat: () => moduleTab('chat', 'chat'),
    wiki: () => moduleTab('wiki', 'wiki'), links: () => moduleTab('links', 'links'),
    ...(moduleTabs.length ? { proxies: proxiesTab, apps: appsTab, federation: federationTab } : {}) }[tab] || usersTab)();
  $app.replaceChildren(...[head, admin.barHost, problems, nav, body].filter(Boolean));
  markDirty();
}

// a node's name (D-U19: its id is a GUID; the name is what people see and change), else its id
function nodeLabel(id) {
  const n = ((admin.draft && admin.draft.nodes) || {})[id];
  return (n && n.name) || id;
}

// the nodes' ids, in the order of their names
function nodeNames() {
  return Object.keys(admin.draft.nodes).sort((a, b) => nodeLabel(a).localeCompare(nodeLabel(b)));
}

// ---- module tabs: the files module (osada 0.1's storage limits, nightly backup), proxies, apps ----

const GiB = 2 ** 30;
function fmtBytes(n) {
  if (n == null) return '';
  for (const [u, s] of [['T', 40], ['G', 30], ['M', 20], ['K', 10]]) if (n >= 2 ** s) return +(n / 2 ** s).toFixed(n >= 10 * 2 ** s ? 0 : 1) + ' ' + u + 'B';
  return n + ' B';
}
function fmtLimit(n) {
  for (const [u, s] of [['T', 40], ['G', 30], ['M', 20]]) if (n >= 2 ** s && n % 2 ** s === 0) return n / 2 ** s + u;
  return +(n / GiB).toFixed(2) + 'G';
}

/// A box that fills itself from `load` (a promise), or says why not.
function loading(load, draw) {
  const box = h('div', {}, h('p', { class: 'dim' }, 'loading…'));
  load.then(v => box.replaceChildren(draw(v)), err => box.replaceChildren(h('p', { class: 'bad' }, err.message)));
  return box;
}

function filesTab() {
  const limits = loading(api('GET', '/api/users'), users => {
    const rows = users.map(u => ({ id: u.id, name: u.username, quota: u.quota, used: u.used, state: u.disabled ? 'disabled' : 'active' }));
    const g = grid({
      id: 'file-limits', rows, sort: 'name',
      columns: [
        { key: 'name', label: 'user', cell: r => h('b', {}, r.name) },
        { key: 'state', label: 'state', type: 'enum', options: ['active', 'disabled'] },
        { key: 'quota', label: 'storage limit', type: 'number', cell: r => r.quota == null ? h('span', { class: 'dim' }, 'none') : fmtLimit(r.quota) },
        { key: 'used', label: 'used', type: 'number', cell: r => r.used == null ? h('span', { class: 'dim' }, '…') : h('span', { class: r.quota && r.used >= r.quota * 0.8 ? 'warn' : null }, fmtBytes(r.used) + (r.quota ? ` (${Math.round(100 * r.used / r.quota)} %)` : '')) },
        { key: 'set', label: '', value: () => '', cell: r => h('button', { type: 'button', class: 'set-limit', onclick: () => {
          const input = h('input', { value: r.quota == null ? '' : fmtLimit(r.quota), placeholder: '50G, 500M, 1.5T; empty: no limit' });
          const m = modal(`storage limit of ${r.name}`, h('div', { class: 'stack' }, h('p', { class: 'dim' }, 'Room this account may use in its home.'), input), [
            h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
            h('button', { type: 'button', class: 'primary', onclick: async () => {
              m.close();
              try { await api('PATCH', `/api/users/${r.id}`, { quota: input.value }); status(`${r.name}: ${input.value.trim() ? 'limit ' + input.value.trim() : 'no limit'}`, 'ok'); drawAdmin(); }
              catch (err) { status(err.message, 'bad'); }
            } }, 'set')]);
          input.focus();
        } }, 'set limit') },
      ],
      empty: 'no users',
    });
    return g.node;
  });
  return h('div', {},
    win('files: storage limits', limits, h('p', { class: 'dim' }, 'How much each account may keep in its home; what it uses now. (Outside networks are on the users tab.)')),
    win('files: where they are (nodes and other storage)', filesWhere()),
    win('files: other storage (SFTP, S3)', otherStorage()),
    win('files: backups', backupsBox()));
}

// Round 14/17: the backups, of two kinds, several of each: live (a copy of the files kept up to
// date) and nightly (complete snapshots of what the whole cluster needs, where earlier versions
// of files come from)
const NIGHTLY_PARTS = [['files', 'files (everyone\'s homes, the groups\' folders, deleted users\' files)'], ['wiki', 'the wiki\'s pictures'],
  ['database', 'database and settings (osada 0.1\'s database, its key, the unit, the program)'],
  ['nodes', 'every node\'s data (the cluster\'s store: directory, sign-in, chat; keys, certificates, static sites)'],
  ['mail', 'mail (Stalwart\'s data, settings and program, on the nodes it runs on)']];
function backupOf(b, parts) {
  parts = parts || NIGHTLY_PARTS;
  if ((b.kind || 'live') === 'live') return b.source === 'all' ? 'all files' : `the files of node ${nodeLabel(b.source)}`;
  const w = b.what && b.what.length ? b.what : parts.map(p => p[0]);
  return w.length === parts.length ? 'the whole cluster (' + w.join(', ') + ')' : w.join(', ');
}
function backupsBox() {
  const ov = admin.ov, backups = (ov.settings && ov.settings.backups) || [];
  const storages = ov.storages || {};
  const where = t => t.startsWith('node:') ? `node ${nodeLabel(t.slice(5))}` : `storage ${t.slice(8)}`;
  const box = h('div', {});
  let live = null, bk = null, poll = null;
  const parts = () => (bk && bk.parts) || NIGHTLY_PARTS;
  const save = async (list, what) => {
    try { await api('POST', '/api/admin/backups', list); status(what, 'ok'); await reloadAdmin(); } catch (e) { status(e.message, 'bad'); }
  };
  const liveTargets = [...nodeNames().map(id => ['node:' + id, `node ${nodeLabel(id)}`]), ...Object.keys(storages).map(n => ['storage:' + n, `storage ${n}`])];
  const taken = name => backups.some(b => (b.kind || 'live') === 'nightly' && b.target === name);
  const nightlyTargets = cur => Object.entries(storages).map(([n, s]) => ['storage:' + n, `storage ${n}: ${s.place}` + (s.kind !== 'sftp' ? ' (S3: with A12)' : !s.host_key ? ' (test it first)' : taken('storage:' + n) && 'storage:' + n !== cur ? ' (has a nightly backup)' : ''),
    s.kind !== 'sftp' || (taken('storage:' + n) && 'storage:' + n !== cur)]);
  const sources = [['all', 'all files'], ...nodeNames().map(id => [id, `the files of node ${nodeLabel(id)}`])];
  const runsOf = name => {
    const el = h('div', { class: 'stack' });
    const paint = d => {
      const mine = (d.runs || []).filter(r => r.name === name);
      const n = (d.nightly || []).find(x => x.name === name);
      el.replaceChildren(
        d.running && d.current === name ? h('p', { class: 'warn' }, `running: ${d.status || 'starting'}`) : h('p', { class: 'dim' }, n && n.snapshots.length ? `on the target: ${n.snapshots.join(', ')}` : 'no snapshots yet'),
        grid({
          id: 'backup-runs', sort: 'started', dir: 'desc', empty: 'no runs yet',
          rows: mine.map(r => ({ ...r, took: r.finished ? Math.max(0, r.finished - r.started) : null, result: r.ok === true ? (r.message && r.message.startsWith('incomplete') ? 'incomplete' : 'ok') : r.finished || !d.running ? 'failed' : 'running' })),
          columns: [{ key: 'started', label: 'started', type: 'date', value: r => r.started * 1000 }, { key: 'took', label: 'took (s)', type: 'number' },
            { key: 'result', label: 'result', type: 'enum', options: ['ok', 'incomplete', 'failed', 'running'] }, { key: 'uploaded', label: 'files sent', type: 'number' },
            { key: 'bytes', label: 'size', type: 'number', cell: r => fmtBytes(r.bytes) }, { key: 'linked', label: 'linked', type: 'number' }, { key: 'message', label: 'message' }],
        }).node);
      if (d.running) setTimeout(() => { if (el.isConnected) api('GET', '/api/backup').then(paint, () => {}); }, 2000);
    };
    if (bk) paint(bk); else api('GET', '/api/backup').then(paint, () => {});
    return el;
  };
  // a backup's card: a new one (`b` null) or one there is
  const card = (b, kind) => {
    const fresh = !b;
    b = b ? { ...b, kind: b.kind || 'live', what: [...(b.what || [])] } : { name: '', kind, source: 'all', target: '', time: '03:15', what: [] };
    const firstOk = l => (l.find(t => !t[2]) || [''])[0];
    if (!b.target) b.target = b.kind === 'nightly' ? firstOk(nightlyTargets(null)) : firstOk(liveTargets);
    const chosen = new Set(b.what.length ? b.what : parts().map(p => p[0]));
    const name = h('input', { value: b.name, disabled: !fresh, placeholder: 'offsite (a-z, 0-9, -)', maxlength: 40 });
    const time = h('input', { type: 'time', value: b.time || '03:15' });
    const body = h('div', { class: 'stack' });
    const paint = () => {
      const lbl = t => h('span', { class: 'lbl' }, t);
      const nightly = b.kind === 'nightly';
      body.replaceChildren(
        h('p', { class: 'dim' }, nightly
          ? 'Complete snapshots every night on SFTP storage, of what is ticked: with all of it the whole cluster can be rebuilt (RESTORE.md on the target says how). Three are kept (the newest, one up to a week older, one one to four weeks older); what is on the target already is hard-linked, not sent again. Mail: Stalwart stops for a few seconds while its data is copied. Earlier versions of files come from the first nightly backup with the files.'
          : 'A copy of the files kept up to date: what users change goes to the target within a minute, removals too (into live/ on it). Earlier versions come from a nightly backup.'),
        lbl('kind'), fresh ? select([['nightly', 'nightly: complete snapshots (the whole cluster, earlier versions)'], ['live', 'live: a copy of the files, kept up to date']], b.kind, v => {
          b.kind = v; b.target = v === 'nightly' ? firstOk(nightlyTargets(null)) : firstOk(liveTargets); paint();
        }, { 'data-kind': '' }) : h('b', {}, nightly ? 'nightly' : 'live'),
        lbl('name'), name,
        lbl('backup of what'),
        nightly ? h('div', { class: 'stack', 'data-what': '' }, ...parts().map(([p, about]) => h('label', { class: 'row' }, check(chosen.has(p), on => { if (on) chosen.add(p); else chosen.delete(p); }, { 'data-part': p }), about)))
          : select(sources, b.source, v => { b.source = v; }),
        lbl('to'), nightly ? select(nightlyTargets(fresh ? null : b.target), b.target, v => { b.target = v; }, { 'data-to': '' }) : select(liveTargets, b.target, v => { b.target = v; }),
        nightly ? h('label', { class: 'row' }, 'every night at', time) : '',
        nightly && !Object.values(storages).some(s => s.kind === 'sftp') ? h('p', { class: 'warn' }, 'There is no SFTP storage yet: add one in other storage (above) and test it.') : '',
        h('label', { class: 'row' }, check(!b.off, on => { b.off = !on; }), 'on'),
        nightly && !fresh ? h('div', { class: 'row' },
          h('button', { type: 'button', onclick: () => api('POST', `/api/backup/test?name=${encodeURIComponent(b.name)}`).then(r => status(`the target answers: ${r.entries} entries`, 'ok'), err => status(err.message, 'bad')) }, 'test'),
          h('button', { type: 'button', onclick: () => api('POST', `/api/backup/run?name=${encodeURIComponent(b.name)}`).then(() => api('GET', '/api/backup')).then(d => { bk = d; paint(); draw(); status(`${b.name} is running`, 'ok'); }, err => status(err.message, 'bad')) }, 'run now')) : '',
        nightly && !fresh ? runsOf(b.name) : '');
    };
    paint();
    const m = modal(fresh ? 'a new backup' : `${b.kind} backup ${b.name}`, body, [
      fresh ? null : h('button', { type: 'button', class: 'danger', onclick: () => { m.close(); confirmModal(`remove the ${b.kind} backup`, `Stop ${b.name}? What is on ${where(b.target)} stays there.`, () => save(backups.filter(x => x.name !== b.name), `${b.name} removed`)); } }, 'remove'),
      h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
      h('button', { type: 'button', class: 'primary', onclick: () => {
        b.name = name.value.trim().toLowerCase();
        if (!b.name) { status('give it a name', 'bad'); return; }
        if (!b.target) { status('it goes where? (other storage, above)', 'bad'); return; }
        let x;
        if (b.kind === 'nightly') {
          const what = parts().map(p => p[0]).filter(p => chosen.has(p));
          if (!what.length) { status('tick what it backs up', 'bad'); return; }
          x = { name: b.name, kind: 'nightly', source: 'all', target: b.target, time: time.value || '03:15', ...(what.length === parts().length ? {} : { what }), ...(b.off ? { off: true } : {}) };
        } else {
          x = { name: b.name, kind: 'live', source: b.source, target: b.target, ...(b.off ? { off: true } : {}) };
        }
        m.close();
        save(fresh ? [...backups, x] : backups.map(y => y.name === x.name ? x : y), `${x.name} saved`);
      } }, 'save'),
    ], { cls: 'pickmodal' });
  };
  const draw = () => {
    const rows = backups.map(b => ({ b, kind: b.kind || 'live', name: b.name, of: backupOf(b, parts()), to: where(b.target), st: (live || {})[b.name] }));
    // osada 0.1's nightly backup, until it moved into the cluster's settings (it does so by itself)
    if (bk && !bk.migrated && bk.config) rows.push({ legacy: true, kind: 'nightly', name: 'nightly', of: 'files, wiki, database and settings', to: bk.config.storage ? `storage ${bk.config.storage}` : `its own SFTP target (${bk.config.username}@${bk.config.host}:${bk.config.root}), moving to other storage` });
    const state = r => {
      if (r.kind === 'nightly') {
        if (!bk) return '…';
        const n = (bk.nightly || []).find(x => x.name === r.name);
        if (!n) return r.b && r.b.off ? 'off' : '…';
        if (bk.running && bk.current === r.name) return 'running: ' + (bk.status || 'starting');
        const last = (bk.runs || []).find(x => x.name === r.name);
        const res = !last ? '' : `; last run ${new Date(last.started * 1000).toLocaleString()}: ` + (last.ok ? (last.message && last.message.startsWith('incomplete') ? last.message : 'ok') : 'failed: ' + (last.message || ''));
        return `${n.enabled ? 'every night at ' + n.time : 'off'}${n.snapshots.length ? `; ${n.snapshots.length} snapshots, newest ${n.snapshots[0]}` : ''}${res}`;
      }
      if (r.b.off) return 'off';
      const s = r.st;
      if (!s) return 'the first pass comes within a minute';
      const at = new Date(s.at).toLocaleString();
      return s.ok ? `up to date at ${at}: ${s.files} files, ${fmtBytes(s.bytes)}; last pass sent ${s.copied}, removed ${s.removed}` : `${at}: ${s.error}`;
    };
    const bad = r => r.kind === 'live' ? r.st && !r.st.ok : bk && (bk.runs || []).find(x => x.name === r.name && x.finished) && !(bk.runs || []).find(x => x.name === r.name && x.finished).ok;
    const g = grid({
      id: 'backups', rows, sort: null, empty: 'none',
      columns: [
        { key: 'kind', label: 'kind', value: r => r.kind, cell: r => h('b', {}, r.kind) },
        { key: 'name', label: 'name' },
        { key: 'of', label: 'backup of what', cell: r => h('span', { class: 'cut', title: r.of }, r.of) },
        { key: 'to', label: 'to' },
        { key: 'state', label: 'state', value: state, cell: r => { const t = state(r); return h('span', { class: bad(r) ? 'bad' : 'dim', title: t }, t); } },
      ],
      onRow: r => r.legacy ? status('osada 0.1\'s nightly backup moves into the cluster\'s settings by itself; then it is set here', 'ok') : card(r.b),
      extras: [h('button', { type: 'button', 'data-add-backup': '', onclick: () => card(null, 'nightly') }, 'add a backup')],
    });
    box.replaceChildren(g.node, h('p', { class: 'dim' }, 'Live: a copy of the files kept up to date on a node or other storage (SFTP now; S3 and nodes as targets come next). ' +
      'Nightly: complete snapshots on SFTP storage of what is ticked, by default everything needed to rebuild the whole cluster: files, the wiki, the database and settings, every node\'s data and mail. ' +
      'Several of either kind; click one to change it. The "for files" column above sets them too.'));
    clearInterval(poll);
    if (bk && bk.running) poll = setInterval(() => { if (!box.isConnected) { clearInterval(poll); return; } api('GET', '/api/backup').then(v => { bk = v; draw(); }, () => {}); }, 3000);
  };
  Promise.all([api('GET', '/api/backup/live').then(v => v.live).catch(() => ({})), api('GET', '/api/backup').catch(() => null)]).then(([l, b]) => { live = l; bk = b; draw(); });
  draw();
  return box;
}

// the roles a node has for files (T2), as node control's roles
const FILE_ROLES = [['store', 'storage', 'keeps files: the files of the OUs placed on it (its files authority)'],
  ['cache', 'cache', 'keeps copies of files fetched through it (an edge); sends one only after home said yes to the very request'],
  ['public-cache', 'external cache', 'keeps public links, the pages and static files for when home is away (as edge1 was in 0.1)']];

/// T2: every node with its roles for files and what it keeps; other storage; where the nightly backup goes.
function filesWhere() {
  const d = admin.draft, ov = admin.ov;
  let info = {}, usage = null, bk = null;
  const storages = ov.storages || {};
  const ousOf = id => (ov.ous || []).filter(o => o.resolved && o.resolved.files && o.resolved.files.node === id).map(o => o.path);
  const backups = (ov.settings && ov.settings.backups) || [];
  // the nightly backups that go to a row (Round 17: several, each to SFTP storage)
  const nightlyTo = key => backups.filter(b => b.kind === 'nightly' && b.target === key);
  const homeBacked = r => r.kind === 'node' && usage && usage.node === r.id && backups.some(b => b.kind === 'nightly' && !b.off && (!b.what || !b.what.length || b.what.includes('files')));
  const holds = r => {
    if (r.kind === 'node') {
      const parts = [];
      if (usage && usage.node === r.id) parts.push(`files ${fmtBytes(usage.total)} (` + usage.parts.filter(p => p.bytes).map(p => `${p.name} ${fmtBytes(p.bytes)}`).join(', ') + ')');
      const f = info[r.id] && info[r.id].files;
      if (f && f.cache) parts.push(`cache ${fmtBytes(f.cache.bytes)} of ${d.nodes[r.id].cache_gb || 20} GB`);
      if (f && f.keep) parts.push(`kept for when home is away ${fmtBytes(f.keep.bytes)}`);
      return parts.join(' · ');
    }
    return nightlyTo(r.key).map(b => { const n = bk && (bk.nightly || []).find(x => x.name === b.name); return n && n.snapshots.length ? `nightly ${b.name}: ${n.snapshots.length} snapshots, newest ${n.snapshots[0]}` : ''; }).filter(x => x).join(' · ');
  };
  const rows = () => [
    ...nodeNames().map(id => ({ key: 'node:' + id, kind: 'node', id, name: nodeLabel(id) })),
    ...Object.entries(storages).map(([n, s]) => ({ key: 'storage:' + n, kind: s.kind, name: n, s })),
  ];
  // Round 14: what each node and storage is for files, a multi-pick each: a node's roles (node
  // control: save after), a live backup to it, a nightly backup to it (at once)
  const tgt = r => r.kind === 'node' ? 'node:' + r.id : 'storage:' + r.name;
  const roleBoxes = r => {
    const opts = [], chosen = new Set();
    if (r.kind === 'node') {
      for (const [role, label, about] of FILE_ROLES) { opts.push([role, label, about]); if (d.nodes[r.id].roles.includes(role)) chosen.add(role); }
    }
    const t = tgt(r);
    const isLive = b => (b.kind || 'live') === 'live';
    opts.push(['live', 'live backup here', 'a copy of all files, kept up to date: changes go here a minute after they are made']); if (backups.some(b => isLive(b) && b.target === t)) chosen.add('live');
    if (r.kind === 'sftp') { opts.push(['nightly', 'nightly backup here', 'complete snapshots every night of the whole cluster (earlier versions come from them)']); if (nightlyTo(r.key).length) chosen.add('nightly'); }
    if (!opts.length) return h('span', { class: 'dim' }, '—');
    return multiPick(opts, chosen, async (v, on) => {
      if (FILE_ROLES.some(f => f[0] === v)) {
        const set = new Set(d.nodes[r.id].roles);
        if (on) set.add(v); else set.delete(v);
        d.nodes[r.id].roles = ROLES.filter(x => set.has(x));
        markDirty();
      } else if (v === 'live') {
        const free = n => backups.some(b => b.name === n) ? free(n.slice(0, 37) + '-2') : n;
        const name = free(('live-' + (r.kind === 'node' ? nodeLabel(r.id) : r.name)).toLowerCase().replace(/[^a-z0-9-]+/g, '-').slice(0, 40));
        const list = on ? [...backups.filter(b => !(isLive(b) && b.target === t)), { name, kind: 'live', source: 'all', target: t }] : backups.filter(b => !(isLive(b) && b.target === t));
        try { await api('POST', '/api/admin/backups', list); status(on ? `a live backup of all files to ${r.name}` : `no live backup to ${r.name}`, 'ok'); await reloadAdmin(); }
        catch (e) { status(e.message, 'bad'); drawAdmin(); }
      } else if (v === 'nightly') {
        const send = async (list, what) => {
          try { await api('POST', '/api/admin/backups', list); status(what, 'ok'); await reloadAdmin(); }
          catch (e) { status(e.message, 'bad'); drawAdmin(); }
        };
        if (on) {
          const free = n => backups.some(b => b.name === n) ? free(n.slice(0, 37) + '-2') : n;
          const name = free(('nightly-' + r.name).toLowerCase().replace(/[^a-z0-9-]+/g, '-').slice(0, 40));
          await send([...backups, { name, kind: 'nightly', source: 'all', target: t, time: '03:15' }], `nightly backup ${name}: the whole cluster to ${r.name}, every night at 03:15 (backups, below, to change it)`);
        } else {
          const gone = nightlyTo(r.key).map(b => b.name);
          confirmModal('remove the nightly backup', `Stop ${gone.join(', ')}? Its snapshots on ${r.name} stay there.`, () => send(backups.filter(b => !gone.includes(b.name)), `${gone.join(', ')} removed`));
          drawAdmin();
        }
      }
    }, { 'data-for': r.key });
  };
  const g = grid({
    id: 'files-where', rows: rows(), sort: null,
    columns: [
      { key: 'name', label: 'where', cell: r => h('span', {}, h('b', {}, r.name), h('span', { class: 'dim' }, ' ' + (r.kind === 'node' ? 'node' : r.kind))) },
      { key: 'roles', label: 'for files', value: r => r.kind === 'node' ? d.nodes[r.id].roles.filter(x => FILE_ROLES.some(f => f[0] === x)).join(' ') : 'other storage', cell: roleBoxes },
      { key: 'ous', label: 'files of the OUs', value: r => r.kind === 'node' ? ousOf(r.id).join(', ') : '', cell: r => r.kind !== 'node' ? '' : h('span', { class: 'cut', title: ousOf(r.id).join(', ') }, ousOf(r.id).join(', ') || h('span', { class: 'dim' }, '—')) },
      { key: 'holds', label: 'keeps', value: r => holds(r), cell: r => h('span', { class: 'cut', title: holds(r) }, holds(r) || h('span', { class: 'dim' }, r.kind === 'node' && !info[r.id] ? '…' : '—')) },
      { key: 'space', label: 'space', value: r => r.kind === 'node' ? '' : r.s.place, cell: r => {
        if (r.kind !== 'node') return h('code', { class: 'cut' }, r.s.place);
        const i = info[r.id];
        return !i ? h('span', { class: 'dim' }, '…') : i.error ? h('span', { class: 'bad', title: i.error }, 'no answer') : i.storage ? `${fmtBytes(i.storage.size - i.storage.free)} used of ${fmtBytes(i.storage.size)}` : '—';
      } },
      { key: 'backup', label: 'nightly backup', value: r => nightlyTo(r.key).length ? 'target' : homeBacked(r) ? 'backed up' : '',
        cell: r => nightlyTo(r.key).length ? h('span', {}, ...nightlyTo(r.key).map(b => h('span', { class: b.off ? 'dim' : 'ok' }, b.off ? `${b.name} goes here (off) ` : `${b.name} goes here at ${b.time || '03:15'} `)))
          : homeBacked(r) ? h('span', { class: 'dim' }, 'its files are backed up') : r.kind === 'node' && backups.some(b => b.kind === 'nightly' && !b.off && (!b.what || !b.what.length || b.what.includes('nodes'))) ? h('span', { class: 'dim' }, 'its data is backed up') : '' },
    ],
  });
  const refresh = () => g.refresh(rows());
  api('GET', '/api/admin/nodes/info').then(v => { info = v; refresh(); }).catch(() => {});
  api('GET', '/api/backup/usage').then(v => { usage = v; refresh(); }).catch(() => {});
  api('GET', '/api/backup').then(v => { bk = v; refresh(); }).catch(() => {});
  return h('div', {}, g.node,
    h('p', { class: 'dim' }, 'Roles here are node control\'s (save it after a change): storage keeps files (the OUs whose files are placed on the node; ' +
      'node control\'s defaults and OU links place them); cache and external cache are for edges. A nightly backup goes to SFTP storage (other storage, below). ' +
      'Several nodes as mirrors or stripes, files on other storage and nightly backups to a node or S3 come with chunked storage (A12, the next release).'));
}

// other storage (T2): SFTP folders and S3 buckets, kept in the cluster's settings; secrets are never shown back
function otherStorage() {
  const storages = admin.ov.storages || {};
  const after = async (msg) => {
    try { const v = await api('GET', '/api/admin/overview'); admin.ov.storages = v.storages; } catch (e) { /* keep */ }
    if (msg) status(msg, 'ok');
    drawAdmin();
  };
  const g = grid({
    id: 'other-storage', rows: Object.entries(storages).map(([name, s]) => ({ name, ...s })), sort: 'name', empty: 'none yet',
    columns: [
      { key: 'name', label: 'name', cell: r => h('b', {}, r.name) },
      { key: 'kind', label: 'kind', type: 'enum', options: ['sftp', 's3'] },
      { key: 'place', label: 'where', cell: r => h('code', { class: 'cut', title: r.place }, r.place) },
      { key: 'secret', label: 'secret', value: r => r.secret_set.join(', '), cell: r => r.secret_set.length ? h('span', { class: 'ok' }, 'set (' + r.secret_set.join(', ') + ')') : h('span', { class: 'warn' }, 'none') },
      { key: 'host_key', label: 'server key', value: r => r.host_key || '', cell: r => r.kind !== 'sftp' ? '' : r.host_key ? h('code', { class: 'cut', title: r.host_key }, r.host_key) : h('span', { class: 'warn' }, 'not pinned: test it') },
      { key: 'note', label: 'note', value: r => r.note || '' },
      { key: 'act', label: '', value: () => '', cell: r => h('span', { class: 'row' },
        h('button', { type: 'button', onclick: async (e) => {
          e.target.disabled = true;
          try { const v = await api('POST', `/api/admin/storages/${r.name}/test`); await after(v.message); }
          catch (err) { status(err.message, 'bad'); e.target.disabled = false; }
        } }, 'test'),
        h('button', { type: 'button', onclick: () => storageForm(r.name, storages[r.name], after) }, 'change'),
        h('button', { type: 'button', class: 'danger', onclick: () => confirmModal(`remove storage ${r.name}`, `Remove ${r.name} (${r.place}) from the cluster's settings? Nothing on it is touched. A nightly backup that goes there stops working until it goes elsewhere.`, async () => {
          try { await api('DELETE', `/api/admin/storages/${r.name}`); await after(`storage ${r.name} removed`); } catch (err) { status(err.message, 'bad'); }
        }) }, 'remove')) },
    ],
  });
  return h('div', {}, g.node,
    h('div', { class: 'row' }, h('button', { type: 'button', onclick: () => storageForm(null, null, after) }, 'add other storage')),
    h('p', { class: 'dim' }, 'Places outside the nodes that files can go to: an SFTP server\'s folder, an S3 bucket (AWS, MinIO, Ceph, Garage, …). ' +
      'Kept in the cluster\'s settings; passwords, keys and secret keys are never shown back (an empty field keeps them). The test connects from the node serving this page; ' +
      'an SFTP server\'s key is pinned at the first test. Backups (below) go to SFTP ones: live and nightly.'));
}

function storageForm(name, s, after) {
  s = s || { kind: 'sftp', secret_set: [] };
  const inp = (attrs) => h('input', { spellcheck: false, autocomplete: 'off', ...attrs });
  const has = k => (s.secret_set || []).includes(k);
  const secretInp = (k) => inp({ type: 'password', autocomplete: 'new-password', placeholder: has(k) ? '(set; empty keeps it)' : '' });
  const f = {
    name: inp({ value: name || '', placeholder: 'nas, offsite-s3 (a-z, 0-9, -)', disabled: !!name, maxlength: 40 }),
    note: inp({ value: s.note || '', placeholder: 'what it is for', maxlength: 200 }),
    host: inp({ value: s.host || '', placeholder: 'name or address' }), port: inp({ type: 'number', value: s.port || 22, min: 1, max: 65535 }),
    username: inp({ value: s.username || '' }), root: inp({ value: s.root || '', placeholder: '(the login folder)' }),
    password: secretInp('password'), key: h('textarea', { rows: 4, spellcheck: false, placeholder: has('key') ? '(set; empty keeps it)' : 'OpenSSH or PEM private key' }), passphrase: secretInp('passphrase'),
    endpoint: inp({ value: s.endpoint || '', placeholder: 'https://s3.example.com' }), region: inp({ value: s.region || '', placeholder: 'us-east-1' }),
    bucket: inp({ value: s.bucket || '' }), prefix: inp({ value: s.prefix || '', placeholder: 'osada/ (optional)' }),
    access_key: inp({ value: s.access_key || '' }), secret_key: secretInp('secret_key'),
  };
  let kind = s.kind, auth = s.auth || 'password', vhost = !!s.virtual_host, clear = false, resetKey = false;
  const lbl = (text, el) => [h('span', { class: 'lbl' }, text), el];
  const pw = h('div', { class: 'stack' }, ...lbl('password', f.password));
  const kb = h('div', { class: 'stack' }, ...lbl('private key', f.key), ...lbl('key passphrase', f.passphrase));
  const sftpBox = h('div', { class: 'stack' }, ...lbl('host', f.host), ...lbl('port', f.port), ...lbl('user', f.username),
    ...lbl('log in with', select([['password', 'password'], ['key', 'private key']], auth, x => { auth = x; sync(); })), pw, kb, ...lbl('folder', f.root),
    s.host_key ? h('label', { class: 'row' }, check(false, x => { resetKey = x; }), `forget the pinned server key (${s.host_key})`) : '');
  const s3Box = h('div', { class: 'stack' }, ...lbl('endpoint', f.endpoint), ...lbl('region', f.region), ...lbl('bucket', f.bucket),
    h('label', { class: 'row' }, check(vhost, x => { vhost = x; }), 'the bucket in the host name (AWS); else in the path'),
    ...lbl('prefix (a folder in the bucket)', f.prefix), ...lbl('access key', f.access_key), ...lbl('secret key', f.secret_key));
  const sync = () => { sftpBox.hidden = kind !== 'sftp'; s3Box.hidden = kind !== 's3'; pw.hidden = auth !== 'password'; kb.hidden = auth !== 'key'; };
  sync();
  const m = modal(name ? `storage ${name}` : 'other storage', h('div', { class: 'stack' },
    ...lbl('name', f.name), ...lbl('kind', select([['sftp', 'SFTP (a folder on an SSH server)'], ['s3', 'S3 (a bucket)']], kind, x => { kind = x; sync(); }, name ? { disabled: true } : undefined)),
    sftpBox, s3Box, ...lbl('note', f.note),
    name && (s.secret_set || []).length ? h('label', { class: 'row' }, check(false, x => { clear = x; }), 'forget the secrets left empty') : ''), [
    h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
    h('button', { type: 'button', class: 'primary', onclick: async () => {
      const n = f.name.value.trim();
      const body = kind === 'sftp'
        ? { kind, host: f.host.value, port: +f.port.value || 22, username: f.username.value, auth, root: f.root.value, secret: { password: f.password.value, key: f.key.value, passphrase: f.passphrase.value } }
        : { kind, endpoint: f.endpoint.value, region: f.region.value, bucket: f.bucket.value, prefix: f.prefix.value, access_key: f.access_key.value, virtual_host: vhost, secret: { secret_key: f.secret_key.value } };
      Object.assign(body, { note: f.note.value, clear_secret: clear, reset_host_key: resetKey });
      try { await api('PUT', `/api/admin/storages/${encodeURIComponent(n)}`, body); m.close(); await after(`storage ${n} saved${kind === 'sftp' ? '; test it to pin its server key' : ''}`); }
      catch (err) { status(err.message, 'bad'); }
    } }, 'save')], { cls: 'pickmodal' });
}

/// The proxies (D-U7): instances of the module kind proxy, and who may use them.
function proxiesTab() {
  const d = admin.draft;
  const list = Object.entries(d.modules || {}).filter(([, m]) => m.kind === 'proxy');
  const cards = list.map(([name, m]) => win(`${name}${m.title ? ' · ' + m.title : ''}`,
    h('dl', { class: 'kv' }, h('dt', {}, 'its name'), h('dd', {}, h('code', {}, 'https://' + m.host)), h('dt', {}, 'upstream'), h('dd', {}, h('code', {}, m.upstream)), h('dt', {}, 'node'), h('dd', {}, nodeLabel(m.node))),
    grid({ id: `proxy-access-${name}`, columns: [{ key: 'subject', label: 'who has access', cell: a => a.subject_label }, { key: 'via', label: 'how' }],
      fetch: remote('access', { module: name }), empty: 'nobody has access' }).node,
    h('p', { class: 'dim' }, 'Access is given on the users tab (a user\'s card, or a mass update). Change or remove the proxy on the modules tab.')));
  return h('div', {},
    win('proxies', h('p', {}, 'A proxy shows another program at a name of its own (never on osada\'s), in a tab of osada, to those with access to it, and only while their osada session lasts — as apptab does. ',
      'Its name must reach this server: DNS, and https (at home the https listener, outside the edge).'),
      h('p', {}, h('a', { href: '/admin/modules', 'data-go': true }, 'add one on the modules tab (kind: proxy)'))),
    ...(cards.length ? cards : [h('p', { class: 'dim' }, 'no proxies yet')]));
}

/// Sign-in for other programs (OpenID Connect, osada 0.1's oidc.rs).
function appsTab() {
  const hand = (title, o, id, secret) => modal(title, h('div', { class: 'stack' },
    h('p', {}, secret ? 'The program needs these; the secret is shown only now:' : 'The program needs these:'),
    h('pre', { class: 'secret' }, `issuer:        ${o.issuer || '(not set)'}\nclient_id:     ${id}${secret ? '\nclient_secret: ' + secret : ''}`)), [h('button', { type: 'button', class: 'primary', onclick: () => { drawAdmin(); } }, 'done')]);
  const dialogFor = (o, c) => {
    const name = h('input', { value: c ? c.name : '', disabled: !!c, placeholder: 'e.g. tracker; it is the client_id' });
    const uris = h('textarea', { rows: 3, spellcheck: false, placeholder: 'http://192.168.1.11/oauth2/callback' });
    uris.value = c ? c.redirect_uris.join('\n') : '';
    const who = h('input', { value: c ? c.users : '', placeholder: 'everyone' });
    let pub = false;
    const m = modal(c ? `edit ${c.name}` : 'add a program', h('div', { class: 'stack' },
      h('span', { class: 'lbl' }, 'name'), name, h('span', { class: 'lbl' }, 'redirect URIs, one per line'), uris,
      h('span', { class: 'lbl' }, 'who may sign in: user names, comma separated; empty: everyone'), who,
      c ? null : h('label', { class: 'row' }, check(false, x => { pub = x; }), 'public: an app on a phone or in a browser, which cannot keep a secret (it must use PKCE)')), [
      h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
      h('button', { type: 'button', class: 'primary', onclick: async () => {
        try {
          const r = c ? await api('PATCH', `/api/oidc/clients/${c.id}`, { redirect_uris: uris.value, users: who.value })
            : await api('POST', '/api/oidc/clients', { name: name.value.trim(), redirect_uris: uris.value, users: who.value, public: pub });
          m.close();
          if (c) drawAdmin(); else hand('program added', o, r.client_id, r.client_secret);
        } catch (err) { status(err.message, 'bad'); }
      } }, c ? 'save' : 'add')], { cls: 'pickmodal' });
  };
  return loading(api('GET', '/api/oidc/clients'), o => {
    const g = grid({
      id: 'oidc-clients', sort: 'name', empty: 'no programs yet',
      rows: o.clients.map(c => ({ ...c, kind: c.public ? 'public (PKCE)' : 'with secret', who: c.users || 'everyone', uris: c.redirect_uris.join(' ') })),
      columns: [{ key: 'name', label: 'program', cell: c => h('b', {}, c.name) }, { key: 'kind', label: 'kind', type: 'enum', options: ['public (PKCE)', 'with secret'] },
        { key: 'uris', label: 'redirect URIs', cell: c => h('div', { class: 'uris' }, c.redirect_uris.map(u => h('span', { title: u }, u))) }, { key: 'who', label: 'who' }, { key: 'signed_in', label: 'signed in', type: 'number' },
        { key: 'last_used', label: 'last used', type: 'date', value: c => c.last_used ? c.last_used * 1000 : null },
        { key: 'act', label: '', value: () => '', cell: c => h('span', { class: 'row' },
          h('button', { type: 'button', onclick: () => dialogFor(o, c) }, 'edit'),
          c.public ? null : h('button', { type: 'button', onclick: () => confirmModal('new secret', `Make a new secret for ${c.name}? It stops working until it has the new one.`, async () => {
            try { const r = await api('POST', `/api/oidc/clients/${c.id}/secret`); hand('new secret', o, c.name, r.client_secret); } catch (err) { status(err.message, 'bad'); }
          }) }, 'new secret'),
          h('button', { type: 'button', class: 'danger', onclick: () => confirmModal('remove the program', `Remove ${c.name}? Nobody can sign in to it with osada any more.`, async () => {
            try { await api('DELETE', `/api/oidc/clients/${c.id}`); drawAdmin(); } catch (err) { status(err.message, 'bad'); }
          }) }, 'remove')) }],
    });
    return win('sign-in for other programs (OpenID Connect)',
      h('p', { class: 'dim' }, o.issuer ? `Programs can let people sign in with their osada account. Issuer: ${o.issuer}` : 'Off: set OSADA_OIDC_ISSUER in /etc/osada/osada.env and restart osada. Programs can be added already.'),
      g.node, h('div', { class: 'row' }, h('button', { type: 'button', class: 'primary', onclick: () => dialogFor(o, null) }, 'add a program')));
  });
}

/// Federation with other osada instances (Round 22; osada 0.1's side, osada1/federation.rs), and the UPNs' domain.
function federationTab() {
  return loading(api('GET', '/api/federation'), f => {
    const url = h('input', { placeholder: 'osada.example.org', spellcheck: false, size: 36 });
    const upn = h('input', { value: f.upn_domain || '', spellcheck: false, size: 28 });
    const g = grid({
      id: 'fed-peers', sort: 'host', empty: 'none yet',
      rows: f.peers,
      columns: [{ key: 'host', label: 'instance', cell: p => h('b', {}, p.host) },
        { key: 'upn_domain', label: 'its people', cell: p => h('code', {}, 'name@' + p.upn_domain) },
        { key: 'state', label: 'state', cell: p => h('span', { class: p.ok ? 'ok' : 'bad' }, p.state) },
        { key: 'added', label: 'added', type: 'date', value: p => p.added * 1000 }, { key: 'by', label: 'by' },
        { key: 'act', label: '', value: () => '', cell: p => h('button', { type: 'button', class: 'danger', onclick: () => confirmModal('stop federating',
          `Stop federating with ${p.host}? What its people share with people here, and what people here share with its people, stop working.`, async () => {
            try { await api('DELETE', '/api/federation/peers/' + encodeURIComponent(p.host)); drawAdmin(); } catch (err) { status(err.message, 'bad'); }
          }) }, 'remove') }],
    });
    return h('div', { class: 'stack' },
      win('this instance', h('dl', { class: 'kv' },
        h('dt', {}, 'address'), h('dd', {}, f.base ? h('code', {}, f.base) : h('span', { class: 'bad' }, 'no public URL (OSADA_PUBLIC_URL): no federation')),
        h('dt', {}, 'UPN domain'), h('dd', {}, h('span', { class: 'row' }, upn, h('button', { type: 'button', onclick: () => mailAct({ op: 'upn_domain', domain: upn.value.trim() }, 'UPN domain saved') }, 'save'))),
        h('dt', {}, 'others find it at'), h('dd', {}, h('code', {}, f.discovery || '—')),
        h('dt', {}, 'anyone may ask'), h('dd', {}, h('code', {}, f.who || '—'), h('span', { class: 'dim' }, ' yes or no, for one exact UPN; 20 questions a minute from an address')),
        h('dt', {}, 'shares'), h('dd', {}, `${f.incoming} from people there, ${f.outgoing} to people there`),
        // Round 24: open federation, here (the owner)
        h('dt', {}, 'open federation'), h('dd', {}, openSwitch(f))),
        h('p', { class: 'dim' }, 'Every account is also name@<UPN domain>: it signs in with that or its name, the share window and other instances know it by it, and with a mailbox it is an address of the mailbox too (mail arrives once the domain has its MX record).')),
      f.open ? blockedWin(f) : win('federated instances',
        h('p', { class: 'dim' }, 'Both sides add each other. Then people here find people there by their UPN in the share window and share with them, view only; each instance signs what it asks the other. Open federation (above) lets every instance in instead, but those blocked.'),
        g.node, h('div', { class: 'row' }, url, h('button', { type: 'button', class: 'primary', onclick: async () => {
          try { await api('POST', '/api/federation/peers', { url: url.value.trim() }); status('added', 'ok'); drawAdmin(); } catch (err) { status(err.message, 'bad'); }
        } }, 'add'))));
  });
}

/// Open federation's switch (Round 24): on, every osada instance may federate but those blocked.
function openSwitch(f) {
  const open = h('input', { type: 'checkbox' });
  open.checked = !!f.open;
  open.onchange = async () => {
    try { await api('POST', '/api/federation/settings', { open: open.checked }); status(open.checked ? 'open federation on' : 'open federation off', 'ok'); drawAdmin(); }
    catch (err) { open.checked = !open.checked; status(err.message, 'bad'); }
  };
  return h('label', { class: 'row' }, open, h('span', { class: 'dim' },
    'every osada instance may find people here by their UPN and share with them, but those blocked (off: only the instances listed, each side adding the other)'));
}

/// Open federation's list (Round 24): the instances blocked; a name blocks its subdomains too.
function blockedWin(f) {
  const host = h('input', { placeholder: 'osada.example.org', spellcheck: false, size: 36 });
  const g = grid({
    id: 'fed-blocked', sort: 'host', empty: 'none blocked',
    rows: (f.blocked || []).map(x => ({ host: x })),
    columns: [{ key: 'host', label: 'instance (and its subdomains)', cell: b => h('b', {}, b.host) },
      { key: 'act', label: '', value: () => '', cell: b => h('button', { type: 'button', onclick: async () => {
        try { await api('DELETE', '/api/federation/blocked/' + encodeURIComponent(b.host)); drawAdmin(); } catch (err) { status(err.message, 'bad'); }
      } }, 'unblock') }],
  });
  return win('blocked instances',
    h('p', { class: 'dim' }, 'Open federation is on: every osada instance may find people here by their UPN and share with them, but these.'),
    g.node, h('div', { class: 'row' }, host, h('button', { type: 'button', class: 'danger', onclick: async () => {
      try { await api('POST', '/api/federation/blocked', { host: host.value.trim() }); status('blocked', 'ok'); drawAdmin(); } catch (err) { status(err.message, 'bad'); }
    } }, 'block')));
}


// ---- tables and pickers over the node's data (a page at a time) ----

/// A grid's fetch over table `name` of the node (node/tables.rs).
function remote(name, params) {
  return q => api('POST', `/api/admin/table/${name}`, { ...q, params: typeof params === 'function' ? params() : (params || {}) });
}

/// Up to 15 rows of table `name` whose `col` contains the text typed.
function lookup(name, col, params) {
  return async text => (await api('POST', `/api/admin/table/${name}`, {
    filter: text ? { groups: [[{ col, op: 'contains', vals: [text] }]] } : null, sort: col, limit: 15, params: params || {},
  })).rows;
}

const OU_COLS = [{ key: 'path', label: 'path' }, { key: 'users', label: 'users', type: 'number' }, { key: 'groups', label: 'groups', type: 'number' },
  { key: 'tier0', label: 'tier 0', type: 'bool' }, { key: 'second_factor', label: 'second factor', type: 'bool' }];
const USER_COLS = [{ key: 'name', label: 'user', cell: u => h('b', {}, u.name) }, { key: 'display', label: 'name' }, { key: 'ou', label: 'OU', cell: u => u.ou_path },
  { key: 'state', label: 'state', type: 'enum', options: ['active', 'disabled'], cell: u => u.disabled ? h('span', { class: 'bad' }, 'disabled') : 'active' }];
const GROUP_COLS = [{ key: 'name', label: 'group', cell: g => h('b', {}, g.name) }, { key: 'display', label: 'for' }, { key: 'ou', label: 'OU', cell: g => g.ou_path },
  { key: 'members', label: 'members', type: 'number' }];

function ouPicker(perm, onpick) {
  const params = perm ? { perm } : {};
  return picker({ title: 'OUs', placeholder: 'type an OU path…', key: o => o.id, label: o => o.path, onpick,
    lookup: lookup('ous', 'path', params), fetch: remote('ous', params), columns: OU_COLS });
}

function userPicker(onpick) {
  return picker({ title: 'users', placeholder: 'type a user…', key: u => u.id, label: u => `${u.name}${u.display ? ' · ' + u.display : ''}`, onpick,
    lookup: lookup('users', 'name'), fetch: remote('users'), columns: USER_COLS });
}

function groupPicker(onpick) {
  return picker({ title: 'groups', placeholder: 'type a group…', key: g => g.id, label: g => `${g.name}${g.display ? ' · ' + g.display : ''}`, onpick,
    lookup: lookup('groups', 'name'), fetch: remote('groups'), columns: GROUP_COLS });
}

const KINDS = ['files', 'wiki', 'links', 'mail', 'chat', 'calendar', 'contacts', 'proxy', 'web'];
const MODULE_COLS = [{ key: 'name', label: 'module', cell: m => h('b', {}, m.name) }, { key: 'kind', label: 'kind', type: 'enum', options: KINDS },
  { key: 'title', label: 'title' }, { key: 'node', label: 'node', value: m => nodeLabel(m.node) }, { key: 'host', label: 'name (its own)', value: m => m.host || '' }];

function moduleRows() {
  return Object.entries(admin.draft.modules || {}).map(([name, m]) => ({ name, ...m }));
}

function modulePicker(onpick) {
  return picker({ title: 'modules', placeholder: 'type a module…', items: moduleRows, key: m => m.name, label: m => `${m.name}${m.title ? ' · ' + m.title : ''}`, onpick, columns: MODULE_COLS });
}

function nodePicker(names, value, onpick, empty) {
  const rows = () => names.map(n => ({ name: n, label: nodeLabel(n), roles: (admin.draft.nodes[n] || {}).roles || [], link: n === admin.ov.me ? 'this node' : admin.ov.links[n] !== undefined ? `${admin.ov.links[n].toFixed(2)} ms` : 'no link' }));
  return picker({
    title: 'nodes', placeholder: empty || 'type a node…', items: rows, key: r => r.name, label: r => r.label || nodeLabel(r.name), value, onpick, empty,
    columns: [{ key: 'label', label: 'node' }, { key: 'roles', label: 'roles' }, { key: 'link', label: 'link' }, { key: 'name', label: 'id' }],
  });
}

// ---- web: the sites (D-U21), their containers (D-U23) ----

const WEB_KINDS = ['static', 'container', 'upstream', 'job'];

// Round 14: the web nodes: each an edge, a host of static files, of containers (a web node not set
// does all), and the authoritative node: it holds the sites' configuration and is not exposed
const WEB_FUNCS = [['edge', 'edge', 'exposes sites: TLS for their names, their pages from here'], ['static_files', 'static files', 'hosts sites of static files'],
  ['containers', 'containers', 'hosts sites that are containers']];
function webFuncs(id) {
  const w = (admin.draft.web || {}).nodes || {};
  return w[id] || { edge: true, static_files: true, containers: true };
}
function webNodesBox() {
  const d = admin.draft;
  const webNodes = nodeNames().filter(id => hasModule(new Set(d.nodes[id].roles), d.nodes[id].rules || {}, MODULES.find(m => m.tag === 'web')));
  const auth = (d.web || {}).authority || '';
  const g = grid({
    id: 'web-nodes', rows: webNodes.map(id => ({ id, name: nodeLabel(id) })), sort: null, empty: 'no node has web yet: give it to one in the nodes tab',
    columns: [
      { key: 'name', label: 'web node', cell: r => h('span', {}, h('b', {}, r.name), r.id === auth ? h('span', { class: 'pill on' }, ' authoritative') : null) },
      { key: 'does', label: 'what it does', value: r => WEB_FUNCS.filter(([k]) => webFuncs(r.id)[k]).map(([, l]) => l).join(', '), cell: r => {
        const f = webFuncs(r.id);
        return multiPick(WEB_FUNCS, new Set(WEB_FUNCS.filter(([k]) => f[k]).map(([k]) => k)), (_, __, now) => {
          d.web = d.web || {};
          d.web.nodes = d.web.nodes || {};
          d.web.nodes[r.id] = { edge: now.has('edge'), static_files: now.has('static_files'), containers: now.has('containers') };
          markDirty();
        }, { 'data-web-node': r.id });
      } },
    ],
  });
  const authSel = select([['', '— none —'], ...webNodes.map(id => [id, nodeLabel(id)])], auth, v => {
    d.web = d.web || {};
    if (v) d.web.authority = v; else delete d.web.authority;
    markDirty();
  }, { id: 'web-authority' });
  return win('web: its nodes', g.node,
    h('div', { class: 'row' }, h('span', { class: 'lbl' }, 'authoritative node (the single source of truth; not exposed)'), authSel),
    h('p', { class: 'dim' }, 'The nodes with web (nodes tab). Edges expose sites; static files and containers say where sites of each kind may run. A site placed against this is marked ⚠ below. ' +
      'The authoritative node keeps the master copy of every static site of its own folder: edits go there, the node a site runs on gets a copy within 30 s, and while that node cannot be reached the edges serve the site from the authoritative node. Save node control after a change.'));
}

function webTab() {
  return h('div', {}, webNodesBox(), webSites());
}

function webSites() {
  const d = admin.draft;
  d.modules ||= {};
  // Round 14: the web nodes that are edges, and those that host sites (static files or containers)
  const webMod = MODULES.find(m => m.tag === 'web');
  const webNodes = () => nodeNames().filter(n => d.nodes[n] && hasModule(new Set(d.nodes[n].roles), d.nodes[n].rules || {}, webMod));
  const edges = () => webNodes().filter(n => webFuncs(n).edge);
  const hosts = cur => { const l = webNodes().filter(n => webFuncs(n).static_files || webFuncs(n).containers); if (cur && !l.includes(cur)) l.push(cur); return l.length ? l : nodeNames(); };
  let state = {}, allowed = null;
  // a cluster administrator sees every site; others those of the OUs they manage (the server's list, D-U28)
  const list = () => Object.entries(d.modules).filter(([, m]) => m.kind === 'web').filter(([name]) => admin.ov.cluster_admin || (allowed && allowed.has(name)))
    .map(([name, m]) => ({ name, m, s: m.site || {} }));
  const what = s => {
    const c = s.content || { kind: 'static' };
    // Round 18: a container's osada folders
    // Round 19: where its outgoing connections exit
    const exit = c.egress ? ` · exits by ${nodeLabel(c.egress)}` : '';
    return c.kind === 'container' ? c.image + ((c.folders || []).length ? ' + ' + c.folders.map(f => { const l = folderLine(f).split(' '); return `${l.slice(0, -1 - (f.write ? 1 : 0)).join(' ')} → ${f.to}${f.write ? ' (rw)' : ''}`; }).join(', ') : '') + exit
      : c.kind === 'upstream' ? c.url
      : c.kind === 'job' ? `${c.image} · ${c.schedule}${(c.writes || []).length ? ' → ' + c.writes.map(w => `${w.site}:${w.path}`).join(', ') : ''}${exit}`
      : 'web/<name>/' + (c.link ? ` → ${c.link.share ? 'share:' + c.link.share : c.link.group ? 'group:' + c.link.group : c.link.user}:${c.link.path}` : '');
  };
  const msg = h('div', { class: 'msg' });
  const act = async (name, action, b) => {
    b.disabled = true;
    try {
      await api('POST', '/api/admin/web/action', { site: name, action });
      msg.className = 'msg ok'; msg.textContent = `${name}: ${action} asked for; its node does it within seconds`;
      setTimeout(load, 6000);
    } catch (e) { msg.className = 'msg bad'; msg.textContent = `${name}: ${e.message}`; }
    b.disabled = false;
  };
  // Round 19: what went out by its exit node (the node it runs on counts)
  const egressBit = r => {
    const e = state[r.name] && state[r.name].egress;
    if (!e) return null;
    const s = e.stats || {};
    const tip = s.connections ? `${s.connections} connections out by ${e.exit_name}, ${s.failed || 0} failed; ${fmtBytes(s.bytes_out || 0)} out, ${fmtBytes(s.bytes_in || 0)} in; the last to ${s.last}${s.last_error ? '\nthe last failure: ' + s.last_error : ''}` : `nothing out by ${e.exit_name} yet (since its node started)`;
    return h('span', { class: s.failed && s.last_error ? 'warn' : 'dim', title: tip }, ` · out by ${e.exit_name}${s.connections ? ` (${s.connections}${s.failed ? `, ${s.failed} failed` : ''})` : ''}`);
  };
  const stateCell = r => {
    if ((r.s.content || {}).kind === 'job') {
      const jst = state[r.name] && state[r.name].job;
      if (!jst) return h('span', { class: 'dim' }, 'asking…');
      if (jst.error) return h('span', { class: 'bad', title: jst.error }, 'no answer');
      const last = jst.last || {};
      const ok = !last.result || last.result === 'done';
      return h('span', { title: (last.log || '').slice(-1500) },
        h('span', { class: jst.state === 'running' ? 'ok' : 'dim' }, jst.state || 'waiting'),
        last.result ? [' · last ', h('span', { class: ok ? 'ok' : 'bad' }, last.result), last.at ? h('span', { class: 'dim' }, ` ${age(last.at * 1000)}`) : null] : null,
        jst.next ? h('span', { class: 'dim' }, ` · next ${new Date(jst.next * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`) : null, egressBit(r));
    }
    if ((r.s.content || {}).kind !== 'container') return h('span', { class: 'dim' }, '—');
    const st = state[r.name];
    if (!st) return h('span', { class: 'dim' }, 'asking…');
    const c = st.container || {};
    if (c.error) return h('span', { class: 'bad', title: c.error }, 'no answer');
    const last = c.last ? ` · ${c.last.what}` : '';
    return h('span', {}, h('span', { class: c.state === 'running' ? 'ok' : 'bad' }, c.state || 'unknown'), h('span', { class: 'dim' }, ` ${c.image_id || ''}${c.port ? ' :' + c.port : ''}${last}`), egressBit(r));
  };
  const g = grid({
    id: 'web-sites',
    columns: [
      { key: 'name', label: 'site', cell: r => h('b', {}, r.name) },
      { key: 'names', label: 'names', value: r => siteNames(r).join(' '), cell: r => h('span', { class: 'wrap' }, siteNames(r).join(', '), r.s.under_web ? h('span', { class: r.s.open ? 'warn' : 'dim' }, r.s.open ? ' (/web/: open to anyone)' : ' (/web/: signed in only)') : null, r.s.office ? h('span', { class: 'dim' }, ' (the office suite)') : null) },
      { key: 'kind', label: 'content', value: r => (r.s.content || {}).kind || 'static', cell: r => h('span', {}, h('b', {}, (r.s.content || {}).kind || 'static'), ' ', h('code', {}, what(r.s))) },
      { key: 'ou', label: 'OU', value: r => (state[r.name] && state[r.name].ou_path) || '/' },
      { key: 'runs', label: 'runs on', value: r => nodeLabel(r.m.node), cell: r => {
        // Round 14: a node that does not host this kind of site
        const kind = (r.s.content || {}).kind || 'static';
        const need = kind === 'container' ? 'containers' : kind === 'static' ? 'static_files' : null;
        if (need && !webFuncs(r.m.node)[need]) return h('span', { class: 'warn', title: `${nodeLabel(r.m.node)} does not host ${need === 'containers' ? 'containers' : 'static files'} (web: its nodes)` }, nodeLabel(r.m.node) + ' ⚠');
        return state[r.name] && state[r.name].files_elsewhere
          ? h('span', { class: 'warn', title: `its files belong to its OU, kept on ${state[r.name].files_home}; they are on ${nodeLabel(r.m.node)}` }, nodeLabel(r.m.node) + ' ⚠')
          : nodeLabel(r.m.node);
      } },
      { key: 'exposed', label: 'exposed on', value: r => (r.s.exposed_on || []).map(nodeLabel).join(', ') || 'nowhere (off)', cell: r => {
        const on = r.s.exposed_on || [];
        if (!on.length) return h('span', { class: 'dim' }, 'nowhere (off)');
        return h('span', {}, on.map((n, i) => [i ? ', ' : '', webFuncs(n).edge ? nodeLabel(n) : h('span', { class: 'warn', title: `${nodeLabel(n)} is not an edge (web: its nodes)` }, nodeLabel(n) + ' ⚠')]));
      } },
      { key: 'front', label: 'front', value: r => r.s.front || 'tls' },
      { key: 'state', label: 'state', value: r => (state[r.name] && state[r.name].container && state[r.name].container.state) || '', cell: stateCell },
      { key: 'files', label: '', value: () => '', cell: r => ['static', 'job'].includes((r.s.content || {}).kind) ? h('button', { type: 'button', title: 'the files of its folder', onclick: e => { e.stopPropagation(); siteFiles(r.name); } }, 'files') : '' },
      { key: 'act', label: '', value: () => '', cell: r => (r.s.content || {}).kind === 'job' ? h('button', { type: 'button', class: 'job-run', title: 'run it now (its schedule stays)', onclick: e => { e.stopPropagation(); act(r.name, 'run', e.target); } }, 'run now')
        : (r.s.content || {}).kind === 'container' ? h('span', { class: 'row' },
        h('button', { type: 'button', title: 'pull the image; if it is newer, make the container anew with the same settings and remove the old image', onclick: e => { e.stopPropagation(); act(r.name, 'upgrade', e.target); } }, 'upgrade'),
        h('button', { type: 'button', onclick: e => { e.stopPropagation(); act(r.name, 'restart', e.target); } }, 'restart')) : '' },
    ],
    rows: list(), sort: 'name', onRow: r => edit(r.name, false), empty: 'no sites yet',
  });
  const load = async () => {
    try {
      const v = await api('GET', '/api/admin/web');
      state = Object.fromEntries((v.sites || []).map(x => [x.name, x]));
      allowed = new Set((v.sites || []).map(x => x.name));
      g.refresh(list());
    } catch (e) { msg.className = 'msg bad'; msg.textContent = e.message; }
  };
  load();
  const edit = (name, fresh) => {
    const m = fresh ? { kind: 'web', title: '', node: edges()[0] || nodeNames()[0] } : d.modules[name];
    const s = structuredClone(m.site || {});
    s.names ||= []; s.exposed_on ||= (fresh && edges()[0] ? [edges()[0]] : []); s.routes ||= []; s.front ||= 'tls';
    const c = s.content || { kind: 'static' };
    const area = (rows, value, ph) => { const t = h('textarea', { rows, spellcheck: false, placeholder: ph }); t.value = value; return t; };
    const nameIn = h('input', { value: fresh ? '' : name, disabled: !fresh, placeholder: 'pplc', pattern: '[a-z][a-z0-9\\-]*' });
    const title = h('input', { value: m.title || '' });
    const names = area(2, s.names.join('\n'), 'pplc.example.net\nwww.pplc.example.net');
    let runs = m.node;
    const rp = nodePicker(hosts(m.node), m.node, row => { if (row) runs = row.name; });
    // its OU (D-U28): who manages it, where its files belong
    let ou = s.ou || (fresh ? d.web_ou : null) || null;
    const ouNow = h('code', {}, (state[name] && state[name].ou_path) || '/');
    const op = ouPicker('ou-manage', row => { if (row) { ou = row.id; ouNow.textContent = row.path; } });
    const exposed = h('span', { class: 'row wrap' }, edges().length ? edges().map(n => h('label', { class: 'row' }, check(s.exposed_on.includes(n), on => { s.exposed_on = on ? [...s.exposed_on, n] : s.exposed_on.filter(x => x !== n); }), nodeLabel(n))) : h('span', { class: 'dim' }, 'no node has the role edge-http'));
    const front = select([['tls', 'its own certificate (https)'], ['cloudflare', 'behind Cloudflare (plain http, Cloudflare\'s addresses only)']], s.front, v => { s.front = v; });
    let spa = !!c.spa;
    const linkUser = h('input', { value: c.link ? (c.link.group ? 'group:' + c.link.group : c.link.user) : '', placeholder: 'user name, or group:<name>' });
    const linkPath = h('input', { value: c.link ? c.link.path : '', placeholder: '/www/site' });
    const image = h('input', { value: c.image || '', placeholder: 'docker.io/searxng/searxng:latest', size: 40 });
    const port = h('input', { type: 'number', min: 1, max: 65535, value: c.port || 8080, style: 'width:7em' });
    const vols = area(2, (c.volumes || []).join('\n'), 'searxng:/etc/searxng');
    const env = area(3, Object.entries(c.env || {}).map(([k, v]) => `${k}=${v}`).join('\n'), 'BASE_URL=https://searx.example.org/');
    // Round 18: osada folders in the container
    const folders = area(3, (c.folders || []).map(folderLine).join('\n'), 'https://osada.example.org/s/AbCd… /media/movies\nuser:anna/Music /media/music\ngroup:family/Photos /photos rw');
    const url = h('input', { value: c.url || '', placeholder: 'http://127.0.0.1:8080', size: 40 });
    const routes = area(3, s.routes.map(r => `${r.prefix} ${r.upstream}`).join('\n'), '/api/pi/ http://10.0.0.2:8080/');
    // Round 18b: a job
    const jImage = h('input', { value: c.kind === 'job' ? c.image : 'docker.io/library/python:3.12-alpine', size: 40, id: 'job-image' });
    const jCommand = area(3, (c.command || []).join('\n'), 'python3\nmorning.py\n/out/morning.html');
    const jSchedule = h('input', { value: c.schedule || '', placeholder: '4,24,44 * * * *', size: 24, id: 'job-schedule' });
    const jEnv = area(2, Object.entries(c.kind === 'job' ? c.env || {} : {}).map(([k, v]) => `${k}=${v}`).join('\n'), 'TZ=Europe/Berlin');
    const jWrites = area(3, (c.writes || []).map(w => `${w.file} ${w.site}:${w.path}`).join('\n'), 'morning.html pplc:/morning.html');
    const jHosts = area(2, Object.entries(c.kind === 'job' ? c.hosts || {} : {}).map(([k, v]) => `${k} ${v}`).join('\n'), 'db.example.org 192.168.1.12');
    const jTimeout = h('input', { type: 'number', min: 10, max: 86400, value: c.timeout || '', placeholder: '600', style: 'width:7em' });
    // Round 19: the node its outgoing connections exit by (every node can be one: nothing to deploy)
    let egress = c.egress || '';
    const egressPick = id => select([['', 'the node it runs on (directly)'], ...nodeNames().map(n => [n, nodeLabel(n)])], egress, v => { egress = v; }, { id });
    const cEgress = egressPick('site-egress'), jEgress = egressPick('job-egress');
    const egressNote = () => h('p', { class: 'dim' }, 'Another node: osada sets HTTP_PROXY and HTTPS_PROXY in it to a proxy in its own network, and what honours them (curl, Python, Go, .NET…) goes out by that node, over the link; that node resolves the names. What ignores them goes out directly.');
    // Round 18: at /web/<name>/ on osada's names, the office suite, headers of its own
    let underWeb = !!s.under_web, office = !!s.office, open = !!s.open;
    const headers = area(2, Object.entries(s.headers || {}).map(([k, v]) => `${k}: ${v}`).join('\n'), 'Cross-Origin-Opener-Policy: same-origin\nCross-Origin-Embedder-Policy: require-corp');
    const parts = {
      static: h('div', { class: 'stack' }, h('label', { class: 'row' }, check(spa, on => { spa = on; }), 'a path with no file gets index.html (single-page app)'),
        h('span', { class: 'lbl' }, 'its folder as a osada folder (optional): a user\'s name and a folder of theirs'), h('span', { class: 'row' }, linkUser, linkPath),
        h('p', { class: 'dim' }, 'A linked site runs on the node with osada\'s files (home, home) and may be exposed on any edge: what is in that folder is the site, at once.')),
      container: h('div', { class: 'stack' }, h('span', { class: 'lbl' }, 'image'), image, h('span', { class: 'lbl' }, 'the port it answers on, inside'), port,
        h('span', { class: 'lbl' }, 'named volumes, one a line (name:/path, :ro to read only)'), vols, h('span', { class: 'lbl' }, 'environment, one a line (NAME=value)'), env,
        h('span', { class: 'lbl', id: 'site-folders' }, 'osada folders, one a line: a public link (its address), user:<name>/<folder> or group:<name>/<folder>, then the path inside; read only, rw at the end to write (what it writes belongs to the container\'s user)'), folders,
        h('p', { class: 'dim' }, 'A container with osada folders runs on the node with osada\'s files (home). A folder that goes away (a link removed or expired) stops the container until it is back.'),
        h('span', { class: 'lbl' }, 'outgoing connections exit by'), cEgress, egressNote()),
      upstream: h('div', { class: 'stack' }, h('span', { class: 'lbl' }, 'upstream'), url),
      job: h('div', { class: 'stack' },
        h('p', { class: 'dim' }, 'A job is not served: its node runs a container on a schedule, as the account of osada\'s files, in the job\'s own folder (web/<name>/, at /job, where it starts; its files button), with /out for what it makes. It may write only the files named below: after a run that ends well, each is put into that site\'s folder.'),
        h('span', { class: 'lbl' }, 'image'), jImage,
        h('span', { class: 'lbl' }, 'command, one argument a line (empty: the image\'s own)'), jCommand,
        h('span', { class: 'lbl' }, 'when: cron\'s five fields, minute hour day month weekday (the node\'s time)'), jSchedule,
        h('span', { class: 'lbl' }, 'environment, one a line (NAME=value)'), jEnv,
        h('span', { class: 'lbl', id: 'job-writes' }, 'files it may write, one a line: the file it leaves in /out, then site:/path in that site\'s folder'), jWrites,
        h('span', { class: 'lbl', id: 'job-hosts' }, 'names at an address of their own, one a line: name, then its IP (a public name that leads home, reached at home)'), jHosts,
        h('span', { class: 'lbl' }, 'the longest a run may take, seconds (empty: 600)'), jTimeout,
        h('span', { class: 'lbl' }, 'outgoing connections exit by'), jEgress, egressNote()),
    };
    let kind = c.kind || 'static';
    const partHost = h('div', {});
    const drawPart = () => partHost.replaceChildren(parts[kind]);
    drawPart();
    const bad = h('div', { class: 'msg' });
    const box = modal(fresh ? 'add a site' : `site ${name}`, h('div', { class: 'stack' },
      h('span', { class: 'lbl' }, 'name'), nameIn, h('span', { class: 'lbl' }, 'title'), title,
      h('span', { class: 'lbl' }, 'host names, one a line'), names,
      h('span', { class: 'lbl' }, 'exposed on: the edges that answer its names'), exposed,
      h('label', { class: 'row', id: 'site-under-web' }, check(underWeb, on => { underWeb = on; }), 'also at /web/<name>/ on osada\'s own names (its pages are then osada\'s own: they could act as whoever is signed in, so only for sites you trust; osada\'s cookies are not passed to it)'),
      h('label', { class: 'row', id: 'site-open' }, check(open, on => { open = on; }), 'at /web/<name>/ also for people not signed in to osada (not recommended: by default osada checks the session on every request there, so only osada itself faces the internet; apps of its own, e.g. on a TV, cannot get in then)'),
      h('label', { class: 'row', id: 'site-office' }, check(office, on => { office = on; }), 'the office suite: files offer "open in office" with its page at /web/<name>/'),
      // OUs show only where there is more than the root (D-U30: a deployment with one OU never sees them)
      Object.keys(d.ous || {}).length > 1 ? [h('span', { class: 'lbl' }, 'OU: who manages it, and whose files it is (a static site\'s files belong on its OU\'s files node)'), h('span', { class: 'row' }, ouNow, op.node)] : null,
      h('span', { class: 'lbl' }, 'runs on: the node with its folder or container'), rp.node,
      h('span', { class: 'lbl' }, 'front'), front,
      h('span', { class: 'lbl' }, 'content'), select(WEB_KINDS.map(k => [k, k]), kind, v => { kind = v; drawPart(); }), partHost,
      h('span', { class: 'lbl' }, 'routes: a path prefix and an upstream, one a line (nginx\'s proxy_pass: an upstream with a path takes the prefix\'s place)'), routes,
      h('span', { class: 'lbl' }, 'headers its answers carry, one a line (Name: value)'), headers,
      h('p', { class: 'dim' }, 'A site\'s files are in the folder web/<name>/ of the node it runs on. Containers run there too, as root\'s podman (osada-web-<name>, on 127.0.0.1 only). Save node control after.'), bad), [
      fresh ? null : h('button', { type: 'button', class: 'danger', onclick: () => { delete d.modules[name]; box.close(); markDirty(); drawAdmin(); } }, 'remove'),
      h('button', { type: 'button', onclick: () => box.close() }, 'cancel'),
      h('button', { type: 'button', class: 'primary', onclick: () => {
        const n = fresh ? nameIn.value.trim() : name;
        if (!/^[a-z][a-z0-9-]*$/.test(n)) { bad.className = 'msg bad'; bad.textContent = 'a name is a-z, 0-9 and -, starting with a letter'; return; }
        if (fresh && d.modules[n]) { bad.className = 'msg bad'; bad.textContent = `there is a module ${n} already`; return; }
        let content;
        if (kind === 'static') {
          content = { kind, spa };
          const who = linkUser.value.trim();
          if (who) content.link = who.startsWith('group:') ? { group: who.slice(6), path: linkPath.value.trim() || '/' } : { user: who, path: linkPath.value.trim() || '/' };
        } else if (kind === 'container') {
          const kv = env.value.split('\n').map(x => x.trim()).filter(Boolean).map(x => { const i = x.indexOf('='); return i < 0 ? [x, ''] : [x.slice(0, i), x.slice(i + 1)]; });
          let fl;
          try { fl = folders.value.split('\n').map(x => x.trim()).filter(Boolean).map(folderOf); } catch (e) { bad.className = 'msg bad'; bad.textContent = e.message; return; }
          content = { kind, image: image.value.trim(), port: +port.value || 0, volumes: vols.value.split('\n').map(x => x.trim()).filter(Boolean), env: Object.fromEntries(kv), ...(fl.length ? { folders: fl } : {}), ...(egress ? { egress } : {}) };
        } else if (kind === 'job') {
          const lines = t => t.value.split('\n').map(x => x.trim()).filter(Boolean);
          const kv = lines(jEnv).map(x => { const i = x.indexOf('='); return i < 0 ? [x, ''] : [x.slice(0, i), x.slice(i + 1)]; });
          let writes;
          try {
            writes = lines(jWrites).map(x => { const m = /^(\S+)\s+([a-z][a-z0-9-]*):(\/\S.*)$/.exec(x); if (!m) throw new Error(`${x}: a file, then site:/path`); return { file: m[1], site: m[2], path: m[3] }; });
          } catch (e) { bad.className = 'msg bad'; bad.textContent = e.message; return; }
          const hosts = Object.fromEntries(lines(jHosts).map(x => x.split(/\s+/)).filter(x => x.length >= 2).map(x => [x[0].toLowerCase(), x[1]]));
          content = { kind, ...(Object.keys(hosts).length ? { hosts } : {}), image: jImage.value.trim(), command: jCommand.value.split('\n').map(x => x.replace(/\r$/, '')).filter(x => x !== ''), schedule: jSchedule.value.trim().replace(/\s+/g, ' '), env: Object.fromEntries(kv), writes, ...(+jTimeout.value ? { timeout: +jTimeout.value } : {}), ...(egress ? { egress } : {}) };
        } else content = { kind, url: url.value.trim() };
        const site = {
          names: names.value.split(/[\s,]+/).map(x => x.trim().toLowerCase()).filter(Boolean), exposed_on: s.exposed_on, content, front: s.front, ...(ou && ou !== 1 ? { ou } : {}),
          routes: routes.value.split('\n').map(x => x.trim()).filter(Boolean).map(x => { const [prefix, upstream] = x.split(/\s+/); return { prefix, upstream: upstream || '' }; }),
          ...(underWeb ? { under_web: true } : {}), ...(underWeb && open ? { open: true } : {}), ...(office ? { office: true } : {}),
          headers: Object.fromEntries(headers.value.split('\n').map(x => x.trim()).filter(Boolean).map(x => { const i = x.indexOf(':'); return i < 0 ? [x, ''] : [x.slice(0, i).trim(), x.slice(i + 1).trim()]; })),
        };
        // a job is not served (Round 18b)
        if (kind === 'job') { site.names = []; site.exposed_on = []; site.routes = []; delete site.under_web; delete site.open; delete site.office; }
        d.modules[n] = { kind: 'web', title: title.value.trim(), node: runs, site };
        box.close(); markDirty(); drawAdmin();
      } }, 'keep'),
    ].filter(Boolean), { cls: 'pickmodal' });
  };
  return h('div', {},
    win('web: every site, where it is exposed and where it runs (click one to change it)', g.node, msg,
      h('div', { class: 'row' }, h('button', { type: 'button', class: 'primary', onclick: () => edit(null, true) }, 'add a site'),
        h('button', { type: 'button', onclick: load }, 'refresh state'),
        // the OU new sites go into (D-U30), where there is more than the root
        admin.ov.cluster_admin && Object.keys(d.ous || {}).length > 1 ? (() => {
          const now = h('code', {}, d.web_ou ? (admin.ov.ous && admin.ov.ous[d.web_ou] ? admin.ov.ous[d.web_ou].name : `OU ${d.web_ou}`) : '/');
          const op = ouPicker(null, row => { if (row) { if (row.id === 1) delete d.web_ou; else d.web_ou = row.id; now.textContent = row.path; markDirty(); } });
          return h('span', { class: 'row' }, h('span', { class: 'lbl' }, 'new sites go into'), now, op.node);
        })() : null)),
    h('p', { class: 'dim' }, 'Exposed on: the edge nodes whose 80/443 answer the names (DNS must point there). Runs on: where its files or container are; an edge passes requests over the node link when it runs elsewhere. Behind Cloudflare: plain http from Cloudflare\'s addresses only, the visitor from CF-Connecting-IP. Upgrade pulls the image and, if it is newer, makes the container anew with the same settings.'));
}

// a container's osada folder (Round 18) as a line: <source> <path inside> [rw]; the source a
// public link's address or share:<token>[/sub], user:<name>/<folder>, group:<name>/<folder>
function folderLine(f) {
  const rest = f.path && f.path !== '/' ? f.path : '';
  const src = f.share ? `share:${f.share}${rest}` : f.group ? `group:${f.group}${rest}` : `user:${f.user}${rest}`;
  return `${src} ${f.to}${f.write ? ' rw' : ''}`;
}
function folderOf(line) {
  const w = line.split(/\s+/);
  let write = false;
  if (/^(rw|ro)$/.test(w[w.length - 1])) write = w.pop() === 'rw';
  const to = w.pop();
  const src = w.join(' ');
  if (!src || !to || !to.startsWith('/')) throw new Error(`${line}: a folder, then the path inside (from /)`);
  let m;
  const path = p => (p ? decodeURIComponent(p) : '/');
  if ((m = /^https?:\/\/[^/]+\/s\/([A-Za-z0-9_-]+)(\/.*)?$/.exec(src)) || (m = /^share:([A-Za-z0-9_-]+)(\/.*)?$/.exec(src))) return { share: m[1], path: path(m[2]), to, write };
  if ((m = /^user:([^/]+)(\/.*)?$/.exec(src))) return { user: m[1], path: m[2] || '/', to, write };
  if ((m = /^group:([^/]+)(\/.*)?$/.exec(src))) return { group: m[1], path: m[2] || '/', to, write };
  throw new Error(`${src}: a public link's address, user:<name>/<folder> or group:<name>/<folder>`);
}

// a site's host names, and /web/<name>/ when it is there too (Round 18)
function siteNames(r) {
  return [...(r.s.names || []), ...(r.s.under_web ? [`/web/${r.name}/`] : [])];
}

// a site's folder (T13): browse, edit text files, download, upload, new folder, rename, delete
const TEXT_EXT = /\.(html?|css|js|mjs|json|txt|md|xml|svg|csv|ya?ml|toml|ini|conf|webmanifest|map|htaccess|py|sh)$/i;
function siteFiles(site) {
  let path = '';
  const msg = h('div', { class: 'msg' });
  const crumbs = h('div', { class: 'row' });
  const listHost = h('div', {});
  const call = (op, extra) => api('POST', '/api/admin/web/files', { site, op, path, ...extra });
  const say = (text, cls) => { msg.className = 'msg ' + (cls || 'ok'); msg.textContent = text; };
  const join = (a, b) => (a ? a + '/' : '') + b;
  const b64 = buf => { let s = ''; const u = new Uint8Array(buf); for (let i = 0; i < u.length; i += 0x8000) s += String.fromCharCode.apply(null, u.subarray(i, i + 0x8000)); return btoa(s); };
  const draw = async () => {
    crumbs.replaceChildren(h('a', { href: '#', onclick: e => { e.preventDefault(); path = ''; draw(); } }, `web/${site}`),
      ...path.split('/').filter(Boolean).map((p, i, all) => [' / ', h('a', { href: '#', onclick: e => { e.preventDefault(); path = all.slice(0, i + 1).join('/'); draw(); } }, p)]).flat());
    let v;
    try { v = await call('list'); } catch (e) { say(e.message, 'bad'); listHost.replaceChildren(); return; }
    const rows = v.entries.map(x => ({ ...x, mtime: x.mtime * 1000 }));
    const g = grid({
      id: 'site-files', rows, sort: 'name', empty: 'empty folder',
      columns: [
        { key: 'name', label: 'name', cell: x => x.dir ? h('a', { href: '#', onclick: e => { e.preventDefault(); path = join(path, x.name); draw(); } }, x.name + '/') : h('span', {}, x.name) },
        { key: 'size', label: 'size', type: 'number', cell: x => x.dir ? '' : fmtBytes(x.size) },
        { key: 'mtime', label: 'changed', type: 'date' },
        // who may change it (Round 18b): its mode and owner, and the jobs that write it
        { key: 'mode', label: 'permissions', cell: x => h('code', { title: `owner ${x.owner}, group ${x.group}` }, x.mode || '') },
        { key: 'owner', label: 'owner', value: x => `${x.owner || ''}:${x.group || ''}` },
        { key: 'writers', label: 'written by', value: x => (x.writers || []).map(w => w.job).join(' '),
          cell: x => (x.writers || []).length ? h('span', { class: 'warn', title: 'a job puts this file here after each run that ends well' }, x.writers.map(w => `job ${w.job} (${w.schedule})`).join(', ')) : '' },
        { key: 'act', label: '', value: () => '', cell: x => h('span', { class: 'row' },
          !x.dir && TEXT_EXT.test(x.name) ? h('button', { type: 'button', onclick: () => editText(join(path, x.name)) }, 'edit') : null,
          !x.dir ? h('a', { class: 'button', href: `/api/admin/web/file?site=${encodeURIComponent(site)}&path=${encodeURIComponent(join(path, x.name))}` }, 'download') : null,
          h('button', { type: 'button', onclick: async () => {
            const to = prompt(`rename ${x.name} to (a path in web/${site})`, join(path, x.name));
            if (!to || to === join(path, x.name)) return;
            try { await api('POST', '/api/admin/web/files', { site, op: 'rename', path: join(path, x.name), to }); say(`${x.name} → ${to}`); draw(); } catch (e) { say(e.message, 'bad'); }
          } }, 'rename'),
          h('button', { type: 'button', class: 'danger', onclick: async () => {
            const what = join(path, x.name);
            if (!confirm(x.dir ? `Delete the folder ${what} and everything in it?` : `Delete ${what}?`)) return;
            try { await api('POST', '/api/admin/web/files', { site, op: 'delete', path: what, all: x.dir }); say(`${what} deleted`); draw(); } catch (e) { say(e.message, 'bad'); }
          } }, 'delete')) },
      ],
    });
    // a job's own folder (Round 18b): what it reads and writes, and where its files go
    const jobNote = v.job ? h('p', { class: 'dim', id: 'job-folder-note' }, `The job's own folder (at /job, where it starts; it may change anything here). It runs ${v.job.schedule}; after a run that ends well, `,
      (v.job.writes || []).length ? v.job.writes.map((w, i) => [i ? ', ' : '', h('code', {}, `/out/${w.file}`), ' → ', h('code', {}, `${w.site}:${w.path}`)]) : 'nothing goes anywhere else', '.') : null;
    listHost.replaceChildren(...[jobNote, g.node].filter(Boolean));
  };
  const editText = async (file, fresh) => {
    let text = '';
    if (!fresh) {
      try { const v = await api('POST', '/api/admin/web/files', { site, op: 'read', path: file }); text = new TextDecoder().decode(Uint8Array.from(atob(v.data), c => c.charCodeAt(0))); } catch (e) { say(e.message, 'bad'); return; }
    }
    const area = h('textarea', { rows: 28, spellcheck: false, style: 'width:100%;font-family:monospace;font-size:12px' });
    area.value = text;
    const box = modal(`web/${site}/${file}`, area, [
      h('button', { type: 'button', onclick: () => box.close() }, 'cancel'),
      h('button', { type: 'button', class: 'primary', onclick: async () => {
        try { await api('POST', '/api/admin/web/files', { site, op: 'write', path: file, data: b64(new TextEncoder().encode(area.value)) }); box.close(); say(`${file} saved`); draw(); } catch (e) { say(e.message, 'bad'); }
      } }, 'save'),
    ], { cls: 'pickmodal' });
  };
  const upload = h('input', { type: 'file', multiple: true, onchange: async e => {
    for (const f of e.target.files) {
      if (f.size > 10 * 1024 * 1024) { say(`${f.name}: over 10 MiB; put it into the site's folder on its node`, 'bad'); continue; }
      try { await api('POST', '/api/admin/web/files', { site, op: 'write', path: join(path, f.name), data: b64(await f.arrayBuffer()) }); say(`${f.name} uploaded`); } catch (err) { say(`${f.name}: ${err.message}`, 'bad'); }
    }
    e.target.value = '';
    draw();
  } });
  const box = modal(`files of ${site}`, h('div', { class: 'stack' }, crumbs,
    h('div', { class: 'row' },
      h('button', { type: 'button', onclick: () => { path = path.split('/').slice(0, -1).join('/'); draw(); } }, 'up'),
      h('button', { type: 'button', onclick: async () => {
        const n = prompt('a new folder\'s name');
        if (!n) return;
        try { await api('POST', '/api/admin/web/files', { site, op: 'mkdir', path: join(path, n) }); draw(); } catch (e) { say(e.message, 'bad'); }
      } }, 'new folder'),
      h('label', { class: 'button' }, 'upload…', upload),
      h('button', { type: 'button', onclick: () => { const n = prompt('a new file\'s name (index.html)'); if (n) editText(join(path, n), true); } }, 'new text file')),
    listHost, msg,
    h('p', { class: 'dim' }, 'The folder on the node the site runs on: what it serves, at once. Files up to 10 MiB here.')), [
    h('button', { type: 'button', onclick: () => box.close() }, 'close'),
  ], { cls: 'pickmodal' });
  draw();
}

// ---- dns (T11): what the deployment needs, what public DNS says; where Stalwart is (T12) ----

function dnsTab() {
  const host = h('div', {}, h('p', { class: 'dim' }, 'asking public DNS for every record (Cloudflare\'s resolver)…'));
  const save = (name, text) => { const a = h('a', { href: URL.createObjectURL(new Blob([text], { type: 'text/plain' })), download: name }); document.body.append(a); a.click(); a.remove(); };
  api('GET', '/api/admin/dns').then(v => {
    const g = grid({
      id: 'dns', rows: v.rows.map((r, i) => ({ ...r, i })), sort: 'state', empty: 'nothing to check',
      columns: [
        { key: 'state', label: 'state', type: 'enum', options: ['ok', 'missing', 'differs', 'error'], cell: r => h('span', { class: r.state === 'ok' ? 'ok' : 'bad' }, r.state) },
        { key: 'name', label: 'name' }, { key: 'rtype', label: 'type', type: 'enum', options: ['A', 'AAAA', 'CNAME', 'MX', 'TXT', 'SRV', 'CAA', 'NS'] },
        { key: 'value', label: 'wanted', cell: r => h('span', { class: 'cut', title: r.value }, r.value) },
        { key: 'found', label: 'found', value: r => (r.found || []).join(' '), cell: r => h('span', { class: 'cut', title: (r.found || []).join('\n') }, (r.found || []).join(', ') || '—') },
        { key: 'why', label: 'for' },
      ],
    });
    // Round 18: a Stalwart per mail instance
    const boxes = (v.stalwarts || []).map(x => {
      const st = x.state || {};
      return win(`Stalwart ${x.instance} (a mail server)`, h('dl', { class: 'kv' },
        h('dt', {}, 'node'), h('dd', {}, x.node || '—'),
        h('dt', {}, 'its pages'), h('dd', {}, x.host ? stalwartLinks(x.host) : '—'),
        h('dt', {}, 'running'), h('dd', {}, st.active ? h('span', { class: st.active === 'active' ? 'ok' : 'bad' }, st.active) : '—'),
        h('dt', {}, 'version'), h('dd', {}, st.version || '—'),
        h('dt', {}, 'its data'), h('dd', {}, st.data_bytes != null ? fmtBytes(st.data_bytes) + ' in /opt/stalwart/data' : '—'),
        h('dt', {}, 'domains'), h('dd', {}, st.dns ? Object.keys(st.dns).length + ' (their records are in the check above)' : (st.error || 'not known yet')),
        h('dt', {}, 'last asked'), h('dd', {}, st.at ? age(st.at * 1000) : '—')),
        h('p', { class: 'dim' }, 'To move it: on the new node install Stalwart and osada (an edge with edge-mail), stop Stalwart, copy /opt/stalwart (data, etc), start it there; then the mail instance\'s node (modules), and DNS: the names above (MX targets, mail\'s A and AAAA). The check above shows what is left.'));
    });
    host.replaceChildren(
      win(`DNS: ${v.total - v.bad} of ${v.total} records as they should be`, ...(v.notes || []).map(n => h('p', { class: 'warn' }, n)), g.node,
        h('div', { class: 'row' },
          h('button', { type: 'button', onclick: () => save('osada-all.zone', v.zone_all) }, 'zone file: all'),
          h('button', { type: 'button', class: v.bad ? 'primary' : null, onclick: () => save('osada-missing.zone', v.zone_missing) }, `zone file: the ${v.bad} not as they should be`)),
        h('p', { class: 'dim' }, 'Names of sites, certificates and proxies at the public addresses of the edges that answer them (node cards); sites behind Cloudflare at Cloudflare\'s; every record each Stalwart wants for its domains. Looked up by DNS over HTTPS, as the world sees them.')),
      ...(boxes.length ? boxes : [win('Stalwart (the mail server)', h('p', { class: 'dim' }, 'no mail instance with a host'))]));
  }).catch(e => host.replaceChildren(h('p', { class: 'bad' }, e.message)));
  return h('div', {}, host);
}

// ---- mail (D-U38): the mail instance's Stalwart: domains, mailboxes and their sizes ----

let mailInstance = null;
function mailTab() {
  const box = h('div', {}, h('p', { class: 'dim' }, 'asking the mail instance…'));
  api('GET', '/api/admin/mail' + (mailInstance ? '?instance=' + encodeURIComponent(mailInstance) : '')).then(v => {
    mailInstances = v.instances || [];
    if (!v.instance) {
      box.replaceChildren(win('mail', h('p', {}, v.why || 'no mail instance'),
        h('p', { class: 'dim' }, 'Mail is Stalwart, on a node (the mail host): install Stalwart there (INSTALL.md: mail), give that node the role edge-mail (nodes), then add a module of kind mail on it (modules) with its host name, e.g. mail.example.org. This tab then manages its domains and mailboxes. Several instances (a Stalwart each, on nodes of their own) spread the load: each OU\'s mailboxes go to the instance of its mail master.')));
      return;
    }
    const inst = v.instance;
    mailInstance = inst.name;
    // Round 18: a Stalwart per instance; each OU's mailboxes on the instance of its mail master
    const pick = mailInstances.length > 1
      ? select(mailInstances.map(i => [i.name, `${i.name} (${i.node_name}${i.host ? ', ' + i.host : ''})`]), inst.name, n => { mailInstance = n; drawAdmin(); })
      : null;
    const byOu = v.result && v.result.instance_by_ou ? Object.entries(v.result.instance_by_ou) : [];
    const head = h('dl', { class: 'kv' },
      h('dt', {}, 'instance'), h('dd', {}, pick || inst.name, mailInstances.length > 1 ? h('span', { class: 'dim' }, ` one of ${mailInstances.length}: each a Stalwart of its own`) : null),
      h('dt', {}, 'node'), h('dd', {}, inst.node_name),
      h('dt', {}, 'host'), h('dd', {}, inst.host || '—'),
      h('dt', {}, 'Stalwart\'s pages'), h('dd', {}, inst.host ? stalwartLinks(inst.host) : '—'),
      mailInstances.length > 1 && byOu.length ? [h('dt', {}, 'new mailboxes'), h('dd', {}, byOu.map(([ou, i], k) => [k ? ', ' : '', h('code', {}, ou), ' → ', i || '—']),
        h('div', { class: 'dim' }, 'An OU\'s mailboxes are made on the instance of its mail master (above: the master node for each OU). A mailbox stays on its instance; moving its mail to another is not built yet.'))] : null);
    if (!v.result) {
      box.replaceChildren(win('mail instance', head, h('p', { class: 'bad' }, v.why),
        h('p', { class: 'dim' }, `On ${inst.node_name}, osada talks to Stalwart at http://127.0.0.1:8080 with the credential in <its node folder>/mail/stalwart.auth (an API key of Stalwart's, or "Basic <base64 of user:password>"), readable by the node's account only.`)));
      return;
    }
    const r = v.result;
    const doms = grid({
      id: 'mail-domains', rows: r.domains, empty: 'no domains yet',
      columns: [
        { key: 'name', label: 'domain' },
        { key: 'enabled', label: 'state', value: d => d.enabled ? 'on' : 'off', cell: d => d.enabled ? h('span', { class: 'ok' }, 'on') : h('span', { class: 'warn' }, 'off') },
        { key: 'catch_all', label: 'catch-all', value: d => d.catch_all || '' },
        { key: 'boxes', label: 'mailboxes', type: 'number', value: d => r.accounts.filter(a => a.domain === d.id).length },
      ],
      onRow: d => domainCard(d),
      extras: [h('button', { type: 'button', onclick: () => addDomain() }, 'add a domain')],
    });
    const pct = a => a.quota ? Math.round(100 * (a.used || 0) / a.quota) : null;
    mailCatchAlls = Object.fromEntries(r.domains.map(d => [d.name, d.catch_all]));
    // osada's mailboxes (D-U43): Stalwart's accounts, osada's or not, and the mailboxes not made yet
    const rows = [...r.accounts, ...(r.pending || []).map(p => ({ email: p.address, user: p.user, managed: true, pending: true, error: p.error, mailbox: p.mailbox }))];
    for (const a of rows) if (a.managed && a.user && r.errors && r.errors[a.user.id]) a.error = r.errors[a.user.id];
    const suggested = r.accounts.filter(a => !a.managed && a.user);
    const boxes = grid({
      id: 'mail-boxes', rows, empty: 'no mailboxes yet',
      columns: [
        { key: 'email', label: 'address' },
        { key: 'user', label: 'osada account', value: a => a.user ? a.user.name : '', cell: a => a.user ? h('a', { href: `/admin/users/${a.user.id}`, 'data-go': true }, a.user.name) : h('span', { class: 'dim' }, '—') },
        { key: 'managed', label: 'kept by', value: a => a.pending ? 'osada, being made' : a.managed ? 'osada' : 'Stalwart only',
          cell: a => a.pending ? h('span', { class: 'warn' }, 'osada, being made') : a.managed ? 'osada' : h('span', { class: 'dim', title: a.user ? `adopt it as ${a.user.name}'s mailbox` : 'no osada account named' }, a.user ? 'Stalwart only (adopt?)' : 'Stalwart only') },
        { key: 'other', label: 'other addresses', value: a => mailAliases(a).join(' '), cell: a => { const l = mailAliases(a); return l.length ? h('span', { class: 'cut', title: l.join('\n') }, l.join(', ')) : ''; } },
        { key: 'used', label: 'used', type: 'number', value: a => a.used || 0, cell: a => a.pending ? '' : fmtBytes(a.used || 0) },
        { key: 'quota', label: 'size', type: 'number', value: a => a.quota || 0, cell: a => a.pending ? '' : a.quota ? fmtBytes(a.quota) : h('span', { class: 'dim' }, 'no limit') },
        { key: 'full', label: 'full', type: 'number', value: a => pct(a) || 0, cell: a => pct(a) === null ? '' : h('span', { class: pct(a) >= 90 ? 'warn' : null }, `${pct(a)} %`) },
        { key: 'state', label: 'state', value: a => a.pending ? '' : a.disabled ? 'off' : 'on', cell: a => a.pending ? '' : a.disabled ? h('span', { class: 'warn' }, 'off') : 'on' },
        { key: 'error', label: 'problem', value: a => a.error || '', cell: a => a.error ? h('span', { class: 'bad' }, a.error) : '' },
      ],
      onRow: a => mailboxCard(a),
      extras: [
        h('button', { type: 'button', onclick: () => newMailbox(r) }, 'new mailbox'),
        suggested.length ? h('button', { type: 'button', title: suggested.map(a => `${a.email} → ${a.user.name}`).join('\n'), onclick: () => confirmModal('adopt mailboxes',
          `Keep these ${suggested.length} accounts as osada's mailboxes of the accounts shown (${suggested.map(a => a.user.name).join(', ')})? Nothing changes in Stalwart; from then on osada keeps them.`,
          async () => { for (const a of suggested) { try { await api('POST', '/api/admin/mail/action', { op: 'mailbox_set', user: a.user.id, address: a.email, quota: a.quota || null, ...(mailInstance ? { instance: mailInstance } : {}) }); } catch (e) { status(`${a.email}: ${e.message}`, 'bad'); return; } } status(`${suggested.length} mailboxes adopted`, 'ok'); drawAdmin(); }) }, `adopt ${suggested.length}`) : null,
        h('button', { type: 'button', title: 'make Stalwart match the mailboxes now (it does once a minute)', onclick: () => mailAct({ op: 'reconcile' }, 'Stalwart matched to the mailboxes') }, 'sync now'),
      ],
    });
    box.replaceChildren(
      win('mail instance', head),
      win('domains (click one for the DNS records it wants, to turn it off or remove it)', doms.node),
      win('mailboxes (click one to set its size, turn it off or remove it)', boxes.node,
        h('p', { class: 'dim' }, 'osada keeps the mailboxes: what you set here (or on a user\'s card) Stalwart is made to match within a minute. Stalwart\'s own accounts that no mailbox names are left alone until adopted. The size is Stalwart\'s disk quota; mail over it is refused. A disabled account\'s mailbox is off.')),
      mailSettingsBox(r));
  }).catch(err => box.replaceChildren(win('mail', h('p', { class: 'bad' }, err.message))));
  return box;
}

let mailInstances = [];
async function mailAct(body, what) {
  // the instance the tab shows (Round 18)
  if (mailInstance && !body.instance && !/^mailbox_|^mail_settings$|^upn_domain$|^reconcile$/.test(body.op)) body = { ...body, instance: mailInstance };
  try { await api('POST', '/api/admin/mail/action', body); status(what, 'ok'); drawAdmin(); }
  catch (err) { status(err.message, 'bad'); }
}

async function mailboxDo(body, what) {
  try {
    const r = await api('POST', '/api/admin/mail/action', body);
    const err = r && r.result === undefined && r.error ? r.error : null;
    if (err) status(`${what}; Stalwart: ${err}`, 'bad'); else status(what, 'ok');
  } catch (e) { status(e.message, 'bad'); return false; }
  if (location.pathname.startsWith('/admin')) drawAdmin();
  return true;
}

function gbInput(bytes) {
  return h('input', { type: 'number', min: 0, step: 0.5, value: bytes ? +(bytes / GiB).toFixed(2) : '', placeholder: 'no limit', style: 'width:8em' });
}

// a mailbox (D-U43), from the mail tab and the user card: its size, on or off, remove; an account of
// Stalwart's alone: adopt it for a user
function mailboxCard(a, after) {
  const done = () => { if (after) after(); };
  if (!a.managed) {
    let pick = a.user ? { id: a.user.id, name: a.user.name } : null;
    const who = h('span', {}, pick ? pick.name : '');
    const m = modal(`${a.email} (Stalwart only)`, h('div', { class: 'stack' },
      h('p', { class: 'dim' }, 'No osada account has this as its mailbox, so osada leaves it alone. Adopt it: it becomes that account\'s mailbox, as it is (nothing changes in Stalwart), and osada keeps it from then on.'),
      h('div', { class: 'row' }, 'for ', who, userPicker(u => { pick = u; who.textContent = u.name; }))), [
      h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
      h('button', { type: 'button', class: 'primary', onclick: async () => { if (!pick) return; m.close(); await mailboxDo({ op: 'mailbox_set', user: pick.id, address: a.email, quota: a.quota || null, ...(mailInstance ? { instance: mailInstance } : {}) }, `${a.email} is ${pick.name}'s mailbox`); done(); } }, 'adopt'),
    ], { cls: 'pickmodal' });
    return;
  }
  const user = a.user.id;
  const size = gbInput(a.quota);
  const off = a.pending ? false : !!a.off;
  const addr = h('input', { value: a.email, spellcheck: false, size: 34 });
  const known = a.mailbox && a.mailbox.aliases;
  const others = h('textarea', { rows: 5, cols: 40, spellcheck: false, placeholder: 'name@example.org\n*@example.org' });
  others.value = mailAliases(a).join('\n');
  if (!known) others.disabled = true;
  const body = h('div', { class: 'stack' },
    a.error ? h('p', { class: 'bad' }, `Stalwart: ${a.error}`) : null,
    a.pending ? h('p', { class: 'warn' }, 'Not in Stalwart yet: osada makes it within a minute (sync now on the mail tab does it at once).') : h('p', { class: 'dim' }, `It uses ${fmtBytes(a.used || 0)}${a.disabled && !off ? '; off while the account is disabled' : ''}.`),
    mailInstances.length > 1 ? h('p', { class: 'dim' }, `On the mail instance ${(a.mailbox && a.mailbox.instance) || mailInstance || '?'}.`) : null,
    h('label', {}, 'address (changing it renames the account in Stalwart; its mail stays)'), addr,
    h('label', {}, 'other addresses, one a line: name@domain, or *@domain for every address of the domain no mailbox has (its catch-all)'), others,
    known ? null : h('p', { class: 'dim' }, 'osada takes this mailbox\'s other addresses and catch-alls from Stalwart at the next sync; then they can be changed here.'),
    h('label', { class: 'row' }, 'size ', size, ' GB (empty or 0: no limit)'));
  const m = modal(`mailbox ${a.email} (${a.user.name})`, body, [
    h('button', { type: 'button', class: 'danger', onclick: () => { m.close(); mailboxRemove(a, done); } }, 'remove…'),
    h('button', { type: 'button', onclick: async () => { m.close(); await mailboxDo({ op: 'mailbox_set', user, off: !off }, `${a.email} ${off ? 'on' : 'off'}`); done(); } }, off ? 'turn on' : 'turn off'),
    h('button', { type: 'button', class: 'primary', onclick: async () => {
      const gb = Math.max(0, +size.value || 0);
      const body = { op: 'mailbox_set', user, address: addr.value.trim().toLowerCase(), quota: gb ? Math.round(gb * GiB) : null };
      if (known) body.aliases = others.value.split(/[\s,;]+/).map(x => x.trim().toLowerCase()).filter(Boolean);
      m.close();
      await mailboxDo(body, `${body.address} saved`);
      done();
    } }, 'save'),
  ], { cls: 'pickmodal' });
  size.focus();
}

// a mailbox's other addresses: osada's when it keeps them, else what Stalwart has (aliases, and *@domain
// for the domains whose catch-all it is)
let mailCatchAlls = {};
function mailAliases(a) {
  if (a.mailbox && a.mailbox.aliases) return a.mailbox.aliases;
  const l = [...(a.aliases || [])];
  for (const [d, to] of Object.entries(mailCatchAlls)) if (to && a.email && to.toLowerCase() === a.email.toLowerCase()) l.push('*@' + d);
  return l;
}

function mailboxRemove(a, done) {
  const typed = h('input', { placeholder: a.email, spellcheck: false, size: 34 });
  const m = modal(`remove the mailbox ${a.email}`, h('div', { class: 'stack' },
    h('p', {}, `Take ${a.email} away from ${a.user.name}:`),
    h('ul', {}, h('li', {}, h('b', {}, 'with its mail'), ': Stalwart\'s account and all its mail are deleted. To do that, type the address:'),
      h('li', {}, h('b', {}, 'keep the account'), ': osada stops keeping it; it stays in Stalwart as it is (Stalwart only).')),
    typed), [
    h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
    h('button', { type: 'button', onclick: async () => { m.close(); await mailboxDo({ op: 'mailbox_remove', user: a.user.id }, `${a.email}: no longer osada's`); done(); } }, 'keep the account'),
    h('button', { type: 'button', class: 'danger', onclick: async () => {
      if (typed.value.trim().toLowerCase() !== a.email.toLowerCase()) { typed.focus(); status('type the address to delete its mail', 'bad'); return; }
      m.close(); await mailboxDo({ op: 'mailbox_remove', user: a.user.id, delete: true }, `${a.email} and its mail removed`); done();
    } }, 'remove with its mail'),
  ], { cls: 'pickmodal' });
}

// a new mailbox: whose, its address (empty: the template's), its size
function newMailbox(r, forUser, after) {
  let pick = forUser || null;
  const who = h('span', {}, pick ? pick.name : '');
  const s = (r && r.settings) || {};
  const addr = h('input', { placeholder: s.template ? `empty: ${s.template}` : 'name@example.org', spellcheck: false, size: 34 });
  const size = gbInput(s.quota);
  // Round 18: on the instance of the OU's mail master unless picked
  let on = '';
  const onPick = mailInstances.length > 1 ? select([['', 'the one for its OU'], ...mailInstances.map(i => [i.name, `${i.name} (${i.node_name})`])], '', v => { on = v; }) : null;
  const m = modal('a new mailbox', h('div', { class: 'stack' },
    forUser ? h('p', {}, `for ${forUser.name}`) : h('div', { class: 'row' }, 'for ', who, userPicker(u => { pick = u; who.textContent = u.name; })),
    h('label', {}, 'address'), addr,
    onPick ? [h('label', {}, 'on the mail instance'), onPick] : null,
    h('label', { class: 'row' }, 'size ', size, ' GB (empty or 0: no limit)'),
    h('p', { class: 'dim' }, s.make_domains ? 'A domain Stalwart lacks is made with it, the mailbox its catch-all.' : 'Its domain must be one of the domains above.')), [
    h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
    h('button', { type: 'button', class: 'primary', onclick: async () => {
      if (!pick) { status('whose mailbox?', 'bad'); return; }
      const gb = Math.max(0, +size.value || 0);
      m.close();
      await mailboxDo({ op: 'mailbox_set', user: pick.id, address: addr.value.trim().toLowerCase() || undefined, quota: gb ? Math.round(gb * GiB) : null, ...(on ? { instance: on } : {}) }, `a mailbox for ${pick.name}`);
      if (after) after();
    } }, 'make it'),
  ], { cls: 'pickmodal' });
  addr.focus();
}

// how new mailboxes are made (D-U43)
function mailSettingsBox(r) {
  const s = r.settings || {};
  const template = h('input', { value: s.template || '', placeholder: '{user}@example.org', spellcheck: false, size: 40 });
  let make = !!s.make_domains, auto = !!s.auto;
  const size = gbInput(s.quota);
  const example = h('span', { class: 'dim' }, r.example ? `anna gets ${r.example}` : 'no template: every new mailbox needs its address');
  return win('new mailboxes', h('div', { class: 'stack' },
    h('label', {}, 'address ({user}: the account\'s name)'), h('div', { class: 'row' }, template, example),
    h('label', { class: 'row' }, check(make, v => { make = v; }), ' make the address\'s domain when Stalwart lacks it (the mailbox its catch-all)'),
    h('label', { class: 'row' }, 'size ', size, ' GB (empty: no limit)'),
    h('label', { class: 'row' }, check(auto, v => { auto = v; }), ' every new account gets a mailbox'),
    h('div', { class: 'row' }, h('button', { type: 'button', class: 'primary', onclick: () => {
      const gb = Math.max(0, +size.value || 0);
      mailAct({ op: 'mail_settings', settings: { template: template.value.trim(), make_domains: make, quota: gb ? Math.round(gb * GiB) : null, auto } }, 'saved');
    } }, 'save'))));
}

function domainCard(d) {
  const m = modal(`domain ${d.name}`, h('div', { class: 'stack' },
    h('p', { class: 'dim' }, 'The DNS records Stalwart wants for it (DKIM keys, SPF, DMARC, MX…): put them in its zone; admin → dns shows what DNS says.'),
    h('pre', { class: 'zone', style: 'max-height:24em;overflow:auto;white-space:pre-wrap' }, d.dns || '—')), [
    h('button', { type: 'button', class: 'danger', onclick: () => { m.close(); confirmModal('remove domain', `Remove ${d.name} from Stalwart? Its mailboxes must go first; mail for it is then refused.`, () => mailAct({ op: 'domain_remove', id: d.id }, `${d.name} removed`)); } }, 'remove'),
    h('button', { type: 'button', onclick: () => { m.close(); mailAct({ op: 'domain_enable', id: d.id, on: !d.enabled }, `${d.name} ${d.enabled ? 'off' : 'on'}`); } }, d.enabled ? 'turn off' : 'turn on'),
    h('button', { type: 'button', class: 'primary', onclick: () => m.close() }, 'close'),
  ], { cls: 'pickmodal' });
}

function addDomain() {
  const input = h('input', { placeholder: 'example.org', spellcheck: false, size: 30 });
  const m = modal('a new mail domain', h('div', { class: 'stack' },
    h('p', { class: 'dim' }, 'Stalwart makes its DKIM keys; the DNS records to publish are on the domain\'s card afterwards.'), input), [
    h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
    h('button', { type: 'button', class: 'primary', onclick: () => { const n = input.value.trim().toLowerCase(); if (!n) return; m.close(); mailAct({ op: 'domain_add', name: n }, `${n} added`); } }, 'add'),
  ], { cls: 'pickmodal' });
  input.focus();
}

// the user card's mailbox (D-U38, D-U43): its address, use, size and state; make one, change it
function mailboxOf(u, may) {
  const dd = h('dd', {}, h('span', { class: 'dim' }, '…'));
  const again = () => { dd.replaceChildren(h('span', { class: 'dim' }, '…')); api('GET', `/api/admin/users/${u.id}`).then(c => { u = c.user || u; draw(); }).catch(draw); };
  const draw = () => api('GET', '/api/admin/mail?user=' + encodeURIComponent(u.id)).then(v => {
    mailInstances = v.instances || mailInstances;
    if (v.instance) mailInstance = v.instance.name;
    const r = v.result || {};
    const mine = u.mailbox;
    const a = (r.accounts || []).find(x => mine ? (x.email || '').toLowerCase() === mine.address : x.user && x.user.id === u.id);
    if (!mine) {
      dd.replaceChildren(h('span', { class: 'dim' }, !v.instance ? 'no mail instance' : a ? `none of osada's; Stalwart has ${a.email} ` : 'none '),
        may && v.instance && a ? h('button', { type: 'button', id: 'mailbox-adopt', onclick: () => mailboxCard(a, again) }, 'adopt it') : null,
        may && v.instance && !a ? h('button', { type: 'button', id: 'mailbox-make', onclick: () => newMailbox(r, { id: u.id, name: u.name }, again) }, 'make one') : null);
      return;
    }
    mailCatchAlls = Object.fromEntries((r.domains || []).map(d => [d.name, d.catch_all]));
    const row = a ? { ...a, user: { id: u.id, name: u.name }, managed: true, off: mine.off, mailbox: mine } : { email: mine.address, user: { id: u.id, name: u.name }, managed: true, pending: true, error: (r.errors || {})[u.id], mailbox: mine };
    const more = mailAliases(row).length;
    dd.replaceChildren(h('span', { id: 'mailbox-of' }, a ? `${a.email}${more ? ` (+${more} other)` : ''} · ${fmtBytes(a.used || 0)} of ${a.quota ? fmtBytes(a.quota) : 'no limit'}${a.disabled ? ' · off' : ''} ` : `${mine.address} · being made${row.error ? ': ' + row.error : ''} `),
      may ? h('button', { type: 'button', id: 'mailbox-size', onclick: () => mailboxCard(row, again) }, 'change') : null);
  }).catch(() => dd.replaceChildren(h('span', { class: 'dim' }, u.mailbox ? u.mailbox.address : '—')));
  draw();
  return dd;
}

// ---- nodes ----

function nodesTab() {
  const d = admin.draft, ov = admin.ov;
  let info = {};
  const pct = (a, b) => b ? Math.round(100 * a / b) : 0;
  const usedFor = id => {
    const sitesHere = Object.entries(d.modules || {}).filter(([, m]) => m.kind === 'web' && m.node === id).map(([n]) => n);
    const exposed = Object.entries(d.modules || {}).filter(([, m]) => m.kind === 'web' && m.site && (m.site.exposed_on || []).includes(id)).map(([n]) => n);
    const certs = Object.entries(d.certs || {}).filter(([, c]) => c.issuer === id).map(([n]) => n);
    const mods = Object.entries(d.modules || {}).filter(([, m]) => m.kind !== 'web' && m.node === id).map(([n, m]) => `${n} (${m.kind})`);
    const auth = DOMAINS.filter(x => d.defaults[x] === id);
    return [auth.length ? 'authority: ' + auth.join(', ') : null, mods.length ? 'modules: ' + mods.join(', ') : null,
      sitesHere.length ? 'runs: ' + sitesHere.join(', ') : null, exposed.length ? 'exposes: ' + exposed.join(', ') : null,
      certs.length ? 'issues: ' + certs.join(', ') : null].filter(Boolean).join(' · ') || '—';
  };
  const rows = () => nodeNames().map(id => ({ id, name: nodeLabel(id), n: d.nodes[id], i: info[id] }));
  // the node the browser is connected to (an edge relaying to this one, or this one)
  const entry = ov.via || ov.me;
  // the round trip from the node the browser is connected to (Round 19): the answering node's own
  // pings, or what the entry node reports of its links (nodes/info); undefined: not known yet
  const rttFrom = id => {
    if (entry === ov.me) return ov.links[id] !== undefined ? ov.links[id] : null;
    const e = info[entry];
    if (!e) return undefined;
    const r = (e.link_rates || {})[id];
    return r && r.live !== false && r.rtt_ms != null ? r.rtt_ms : null;
  };
  // the speed of the link from there to that node and back (Mbit/s, measured or seen in traffic)
  const speedFrom = id => {
    const best = (a, b) => { const m = (((info[b] || {}).links || {}).peers || {})[a]; return m ? (m.best || m.mbit_in || 0) : 0; };
    const out = best(entry, id), back = best(id, entry);
    return out || back ? `≈ ${out ? fmtMbit(out) : '?'} / ${back ? fmtMbit(back) : '?'} Mbit/s (${nodeLabel(entry)} → ${nodeLabel(id)} / back)` : '';
  };
  const linkText = r => {
    if (r.id === entry) return r.id === ov.me ? 'you are connected here · answers' : 'you are connected here';
    const ms = rttFrom(r.id);
    return [ms === undefined ? '…' : ms === null ? 'no link' : `${ms.toFixed(2)} ms`, r.id === ov.me ? 'answers these pages' : null].filter(Boolean).join(' · ');
  };
  const where = ov.via
    ? `You are connected to ${nodeLabel(ov.via)}, which relays to ${nodeLabel(ov.me)}; ${nodeLabel(ov.me)} answers these pages. The link times are ${nodeLabel(ov.via)}'s.`
    : `You are connected to ${nodeLabel(ov.me)}, which answers these pages; the link times are its own.`;
  const nodes = grid({
    id: 'nodes',
    columns: [
      { key: 'name', label: 'node', cell: r => h('span', {}, h('b', {}, r.name), ov.voter === r.id ? h('span', { class: 'pill on', title: 'the IdP master of /: the master of everything in the background' }, ' IdP /') : null, h('div', { class: 'dim', style: 'font-size:smaller' }, r.id)) },
      { key: 'link', label: `link (from ${nodeLabel(entry)})`, value: linkText, cell: r => {
        if (r.id === entry) return h('span', {}, h('b', {}, 'you are connected here'), r.id === ov.me ? h('span', { class: 'dim' }, ' · answers') : null);
        const v = rttFrom(r.id), sp = speedFrom(r.id);
        const ms = v === undefined ? h('span', { class: 'dim' }, '…') : v === null ? h('span', { class: 'bad' }, 'no link') : h('span', { class: 'ok', title: sp || null }, `${v.toFixed(2)} ms`);
        return r.id === ov.me ? h('span', {}, ms, h('span', { class: 'dim' }, ' · answers these pages')) : ms;
      } },
      { key: 'modules', label: 'modules', value: r => modulesOf(r.n).map(m => m.label).join(' '), cell: r => h('span', { class: 'row wrap' }, modulesOf(r.n).map(m => h('span', { class: 'pill on' }, m.label))) },
      { key: 'used', label: 'used for', value: r => usedFor(r.id), cell: r => h('span', { class: 'cut', title: usedFor(r.id) }, usedFor(r.id)) },
      { key: 'storage', label: 'storage', type: 'number', value: r => r.i && r.i.storage ? pct(r.i.storage.size - r.i.storage.free, r.i.storage.size) : null,
        cell: r => !r.i ? h('span', { class: 'dim' }, '…') : r.i.error ? h('span', { class: 'bad', title: r.i.error }, 'no answer') : !r.i.storage ? '—'
          : h('span', { class: pct(r.i.storage.size - r.i.storage.free, r.i.storage.size) > 85 ? 'warn' : null }, `${fmtBytes(r.i.storage.size - r.i.storage.free)} of ${fmtBytes(r.i.storage.size)}`) },
      // a node that cannot read its load or memory (home's SELinux domain did not allow it) shows a dash
      { key: 'cpu', label: 'CPU', type: 'number', value: r => r.i && r.i.load && r.i.load.length ? r.i.load[0] / (r.i.cpus || 1) : null,
        cell: r => !r.i || r.i.error ? '' : r.i.load && r.i.load.length ? h('span', { class: r.i.load[0] > r.i.cpus ? 'warn' : null }, `${r.i.load[0].toFixed(2)} / ${r.i.cpus}`) : h('span', { class: 'dim', title: 'the node could not read its load' }, '—') },
      { key: 'ram', label: 'RAM', type: 'number', value: r => r.i && r.i.mem && r.i.mem.total ? pct(r.i.mem.total - r.i.mem.available, r.i.mem.total) : null,
        cell: r => !r.i || r.i.error ? '' : r.i.mem && r.i.mem.total ? `${fmtBytes(r.i.mem.total - r.i.mem.available)} of ${fmtBytes(r.i.mem.total)}` : h('span', { class: 'dim', title: 'the node could not read its memory' }, '—') },
      { key: 'exposed', label: 'exposed', value: r => exposedText(r.i), cell: r => {
        const t = exposedText(r.i), others = exposedOpen(r.i).filter(x => !x.osada);
        return h('span', { class: others.length ? 'warn cut' : 'cut', title: t }, t);
      } },
      { key: 'version', label: 'version', value: r => (r.i && r.i.version) || '' },
      { key: 'agent', label: 'agent', value: r => r.i && r.i.agent && r.i.agent.running ? 'yes' : 'no', cell: r => r.i && r.i.agent && r.i.agent.running ? h('span', { class: 'ok' }, 'root agent') : h('span', { class: 'dim' }, '—') },
    ],
    rows: rows(), onRow: r => nodeCard(r.id, info[r.id]),
  });
  // certificates (D-U26): node control's; a site with a certificate of its own has one implicitly (web)
  const certRows = () => Object.entries(d.certs || {}).map(([name, c]) => ({ name, c }));
  const certState = r => {
    if (!Object.keys(info).length) return '…';
    const want = [r.c.issuer, ...(r.c.copies || []).filter(x => x !== r.c.issuer)];
    const has = id => info[id] && info[id].certs && info[id].certs[r.name];
    const mine = has(r.c.issuer);
    if (!mine) return 'not issued yet (the issuer tries within a minute of saving; DNS must point at the edges)';
    const at = new Date((mine.issued || 0) * 1000);
    return `issued ${isNaN(at) ? '?' : at.toISOString().slice(0, 10)}, on ${want.filter(has).length} of ${want.length} nodes`;
  };
  const certs = grid({
    id: 'certs', empty: 'none yet: add one for the names the edges answer (osada\'s own, e.g. osada.example.org)',
    columns: [
      { key: 'name', label: 'certificate' },
      { key: 'names', label: 'host names', value: r => r.c.names.join(' ') },
      { key: 'issuer', label: 'issuer (runs ACME)', value: r => nodeLabel(r.c.issuer) },
      { key: 'copies', label: 'copies', value: r => (r.c.copies || []).map(nodeLabel).join(', ') },
      { key: 'state', label: 'state', value: certState, cell: r => { const t = certState(r); return h('span', { class: t.startsWith('issued') ? 'ok' : 'dim', title: t }, t); } },
    ],
    rows: certRows(), onRow: r => certCard(r.name),
    extras: [h('button', { type: 'button', onclick: () => certCard(null) }, 'add a certificate')],
  });
  const map = h('div', { id: 'node-map', class: 'node-map' }, h('p', { class: 'dim' }, 'asking the nodes…'));
  // what the nodes say; a part that cannot be drawn says why instead of leaving the cells empty
  const drawInfo = v => {
    info = v;
    for (const [what, f] of [['nodes', () => nodes.refresh(rows())], ['certificates', () => certs.refresh(certRows())], ['map', () => nodeMap(map, info, ov)]]) {
      try { f(); } catch (e) { console.error(e); status(`${what}: ${e.message}`, 'bad'); }
    }
  };
  api('GET', '/api/admin/nodes/info').then(drawInfo).catch(e => status(`the nodes' state: ${e.message}`, 'bad'));
  const defaults = grid({
    id: 'defaults',
    columns: [
      { key: 'domain', label: 'domain' },
      { key: 'node', label: 'default authority', value: r => d.defaults[r.domain] || '', cell: r => nodePicker(nodeNames(), d.defaults[r.domain] || '', row => {
        if (row) d.defaults[r.domain] = row.name; else delete d.defaults[r.domain];
        markDirty();
      }, '— none —').node },
    ],
    rows: DOMAINS.map(domain => ({ domain })),
  });
  const secrets = h('div', { class: 'row' }, nodeNames().map(name => h('label', { class: 'row' },
    check(d.default_secrets.includes(name), on => {
      d.default_secrets = on ? [...d.default_secrets, name].sort() : d.default_secrets.filter(x => x !== name);
      markDirty();
    }), nodeLabel(name))));
  const again = h('button', { type: 'button', onclick: () => api('GET', '/api/admin/nodes/info').then(drawInfo).catch(e => status(e.message, 'bad')) }, 'again');
  return h('div', {},
    win('nodes (click one for its card: name, roles, rules, addresses, disks)', h('p', { class: 'dim', id: 'you-are-here' }, where), nodes.node),
    win('map: the nodes and their links (round trip; what each carries, the last minute and the last 10 minutes)', h('div', { class: 'row' }, again,
      h('span', { class: 'dim' }, 'QUIC links: round trip, measured speed (the first node → the second / back, as the link\'s card says), what it carries now. A line is thicker the more it carries; a node that does not answer is red. Drag nodes about; the wheel zooms.')), map),
    win('certificates for the public names (one node runs ACME for each, Let\'s Encrypt; the others get copies)', certs.node,
      h('p', { class: 'dim' }, 'For the names the edges answer: osada\'s own (its pages, sign-in, files) and proxies. A site with a certificate of its own (web) needs none here. The names\' DNS must point at the edges (dns) before the issuer can get it.')),
    win('where secrets are kept', h('p', {}, 'Every node with IdP keeps the secrets (password hashes, second factors): ', h('b', { id: 'secrets-on' }, (d.default_secrets || []).map(nodeLabel).join(', ') || 'none'), '.'),
      h('p', { class: 'dim' }, 'Give a node IdP (its card) and it keeps them, signs people in and signs their tokens, also while the others are away; take IdP off and it erases its copy. An OU may name its own (IdP tab: tier 0 on a few nodes). Which node is the master of each module for each OU: the IdP tab and each module\'s tab.')));
}

// ---- what a node exposes (D-U35) ----------------------------------------------------

// the ports a peer could reach at its public addresses, with what listens there
function exposedOpen(i) {
  const e = i && i.exposure;
  if (!e || !e.probe || !e.probe.results) return [];
  const by = {};
  for (const r of e.probe.results) if (r.open) (by[r.port] = by[r.port] || []).push(r.addr);
  return Object.entries(by).map(([port, addrs]) => {
    const s = (e.listening || []).find(x => x.proto === 'tcp' && x.port === +port) || {};
    return { port: +port, addrs, process: s.process || s.by, osada: !!s.osada || !!s.by };
  }).sort((a, b) => a.port - b.port);
}

function exposedText(i) {
  const e = i && i.exposure;
  if (!e) return i && i.error ? '' : '…';
  if (!e.probe) return 'not checked yet';
  if (e.probe.error) return `not checked: ${e.probe.error}`;
  const open = exposedOpen(i);
  if (!open.length) return 'nothing reachable';
  const mine = open.filter(x => x.osada).map(x => x.port), others = open.filter(x => !x.osada).map(x => `${x.port}${x.process ? ' ' + x.process : ''}`);
  return [mine.length ? `osada: ${mine.join(', ')}` : null, others.length ? `others: ${others.join(', ')}` : null].filter(Boolean).join(' · ');
}

function exposureBox(id, i) {
  const box = h('div', { class: 'stack' });
  const draw = i => {
    const e = i && i.exposure;
    if (!e) { box.replaceChildren(h('p', { class: 'dim' }, i && i.error ? 'the node does not answer' : '…')); return; }
    const p = e.probe;
    const reach = (proto, port) => {
      if (proto !== 'tcp') return null;
      if (!p || !p.results) return undefined;
      const r = p.results.filter(x => x.port === port);
      return r.length ? r.filter(x => x.open).map(x => x.addr) : undefined;
    };
    const rows = (e.listening || []).map(s => ({ ...s, reach: reach(s.proto, s.port) }));
    const g = grid({
      id: 'node-exposure', rows, empty: 'nothing listens beyond loopback',
      columns: [
        { key: 'proto', label: 'proto' },
        { key: 'port', label: 'port', type: 'number' },
        { key: 'addr', label: 'listens on' },
        { key: 'process', label: 'process', value: s => s.process || '', cell: s => s.process || (s.by ? 'stalwart' : h('span', { class: 'dim', title: 'another account\'s process: osada may not look' }, '?')) },
        { key: 'osada', label: 'managed', value: s => s.osada ? 'osada' : s.by || 'not managed', cell: s => s.osada ? h('span', { class: 'ok' }, 'osada') : s.by ? h('span', { class: 'ok', title: 'run by osada\'s mail instance on this node' }, s.by) : h('span', { class: 'dim' }, 'not managed') },
        { key: 'reach', label: 'from the internet', value: s => s.reach === null ? 'UDP: not checked' : s.reach === undefined ? 'not checked' : s.reach.length ? 'reachable' : 'not reachable',
          cell: s => s.reach === null ? h('span', { class: 'dim' }, s.osada ? 'the link (QUIC)' : 'UDP: not checked')
            : s.reach === undefined ? h('span', { class: 'dim' }, 'not checked') : s.reach.length ? h('span', { class: s.osada || s.by ? 'ok' : 'warn', title: s.reach.join(', ') }, `reachable at ${s.reach.join(', ')}`) : h('span', { class: 'dim' }, 'not reachable') },
      ],
    });
    const when = !p ? 'Not checked yet: a peer checks every hour (the first 90 s after a start).'
      : p.error ? `Last check ${new Date(p.at).toLocaleString()}: ${p.error}`
      : `Checked ${new Date(p.at).toLocaleString()} from ${nodeLabel(p.by)}: TCP connections to ${[...new Set(p.results.map(r => r.addr))].join(', ') || 'no public address'}; what a firewall drops is not reachable.`;
    box.replaceChildren(h('p', { class: 'dim' }, `Public addresses: ${(e.public || []).join(', ') || 'none on its interfaces or its card'}. ${when}`), g.node,
      h('div', { class: 'row' }, h('button', { type: 'button', onclick: async ev => {
        ev.target.disabled = true;
        try { const v = await api('POST', '/api/admin/nodes/exposure', { node: id }); draw({ exposure: v.exposure }); status('checked', 'ok'); }
        catch (err) { status(err.message, 'bad'); }
        ev.target.disabled = false;
      } }, 'check now')));
  };
  draw(i);
  return box;
}

// ---- the node map (D-U34) ---------------------------------------------------------

let cytoscapeLoad = null;
function cytoscapeReady() {
  if (window.cytoscape) return Promise.resolve(window.cytoscape);
  if (!cytoscapeLoad) {
    const src = (document.querySelector('meta[name="osada-cytoscape"]') || {}).content;
    cytoscapeLoad = new Promise((ok, bad) => {
      const s = document.createElement('script');
      s.src = src;
      s.onload = () => ok(window.cytoscape);
      s.onerror = () => { cytoscapeLoad = null; bad(new Error('the map\'s script did not load')); };
      document.head.append(s);
    });
  }
  return cytoscapeLoad;
}

function fmtRate(bps) {
  if (bps === null || bps === undefined) return '…';
  const bits = bps * 8;
  return bits >= 1e6 ? `${(bits / 1e6).toFixed(1)} Mbit/s` : bits >= 1e3 ? `${(bits / 1e3).toFixed(1)} kbit/s` : `${Math.round(bits)} bit/s`;
}

// the links as the nodes report them: one line a pair, from what either end counts (the end that
// sent it is the one whose tx it is)
function nodeLinks(info) {
  const pairs = {};
  for (const [a, i] of Object.entries(info)) {
    for (const [b, r] of Object.entries((i && i.link_rates) || {})) {
      const key = [a, b].sort().join(' ');
      const p = pairs[key] || (pairs[key] = { a: [a, b].sort()[0], b: [a, b].sort()[1], rtt: [], ab: {}, ba: {} });
      p.rtt.push(r.rtt_ms);
      // a's tx goes a → b; a's rx came b → a
      const fwd = a === p.a ? 'ab' : 'ba', back = a === p.a ? 'ba' : 'ab';
      for (const w of ['1m', '10m']) {
        if (r['tx_' + w] != null) p[fwd][w] = Math.max(p[fwd][w] || 0, r['tx_' + w]);
        if (r['rx_' + w] != null) p[back][w] = Math.max(p[back][w] || 0, r['rx_' + w]);
      }
    }
  }
  // the measured speed each way (D-U40: a test a week and background fills): what b measured
  // coming from a is a → b
  for (const p of Object.values(pairs)) {
    const m = (from, to) => (((info[to] || {}).links || {}).peers || {})[from];
    p.speed = { ab: m(p.a, p.b), ba: m(p.b, p.a) };
  }
  return Object.values(pairs);
}

function fmtMbit(x) {
  return x >= 100 ? `${Math.round(x)}` : x >= 10 ? x.toFixed(0) : x.toFixed(1);
}

let nodeMapCy = null;
async function nodeMap(box, info, ov) {
  let cy;
  try { cy = await cytoscapeReady(); } catch (e) { box.replaceChildren(h('p', { class: 'bad' }, e.message)); return; }
  // one map at a time; none drawn into a page that is gone
  if (nodeMapCy) { try { nodeMapCy.destroy(); } catch (e) { /* gone */ } nodeMapCy = null; }
  if (!box.isConnected) return;
  const links = nodeLinks(info);
  const css = getComputedStyle(document.documentElement);
  const c = (v, d) => (css.getPropertyValue(v) || '').trim() || d;
  const fg = c('--fg', '#ddd'), dim = c('--fg-dim', '#888'), accent = c('--accent', '#6c6'), bad = c('--danger', '#d55'), panel = c('--panel', '#222');
  // a node's label in lines of at most MAP_COLS characters (its modules wrapped, never one long
  // line over the box's edges), the box sized to it (Round 19)
  const MAP_COLS = 24;
  const cut = t => t.length > MAP_COLS ? t.slice(0, MAP_COLS - 1) + '…' : t;
  const wrapList = (words, sep) => {
    const out = [];
    let cur = '';
    for (const w of words) {
      if (cur && (cur + sep + w).length > MAP_COLS) { out.push(cur + sep.trimEnd()); cur = w; } else cur = cur ? cur + sep + w : w;
    }
    if (cur) out.push(cur);
    return out.map(cut);
  };
  const nodes = nodeNames().map(id => {
    const i = info[id] || {};
    const up = i.links && i.links.tx_peak_mbit ? `uplink ≈ ${fmtMbit(i.links.tx_peak_mbit)} Mbit/s` : '';
    const mods = modulesOf(admin.draft.nodes[id] || { roles: [], rules: {} }).map(m => m.label);
    const lines = [cut(`${nodeLabel(id)}${ov.voter === id ? ' (IdP /)' : ''}`), cut(i.error ? 'no answer' : i.host || ''), ...wrapList(mods, ', '), up ? cut(up) : null].filter(x => x !== null && x !== '');
    const cols = Math.max(...lines.map(l => l.length), 10);
    return { data: { id, label: lines.join('\n'), mods: mods.join(', ') || 'none', down: i.error ? 1 : 0, here: id === (ov.via || ov.me) ? 1 : 0,
      bw: Math.round(cols * 6.8 + 26), bh: Math.round(lines.length * 14 + 18) } };
  });
  const known = new Set(nodeNames());
  const edges = links.filter(l => known.has(l.a) && known.has(l.b)).map(l => {
    const rtt = l.rtt.length ? Math.min(...l.rtt) : null;
    const one = (l.ab['1m'] || 0) + (l.ba['1m'] || 0);
    const line = (from, to, x) => `${nodeLabel(from)} → ${nodeLabel(to)}: ${fmtRate(x['1m'])} (10 min: ${fmtRate(x['10m'])})`;
    const sp = (from, to, m) => m && (m.best || m.mbit_in) > 0
      ? `${nodeLabel(from)} → ${nodeLabel(to)}: ≈ ${fmtMbit(m.best || m.mbit_in)} Mbit/s (${m.mbit_in > 0 ? `measured ${fmtMbit(m.mbit_in)}, ${m.tested ? 'tested ' + new Date(m.tested).toLocaleDateString() : 'from transfers'}` : 'not tested'}${m.seen_in > 0 ? `; seen in traffic ${fmtMbit(m.seen_in)} on ${new Date(m.seen_at).toLocaleString()}` : ''})`
      : `${nodeLabel(from)} → ${nodeLabel(to)}: speed not measured yet (a test runs 5-35 min after a start, then weekly)`;
    // a link carries at least what it carries now (Round 19): the measured speed, or more when
    // traffic (the node's best seen, or this minute) shows more
    const best = (m, x) => Math.max(m ? (m.best || m.mbit_in || 0) : 0, x && x['1m'] ? x['1m'] * 8 / 1e6 : 0);
    const speeds = [best(l.speed.ab, l.ab), best(l.speed.ba, l.ba)].map(x => x > 0 ? fmtMbit(x) : '?');
    const speedLabel = speeds.every(x => x === '?') ? '' : `≈ ${speeds[0]} / ${speeds[1]} Mbit/s`;
    return { data: { id: `${l.a}~${l.b}`, source: l.a, target: l.b, w: Math.max(1.5, Math.min(12, 1.5 + Math.log10(1 + one * 8 / 1000) * 2.5)),
      label: [rtt === null ? '' : rtt.toFixed(1) + ' ms', speedLabel, `↔ ${fmtRate(one)}`].filter(Boolean).join('\n'),
      title: [rtt === null ? null : `round trip ${rtt.toFixed(2)} ms (QUIC)`, 'speed (measured):', sp(l.a, l.b, l.speed.ab), sp(l.b, l.a, l.speed.ba),
        'now:', line(l.a, l.b, l.ab), line(l.b, l.a, l.ba)].filter(Boolean).join('\n') } };
  });
  const area = h('div', { class: 'node-map-area' });
  const tip = h('pre', { class: 'node-map-tip dim' }, 'Click a link for what it carries each way.');
  box.replaceChildren(area, tip);
  const g = nodeMapCy = cy({
    container: area,
    elements: [...nodes, ...edges],
    // room for the boxes and the links' labels between them
    layout: nodes.length > 2 ? { name: 'circle', padding: 30, avoidOverlap: true, nodeDimensionsIncludeLabels: true, spacingFactor: 1.35 }
      : { name: 'grid', padding: 30, avoidOverlap: true, avoidOverlapPadding: 120, nodeDimensionsIncludeLabels: true },
    // Round 24: a mouse wheel's notch zooms noticeably (0.3 was slow with many nodes)
    wheelSensitivity: 1.5,
    style: [
      { selector: 'node', style: { shape: 'round-rectangle', width: 'data(bw)', height: 'data(bh)', 'background-color': panel, 'border-width': 2, 'border-color': accent,
        label: 'data(label)', 'text-wrap': 'wrap', 'text-max-width': 240, 'text-valign': 'center', 'text-halign': 'center', color: fg, 'font-size': 11, 'font-family': 'monospace' } },
      { selector: 'node[down = 1]', style: { 'border-color': bad, color: bad } },
      { selector: 'node[here = 1]', style: { 'border-width': 4 } },
      { selector: 'edge', style: { width: 'data(w)', 'line-color': dim, 'curve-style': 'bezier', label: 'data(label)', 'text-wrap': 'wrap', 'font-size': 10,
        color: fg, 'text-background-color': panel, 'text-background-opacity': 0.85, 'text-background-padding': 2, 'font-family': 'monospace' } },
      { selector: 'edge:selected', style: { 'line-color': accent } },
    ],
  });
  g.on('tap', 'edge', e => { tip.textContent = e.target.data('title'); });
  // the link clicked (selected): its numbers readable at any zoom (Round 24)
  const readable = () => {
    const px = Math.max(10, Math.round(13 / g.zoom()));
    g.edges().forEach(x => x.selected() ? x.style({ 'font-size': px, 'text-background-opacity': 1, 'z-index': 10 }) : x.removeStyle('font-size text-background-opacity z-index'));
  };
  g.on('select unselect', 'edge', readable);
  g.on('zoom', () => { if (g.edges(':selected').length) readable(); });
  g.on('tap', 'node', e => { const i = info[e.target.id()] || {}; tip.textContent = `${nodeLabel(e.target.id())}: ${i.error ? 'no answer: ' + i.error : `${i.host || ''}, osada ${i.version || '?'}, load ${i.load && i.load.length ? i.load[0].toFixed(2) : '?'} / ${i.cpus || '?'} CPUs`}\nmodules: ${e.target.data('mods')}`; });
  if (!edges.length) tip.textContent = 'No link has been counted yet: the nodes sample their links every 10 s.';
}

// a certificate of node control (D-U26): its host names, the node that runs ACME for it, the copies
function certCard(name) {
  const d = admin.draft;
  d.certs = d.certs || {};
  const edges = nodeNames().filter(id => (d.nodes[id].roles || []).includes('edge-http'));
  const c = name ? structuredClone(d.certs[name]) : { names: [], issuer: edges[0] || '', copies: edges.slice(1) };
  let free = 'osada';
  for (let i = 2; d.certs[free]; i++) free = `cert-${i}`;
  const nameInp = h('input', { value: name || free, maxlength: 63, pattern: '[a-z0-9\\-]+', disabled: !!name });
  const names = h('textarea', { rows: 3, spellcheck: false, placeholder: 'osada.example.org\napptab.example.org' });
  names.value = c.names.join('\n');
  let issuer = c.issuer;
  const copies = new Set(c.copies || []);
  const issuerPick = nodePicker(edges.length ? edges : nodeNames(), issuer, row => { issuer = row ? row.name : ''; }, 'the node that runs ACME');
  const copyBoxes = h('span', { class: 'row wrap' }, nodeNames().map(id => h('label', { class: 'row' }, check(copies.has(id), on => { if (on) copies.add(id); else copies.delete(id); }), nodeLabel(id))));
  const msg = h('div', { class: 'msg' });
  const box = modal(name ? `certificate ${name}` : 'a new certificate', h('div', { class: 'stack' },
    h('span', { class: 'lbl' }, 'name (a-z, 0-9, -)'), nameInp,
    h('span', { class: 'lbl' }, 'host names, one a line (the first is its subject)'), names,
    h('span', { class: 'lbl' }, 'issuer: an edge whose plain http (port 80) the names reach; it runs ACME (http-01)'), issuerPick.node,
    h('span', { class: 'lbl' }, 'copies: every other edge that answers these names (they ask the issuer for ACME\'s challenges too)'), copyBoxes,
    msg, h('p', { class: 'dim' }, 'Changes here go into node control: save it after.')), [
    name ? h('button', { type: 'button', class: 'danger', onclick: () => { delete d.certs[name]; box.close(); markDirty(); drawAdmin(); } }, 'remove') : null,
    h('button', { type: 'button', onclick: () => box.close() }, 'cancel'),
    h('button', { type: 'button', class: 'primary', onclick: () => {
      const nm = name || nameInp.value.trim();
      const hosts = names.value.split(/[\s,]+/).map(x => x.trim().toLowerCase()).filter(Boolean);
      const bad = !/^[a-z0-9-]+$/.test(nm) ? 'the name: a-z, 0-9 and -' : !name && d.certs[nm] ? `there is a certificate ${nm} already` : !hosts.length ? 'at least one host name' : !issuer ? 'an issuer' : '';
      if (bad) { msg.className = 'msg bad'; msg.textContent = bad; return; }
      d.certs[nm] = { names: hosts, issuer, copies: [...copies].filter(x => x !== issuer) };
      box.close(); markDirty(); drawAdmin();
    } }, 'keep'),
  ].filter(Boolean), { cls: 'pickmodal' });
}

// a node's card (T10): its name, GUID, roles, rules, addresses; what it has: storage, disks
function nodeCard(id, i) {
  const d = admin.draft, n = d.nodes[id];
  const name = h('input', { value: n.name || '', placeholder: id, maxlength: 63 });
  const addrs = h('textarea', { rows: 2, spellcheck: false, placeholder: '198.18.0.120\n2001:2:0:992::1' });
  addrs.value = (n.addrs || []).join('\n');
  const link = h('input', { value: n.link || '', placeholder: '203.0.113.7:7441 (none: nobody dials it; it dials out)', spellcheck: false });
  const roles = new Set(n.roles);
  const roleHost = h('span', { class: 'row wrap' });
  // the voter is the IdP master of / now (Round 14): kept as it is, never shown
  const drawRoles = () => roleHost.replaceChildren(...ROLES.filter(r => r !== 'voter').map(role => h('label', { class: 'row', title: role }, check(roles.has(role), on => { if (on) roles.add(role); else roles.delete(role); drawModules(); }), role)));
  const roleBoxes = roleHost;
  const cache = h('input', { type: 'number', min: 1, max: 100000, value: n.cache_gb || 20, style: 'width:7em' });
  const logBatch = h('input', { type: 'number', min: 50, max: 20000, value: n.log_batch || 500, style: 'width:7em' });
  // where it keeps copies of files (D-U41): a volume of its own, picked by its drives below
  let filesDir = n.files_dir || '';
  const filesLbl = h('span', { class: 'row' });
  const showFiles = () => filesLbl.replaceChildren(filesDir ? h('code', { id: 'files-dir' }, `${filesDir}/osada-${id.slice(0, 8)}…`) : h('span', { class: 'dim', id: 'files-dir' }, 'its data folder'),
    filesDir ? h('button', { type: 'button', onclick: () => { filesDir = ''; showFiles(); } }, 'back to the data folder') : null);
  showFiles();
  const SYSTEM_MOUNTS = ['/', '/boot', '/boot/efi', '/usr', '/etc', '/var', '/var/lib', '/home', '/root', '/tmp'];
  const rules = Object.assign({}, n.rules);
  const ruleRows = h('div', { class: 'kvgrid' });
  const drawRules = () => ruleRows.replaceChildren(...RULE_DOMAINS.map(dom => [h('span', { class: 'lbl' }, dom),
    select([['may', 'may'], ['always', 'always'], ['never', 'never']], rules[dom] || 'may', v => { if (v === 'may') delete rules[dom]; else rules[dom] = v; drawModules(); })]).flat());
  // the modules on this node (Round 14): each module's tab decides what it does there
  const moduleHost = h('span', { class: 'row wrap', id: 'node-modules' });
  const drawModules = () => moduleHost.replaceChildren(...MODULES.map(m => h('label', { class: 'row', title: m.domain ? `rule for ${m.domain}: may; roles ${m.roles.join(', ')}` : `roles ${m.roles.join(', ')}` },
    check(hasModule(roles, rules, m), on => { setModule(roles, rules, m, on); drawModules(); drawRoles(); drawRules(); }, { 'data-module': m.tag }), m.label)));
  drawModules(); drawRoles(); drawRules();
  const msg = h('div', { class: 'msg' });
  const facts = !i ? h('p', { class: 'dim' }, 'asking the node…') : i.error ? h('p', { class: 'bad' }, `no answer: ${i.error}`) : h('dl', { class: 'kv' },
    h('dt', {}, 'host'), h('dd', {}, i.host || ''), h('dt', {}, 'version'), h('dd', {}, i.version || ''),
    h('dt', {}, 'up'), h('dd', {}, i.uptime ? `${Math.floor(i.uptime / 86400)} d ${Math.floor(i.uptime % 86400 / 3600)} h` : ''),
    h('dt', {}, 'data folder'), h('dd', {}, h('code', {}, i.data || ''), i.storage ? ` · ${fmtBytes(i.storage.size - i.storage.free)} used of ${fmtBytes(i.storage.size)}` : ''),
    h('dt', {}, 'root agent'), h('dd', {}, i.agent && i.agent.running ? 'running' : 'none (osada-web-agent.service; disks cannot be formatted)'),
    // its own signing key (D-U45): idp nodes sign with theirs; all are published together
    h('dt', {}, 'signing key'), h('dd', {}, !i.idp_key ? h('span', { class: 'dim' }, n.roles.includes('idp') ? 'not made yet' : 'none (not an idp node)')
      : h('span', {}, h('code', { title: i.idp_key.kid }, i.idp_key.kid.slice(0, 16)), ` since ${new Date(i.idp_key.since).toLocaleDateString()}`, i.idp_key.published ? '' : h('span', { class: 'warn' }, ' (not published yet)'), ' ',
        h('button', { type: 'button', id: 'withdraw-key', onclick: () => confirmModal('withdraw the signing key', `Withdraw ${nodeLabel(id)}'s signing key? Tokens it signed stop being good everywhere; it makes a new key within a minute.`, async () => {
          try { await api('POST', '/api/admin/nodes/withdraw-key', { node: id }); status('withdrawn; a new key comes within a minute', 'ok'); } catch (e) { status(e.message, 'bad'); }
        }) }, 'withdraw'))));
  const flat = [];
  const walk = (list, depth) => (list || []).forEach(x => { flat.push({ ...x, depth }); walk(x.children, depth + 1); });
  walk(i && i.disks, 0);
  const empty = x => x.type === 'disk' && !x.fstype && !(x.children || []).length && (x.mountpoints || []).every(m => m == null);
  const disks = grid({
    id: 'node-disks', rows: flat, empty: 'no disks known',
    columns: [
      { key: 'name', label: 'device', cell: x => h('span', { style: `padding-left:${x.depth * 14}px` }, x.name) },
      { key: 'type', label: 'type' }, { key: 'size', label: 'size', type: 'number', cell: x => fmtBytes(x.size) },
      { key: 'fstype', label: 'file system', value: x => x.fstype || '' }, { key: 'label', label: 'label', value: x => x.label || '' },
      { key: 'mount', label: 'mounted at', value: x => (x.mountpoints || []).filter(Boolean).join(' ') },
      { key: 'files', label: 'files', value: () => '', cell: x => {
        const mp = (x.mountpoints || []).filter(Boolean).find(m => !SYSTEM_MOUNTS.includes(m) && !m.startsWith('/var/lib/containers'));
        if (!mp) return '';
        return mp === filesDir ? h('span', { class: 'pill on' }, 'files here')
          : h('button', { type: 'button', class: 'files-here', onclick: () => { filesDir = mp; showFiles(); } }, 'keep files here');
      } },
      { key: 'act', label: '', value: () => '', cell: x => empty(x) && i.agent && i.agent.running ? h('button', { type: 'button', class: 'danger', onclick: async () => {
        const label = prompt(`A label for the new file system on ${x.name} (a-z, 0-9, -; it is mounted at /var/lib/osada-disks/<label>)`, 'data1');
        if (!label) return;
        const confirmText = prompt(`Everything on ${x.name} (${fmtBytes(x.size)}${x.model ? ', ' + x.model : ''}) is lost. Type ${x.name} to make XFS on it.`);
        if (confirmText !== x.name) { msg.className = 'msg bad'; msg.textContent = 'not confirmed'; return; }
        try { await api('POST', '/api/admin/nodes/disk', { node: id, device: x.name, label, confirm: confirmText }); msg.className = 'msg ok'; msg.textContent = `${x.name}: asked; the agent does it within seconds (refresh the node)`; }
        catch (e) { msg.className = 'msg bad'; msg.textContent = e.message; }
      } }, 'make a file system') : '' },
    ],
  });
  const box = modal(`node ${n.name || id}`, h('div', { class: 'stack' },
    h('span', { class: 'lbl' }, 'name (what people see and type)'), name,
    h('span', { class: 'lbl' }, 'id (GUID, its certificate\'s name; never changes)'), h('code', {}, id),
    h('span', { class: 'lbl' }, 'public addresses, one a line (what DNS should point at for what it exposes)'), addrs,
    h('span', { class: 'lbl' }, 'link: host:port where the other nodes dial it (learnt when a node reaches it there; every node keeps a link to it)'), link,
    h('span', { class: 'lbl' }, 'modules on this node (each module\'s tab sets which OUs it is master for)'), moduleHost,
    h('span', { class: 'lbl' }, 'cache GB (cache, public-cache)'), cache,
    h('span', { class: 'lbl' }, 'files folder: where it keeps copies of files (cache, public cache); a volume of their own: "keep files here" by its drives below. Its root agent makes the folder and restarts the node.'), filesLbl,
    h('span', { class: 'lbl' }, 'log shipping: lines a batch (500 sends about 1,900 lines a second; more for a busy node)'), logBatch,
    h('details', {}, h('summary', {}, 'advanced: roles and rules'), h('span', { class: 'lbl' }, 'roles'), roleBoxes,
      h('span', { class: 'lbl' }, 'rules per domain (always · may · never)'), ruleRows),
    win('what it has', facts, disks.node),
    win('exposed to the internet', exposureBox(id, i)), msg,
    h('p', { class: 'dim' }, 'Changes here go into node control: save it after.')), [
    h('button', { type: 'button', onclick: () => box.close() }, 'cancel'),
    h('button', { type: 'button', class: 'primary', onclick: () => {
      const nm = name.value.trim();
      if (nm) n.name = nm; else delete n.name;
      const a = addrs.value.split(/[\s,]+/).map(x => x.trim()).filter(Boolean);
      if (a.length) n.addrs = a; else delete n.addrs;
      const l = link.value.trim();
      if (l) n.link = l; else delete n.link;
      n.roles = ROLES.filter(r => roles.has(r));
      if (filesDir) n.files_dir = filesDir; else delete n.files_dir;
      const lb = Math.round(+logBatch.value || 500);
      if (lb === 500) delete n.log_batch; else n.log_batch = Math.min(20000, Math.max(50, lb));
      const gb = Math.max(1, Math.round(+cache.value || 20));
      if (gb === 20) delete n.cache_gb; else n.cache_gb = gb;
      n.rules = rules;
      // every node with IdP keeps the secrets (the owner, Round 14); an OU may still name its own
      const idpOn = hasModule(new Set(n.roles), n.rules, MODULES.find(m => m.tag === 'idp'));
      d.default_secrets = idpOn ? [...new Set([...(d.default_secrets || []), id])].sort() : (d.default_secrets || []).filter(x => x !== id);
      box.close(); markDirty(); drawAdmin();
    } }, 'keep'),
  ], { cls: 'pickmodal' });
}

// ---- modules: instances of a kind, each with a name of its own, on a node ----

/// Stalwart's own pages at the mail instance's name: its administration, and the users' account
/// pages (app passwords for mail programs, encryption at rest; T6).
function stalwartLinks(host) {
  const a = (p, t) => h('a', { href: `https://${host}${p}`, target: '_blank', rel: 'noopener', class: 'stalwart-link' }, t);
  return h('span', {}, a('/admin/', 'administration ↗'), ' · ', a('/account/', 'users\' account pages ↗'));
}

function modulesTab() {
  const d = admin.draft;
  d.modules ||= {};
  let g = null;
  const edit = m => {
    const title = h('input', { value: m.title });
    const host = h('input', { value: m.host || '', placeholder: 'apps.example.org' });
    const upstream = h('input', { value: m.upstream || '', placeholder: 'http://192.168.1.11' });
    let node = m.node;
    const p = nodePicker(nodeNames(), m.node, row => { if (row) node = row.name; });
    const proxy = m.kind === 'proxy';
    const who = grid({ id: 'module-access', columns: [{ key: 'subject', label: 'who', cell: a => a.subject_label }, { key: 'via', label: 'how' }],
      fetch: remote('access', { module: m.name }), empty: 'nobody has access' });
    const box = modal(`module ${m.name} (${m.kind})`, h('div', { class: 'stack' },
      h('span', { class: 'lbl' }, 'title'), title, h('span', { class: 'lbl' }, 'node'), p.node,
      proxy ? [h('span', { class: 'lbl' }, 'its own name (https)'), host, h('span', { class: 'lbl' }, 'upstream'), upstream] : null,
      h('p', { class: 'dim' }, proxy
        ? 'Its pages are shown at a name of their own, never on osada\'s; only for those with access, and only while their osada session lasts. Save node control after.'
        : 'Moving it to another node hands its data over there (planned handover). Save node control after.'),
      m.kind === 'mail' && m.host ? h('p', {}, 'Stalwart\'s own pages: ', stalwartLinks(m.host)) : null,
      h('span', { class: 'lbl' }, 'who has access'), who.node), [
      h('button', { type: 'button', class: 'danger', onclick: () => { delete d.modules[m.name]; box.close(); markDirty(); g.refresh(moduleRows()); } }, 'remove'),
      h('button', { type: 'button', onclick: () => box.close() }, 'cancel'),
      h('button', { type: 'button', class: 'primary', onclick: () => {
        d.modules[m.name] = proxy ? { kind: m.kind, title: title.value, node, host: host.value.trim().toLowerCase(), upstream: upstream.value.trim().replace(/\/+$/, '') } : { ...d.modules[m.name], kind: m.kind, title: title.value, node };
        box.close(); markDirty(); g.refresh(moduleRows());
      } }, 'keep'),
    ], { cls: 'pickmodal' });
  };
  g = grid({ id: 'modules', columns: MODULE_COLS, rows: moduleRows(), sort: 'name', onRow: edit, empty: 'no modules yet' });
  let kind = 'wiki', node = null;
  const msg = h('div', { class: 'msg' });
  const np = nodePicker(nodeNames(), null, row => { node = row ? row.name : null; });
  const proxyOnly = h('span', { class: 'row', hidden: true },
    h('input', { name: 'host', placeholder: 'its own name (apps.example.org)' }),
    h('input', { name: 'upstream', placeholder: 'upstream (http://192.168.1.11)' }));
  const form = h('form', { class: 'row', onsubmit: e => {
    e.preventDefault();
    const f = new FormData(form);
    const name = f.get('name').trim();
    if (d.modules[name]) { msg.className = 'msg bad'; msg.textContent = `there is a module ${name} already`; return; }
    if (!node) { msg.className = 'msg bad'; msg.textContent = 'pick a node'; return; }
    d.modules[name] = { kind, title: f.get('title').trim(), node };
    if (kind === 'proxy') Object.assign(d.modules[name], { host: f.get('host').trim().toLowerCase(), upstream: f.get('upstream').trim().replace(/\/+$/, '') });
    msg.className = 'msg ok'; msg.textContent = `${name} added here; save node control to keep it`;
    form.reset();
    markDirty();
    g.refresh(moduleRows());
  } },
    h('input', { name: 'name', placeholder: 'name (mail-eu)', required: true, pattern: '[a-z][a-z0-9\\-]*' }),
    select(KINDS.map(k => [k, k]), kind, v => { kind = v; proxyOnly.hidden = v !== 'proxy'; }),
    h('input', { name: 'title', placeholder: 'title' }),
    np.node,
    proxyOnly,
    h('button', { class: 'primary', type: 'submit' }, 'add'));
  return h('div', {},
    win('modules: each kind may run as several instances, each with a name of its own and on its own node (click one to change it)', g.node),
    win('add a module', form, msg, h('p', { class: 'dim' }, 'The node must be allowed for the kind (its rule is not "never"). Users get access to a module on their card, or by a mass update in the users tab. A proxy shows another program (its upstream) at a name of its own, to those with access, as apptab did; that name must reach this server (DNS, and https: at home the https listener, outside the edge).')));
}

// ---- OUs: a tree, the one picked, and what is in it ----

let pickedOu = null;

function ouPath(id) {
  const d = admin.draft, path = [];
  for (let o = id, n = 0; o != null && d.ous[o] && n < 64; o = d.ous[o].parent, n++) path.unshift(o);
  return path;
}
function masterOf(domain, id) {
  const d = admin.draft, path = ouPath(id);
  const link = o => (d.ous[o].links || {})[domain];
  const enforced = path.find(o => link(o) && link(o).enforced);
  const near = [...path].reverse().find(o => link(o));
  const at = enforced != null ? enforced : near;
  if (at != null) return { node: link(at).node, from: at };
  if (d.defaults[domain]) return { node: d.defaults[domain], from: path[0] };
  if (domain !== 'directory') { const m = masterOf('directory', id); return m ? { ...m, idp: true } : null; }
  return null;
}
function rootOu() {
  const d = admin.draft;
  return Object.keys(d.ous).map(Number).find(i => d.ous[i].parent === null || d.ous[i].parent === undefined);
}

// a module's master per OU (Round 14; Round 16: for thousands of OUs): / and the OUs that set their
// own master, by full path; "+ new" adds one, picked by searching the OUs' paths. Each a dropdown of
// the nodes with the module; unset: as above (/ as the IdP master); the smaller OU goes first.
function masterTree(domain, label) {
  const d = admin.draft, root = rootOu();
  const mod = MODULES.find(m => m.domain === domain);
  const added = new Set();
  const box = h('div', {});
  const draw = () => {
    const own = Object.keys(d.ous).map(Number).filter(i => i !== root && (d.ous[i].links || {})[domain]);
    const list = [...new Set([...own, ...added])].filter(i => d.ous[i]).sort((x, y) => pathOf(x).localeCompare(pathOf(y)));
    const row = i => {
      const ou = d.ous[i];
      const mine = i === root ? ((ou.links || {})[domain] || (d.defaults[domain] ? { node: d.defaults[domain] } : null)) : (ou.links || {})[domain];
      const above = i === root ? (domain !== 'directory' ? masterOf('directory', root) : null) : masterOf(domain, ou.parent);
      const may = nodeNames().filter(id => mod ? hasModule(new Set(d.nodes[id].roles), d.nodes[id].rules || {}, mod) : true);
      const opts = [['', i === root ? (domain === 'directory' ? '— none —' : `as the IdP (${above ? nodeLabel(above.node) : 'none'})`) : `as above (${above ? nodeLabel(above.node) : 'none'})`],
        ...may.map(id => [id, nodeLabel(id)])];
      if (mine && !may.includes(mine.node)) opts.push([mine.node, `${nodeLabel(mine.node)} (no ${label} here)`]);
      const sel = select(opts, mine ? mine.node : '', v => {
        if (i === root && !(ou.links || {})[domain]) { if (v) d.defaults[domain] = v; else delete d.defaults[domain]; }
        else if (v) { ou.links = ou.links || {}; ou.links[domain] = { node: v, enforced: !!(mine && mine.enforced) }; }
        else delete ou.links[domain];
        markDirty(); draw();
      }, { 'data-ou': i, 'data-domain': domain });
      const eff = masterOf(domain, i);
      return h('tr', {}, h('td', {}, h('code', {}, pathOf(i)), mine && mine.enforced ? h('span', { class: 'pill on', title: 'enforced: what is set below does not count' }, ' enforced') : null),
        h('td', {}, sel),
        h('td', {}, eff ? h('span', { class: eff.from === i && mine ? 'ok' : 'dim' }, nodeLabel(eff.node), eff.idp ? ' (the IdP master)' : '') : h('span', { class: 'bad' }, 'none')),
        h('td', {}, i === root ? '' : h('a', { class: 'bad', title: 'goes as the OU above again', onclick: () => { if (ou.links) delete ou.links[domain]; added.delete(i); markDirty(); draw(); } }, '✕')));
    };
    const adder = h('span', { class: 'row', hidden: true },
      ouPicker(null, r => { if (r && d.ous[r.id] && r.id !== root) { added.add(Number(r.id)); draw(); } }).node,
      h('span', { class: 'dim' }, 'search by full path, e.g. /EMEA/DE'));
    box.replaceChildren(h('table', { class: 'grid master-tree' },
      h('thead', {}, h('tr', {}, h('th', {}, 'OU'), h('th', {}, `${label} master here`), h('th', {}, 'in effect'), h('th', {}, ''))),
      h('tbody', {}, [root, ...list].map(row))),
      h('div', { class: 'row' }, h('button', { type: 'button', class: 'master-new', onclick: () => { adder.hidden = false; const inp = adder.querySelector('input'); if (inp) inp.focus(); } }, '+ new'), adder),
      h('p', { class: 'dim' }, `Listed: / and the OUs with a ${label} master of their own (${list.length}); every other OU goes as the one above it.`));
  };
  draw();
  return box;
}

// a module's tab (Round 14): which node is its master for which OU; its own settings below
function moduleTab(domain, label, ...extra) {
  const mod = MODULES.find(m => m.domain === domain);
  const tagged = nodeNames().filter(id => hasModule(new Set(admin.draft.nodes[id].roles), admin.draft.nodes[id].rules || {}, mod));
  return h('div', {},
    win(`${label}: the master node for each OU`, masterTree(domain, label),
      h('p', { class: 'dim' }, tagged.length
        ? `The nodes with ${label} (nodes tab): ${tagged.map(nodeLabel).join(', ')}. The master keeps the OU's ${label} and answers for it; an OU without its own goes as the OU above it, and / as the IdP master. The smaller OU goes first.`
        : `No node has ${label} yet: give it to one in the nodes tab. Until then it goes as the IdP master.`)),
    ...extra);
}

function ousTab() {
  const d = admin.draft;
  const ids = Object.keys(d.ous).map(Number);
  const root = ids.find(i => d.ous[i].parent === null || d.ous[i].parent === undefined);
  if (pickedOu === null || !d.ous[pickedOu]) pickedOu = root;
  const kids = {};
  for (const i of ids) {
    const p = d.ous[i].parent;
    if (p !== null && p !== undefined) (kids[p] ||= []).push(i);
  }
  const detail = h('div', {});
  const tree = h('div', { class: 'tree' });
  const drawTree = () => {
    const item = (i, depth) => {
      const o = d.ous[i], info = ouInfo(i);
      const a = h('a', { class: 'tree-item' + (i === pickedOu ? ' on' : ''), onclick: () => { pickedOu = i; drawTree(); drawDetail(); } },
        i === root ? `/ (${o.name})` : o.name,
        (() => { const m = masterOf('directory', i); return m ? h('span', { class: m.from === i ? 'ok' : 'dim' }, ` · ${nodeLabel(m.node)}`) : null; })(),
        o.tier0 ? h('span', { class: 'pill t0' }, 't0') : null,
        o.second_factor ? h('span', { class: 'pill on' }, '2f') : null,
        info ? h('span', { class: 'dim' }, ` ${info.users}u ${info.groups}g`) : h('span', { class: 'warn' }, ' new'));
      a.style.paddingLeft = `${4 + depth * 16}px`;
      return h('div', {}, a, (kids[i] || []).sort((x, y) => d.ous[x].name.localeCompare(d.ous[y].name)).map(k => item(k, depth + 1)));
    };
    tree.replaceChildren(item(root, 0));
  };
  const drawDetail = () => detail.replaceChildren(ouDetail(pickedOu), ouObjects(pickedOu));
  drawTree();
  drawDetail();
  return h('div', { class: 'split' },
    win('IdP: the OUs and their master node', tree,
      h('p', { class: 'dim' }, 'Each OU\'s IdP master signs its people in and keeps its directory; an OU without its own goes as the OU above it (the smaller OU first). The master of / is the master of everything in the background (it holds the cluster log). Flags are inherited down the tree; tier 0 and a required second factor only tighten. Roles granted over an OU reach everything below it.'),
      win('the IdP master for each OU', masterTree('directory', 'IdP'))),
    detail);
}

function ouDetail(id) {
  const d = admin.draft;
  const ou = d.ous[id];
  const s = ouInfo(id);
  const mine = s ? s.can : [];
  const manage = !s || mine.includes('ou-manage');
  const security = !s || mine.includes('ou-security');
  const secretsHost = h('div', {});
  const drawSecrets = () => secretsHost.replaceChildren(h('div', { class: 'row' },
    h('label', { class: 'row' }, check(!ou.secrets, on => { ou.secrets = on ? null : { nodes: [...(s && s.secrets.nodes ? s.secrets.nodes : [])], enforced: false }; markDirty(); drawSecrets(); }, { disabled: !security }), 'as above'),
    ou.secrets ? [nodeNames().map(name => h('label', { class: 'row' }, check(ou.secrets.nodes.includes(name), on => {
      ou.secrets.nodes = on ? [...ou.secrets.nodes, name].sort() : ou.secrets.nodes.filter(x => x !== name);
      markDirty();
    }, { disabled: !security }), nodeLabel(name))), h('label', { class: 'row' }, check(ou.secrets.enforced, on => { ou.secrets.enforced = on; markDirty(); }, { disabled: !security }), 'enforced')] : null));
  drawSecrets();
  const links = grid({
    id: 'ou-links',
    columns: [
      { key: 'domain', label: 'domain' },
      { key: 'link', label: 'link here', value: r => ou.links[r.domain] ? ou.links[r.domain].node : '', cell: r => manage
        ? nodePicker(nodeNames(), ou.links[r.domain] ? ou.links[r.domain].node : '', row => {
          if (row) ou.links[r.domain] = { node: row.name, enforced: ou.links[r.domain] ? ou.links[r.domain].enforced : false }; else delete ou.links[r.domain];
          markDirty();
        }, 'as above').node
        : (ou.links[r.domain] ? nodeLabel(ou.links[r.domain].node) : 'as above') },
      { key: 'enforced', label: 'enforced', type: 'bool', value: r => !!(ou.links[r.domain] && ou.links[r.domain].enforced),
        cell: r => check(ou.links[r.domain] && ou.links[r.domain].enforced, on => { if (ou.links[r.domain]) { ou.links[r.domain].enforced = on; markDirty(); } }, { disabled: !manage }) },
      { key: 'authority', label: 'authority (as saved)', value: r => s && s.resolved[r.domain] ? (s.resolved[r.domain].node ? nodeLabel(s.resolved[r.domain].node) : s.resolved[r.domain].problem) : '' },
      { key: 'why', label: 'because of', value: r => s && s.resolved[r.domain] && s.resolved[r.domain].why ? s.resolved[r.domain].why : '' },
    ],
    rows: DOMAINS.map(domain => ({ domain })),
  });
  const flags = h('div', { class: 'row' },
    h('label', { class: 'row' }, check(ou.tier0, on => { ou.tier0 = on; markDirty(); }, { disabled: !security }), 'tier 0 (FIDO2 only)'),
    h('label', { class: 'row' }, check(ou.second_factor, on => { ou.second_factor = on; markDirty(); }, { disabled: !security }), 'second factor required'),
    s && s.tier0_from && s.tier0_from !== s.path ? h('span', { class: 'pill t0' }, `tier 0 from ${s.tier0_from}`) : null,
    s && s.second_factor_from && s.second_factor_from !== s.path ? h('span', { class: 'pill on' }, `second factor from ${s.second_factor_from}`) : null);
  // D-U30: policies inherited downwards, only tightened below
  const above = (key, pick) => {
    const vals = [];
    for (let p = ou.parent, n = 0; p != null && d.ous[p] && n < 64; p = d.ous[p].parent, n++) if (d.ous[p][key] != null) vals.push([d.ous[p][key], p]);
    return vals.length ? pick(vals) : null;
  };
  const limitAbove = above('storage_limit', v => v[0]);
  const daysAbove = above('session_days', v => v.reduce((m, x) => x[0] < m[0] ? x : m));
  const parseSize = t => { const m = String(t).trim().match(/^(\d+(?:\.\d+)?)\s*([KMGT])/i); return m ? Math.round(Number(m[1]) * 2 ** (10 * ' KMGT'.indexOf(m[2].toUpperCase()))) : null; };
  const policies = h('div', { class: 'row' },
    h('span', { class: 'lbl' }, 'storage limit of its accounts'),
    h('input', { value: ou.storage_limit ? fmtBytes(ou.storage_limit) : '', size: 8, disabled: !manage, placeholder: limitAbove ? fmtBytes(limitAbove[0]) : 'none',
      onchange: e => { const v = e.target.value.trim(); ou.storage_limit = v ? parseSize(v) : null; if (v && !ou.storage_limit) { e.target.value = ''; status('a size like 50G or 500M', 'bad'); } markDirty(); } }),
    h('span', { class: 'dim' }, limitAbove ? `from ${pathOf(limitAbove[1])}: ${fmtBytes(limitAbove[0])}; lower only` : 'for accounts without their own limit'),
    h('span', { class: 'lbl' }, 'longest sign-in'),
    h('input', { type: 'number', min: 1, value: ou.session_days || '', size: 4, disabled: !security, placeholder: daysAbove ? String(daysAbove[0]) : String(admin.ov.settings.session_days),
      onchange: e => { const v = Number(e.target.value); ou.session_days = v > 0 ? v : null; markDirty(); } }), 'days',
    h('span', { class: 'dim' }, daysAbove ? `from ${pathOf(daysAbove[1])}: ${daysAbove[0]} d; shorter only` : `the cluster's: ${admin.ov.settings.session_days} d`));
  const child = h('form', { class: 'row', onsubmit: e => {
    e.preventDefault();
    const name = new FormData(child).get('name').trim();
    const nid = Math.max(0, ...Object.keys(d.ous).map(Number)) + 1;
    d.ous[nid] = { name, parent: Number(id), links: {}, secrets: null, tier0: false, second_factor: false };
    pickedOu = nid;
    drawAdmin();
  } }, h('span', { class: 'lbl' }, 'new OU below'), h('input', { name: 'name', placeholder: 'name', required: true, pattern: '[^\\/]+' }),
    h('button', { type: 'submit', disabled: !manage }, 'add'));
  void links;
  return win(`OU ${pathOf(id)}`,
    h('div', { class: 'row' }, h('label', { class: 'lbl' }, 'name'),
      h('input', { value: ou.name, disabled: !manage, onchange: e => { ou.name = e.target.value.trim(); markDirty(); } }),
      h('span', { class: 'dim' }, s ? `secrets ${s.secrets_here ? 'kept' : 'not kept'} on this node` : 'new, not saved yet')),
    h('div', { class: 'dim' }, 'your rights here: ', mine.length ? mine.map(p => PERM_LABEL[p] || p).join(', ') : 'none'),
    child, flags, policies,
    h('div', {}, h('span', { class: 'lbl' }, 'secrets kept on '), s && s.secrets.nodes ? h('b', {}, s.secrets.nodes.map(nodeLabel).join(', ')) : null, s && s.secrets.why ? h('span', { class: 'dim' }, ` (${s.secrets.why})`) : null),
    secretsHost);
}

/// Everything that sits in an OU (or below it): users, groups, the roles
/// granted over it, its collections; a page at a time from the node.
function ouObjects(id) {
  const base = ouInfo(id);
  if (!base) return win('in this OU', h('p', { class: 'dim' }, 'not saved yet'));
  let below = false;
  let g = null;
  const toggle = h('label', { class: 'row' }, check(false, on => { below = on; g.refresh(); }), 'include OUs below');
  g = grid({
    id: 'ou-objects',
    columns: [
      { key: 'type', label: 'type', type: 'enum', options: ['user', 'group', 'role', 'collection'] },
      { key: 'name', label: 'name' },
      { key: 'detail', label: 'detail' },
      { key: 'ou', label: 'OU' },
    ],
    fetch: remote('objects', () => ({ ou: String(id), below: below ? '1' : '' })),
    extras: [toggle],
    onRow: r => go(r.go),
    empty: 'nothing in this OU',
  });
  return win(`in ${base.path}`, g.node);
}

// ---- collections ----

function collectionsTab() {
  const g = grid({
    id: 'collections',
    columns: [
      { key: 'id', label: 'collection', cell: c => short(c.id) },
      { key: 'kind', label: 'kind' },
      { key: 'ou', label: 'OU', cell: c => c.ou_path },
      { key: 'epoch', label: 'epoch', type: 'number' },
      { key: 'authority', label: 'authority', cell: c => h('span', {}, h('b', {}, c.authority), c.moving_to ? h('span', { class: 'warn' }, ` → ${c.moving_to}`) : null) },
      { key: 'members', label: 'members', cell: c => c.members.join(', ') },
      { key: 'here', label: 'here (entries)', type: 'number', cell: c => c.here === null ? '—' : c.here },
      { key: 'moving_to', label: 'moving to', hidden: true },
    ],
    fetch: remote('collections'),
    onRow: c => {
      const others = c.members.filter(m => m !== c.authority);
      let to = null;
      const p = nodePicker(others, null, row => { to = row ? row.name : null; });
      const m = modal(`hand over ${c.kind} of ${c.ou_path}`, h('div', {},
        h('p', { class: 'dim' }, `Now at ${c.authority}, epoch ${c.epoch}. A handover that goes against node control is undone within a second; to move it for good, change node control.`),
        others.length ? h('div', { class: 'row' }, h('span', { class: 'lbl' }, 'to'), p.node) : h('p', { class: 'dim' }, 'no other member')), [
        h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
        h('button', { type: 'button', class: 'primary', disabled: !others.length, onclick: async () => {
          if (!to) { status('pick a node', 'bad'); return; }
          m.close();
          try { await api('POST', '/api/admin/handover', { collection: c.id, to }); status(`handing over to ${to}`, 'ok'); await reloadAdmin(1500); }
          catch (err) { status(err.message, 'bad'); }
        } }, 'hand over'),
      ]);
    },
  });
  return win('collections: who is the authority, who keeps a copy (click one to hand it over)',
    h('p', { class: 'dim' }, 'The IdP master of / reconciles every second: each OU gets its directory (on every store node) and its credentials (on the nodes that keep its secrets), and what node control moves is handed over by itself.'),
    g.node);
}

// ---- users and groups ----

function usersTab() {
  let users = null;
  const massBtn = h('button', { type: 'button', onclick: () => massUpdate(users) }, 'mass update…');
  users = grid({
    id: 'users',
    columns: [...USER_COLS.slice(0, 3),
      { key: 'member_of', label: 'member of', cell: u => u.member_of.map(g => g.name).join(', ') },
      { key: 'access_to', label: 'access to', cell: u => u.access_to.map(a => a.via === 'directly' ? a.module : `${a.module} (${a.via})`).join(', ') },
      { key: 'outside', label: 'from outside', cell: u => u.outside ? h('span', {}, u.outside) : h('span', { class: 'warn' }, 'anywhere') },
      { key: 'roles', label: 'roles (direct)', cell: u => u.roles.join(', ') },
      USER_COLS[3], { key: 'id', label: 'id', hidden: true }],
    fetch: remote('users'),
    sort: 'name',
    select: true, key: u => u.id,
    extras: [massBtn],
    onRow: u => go(`/admin/users/${u.id}`),
  });
  const groups = grid({
    id: 'groups',
    columns: [...GROUP_COLS, { key: 'roles', label: 'roles', cell: g => g.roles.join(', ') }, { key: 'id', label: 'id', hidden: true }],
    fetch: remote('groups'),
    sort: 'name',
    onRow: g => go(`/admin/groups/${g.id}`),
  });
  const umsg = h('div', { class: 'msg' });
  const uou = ouPicker('directory-manage');
  const uform = h('form', { class: 'row', onsubmit: async e => {
    e.preventDefault();
    const f = new FormData(uform);
    if (uou.get() === null) { umsg.className = 'msg bad'; umsg.textContent = 'pick an OU'; return; }
    try {
      const u = await api('POST', '/api/admin/users', { name: f.get('name'), display: f.get('display'), ou: uou.get(), password: f.get('password') });
      umsg.className = 'msg ok'; umsg.textContent = `${u.name} made`; uform.reset();
      users.refresh();
    } catch (err) { umsg.className = 'msg bad'; umsg.textContent = err.message; }
  } },
    h('input', { name: 'name', placeholder: 'user name', required: true, pattern: '[a-z0-9][a-z0-9._\\-]*' }),
    h('input', { name: 'display', placeholder: 'display name' }),
    uou.node,
    h('input', { name: 'password', type: 'password', placeholder: 'password (8+)', autocomplete: 'new-password', required: true, minlength: 8 }),
    h('button', { class: 'primary', type: 'submit' }, 'add user'));
  const gmsg = h('div', { class: 'msg' });
  const gou = ouPicker('directory-manage');
  const gform = h('form', { class: 'row', onsubmit: async e => {
    e.preventDefault();
    const f = new FormData(gform);
    if (gou.get() === null) { gmsg.className = 'msg bad'; gmsg.textContent = 'pick an OU'; return; }
    try {
      const g = await api('POST', '/api/admin/groups', { name: f.get('name'), display: f.get('display'), ou: gou.get() });
      gmsg.className = 'msg ok'; gmsg.textContent = `${g.name} made`; gform.reset();
      groups.refresh();
    } catch (err) { gmsg.className = 'msg bad'; gmsg.textContent = err.message; }
  } },
    h('input', { name: 'name', placeholder: 'group name', required: true, pattern: '[a-z0-9][a-z0-9._\\-]*' }),
    h('input', { name: 'display', placeholder: 'what it is for' }),
    gou.node,
    h('button', { class: 'primary', type: 'submit' }, 'add group'));
  return h('div', {},
    win('users (click one for its card)', users.node),
    win('groups (click one for its card)', groups.node),
    h('div', { class: 'cols' },
      win('add a user', uform, umsg, h('p', { class: 'dim' }, '"user_created" goes to the admin log and "password_changed" to the auth log, both with your session.')),
      win('add a group', gform, gmsg, h('p', { class: 'dim' }, 'A group stands for a function ("EMEA helpdesk"): grant it roles, and everyone in it has them.'))));
}

/// A mass update of users: the picked ones, or every one the table's filter
/// finds; a job on the node, with its progress here.
function massUpdate(users) {
  const n = users.picked().length;
  let scope = n ? 'picked' : 'query';
  let kind = 'add_to_group';
  let group = null, module = null;
  const gp = groupPicker(row => { group = row ? row.id : null; });
  const mp = modulePicker(row => { module = row ? row.name : null; });
  const arg = h('div', {});
  const outsideIn = h('input', { placeholder: 'AS5610, 203.0.113.0/24 (empty: from anywhere)', size: 40 });
  const drawArg = () => arg.replaceChildren(kind.endsWith('group') ? gp.node : kind.endsWith('access') ? mp.node : kind === 'set_outside' ? outsideIn : '');
  drawArg();
  const progress = h('div', {});
  const radio = (v, label, disabled) => h('label', { class: 'row' }, h('input', { type: 'radio', name: 'mass-scope', value: v, checked: scope === v, disabled, onchange: () => { scope = v; } }), label);
  const run = h('button', { type: 'button', class: 'primary', onclick: async () => {
    const action = { kind };
    if (kind.endsWith('group')) { if (!group) { status('pick a group', 'bad'); return; } action.group = group; }
    if (kind.endsWith('access')) { if (!module) { status('pick a module', 'bad'); return; } action.module = module; }
    if (kind === 'set_outside') action.outside = outsideIn.value;
    const selection = scope === 'picked' ? { ids: users.picked() } : { query: { ...users.query(), params: {} } };
    run.disabled = true;
    try {
      const { job } = await api('POST', '/api/admin/users/bulk', { selection, action });
      const poll = async () => {
        const j = await api('GET', `/api/admin/jobs/${job}`);
        const pct = j.total ? Math.round(100 * (j.done + j.refused + j.failed) / j.total) : 100;
        progress.replaceChildren(h('div', { class: 'row' }, h('progress', { max: 100, value: pct }), `${j.what}: ${j.done} done, ${j.refused} refused, ${j.failed} failed`),
          j.notes.length ? h('ul', { class: 'dim' }, j.notes.map(x => h('li', {}, x))) : '');
        if (j.finished) { status(`mass update finished: ${j.done} done, ${j.refused} refused, ${j.failed} failed`, j.failed ? 'bad' : 'ok'); users.unpick(); users.refresh(); }
        else setTimeout(poll, 500);
      };
      poll();
    } catch (err) { status(err.message, 'bad'); run.disabled = false; }
  } }, 'run');
  const m = modal('mass update of users', h('div', { class: 'stack' },
    h('span', { class: 'lbl' }, 'which users'),
    radio('picked', `the ${n.toLocaleString()} picked in the table`, !n),
    radio('query', `every user the table's filter finds (${users.total().toLocaleString()})`, false),
    h('span', { class: 'lbl' }, 'what'),
    select([['add_to_group', 'put into group'], ['remove_from_group', 'take out of group'], ['grant_access', 'give access to module'],
      ['revoke_access', 'take access to module'], ['set_outside', 'set outside networks'], ['disable', 'disable'], ['enable', 'enable']], kind, v => { kind = v; drawArg(); }),
    arg,
    h('p', { class: 'dim' }, 'Runs on the node in batches; each user is checked against your rights, and the ones refused are counted. It goes to the admin log with your session.'),
    progress), [h('button', { type: 'button', onclick: () => m.close() }, 'close'), run], { cls: 'pickmodal' });
}

/// What a user or group may use: access to module instances.
function accessGrid(id, params) {
  let g = null;
  g = grid({
    id,
    columns: [...MODULE_COLS.slice(0, 1).map(c => ({ ...c, key: 'module', cell: a => h('b', {}, a.module) })), MODULE_COLS[1], MODULE_COLS[2], MODULE_COLS[3],
      { key: 'via', label: 'how' },
      { key: 'act', label: '', cell: a => a.via === 'directly' ? h('button', { type: 'button', class: 'danger', onclick: async () => {
        try { await api('POST', '/api/admin/access/revoke', { id: a.id }); status('access taken back', 'ok'); g.refresh(); }
        catch (err) { status(err.message, 'bad'); }
      } }, 'take back') : null }],
    fetch: remote('access', params),
    empty: 'no access to any module',
  });
  return g;
}

function accessForm(subject, after) {
  let module = null;
  const mp = modulePicker(row => { module = row ? row.name : null; });
  return h('div', { class: 'row' }, h('span', { class: 'lbl' }, 'give access to'), mp.node,
    h('button', { type: 'button', class: 'primary', onclick: async () => {
      if (!module) { status('pick a module', 'bad'); return; }
      try { await api('POST', '/api/admin/access', { module, subject }); status('access given', 'ok'); after(); }
      catch (err) { status(err.message, 'bad'); }
    } }, 'give'));
}

function grantForm(subject, after) {
  const msg = h('div', { class: 'msg' });
  const roles = admin.ov.roles;
  const about = h('span', { class: 'dim' }, roles[0].about);
  let role = roles[0].name;
  const scope = ouPicker('roles-grant');
  return h('div', {},
    h('div', { class: 'row' },
      h('span', { class: 'lbl' }, 'grant'),
      select(roles.map(r => [r.name, r.name]), role, v => { role = v; about.textContent = roles.find(r => r.name === v).about; }),
      h('span', { class: 'lbl' }, 'over'), scope.node,
      h('button', { class: 'primary', type: 'button', onclick: async () => {
        if (scope.get() === null) { msg.className = 'msg bad'; msg.textContent = 'pick an OU'; return; }
        try { await api('POST', '/api/admin/grants', { scope: scope.get(), role, subject }); status('role granted', 'ok'); msg.textContent = ''; after(); }
        catch (err) { msg.className = 'msg bad'; msg.textContent = err.message; }
      } }, 'grant')),
    about, msg);
}

/// The roles of a user (directly and through groups) or a group.
function grantsGrid(id, params) {
  let g = null;
  g = grid({
    id,
    columns: [
      { key: 'role', label: 'role', cell: x => h('b', {}, x.role) },
      { key: 'scope', label: 'over' },
      { key: 'via', label: 'how' },
      { key: 'act', label: '', cell: x => x.via === 'directly' ? h('button', { type: 'button', class: 'danger', onclick: () => confirmModal('take a role back', `Take ${x.role} over ${x.scope} back?`, async () => {
        try { await api('POST', '/api/admin/grants/revoke', { scope: x.scope_id, id: x.id }); status('role taken back', 'ok'); g.refresh(); }
        catch (err) { status(err.message, 'bad'); }
      }) }, 'take back') : null },
    ],
    fetch: remote('grants', params),
    empty: 'no roles',
  });
  return g;
}

function userCard(id) {
  const box = h('div', {}, h('p', { class: 'dim' }, 'loading…'));
  api('GET', `/api/admin/users/${id}`).then(c => {
    const u = c.user;
    const mine = new Set(c.can);
    const act = async (body, what) => {
      try { await api('POST', '/api/admin/users/update', { id: u.id, ...body }); status(what, 'ok'); await reloadAdmin(500); }
      catch (err) { status(err.message, 'bad'); }
    };
    let groups = null;
    const member = async (group, on) => {
      try { await api('POST', '/api/admin/groups/member', { group, user: u.id, member: on }); status(on ? 'put into the group' : 'taken out', 'ok'); groups.refresh(); }
      catch (err) { status(err.message, 'bad'); }
    };
    groups = grid({
      id: 'card-groups',
      columns: [...GROUP_COLS, { key: 'act', label: '', cell: g => h('button', { type: 'button', onclick: () => member(g.id, false) }, 'take out') }],
      fetch: remote('groups', { member: u.id }),
      onRow: g => go(`/admin/groups/${g.id}`),
      empty: 'in no group',
    });
    const into = groupPicker();
    const roles = grantsGrid('card-roles', { subject_user: u.id });
    const access = accessGrid('card-access', { subject_user: u.id });
    const events = c.sign_ins === null ? h('p', { class: 'dim' }, 'You cannot read the logs about this user.') : logGrid('card-events', { rows: c.sign_ins.map(flatLine) }).node;
    const sessions = grid({
      id: 'card-sessions',
      columns: [
        { key: 'node', label: 'node', cell: r => nodeLabel(r.node) },
        { key: 'sid', label: 'session', cell: s => h('a', { href: `/admin/logs?kind=auth&session=${s.sid}`, 'data-go': true }, short(s.sid)) },
        { key: 'started', label: 'started', type: 'date' }, { key: 'expires', label: 'ends', type: 'date' }, { key: 'addr', label: 'from' },
        { key: 'act', label: '', value: () => '', cell: s => s.node === admin.ov.me && mine.has('account-lock') ? h('button', { type: 'button', class: 'danger', onclick: async () => {
          try { await api('POST', '/api/admin/users/end-session', { user: u.id, sid: s.sid }); status('session ended', 'ok'); await reloadAdmin(300); }
          catch (err) { status(err.message, 'bad'); }
        } }, 'end') : null },
      ],
      rows: c.sessions,
      sort: 'started', dir: 'desc',
      empty: 'no sessions',
    });
    box.replaceChildren(
      h('div', { class: 'row' }, h('a', { href: '/admin/users', 'data-go': true }, '← users')),
      h('div', { class: 'cols' },
        win(`user ${u.name}`, h('dl', { class: 'kv' },
          h('dt', {}, 'name'), h('dd', {}, h('input', { value: u.display, disabled: !mine.has('directory-manage'), onchange: e => act({ display: e.target.value }, 'name changed') })),
          h('dt', {}, 'OU'), h('dd', {}, c.ou_path),
          h('dt', {}, 'id (sub)'), h('dd', {}, h('code', {}, u.id)),
          h('dt', {}, 'state'), h('dd', {}, u.disabled ? h('span', { class: 'bad' }, 'disabled') : 'active'),
          c.legacy_admin ? [h('dt', {}, 'old flag'), h('dd', { class: 'warn' }, 'admin from before roles: may do everything ',
            h('button', { type: 'button', onclick: () => act({ drop_old_admin_flag: true }, 'old admin flag dropped') }, 'drop it'))] : null,
          h('dt', {}, 'in own OU'), h('dd', {}, c.has_in_own_ou.map(p => PERM_LABEL[p] || p).join(', ') || 'no rights'),
          h('dt', {}, 'from outside'), h('dd', {}, u.outside ? h('span', {}, u.outside + ' ') : h('span', { class: 'warn' }, 'anywhere '),
            mine.has('account-lock') ? h('button', { type: 'button', id: 'outside-edit', onclick: () => {
              const input = h('input', { value: u.outside || '', placeholder: 'AS5610, 203.0.113.0/24, 2001:db8::/32', size: 50 });
              const m = modal(`${u.name} from outside`, h('div', { class: 'stack' },
                h('p', { class: 'dim' }, 'Networks this account may be used from through the edge (the public address): CIDR blocks or AS numbers (AS5610 is every network of that provider), comma separated. Empty: from anywhere.'),
                input), [
                h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
                h('button', { type: 'button', class: 'primary', onclick: () => { m.close(); act({ outside: input.value }, 'outside networks set'); } }, 'set'),
              ]);
              input.focus();
            } }, 'change') : null),
          h('dt', {}, 'mailbox'), mailboxOf(u, mine.has('directory-manage')),
          h('dt', {}, 'TOTP'), h('dd', {}, c.totp === null ? h('span', { class: 'dim' }, 'not known here (secrets kept elsewhere)') : c.totp ? ['on ',
            mine.has('password-set') ? h('button', { type: 'button', class: 'danger', onclick: () => confirmModal('reset TOTP', `Take ${u.name}'s TOTP off? Where their OU asks for a second factor, they set it up again at their next sign-in.`, async () => {
              try { await api('POST', '/api/admin/users/reset-totp', { id: u.id }); status('TOTP reset', 'ok'); await reloadAdmin(500); } catch (err) { status(err.message, 'bad'); }
            }) }, 'reset') : null] : 'off'),
          h('dt', {}, 'FIDO2 keys'), h('dd', {}, c.fido2 === null || c.fido2 === undefined ? h('span', { class: 'dim' }, 'not known here (secrets kept elsewhere)') : c.fido2 ? [`${c.fido2} `,
            mine.has('password-set') ? h('button', { type: 'button', class: 'danger', onclick: () => confirmModal('reset FIDO2 keys', `Take every key of ${u.name}'s off? Where FIDO2 is asked for (tier 0), they register one at their next sign-in.`, async () => {
              try { await api('POST', '/api/admin/users/reset-fido2', { id: u.id }); status('FIDO2 keys reset', 'ok'); await reloadAdmin(500); } catch (err) { status(err.message, 'bad'); }
            }) }, 'reset') : null] : 'none')),
          h('div', { class: 'row' },
            h('button', { type: 'button', class: 'danger', disabled: !mine.has('account-lock'), onclick: () => confirmModal(u.disabled ? 'enable' : 'disable', `${u.disabled ? 'Enable' : 'Disable'} ${u.name}?${u.disabled ? '' : ' Their sessions on this node end.'}`, () => act({ disabled: !u.disabled }, u.disabled ? 'enabled' : 'disabled')) }, u.disabled ? 'enable' : 'disable'),
            h('button', { type: 'button', disabled: !mine.has('password-set'), onclick: () => {
              const input = h('input', { type: 'password', minlength: 8, autocomplete: 'new-password' });
              const m = modal(`password of ${u.name}`, h('div', { class: 'stack' }, h('span', { class: 'lbl' }, 'new password (8+)'), input), [
                h('button', { type: 'button', onclick: () => m.close() }, 'cancel'),
                h('button', { type: 'button', class: 'primary', onclick: () => { m.close(); act({ password: input.value }, 'password set'); } }, 'set'),
              ]);
              input.focus();
            } }, 'set password'))),
        win('roles', roles.node, grantForm({ kind: 'user', id: u.id }, () => roles.refresh()),
          h('p', { class: 'dim' }, 'A role reaches the OU it is granted over and everything below it, data included. You can only grant what you hold there yourself.')),
        win('member of', groups.node, h('div', { class: 'row' }, h('span', { class: 'lbl' }, 'put into'), into.node,
          h('button', { type: 'button', onclick: () => { if (into.get()) member(into.get(), true); else status('pick a group', 'bad'); } }, 'add'))),
        win('access to', access.node, accessForm({ kind: 'user', id: u.id }, () => access.refresh()),
          h('p', { class: 'dim' }, 'Module instances this user may use: given directly, or through a group it is in.'))),
      win('recent authentication events (every node)', events),
      win('sessions (every node)', sessions.node, c.unreachable.length ? h('p', { class: 'warn' }, `no answer from ${c.unreachable.map(nodeLabel).join(', ')}`) : null));
  }).catch(err => box.replaceChildren(h('p', { class: 'bad' }, err.message)));
  return box;
}

function groupCard(id) {
  const box = h('div', {}, h('p', { class: 'dim' }, 'loading…'));
  remote('groups')({ filter: { groups: [[{ col: 'id', op: '=', vals: [id] }]] }, limit: 1 }).then(r => {
    const g = r.rows[0];
    if (!g) { box.replaceChildren(win('group', h('p', { class: 'bad' }, 'no such group (or not yours to see)'))); return; }
    let members = null;
    const member = async (user, on) => {
      try { await api('POST', '/api/admin/groups/member', { group: g.id, user, member: on }); status(on ? 'added' : 'taken out', 'ok'); members.refresh(); }
      catch (err) { status(err.message, 'bad'); }
    };
    members = grid({
      id: 'group-members',
      columns: [...USER_COLS, { key: 'act', label: '', cell: u => h('button', { type: 'button', onclick: () => member(u.id, false) }, 'take out') }],
      fetch: remote('users', { member_of: g.id }),
      sort: 'name',
      onRow: u => go(`/admin/users/${u.id}`),
      empty: 'no members',
    });
    const add = userPicker();
    const roles = grantsGrid('group-roles', { subject_group: g.id });
    const access = accessGrid('group-access', { subject_group: g.id });
    box.replaceChildren(
      h('div', { class: 'row' }, h('a', { href: '/admin/users', 'data-go': true }, '← users and groups')),
      h('div', { class: 'cols' },
        win(`group ${g.name}`, h('dl', { class: 'kv' },
          h('dt', {}, 'for'), h('dd', {}, g.display || '—'), h('dt', {}, 'OU'), h('dd', {}, g.ou_path), h('dt', {}, 'id'), h('dd', {}, h('code', {}, g.id))),
          members.node,
          h('div', { class: 'row' }, h('span', { class: 'lbl' }, 'add'), add.node,
            h('button', { type: 'button', onclick: () => { if (add.get()) member(add.get(), true); else status('pick a user', 'bad'); } }, 'add'))),
        win('roles of the group', roles.node, grantForm({ kind: 'group', id: g.id }, () => roles.refresh())),
        win('access to (for every member)', access.node, accessForm({ kind: 'group', id: g.id }, () => access.refresh()))));
  }).catch(err => box.replaceChildren(h('p', { class: 'bad' }, err.message)));
  return box;
}

// ---- logs: the settings, then the log as a table ----

function logsTab() {
  const ov = admin.ov;
  return h('div', {}, ov.logs_root ? logQuery() : null, ov.logs_root ? rulesWindows() : null, ov.cluster_admin ? logSettings() : null, ov.logs_read ? logView() : null);
}

// ---- D-U32, D-U33: rules over the logs, bans ----------------------------------------------

const RULE_NEW = { name: '', description: '', enabled: true, mode: 'aggregate', query: 'dataset = auth | filter event = "sign_in_failed" | stats count() as n by addr | filter n >= 10',
  window: '10m', every: '1m', key: ['addr'], suppress: '1h', severity: 'medium', ban: null };

function ruleForm(r, may, after) {
  const x = structuredClone(r);
  const isNew = !r.name;
  const inp = (k, ph) => h('input', { value: x[k] || '', placeholder: ph || '', oninput: e => { x[k] = e.target.value; } });
  const q = h('textarea', { class: 'ql', rows: 4, spellcheck: 'false', oninput: e => { x.query = e.target.value; } });
  q.value = x.query;
  const key = h('input', { value: (x.key || []).join(', '), placeholder: 'addr', oninput: e => { x.key = e.target.value.split(',').map(s => s.trim()).filter(Boolean); } });
  const banOn = h('input', { type: 'checkbox', checked: !!x.ban });
  const b = x.ban || { field: 'addr', time: '1h', max: '7d', factor: 2 };
  const binp = (k, ph) => h('input', { value: b[k], placeholder: ph, size: 8, oninput: e => { b[k] = k === 'factor' ? Number(e.target.value) : e.target.value; } });
  const banRow = h('div', { class: 'row', hidden: !x.ban }, 'address in', binp('field', 'addr'), 'for', binp('time', '1h'), 'growing ×', binp('factor', '2'), 'up to', binp('max', '7d'));
  banOn.onchange = () => { banRow.hidden = !banOn.checked; };
  const out = h('div', {});
  const msg = h('div', { class: 'msg' });
  const value = () => ({ ...x, ban: banOn.checked ? { ...b, factor: Number(b.factor) || 2 } : null });
  const box = modal(isNew ? 'a new rule' : `rule ${r.name}`, h('div', { class: 'stack' },
    h('span', { class: 'lbl' }, 'name'), inp('name', 'osada-something'),
    h('span', { class: 'lbl' }, 'what it is for'), inp('description'),
    h('label', { class: 'row' }, h('input', { type: 'checkbox', checked: x.enabled, onchange: e => { x.enabled = e.target.checked; } }), 'on'),
    h('div', { class: 'row' }, h('span', { class: 'lbl' }, 'mode'),
      select([['aggregate', 'aggregate: each result row over the window'], ['event', 'event: each new line']], x.mode, v => { x.mode = v; }),
      h('span', { class: 'lbl' }, 'severity'), select(['low', 'medium', 'high', 'critical'].map(s => [s, s]), x.severity, v => { x.severity = v; })),
    h('span', { class: 'lbl' }, 'query'), q,
    h('div', { class: 'row' }, 'window', inp('window', '10m'), 'every', inp('every', '1m'), 'one alert per', key, 'within', inp('suppress', '1h')),
    h('label', { class: 'row' }, banOn, 'ban the address (fail2ban)'), banRow,
    msg, out), [
    !isNew && may ? h('button', { type: 'button', class: 'danger', onclick: async () => {
      if (!confirm(`remove the rule ${r.name}?`)) return;
      try { await api('DELETE', `/api/admin/rules/${encodeURIComponent(r.name)}`); box.close(); after(); } catch (e) { msg.className = 'msg bad'; msg.textContent = e.message; }
    } }, 'remove') : null,
    h('button', { type: 'button', onclick: async () => {
      msg.className = 'msg'; msg.textContent = 'testing…';
      try {
        const t = await api('POST', '/api/admin/rules/test', value());
        msg.className = 'msg ok';
        msg.textContent = `${t.rows.length} rows over the window${t.note ? ' (' + t.note + ')' : ''}` + (t.bans.length ? `; ${t.bans.filter(z => z.what === 'would be banned').length} addresses would be banned` : '');
        out.replaceChildren(h('pre', { class: 'ql-help' }, [t.columns.join('\t'), ...t.rows.slice(0, 20).map(row => row.map(v => typeof v === 'object' ? JSON.stringify(v) : v).join('\t')),
          ...t.bans.slice(0, 20).map(z => `${z.addr}: ${z.what}`)].join('\n')));
      } catch (e) { msg.className = 'msg bad'; msg.textContent = e.message; }
    } }, 'test'),
    h('button', { type: 'button', onclick: () => box.close() }, 'cancel'),
    may ? h('button', { type: 'button', class: 'primary', onclick: async () => {
      try {
        await api('PUT', `/api/admin/rules/${encodeURIComponent(isNew ? x.name : r.name)}`, value());
        box.close(); after();
        status(`rule ${x.name} saved`);
      } catch (e) { msg.className = 'msg bad'; msg.textContent = e.message; }
    } }, 'save') : null,
  ], { cls: 'pickmodal' });
}

function rulesWindows() {
  const rulesBox = h('div', {}, h('p', { class: 'dim' }, 'loading…'));
  const bansBox = h('div', {});
  const alertsBox = h('div', {});
  const load = async () => {
    let v;
    try { v = await api('GET', '/api/admin/rules'); } catch (e) { rulesBox.replaceChildren(h('p', { class: 'bad' }, e.message)); return; }
    const st = v.states || {};
    const g = grid({
      id: 'rules', rows: v.rules.map(r => ({ ...r, ...(st[r.name] || {}) })), sort: 'name', empty: 'no rules',
      onRow: r => ruleForm(v.rules.find(x => x.name === r.name), v.may_change, load),
      columns: [
        { key: 'name', label: 'rule', cell: r => h('b', { title: r.description }, r.name) },
        { key: 'enabled', label: 'on', value: r => r.enabled ? 'on' : 'off', cell: r => h('span', { class: r.enabled ? 'ok' : 'dim' }, r.enabled ? 'on' : 'off') },
        { key: 'mode', label: 'mode' }, { key: 'severity', label: 'severity' },
        { key: 'window', label: 'window', value: r => `${r.window} / ${r.every}` },
        { key: 'ban', label: 'ban', value: r => r.ban ? `${r.ban.field}: ${r.ban.time} → ${r.ban.max}` : '—' },
        { key: 'last_run', label: 'last run', value: r => r.last_run || 0, cell: r => r.last_run ? age(r.last_run) : '—' },
        { key: 'fired', label: 'alerts', value: r => r.fired || 0 },
        { key: 'error', label: 'error', cell: r => r.error ? h('span', { class: 'bad cut', title: r.error }, r.error) : '' },
      ],
    });
    rulesBox.replaceChildren(g.node,
      h('div', { class: 'row' }, v.may_change ? h('button', { type: 'button', onclick: () => ruleForm(RULE_NEW, true, load) }, 'add a rule') : null,
        h('span', { class: 'dim' }, 'Rules run on the node that keeps the logs. ' + (v.may_change ? '' : 'Changing them takes ou-security at the root.'))));
    // bans
    const now = Date.now();
    const bg = grid({
      id: 'bans', rows: v.bans, sort: 'since', dir: 'desc', empty: 'nobody is banned',
      columns: [
        { key: 'addr', label: 'address' }, { key: 'rule', label: 'by' },
        { key: 'since', label: 'since', cell: b => new Date(b.since).toLocaleString() },
        { key: 'until', label: 'until', cell: b => `${new Date(b.until).toLocaleString()} (${Math.round((b.until - now) / 3600e3 * 10) / 10} h)` },
        { key: 'times', label: 'times' },
        { key: 'lift', label: '', value: () => '', cell: b => v.may_change ? h('button', { type: 'button', onclick: async e => {
          e.stopPropagation();
          try { await api('POST', '/api/admin/bans', { addr: b.addr, secs: 0 }); status(`${b.addr} unbanned`); load(); } catch (err) { status(err.message, 'bad'); }
        } }, 'lift') : '' },
      ],
    });
    const addr = h('input', { placeholder: '203.0.113.5' });
    let secs = 86400;
    const never = h('textarea', { rows: 3, spellcheck: 'false', placeholder: '198.18.0.182\n198.51.100.0/24' });
    never.value = (v.never_ban || []).join('\n');
    const nmsg = h('div', { class: 'msg' });
    bansBox.replaceChildren(bg.node,
      h('p', { class: 'dim' }, `Every node that answers the internet drops a banned address's connections, and refuses its requests through Cloudflare; ${v.refused} refused on this node since it started.`),
      v.may_change ? h('div', { class: 'row' }, 'ban', addr, 'for', select([[3600, '1 hour'], [86400, '1 day'], [7 * 86400, '7 days'], [30 * 86400, '30 days']], secs, x => { secs = Number(x); }),
        h('button', { type: 'button', onclick: async () => {
          try { await api('POST', '/api/admin/bans', { addr: addr.value.trim(), secs }); status(`${addr.value.trim()} banned`); load(); } catch (e) { status(e.message, 'bad'); }
        } }, 'ban')) : null,
      h('span', { class: 'lbl' }, 'never banned (one address or subnet a line), besides private ranges, Cloudflare\'s and the nodes\' addresses'), never,
      v.may_change ? h('div', { class: 'row' }, h('button', { type: 'button', onclick: async () => {
        try { await api('PUT', '/api/admin/never-ban', { list: never.value.split('\n').map(s => s.trim()).filter(Boolean) }); nmsg.className = 'msg ok'; nmsg.textContent = 'saved'; } catch (e) { nmsg.className = 'msg bad'; nmsg.textContent = e.message; }
      } }, 'save'), nmsg) : null);
    // the latest alerts
    try {
      const r = await api('POST', '/api/admin/logs/ql', { q: 'dataset = alerts | fields _time, rule, severity, key, note, detail | limit 50', from: now - 30 * 86400e3, to: now + 1000, limit: 50 });
      const rows = r.rows.map(x => Object.fromEntries(r.columns.map((c, i) => [c, x[i]])));
      alertsBox.replaceChildren(grid({ id: 'alerts', rows, sort: '_time', dir: 'desc', empty: 'no alerts in 30 days',
        columns: [{ key: '_time', label: 'when', cell: a => new Date(a._time).toLocaleString() }, { key: 'rule', label: 'rule' }, { key: 'severity', label: 'severity' },
          { key: 'key', label: 'key' }, { key: 'note', label: 'note', value: a => a.note || '' }, { key: 'detail', label: 'what', cell: a => h('span', { class: 'cut', title: a.detail }, a.detail) }] }).node);
    } catch (e) { alertsBox.replaceChildren(h('p', { class: 'bad' }, e.message)); }
  };
  load();
  return h('div', {}, win('rules over the logs (alerts, fail2ban)', rulesBox), win('bans', bansBox), win('alerts (30 days)', alertsBox));
}

// ---- D-U31: the cluster's logs, queried with sift's pipes -----------------------------

const QL_RANGES = [['15m', 'last 15 minutes', 15 * 60e3], ['1h', 'last hour', 3600e3], ['24h', 'last 24 hours', 86400e3],
  ['7d', 'last 7 days', 7 * 86400e3], ['30d', 'last 30 days', 30 * 86400e3], ['365d', 'last year', 365 * 86400e3]];
const QL_TIME_COLS = ['_time', '_ingested', 'until', 'earliest', 'latest'];
const QL_EXAMPLES = [
  ['requests that failed, by address', 'dataset = http | filter status >= 400 | stats count() as n, dc(path) as paths, values(path) as sample by client | sort n desc | limit 50'],
  ['sign-ins that failed', 'dataset = auth | filter event = "sign_in_failed" | fields _time, _node, name, addr, why'],
  ['requests per minute', 'dataset = http | timechart span=1m count() as requests'],
  ['the busiest paths', 'dataset = http | top 20 path'],
  ['file changes', 'dataset = files | filter op != "read" | fields _time, user, op, path, to, how'],
  ['administrative changes', 'dataset = admin | fields _time, actor_name, event, addr'],
];

function qlTime(v) {
  return typeof v === 'number' && v > 1e11 ? new Date(v).toLocaleString() : v;
}

function qlBars(cols, rows) {
  // a timechart: bars of the first number column per bucket
  const vi = cols.findIndex((c, i) => i > 0 && rows.some(r => typeof r[i] === 'number'));
  if (cols[0] !== '_time' || vi < 0 || !rows.length) return null;
  const by = new Map();
  for (const r of rows) by.set(r[0], (by.get(r[0]) || 0) + (Number(r[vi]) || 0));
  const pts = [...by.entries()].sort((a, b) => a[0] - b[0]);
  const max = Math.max(...pts.map(p => p[1]), 1);
  const w = 900, hgt = 120, bw = Math.max(1, Math.floor(w / pts.length) - 1);
  const ns = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(ns, 'svg');
  svg.setAttribute('viewBox', `0 0 ${w} ${hgt}`);
  svg.setAttribute('class', 'ql-bars');
  pts.forEach(([t, v], i) => {
    const r = document.createElementNS(ns, 'rect');
    const bh = Math.max(1, Math.round(v / max * (hgt - 4)));
    r.setAttribute('x', i * (bw + 1)); r.setAttribute('y', hgt - bh); r.setAttribute('width', bw); r.setAttribute('height', bh);
    const tt = document.createElementNS(ns, 'title');
    tt.textContent = `${new Date(t).toLocaleString()}: ${v}`;
    r.append(tt);
    svg.append(r);
  });
  return h('div', {}, svg, h('div', { class: 'dim' }, `${cols[vi]}: at most ${max}`));
}

function qlSaved(v) {
  try {
    if (v === undefined) return JSON.parse(localStorage.getItem('osada:logs:ql') || 'null');
    localStorage.setItem('osada:logs:ql', JSON.stringify(v));
  } catch { return null; }
}

function logQuery() {
  const saved = qlSaved() || {};
  const ta = h('textarea', { class: 'ql', rows: 3, spellcheck: 'false', autocomplete: 'off', placeholder: QL_EXAMPLES[0][1] });
  ta.value = saved.q || 'dataset = http | limit 100';
  let range = saved.range || '24h';
  const custom = { from: h('input', { type: 'datetime-local' }), to: h('input', { type: 'datetime-local' }) };
  const msg = h('div', { class: 'msg' });
  const out = h('div', { id: 'ql-out' });
  const help = h('div', { class: 'dim' }, 'loading the datasets…');
  const span = () => {
    if (range === 'custom') {
      const f = Date.parse(custom.from.value), t = custom.to.value ? Date.parse(custom.to.value) : Date.now();
      return [isNaN(f) ? Date.now() - 86400e3 : f, isNaN(t) ? Date.now() : t];
    }
    const r = QL_RANGES.find(x => x[0] === range) || QL_RANGES[2];
    return [Date.now() - r[2], Date.now() + 1000];
  };
  const run = async () => {
    const q = ta.value.trim();
    if (!q) return;
    qlSaved({ q, range });
    msg.className = 'msg'; msg.textContent = 'asking…';
    const [from, to] = span();
    try {
      const r = await api('POST', '/api/admin/logs/ql', { q, from: Math.floor(from), to: Math.ceil(to), limit: 1000 });
      const cols = r.columns, rows = r.rows.map(x => Object.fromEntries(cols.map((c, i) => [c, x[i]])));
      msg.className = 'msg ok';
      msg.textContent = `${r.rows.length} ${r.kind === 'events' ? 'lines' : 'rows'} in ${r.ms} ms (kept at ${r.authority})`;
      const g = grid({
        id: 'ql:' + (q.match(/^\s*dataset\s*(?:=|in)\s*\(?\s*([a-z_*, ]+)/i) || [, '*'])[1].replace(/\s/g, ''),
        rows, sort: null, pageSize: 100, empty: 'nothing in this range',
        columns: cols.map(c => ({
          key: c, label: c,
          value: x => x[c] == null ? '' : typeof x[c] === 'object' ? JSON.stringify(x[c]) : x[c],
          cell: x => {
            const v = x[c];
            const text = v == null ? '' : QL_TIME_COLS.includes(c) ? qlTime(v) : typeof v === 'object' ? JSON.stringify(v) : String(v);
            const span = h('span', { class: c === '_raw' ? 'cut' : null, title: c === '_raw' ? text : 'double-click: filter on it (shift: without it)' }, text);
            if (v != null && c !== '_raw' && typeof v !== 'object') {
              span.ondblclick = e => {
                const lit = typeof v === 'number' ? String(v) : JSON.stringify(String(v));
                ta.value = `${ta.value.trim()} | filter ${c} ${e.shiftKey ? '!=' : '='} ${lit}`;
                run();
              };
            }
            return span;
          },
        })),
      });
      out.replaceChildren(r.kind === 'timechart' ? (qlBars(cols, r.rows) || '') : '', g.node);
    } catch (e) {
      msg.className = 'msg bad'; msg.textContent = e.message;
    }
  };
  ta.addEventListener('keydown', e => { if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); run(); } });
  const pick = select([...QL_RANGES.map(r => [r[0], r[1]]), ['custom', 'from … to …']], range, v => {
    range = v; customRow.hidden = v !== 'custom';
  });
  const customRow = h('span', { class: 'row', hidden: range !== 'custom' }, 'from', custom.from, 'to', custom.to);
  const examples = select([['', 'examples…'], ...QL_EXAMPLES.map((x, i) => [String(i), x[0]])], '', v => {
    if (v !== '') { ta.value = QL_EXAMPLES[Number(v)][1]; run(); }
  });
  api('GET', '/api/admin/logs/info').then(info => {
    const sizes = info.sizes || {};
    help.replaceChildren(
      h('p', {}, `The cluster's logs are kept at ${info.authority || '—'} (the directory's authority). Each line has _time, _node (where it happened), _ingested (when it came) and _raw (all of it, for search); a dataset adds its own fields:`),
      h('dl', { class: 'kv ql-datasets' }, ...Object.entries(info.datasets).flatMap(([n, cs]) => [
        h('dt', {}, n, sizes[n] ? h('span', { class: 'dim' }, ` ${sizes[n].rows} lines`) : null),
        h('dd', {}, cs.filter(c => !c.name.startsWith('_')).map(c => `${c.name} (${c.type})`).join(', ')),
      ])),
      h('pre', { class: 'ql-help' }, [
        'dataset = NAME | dataset in (A, B) | dataset = *       the first stage; left out: every dataset',
        'filter EXPR        = != < <= > >=, and or not, + - * /, contains, startswith, endswith,',
        '                   ~= regex (!~=), in (…), insubnet "10.0.0.0/8, fd00::/8", not in / not contains …',
        'search "text"      any case, anywhere in the line',
        'fields A, B        alter NAME = EXPR, …     dedup A, B     sort A desc, B     limit N',
        'stats count() as n, dc(x), sum(x), avg(x), min, max, median, p90, p95, p99, percentile(x, 75),',
        '      values(x), first(x), last(x), earliest(), latest(), stdev(x), range(x) by FIELD, …',
        'timechart span=5m count() by FIELD          top 10 FIELD by FIELD',
        'functions: lower upper len trim concat coalesce if round abs floor ceil tostring tonumber toint',
        '           replace(x, re, with) extract(x, re) json(_raw, "key") hour day weekday time(_time) ago(1h) bin(_time, 5m)',
        '_time also compares with "2026-11-17T06:00:00Z"; strings "…" or \'…\', a backslash keeps the next character',
      ].join('\n')));
  }).catch(e => help.replaceChildren(h('span', { class: 'bad' }, e.message)));
  return win('query the cluster\'s logs',
    ta,
    h('div', { class: 'row' }, h('button', { class: 'primary', type: 'button', onclick: run }, 'run'), h('span', { class: 'dim' }, 'ctrl+enter'), pick, customRow, examples, msg),
    out,
    h('details', {}, h('summary', {}, 'how to ask (sift\'s pipes)'), help));
}

function logSettings() {
  const s = structuredClone(admin.ov.settings);
  const msg = h('div', { class: 'msg' });
  const caution = h('span', {});
  const showCaution = () => {
    const keep = s.logs.auth.retention_days;
    const over = s.session_days * 10 > keep * 9;
    caution.className = over ? 'caution' : 'dim';
    caution.textContent = over
      ? `sessions (${s.session_days} d) outlive 90 % of the auth log's ${keep} d: their users are cautioned`
      : `sessions (${s.session_days} d) stay within 90 % of the auth log's ${keep} d`;
  };
  showCaution();
  return win('log settings (the same on every node)',
    h('div', { class: 'row' },
      LOGS.filter(k => s.logs[k]).map(k => h('span', { class: 'row' }, h('span', { class: 'lbl', title: k }, LOG_LABEL[k] || k),
        select(LEVELS.map(l => [l, l, l === 'off' && (k === 'auth' || k === 'admin')]), s.logs[k].verbosity, v => { s.logs[k].verbosity = v; }),
        h('input', { type: 'number', min: 1, value: s.logs[k].retention_days, onchange: e => { s.logs[k].retention_days = Number(e.target.value); showCaution(); } }), 'd')),
      h('span', { class: 'row' }, h('span', { class: 'lbl' }, 'sessions'),
        h('input', { type: 'number', min: 1, value: s.session_days, onchange: e => { s.session_days = Number(e.target.value); showCaution(); } }), 'd'),
      h('button', { class: 'primary', type: 'button', onclick: async () => {
        try { await api('POST', '/api/admin/settings', s); msg.className = 'msg ok'; msg.textContent = 'saved'; await reloadAdmin(400); }
        catch (err) { msg.className = 'msg bad'; msg.textContent = err.message; }
      } }, 'save'), msg),
    caution,
    shipSettings(s));
}

/// T3: the logs out as JSON over HTTPS POST; the secret is write-only.
function shipSettings(s) {
  s.ship = s.ship || { enabled: false, url: '', secret: '', logs: [] };
  s.ship.secret = '';
  const set = admin.ov.ship_secret_set;
  const status = h('div', { class: 'dim' }, 'each node ships its own lines …');
  api('GET', '/api/admin/nodes/info').then(info => {
    const rows = Object.entries(info).map(([id, i]) => {
      const st = i && i.ship;
      const when = t => t ? new Date(t).toLocaleString() : 'never';
      const text = !st ? (i && i.error ? 'no answer' : 'not running (older version)')
        : st.state === 'off' ? 'off'
        : st.state === 'ok' ? `ok, ${st.sent} lines sent, last ${when(st.last_ok)}` + (Object.values(st.backlog || {}).some(n => n) ? `, behind: ${Object.entries(st.backlog).map(([k, n]) => `${k} ${n}`).join(', ')}` : '')
        : `failing since ${when(st.last_ok)}: ${st.error}`;
      return h('div', {}, h('b', {}, nodeLabel(id)), ' ', h('span', { class: !st || st.state === 'off' ? 'dim' : st.state === 'ok' ? 'ok' : 'bad' }, text));
    });
    status.replaceChildren(...rows);
  }).catch(() => { status.textContent = ''; });
  return h('div', { class: 'ship' },
    h('h4', {}, 'ship the logs out (JSON over HTTPS POST)'),
    h('div', { class: 'row' },
      h('label', {}, check(s.ship.enabled, v => { s.ship.enabled = v; }), ' ship'),
      ['auth', 'admin'].map(k => h('label', {}, check(s.ship.logs.includes(k), v => {
        s.ship.logs = v ? [...new Set([...s.ship.logs, k])] : s.ship.logs.filter(x => x !== k); }), ` ${k}`))),
    h('div', { class: 'row' }, h('span', { class: 'lbl' }, 'URL'),
      h('input', { type: 'url', size: 50, placeholder: 'https://logs.example.com/ingest', value: s.ship.url, oninput: e => { s.ship.url = e.target.value.trim(); } })),
    h('div', { class: 'row' }, h('span', { class: 'lbl' }, 'secret'),
      h('input', { type: 'password', size: 30, autocomplete: 'new-password', placeholder: set ? 'set (type to replace)' : 'none (sent as a bearer token)',
        oninput: e => { s.ship.secret = e.target.value; } }),
      set ? h('label', {}, check(false, v => { s.ship.clear_secret = v; }), ' forget it') : null),
    h('div', { class: 'dim' }, 'Every node POSTs its own new lines every 15 seconds as a JSON array of {"@timestamp", id, node, node_name, log, seq, actor, event}, with Authorization: Bearer <secret>. A new URL gets the whole retained log; id tells repeats apart. The secret is never shown back.'),
    status);
}

/// A log entry as a table row.
function flatLine(l) {
  const e = l.event || {};
  const detail = [e.why, e.role ? `${e.role} @ ${e.scope}` : null, e.ou && !e.role ? e.ou : null, e.ended ? `ended ${String(e.ended).slice(0, 8)}` : null,
    e.changes ? (Array.isArray(e.changes) ? e.changes.join('; ') : JSON.stringify(e.changes)) : null, e.factors ? `factors ${e.factors.join(', ')}` : null,
    e.to ? `to ${e.to}` : null].filter(Boolean).join(' · ');
  return {
    time: l.time, node: l.node, event: e.event || '', seq: l.seq,
    user: e.name || e.group_name || (e.user ? String(e.user).slice(0, 8) : ''),
    actor: e.actor_name || '', session: e.sid || '', from: e.addr || '', detail, ou: e.ou || e.scope || '', role: e.role || '', raw: e,
  };
}

/// The log as a grid: `source` is { rows } (a few, here) or { cursor }
/// (the nodes page on, newest first).
function logGrid(id, source, filter, extras) {
  let g = null;
  g = grid({
    id,
    columns: [
      { key: 'time', label: 'age', type: 'date', cell: r => h('span', { title: when(r.time) }, age(r.time)) },
      { key: 'node', label: 'node', cell: r => nodeLabel(r.node) },
      { key: 'event', label: 'event', type: 'enum', options: EVENTS, cell: r => h('span', { class: r.event === 'sign_in_failed' ? 'bad' : null }, r.event) },
      { key: 'user', label: 'user' },
      { key: 'actor', label: 'by' },
      { key: 'session', label: 'session', cell: r => r.session ? short(r.session) : '' },
      { key: 'from', label: 'from' },
      { key: 'detail', label: 'detail' },
      { key: 'ou', label: 'OU', hidden: true },
      { key: 'role', label: 'role', hidden: true },
    ],
    ...source, filter, extras,
    sort: source.rows ? 'time' : null, dir: 'desc',
    rowClass: r => r.event === 'sign_in_failed' ? 'failed' : '',
    onRow: r => {
      const narrow = (col, v) => { m.close(); if (source.cursor) g.setFilter([[{ col, op: '=', vals: [v] }]]); else go(`/admin/logs?kind=auth&${col}=${encodeURIComponent(v)}`); };
      const m = modal(`${r.event} · ${when(r.time)} · ${r.node} #${r.seq}`,
        h('table', { class: 'grid' }, Object.entries(r.raw).map(([k, v]) => h('tr', {}, h('th', {}, k), h('td', {}, typeof v === 'object' ? JSON.stringify(v) : String(v))))), [
          r.session ? h('button', { type: 'button', onclick: () => narrow('session', r.session) }, 'everything of this session') : null,
          r.user ? h('button', { type: 'button', onclick: () => narrow('user', r.user) }, 'everything about this user') : null,
          h('button', { type: 'button', class: 'primary', onclick: () => m.close() }, 'close'),
        ]);
    },
    empty: 'no entries',
  });
  return g;
}

function logView() {
  const params = new URLSearchParams(location.search);
  let kind = params.get('kind') === 'admin' ? 'admin' : 'auth';
  let since = '';
  const groups = [];
  for (const col of ['session', 'user', 'actor', 'event', 'node', 'from']) {
    for (const v of params.getAll(col)) {
      if (!groups.length) groups.push([]);
      groups[0].push({ col, op: '=', vals: [v] });
    }
  }
  const note = h('span', { class: 'warn' });
  let g = null;
  const SINCE = [['', 'any time'], ['3600', 'last hour'], ['86400', 'last day'], ['604800', 'last week'], ['2592000', 'last 30 days']];
  const extras = [
    select([['auth', 'auth log'], ['admin', 'admin log']], kind, v => { kind = v; g.refresh(); }),
    select(SINCE, since, v => { since = v; g.refresh(); }),
    note,
  ];
  g = logGrid('logs', { cursor: async q => {
    const body = { kind, filter: q.filter, before: q.before, limit: q.limit };
    if (since) body.since = Date.now() - Number(since) * 1000;
    const r = await api('POST', '/api/admin/logs/query', body);
    note.textContent = r.unreachable.length ? `no answer from ${r.unreachable.join(', ')}` : '';
    return { rows: r.lines.map(flatLine), next: r.next };
  } }, groups.length ? { groups } : null, extras);
  return win('logs of every node, newest first (the filter runs on the nodes; click an entry for all of it)', g.node);
}

route();
