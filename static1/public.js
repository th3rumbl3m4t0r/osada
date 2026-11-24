/* public links: /s/<token> and /s/<token>/p/<path>. Every navigation is a
   plain page load, so the pages cache well in front of a proxy. */
'use strict';

(async function () {
  await LANG_READY;   // the texts of the language in use (i18n.js)
  const m = /^\/s\/([^/]+)(?:\/p(\/.*))?$/.exec(location.pathname);
  const main = $('#app');
  if (!m) { main.appendChild(h('p', { class: 'bad', text: tx('not a link') })); return; }
  const tok = m[1];
  const sub = '/' + splitPath(m[2] || '').map(s => { try { return decodeURIComponent(s); } catch (e) { return s; } }).join('/');
  const encP = p => { const e = encPath(p); return e ? '/' + e : ''; };
  const base = '/s/' + tok;
  const pageUrl = p => (splitPath(p).length ? base + '/p' + encP(p) : base);
  const rawUrl = p => base + '/raw' + encP(p);
  const setStatus = t => { $('#status').textContent = t || ''; };
  // without home (util.js) this is asked now and then; a page of a link is the same either way
  OFF.ping = base + '/info';
  OFF.changed = on => { if (!on) toast(tx('osada at home answers again')); };

  async function get(url) {
    // with cookies: an SSL-inspecting proxy (Zscaler) keeps its own on this site and
    // refuses requests without them (403 that never reaches osada); /s/ ignores ours
    let r;
    try { r = await fetch(url, { credentials: 'same-origin' }); } catch (e) { setOffline(true); throw new Error(tx('server unreachable')); }
    heard(r);
    if (r.status === 404) throw new Error('404');
    if (!r.ok) throw new Error(offline() ? tx('not kept on this device') : r.status + ' ' + r.statusText);
    return r.json();
  }
  function frame(titleKids, body, cls) {
    return h('section', { class: 'win ' + cls }, h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), titleKids), body);
  }
  function crumbs(root, p, dirLink) {
    const segs = splitPath(p);
    const out = [segs.length ? h('a', { href: base, text: root }) : h('span', { class: 'here', text: root })];
    let acc = '';
    segs.forEach((s, i) => {
      acc += '/' + s;
      out.push(h('span', { class: 'dim', text: ' / ' }));
      out.push(i === segs.length - 1 && !dirLink ? h('span', { class: 'here', text: s }) : h('a', { href: pageUrl(acc), text: s }));
    });
    return h('span', { class: 'crumbs' }, out);
  }

  let ls, e = null;
  busy(true);
  try { ls = await get(base + '/ls' + encP(sub)); } catch (x) {
    e = x;
    // without home a file is also known from its folder's listing
    const up = offline() && splitPath(sub).length ? await get(base + '/ls' + encP(parentPath(sub))).catch(() => null) : null;
    const f = up && (up.entries || []).find(x => !x.dir && x.name === baseName(sub));
    if (f) ls = { path: sub, root_name: up.root_name, owner: up.owner, file: f };
  }
  if (!ls) {
    busy(false);
    document.title = tx('not found') + ' · osada';
    main.appendChild(frame('osada', h('div', { class: 'body' }, h('p', { class: e.message === '404' ? 'dim' : 'bad',
      text: e.message === '404' ? tx('this link does not exist or has expired.') : tx('error: {msg}', { msg: txErr(e.message) }) })), 'narrow'));
    return;
  }
  busy(false);
  const shared = h('span', { class: 'dim', text: tx('shared by {name}', { name: ls.owner }) });

  if (ls.file) {
    const f = ls.file, inFolder = splitPath(sub).length > 0, dir = parentPath(sub);
    let siblings = null;
    if (inFolder) {
      try { siblings = (await get(base + '/ls' + encP(dir))).entries.filter(e => !e.dir).map(e => e.name); } catch (e) { /* no prev / next */ }
    }
    const v = viewer({
      name: f.name, raw: rawUrl(sub), siblings,
      rawOf: n => rawUrl(joinPath(dir, n)),
      md: () => get(base + '/md' + encP(sub)).then(r => r.html),
    });
    const go = n => { if (n) location.href = pageUrl(joinPath(dir, n)); };
    let nav = null;
    if (v.prev || v.next) {
      const p = h('button', { text: tx('< prev'), title: tx('previous (left arrow)'), onclick: () => go(v.prev) });
      const n = h('button', { text: tx('next >'), title: tx('next (right arrow)'), onclick: () => go(v.next) });
      p.disabled = !v.prev;
      n.disabled = !v.next;
      nav = [p, n];
    }
    main.className = 'fill';
    main.appendChild(frame([inFolder ? crumbs(ls.root_name, sub, false) : h('span', { class: 'crumbs' }, h('span', { class: 'here', text: f.name })),
      h('span', { class: 'spacer' }), nav, shared,
      h('a', { class: 'btn-link', href: rawUrl(sub) + '?dl=1', download: '', text: tx('download') })], v.el, 'viewwin'));
    document.title = f.name + ' · osada';
    setStatus(tx('{size}, modified {time}', { size: fmtSize(f.size), time: fmtTime(f.mtime) }));
    document.addEventListener('keydown', e => {
      if (e.ctrlKey || e.metaKey || e.altKey || e.target instanceof HTMLInputElement || e.target instanceof HTMLMediaElement) return;
      if (e.key === 'ArrowLeft' && v.prev) go(v.prev);
      else if (e.key === 'ArrowRight' && v.next) go(v.next);
      else if (e.key === 'Backspace' && inFolder) location.href = pageUrl(dir);
    });
    return;
  }

  const list = ls.entries.slice().sort((a, b) => (b.dir - a.dir) || nameCmp(a.name, b.name));
  // without home: what can be opened here
  const kept = offline() ? await keptHere() : null;
  const rows = list.map(e => {
    const p = joinPath(sub, e.name);
    const gone = kept && !(e.dir ? kept.lists.has(base + '/ls' + encP(p)) : kept.files.has(rawUrl(p)));
    return h('tr', { class: (e.dir ? 'd' : 'f') + (gone ? ' gone' : ''), title: gone ? tx('not kept on this device') : null },
      h('td', { class: 'nm' }, h('a', { href: pageUrl(p), text: e.name + (e.dir ? '/' : '') })),
      h('td', { class: 'sz', text: e.dir ? '' : fmtSize(e.size) }),
      h('td', { class: 'tm', text: fmtTime(e.mtime) }),
      h('td', { class: 'act' }, e.dir ? null : h('a', { class: 'btn-link', href: rawUrl(p) + '?dl=1', download: '', text: tx('download') })));
  });
  if (splitPath(sub).length) {
    rows.unshift(h('tr', { class: 'up' }, h('td', { class: 'nm' }, h('a', { href: pageUrl(parentPath(sub)), text: '../' })), h('td'), h('td'), h('td')));
  }
  const body = h('div', { class: 'listwrap' },
    h('table', { class: 'x11 files' }, h('thead', {}, h('tr', {}, h('th', { class: 'nm', text: tx('name') }), h('th', { class: 'sz', text: tx('size') }), h('th', { class: 'tm', text: tx('modified') }), h('th'))),
      h('tbody', {}, rows)),
    list.length ? null : h('div', { class: 'empty dim', text: tx('empty folder') }));
  main.className = 'fill';
  main.appendChild(frame([crumbs(ls.root_name, sub, true), h('span', { class: 'spacer' }), shared], body, 'filewin'));
  document.title = (splitPath(sub).length ? baseName(sub) : ls.root_name) + ' · osada';
  const files = list.filter(e => !e.dir);
  setStatus(txn(list.length - files.length, '{n} folder', '{n} folders') + ', ' + txn(files.length, '{n} file', '{n} files') + ', ' + fmtSize(files.reduce((a, e) => a + e.size, 0)));
  document.addEventListener('keydown', e => {
    if (e.key === 'Backspace' && splitPath(sub).length && !(e.target instanceof HTMLInputElement)) location.href = pageUrl(parentPath(sub));
  });
})();
