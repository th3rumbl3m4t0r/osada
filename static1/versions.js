/* Earlier versions from the backup's snapshots: the dialog and the version page. Part versions (util.js): loaded when the page opens, it
   uses the app's helpers (app.js, util.js) as globals. */
'use strict';

part('versions', () => {

/* ---- earlier versions, from the backup's snapshots (src/versions.rs). Of a file: what it was,
   once per content. Of a folder: the folder as a backup has it, to look through and restore
   from, deleted files too. Gives true when something was put back. ---- */
function versionsDialog(loc, path) {
  const box = h('div', { class: 'versions' }, h('div', { class: 'dim', text: tx('loading…') }));
  const name = baseName(path) || tx('home');
  const when = s => fmtTime(s.time);
  let put = false, close = null;
  const open = (snap, p) => { close(null); navigate(U.version(snap, loc, p)); };
  const dl = (snap, p) => h('a', { class: 'btn-link', href: U.snapRaw(snap, loc, p) + '?dl=1', download: '', text: tx('download') });
  /* a restore button, off while it runs */
  function putBtn(snap, p, after) {
    const b = btn(tx('restore'), tx('put this version back'), async () => {
      b.disabled = true;
      try { if (await restoreVersion(loc, p, snap)) { put = true; after(); } } catch (e) { fail(e); } finally { b.disabled = false; }
    });
    return b;
  }

  function fileList(v) {
    const list = v.versions.filter(x => !x.dir);
    if (!list.length) {
      box.replaceChildren(h('div', { class: 'dim', text: v.snapshots.length ? tx('no backup has {name}', { name }) : tx('there are no backups yet') }));
      return;
    }
    const rows = list.map(x => h('tr', { class: x.same ? 'off' : null },
      h('td', { class: 'num', text: fmtTime(x.mtime) }),
      h('td', { class: 'num', text: fmtSize(x.size) }),
      h('td', { class: 'dim', text: x.snaps.map(when).join(', ') }),
      h('td', { class: 'act' }, x.same ? h('span', { class: 'tag', text: tx('as now') }) : null,
        btn(tx('open'), null, () => open(x.snaps[0].name, path)), dl(x.snaps[0].name, path),
        x.same ? null : putBtn(x.snaps[0].name, path, () => load().catch(fail)))));
    box.replaceChildren(
      h('div', { class: 'dim', text: v.now ? tx('now: {size}, modified {time}', { size: fmtSize(v.now.size), time: fmtTime(v.now.mtime) }) : tx('{name} is not there now', { name }) }),
      h('table', { class: 'x11' },
        h('thead', {}, h('tr', {}, h('th', { text: tx('modified') }), h('th', { text: tx('size') }), h('th', { text: tx('in the backups of') }), h('th'))),
        h('tbody', {}, rows)));
  }

  function folder(v) {
    if (!v.snapshots.length) { box.replaceChildren(h('div', { class: 'dim', text: tx('there are no backups yet') })); return; }
    const has = v.versions.filter(x => x.dir);
    const sel = h('select', { title: tx('which backup') }, v.snapshots.map(s => {
      const x = has.find(y => y.snaps[0].name === s.name);
      return h('option', { value: s.name, text: tx('backup of {time}', { time: when(s) }) + (x ? ': ' + txn(x.files, '{n} file', '{n} files') + ', ' + fmtSize(x.size) : '') });
    }));
    sel.value = has.length ? has[0].snaps[0].name : v.snapshots[0].name;
    const where = h('div', { class: 'pick-path' });
    const list = h('div', { class: 'vlist' });
    let cur = path, seq = 0;
    const bAll = h('span');
    sel.addEventListener('change', () => show(cur));
    async function show(p) {
      const n = ++seq;
      cur = p;
      where.textContent = tx('home') + (p === '/' ? '/' : p + '/');
      bAll.replaceChildren(p === '/' ? '' : putBtn(sel.value, p, () => show(cur)));
      list.replaceChildren(h('div', { class: 'dim', text: tx('loading…') }));
      let r;
      try { r = await api('GET', U.snapLs(sel.value, loc, p)); } catch (e) {
        if (n === seq) list.replaceChildren(h('div', { class: e.status === 404 ? 'dim' : 'bad', text: e.status === 404 ? tx('this backup does not have this folder') : e.message }));
        return;
      }
      if (n !== seq) return;
      const state = e => e.now === 'gone' ? h('span', { class: 'tag warn', text: tx('deleted since') })
        : e.now === 'changed' ? h('span', { class: 'tag', text: tx('changed since') }) : e.now === 'same' ? h('span', { class: 'tag', text: tx('as now') }) : null;
      const rows = r.entries.sort((a, b) => (b.dir - a.dir) || nameCmp(a.name, b.name)).map(e => {
        const ep = joinPath(p, e.name);
        return h('tr', { class: e.now === 'same' ? 'off' : null },
          h('td', { class: 'nm' }, e.dir ? h('a', { href: '#', text: e.name + '/', onclick: ev => { ev.preventDefault(); show(ep); } }) : e.name, state(e)),
          h('td', { class: 'num', text: fmtSize(e.size), title: e.dir ? txn(e.files, '{n} file', '{n} files') : null }),
          h('td', { class: 'num', text: fmtTime(e.mtime) }),
          h('td', { class: 'act' }, e.dir ? null : [btn(tx('open'), null, () => open(sel.value, ep)), dl(sel.value, ep)],
            e.now === 'same' ? null : putBtn(sel.value, ep, () => show(cur))));
      });
      if (p !== '/') rows.unshift(h('tr', {}, h('td', { class: 'nm', colspan: 4 }, h('a', { href: '#', text: '../', title: tx('up'), onclick: ev => { ev.preventDefault(); show(parentPath(p)); } }))));
      list.replaceChildren(rows.length ? h('table', { class: 'x11' }, h('tbody', {}, rows)) : h('div', { class: 'dim', text: tx('empty folder') }));
    }
    box.replaceChildren(h('div', { class: 'row' }, sel, h('span', { class: 'spacer' }), bAll), where, list);
    show(path);
  }

  async function load() {
    const v = await api('GET', U.versions(loc, path));
    const dir = v.now ? v.now.dir : v.versions.length ? v.versions[0].dir : true;
    if (dir) folder(v); else fileList(v);
  }
  return dialog({
    title: tx('earlier versions of {name}', { name }), wide: true, body: box,
    buttons: [{ label: tx('close'), value: true, primary: true }],
    opened: (w, done) => { close = done; load().catch(e => box.replaceChildren(h('div', { class: 'bad', text: e.message }))); },
  }).then(() => put);
}

/* puts back what backup `snap` has at path: in its place when that is free, else next to it, or a
   file over the one there now. Asks first when something is there. Gives where it went, or null. */
async function restoreVersion(loc, path, snap) {
  const v = await api('GET', U.versions(loc, path));
  const name = baseName(path), ver = v.versions.find(x => x.snaps.some(s => s.name === snap));
  if (!ver) throw new Error(tx('this backup does not have {name}', { name }));
  const time = fmtTime(ver.snaps.find(s => s.name === snap).time);
  let replace = false;
  if (v.now) {
    const files = !ver.dir && !v.now.dir;
    const text = files
      ? tx('{name} is there now. put the one from the backup of {time} next to it, or in its place?', { name, time }) +
        (v.versions.some(x => x.same) ? '' : ' ' + tx('the file as it is now is in no backup: replacing it loses it.'))
      : tx('{name} is there now. the one from the backup of {time} goes next to it.', { name, time });
    const a = await dialog({
      title: tx('restore {name}', { name }), body: h('div', { class: 'dlg-text', text }),
      buttons: [{ label: tx('cancel'), value: null }].concat(files ? [{ label: tx('replace'), value: 'replace', danger: true }] : [],
        [{ label: tx('next to it'), value: 'copy', primary: true }]),
    });
    if (!a) return null;
    replace = a === 'replace';
  }
  busy(true);
  setStatus(tx('restoring {name}…', { name }));
  try {
    const r = await api('POST', '/api/restore', { loc, path, snap, replace });
    toast(tx('restored: {path}', { path: r.path }));
    return r.path;
  } finally { busy(false); setStatus(''); }
}

/* ---- a file as a backup has it: /version/<snap>/<loc>/<path> ---- */
async function versionPage(snap, loc, path) {
  const o = await api('GET', U.versions(loc, path));
  const name = baseName(path), dir = parentPath(path), raw = U.snapRaw(snap, loc, path);
  const ver = o.versions.find(x => !x.dir && x.snaps.some(s => s.name === snap));
  if (!ver) throw new Error(tx('this backup does not have {name}', { name }));
  const when = fmtTime(ver.snaps.find(s => s.name === snap).time);
  const v = viewer({
    name, raw, siblings: null, rawOf: () => raw,
    md: async () => {
      const r = await fetchText(raw, TEXT_MAX);
      const res = await api('POST', '/api/md', { text: r.text, img_base: '/api/snap/' + snap + '/raw/' + loc + encP(dir) + '/', link_base: '/version/' + snap + '/' + loc + encP(dir) + '/' });
      return res.html;
    },
  });
  const back = U.files(loc, dir);
  const el = h('section', { class: 'win viewwin' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), crumbs(loc, '', path, false),
      h('span', { class: 'tag', text: tx('backup of {time}', { time: when }) }), h('span', { class: 'spacer' }),
      ver.same ? null : btn(tx('restore'), tx('put this version back'), () => restoreVersion(loc, path, snap).then(p => { if (p) navigate(U.view(loc, p)); }, fail)),
      h('a', { class: 'btn-link', href: raw + '?dl=1', download: '', text: tx('download') }),
      h('a', { class: 'btn-link', href: back, text: tx('close'), title: tx('close (esc)') })),
    v.el);
  return {
    el, cls: 'fill', title: name + ' (' + when + ')',
    status: tx('{size}, modified {time}', { size: fmtSize(ver.size), time: fmtTime(ver.mtime) }) + (ver.same ? ', ' + tx('as now') : ''),
    destroy: () => v.destroy(),
    key: e => {
      if (isTyping(e) || e.ctrlKey || e.metaKey || e.altKey) return false;
      if (e.key === 'Escape') { navigate(back); return true; }
      return false;
    },
  };
}

return { versionsDialog, restoreVersion, versionPage };
});
