/* Archives: zip (and what is a zip: jar, apk, cbz…), tar, and a tar or a lone file packed
   with gzip, bzip2 or xz (.tar.gz, .tgz, .tar.bz2, .tar.xz, .gz, .bz2, .xz). Part archive, on
   zip; bzip2 and lzma are held while such a file is unpacked. The view lists what is inside as folders and files (name, size, date);
   a file opens in the viewer (over the list) or is downloaded, and the picked ones (or
   everything) are extracted into a folder next to the archive. A comic book archive (cbz,
   cbt) shows its pictures one after the other. Encrypted zip entries and zip64 archives are
   refused, as in zip.js. 7z and rar are parts on this one (sevenz, rar) that read their
   kind into the same entries. */
'use strict';

part('archive', ({ ZIP }) => {

const decUtf8 = new TextDecoder('utf-8', { fatal: true }), dec437 = new TextDecoder('ibm866');

async function gunzip(b) {
  return new Uint8Array(await new Response(new Blob([b]).stream().pipeThrough(new DecompressionStream('gzip'))).arrayBuffer());
}

/* entries: {path, dir, size, time (s), read() -> bytes} */
function zipList(bytes) {
  const raw = ZIP.read(bytes);
  return raw.map(f => {
    const d = f.date, t = f.time;
    const time = d ? Math.floor(new Date((d >> 9) + 1980, ((d >> 5) & 15) - 1, d & 31, t >> 11, (t >> 5) & 63, (t & 31) * 2) / 1000) : 0;
    return { path: f.name.replace(/\\/g, '/'), dir: f.name.endsWith('/'), size: f.size, time, read: () => ZIP.inflate(f) };
  });
}

function tarList(b) {
  const out = [];
  let longName = null, longLink = null, pax = {};
  const str = (o, n) => { let e = o; while (e < o + n && b[e]) e++; try { return decUtf8.decode(b.subarray(o, e)); } catch (x) { return dec437.decode(b.subarray(o, e)); } };
  const oct = (o, n) => parseInt(str(o, n).trim() || '0', 8) || 0;
  for (let o = 0; o + 512 <= b.length;) {
    if (b.subarray(o, o + 512).every(x => x === 0)) break;
    let size = oct(o + 124, 12);
    if (b[o + 124] & 0x80) { size = 0; for (let k = o + 125; k < o + 136; k++) size = size * 256 + b[k]; }   // base-256 sizes
    const type = String.fromCharCode(b[o + 156] || 48);
    const prefix = str(o + 345, 155), name = str(o, 100);
    const body = o + 512;
    if (type === 'L') longName = str(body, size);
    else if (type === 'K') longLink = str(body, size);
    else if (type === 'x') {
      for (const rec of new TextDecoder().decode(b.subarray(body, body + size)).split('\n')) { const m = /^\d+ ([^=]+)=(.*)$/.exec(rec); if (m) pax[m[1]] = m[2]; }
    } else if (type !== 'g') {
      const path = (pax.path || longName || (prefix ? prefix + '/' + name : name)).replace(/^\.\//, '');
      const at = body, n = size, to = pax.linkpath || longLink || str(o + 157, 100);
      // a symbolic link (2) is shown as one; a hard link (1) reads as the file it names
      const e = { path, dir: type === '5' || path.endsWith('/'), size: type === '0' || type === '\0' || type === '7' ? n : 0, time: +(pax.mtime || oct(o + 136, 12)) | 0, read: async () => b.subarray(at, at + n), link: type === '2' ? to : null };
      if (type === '1') { const t = out.find(x => x.path === to.replace(/^\.\//, '')); if (t) Object.assign(e, { size: t.size, read: t.read }); }
      if (path) out.push(e);
      longName = null;
      longLink = null;
      pax = {};
    }
    o = body + Math.ceil(size / 512) * 512;
  }
  return out;
}

/* a file packed alone: gzip, bzip2 (part bzip2) or xz (part lzma), held while unpacking */
async function unwrap(bytes) {
  if (bytes[0] === 0x1F && bytes[1] === 0x8B) return ['gz', await gunzip(bytes)];
  const [part, kind] = bytes[0] === 0x42 && bytes[1] === 0x5A && bytes[2] === 0x68 ? ['bzip2', 'bz2'] : bytes[0] === 0xFD && bytes[1] === 0x37 && bytes[2] === 0x7A && bytes[3] === 0x58 ? ['lzma', 'xz'] : [];
  if (!part) return null;
  const x = await hold(part);
  try { return [kind, kind === 'bz2' ? x.BZIP2.decode(bytes) : x.XZ.decode(bytes)]; } finally { release(part); }
}

/* bytes -> {kind, entries} */
async function readArchive(bytes, name) {
  const ext = (name || '').toLowerCase();
  const isTar = b => b.length >= 512 && String.fromCharCode(...b.subarray(257, 262)) === 'ustar';
  if (bytes[0] === 0x50 && bytes[1] === 0x4B) return { kind: 'zip', entries: zipList(bytes) };
  const packed = await unwrap(bytes);
  if (packed) {
    const [kind, inner] = packed;
    if (isTar(inner) || /\.(tgz|tbz2?|txz|tar\.(gz|bz2|xz))$/.test(ext)) return { kind: 'tar.' + kind, entries: tarList(inner) };
    const base = (name || 'file').split('/').pop().replace(/\.(gz|bz2|xz)$/i, '');
    return { kind, entries: [{ path: base, dir: false, size: inner.length, time: 0, read: async () => inner }] };
  }
  if (isTar(bytes) || /\.(tar|cbt)$/.test(ext)) return { kind: 'tar', entries: tarList(bytes) };
  throw new Error('not an archive osada can open (zip, 7z, rar, tar, gz, bz2, xz)');
}

const PIC = /\.(png|jpe?g|gif|webp|bmp|avif)$/i, COMIC = /\.(cbz|cbt|cbr|cb7)$/i;
/* an entry opened in the viewer is a blob: URL; media and PDF need their type on it */
const TYPES = { pdf: 'application/pdf', svg: 'image/svg+xml', png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp', avif: 'image/avif',
  bmp: 'image/bmp', ico: 'image/x-icon', mp4: 'video/mp4', m4v: 'video/mp4', webm: 'video/webm', mkv: 'video/x-matroska', mov: 'video/quicktime', ogv: 'video/ogg',
  mp3: 'audio/mpeg', m4a: 'audio/mp4', aac: 'audio/aac', ogg: 'audio/ogg', oga: 'audio/ogg', opus: 'audio/ogg', flac: 'audio/flac', wav: 'audio/wav' };
const PEEK_MAX = 512 << 20;
const PEEKS = [];   // entries open in a viewer, innermost last: Esc and arrows go to that one
const isLink = e => e.link !== null && e.link !== undefined;
const peekable = e => !e.dir && !e.locked && !isLink(e) && e.size <= PEEK_MAX && kindOf(e.path.split('/').pop()) !== 'other';

/* The view: folders and files; a file opens in the viewer over the list (an archive in it too).
   host, archive {kind, entries}, opts = {name, extract(entries) -> {n written, kept (there already)}
   or null, md(text) -> rendered HTML or null} */
class ArchiveView {
  constructor(host, arc, opts) {
    this.arc = arc;
    this.opts = opts || {};
    this.urls = [];
    this.dir = '';
    // folders implied by the paths
    const all = new Map();
    for (const e of arc.entries) {
      // a path out of the archive (../) is listed without its ..s, and never extracted
      const raw = e.path.split('/').filter(s => s && s !== '.'), unsafe = raw.includes('..'), parts = raw.filter(s => s !== '..');
      for (let i = 1; i < parts.length; i++) { const p = parts.slice(0, i).join('/'); if (!all.has(p)) all.set(p, { path: p, dir: true, size: 0, time: 0 }); }
      const p = parts.join('/');
      if (p) all.set(p, Object.assign({}, e, { path: p, unsafe }));
    }
    this.all = [...all.values()];
    const files = this.all.filter(e => !e.dir);
    this.count = h('span', { class: 'dim', text: txn(files.length, '{n} file', '{n} files') + ', ' + fmtSize(files.reduce((a, e) => a + e.size, 0)) + ' ' + tx('unpacked') });
    const bAll = this.opts.extract ? h('button', { text: tx('extract all'), title: tx('everything, into a folder next to the archive'), onclick: () => this.extract(files) }) : null;
    this.bSel = this.opts.extract ? h('button', { text: tx('extract picked'), disabled: true, onclick: () => this.extract(this.picked()) }) : null;
    const pics = files.filter(e => PIC.test(e.path)).sort((a, b) => collator.compare(a.path, b.path));
    const bShow = COMIC.test(this.opts.name || '') || (pics.length && pics.length === files.length) ? h('button', { text: tx('pictures'), title: tx('one after the other'), onclick: () => this.show(pics, 0) }) : null;
    this.crumbs = h('span', { class: 'arc-crumbs' });
    this.list = h('div', { class: 'arc-list' });
    clear(host).append(h('div', { class: 'toolbar arc-bar' }, this.crumbs, h('span', { class: 'spacer' }), this.count, bShow, this.bSel, bAll), this.list);
    host.classList.add('arc-host');
    this.host = host;
    this.paint();
    if (COMIC.test(this.opts.name || '') && pics.length) this.show(pics, 0);
  }

  picked() { return [...this.list.querySelectorAll('input:checked')].map(c => this.all.find(e => e.path === c.value)).flatMap(e => (e.dir ? this.all.filter(f => !f.dir && f.path.startsWith(e.path + '/')) : [e])); }

  paint() {
    const pre = this.dir ? this.dir + '/' : '';
    const here = this.all.filter(e => e.path.startsWith(pre) && e.path.length > pre.length && !e.path.slice(pre.length).includes('/'))
      .sort((a, b) => (b.dir - a.dir) || collator.compare(a.path, b.path));
    const segs = this.dir ? this.dir.split('/') : [];
    this.crumbs.replaceChildren(h('a', { href: '#', text: this.opts.name || tx('archive'), onclick: e => { e.preventDefault(); this.cd(''); } }),
      ...segs.flatMap((s, i) => [' / ', h('a', { href: '#', text: s, onclick: e => { e.preventDefault(); this.cd(segs.slice(0, i + 1).join('/')); } })]));
    const rows = here.map(e => {
      const nm = e.path.slice(pre.length);
      const cb = this.opts.extract ? h('input', { type: 'checkbox', value: e.path, onchange: () => { this.bSel.disabled = !this.list.querySelector('input:checked'); } }) : null;
      const title = e.unsafe ? tx('its path leads out of the archive (../): it is not extracted') : e.locked ? tx('encrypted: osada does not open it') : peekable(e) ? tx('open') : tx('download');
      const link = e.dir ? h('a', { href: '#', text: nm + '/', onclick: ev => { ev.preventDefault(); this.cd(e.path); } })
        : isLink(e) ? h('span', { class: 'dim', text: nm + ' \u2192 ' + (e.link || '?'), title: tx('a link: not opened or extracted') })
        : h('a', { href: '#', class: e.unsafe ? 'warn' : e.locked ? 'dim' : null, text: nm, title, onclick: ev => { ev.preventDefault(); (peekable(e) ? this.peek(e) : this.download(e)).catch(fail); } });
      const dl = e.dir || e.locked || isLink(e) ? null : h('button', { class: 'arc-dl', text: '\u2193', title: tx('download'), onclick: () => this.download(e).catch(fail) });
      return h('tr', {}, h('td', { class: 'ck' }, cb), h('td', { class: 'nm' }, link, e.locked ? ' \u{1F512}' : ''), h('td', { class: 'sz', text: e.dir ? '' : fmtSize(e.size) }),
        h('td', { class: 'tm dim', text: e.time ? fmtTime(e.time) : '' }), h('td', { class: 'dl' }, dl));
    });
    this.list.replaceChildren(rows.length ? h('table', { class: 'x11 files' }, h('tbody', {}, rows)) : h('div', { class: 'empty dim', text: tx('empty folder') }));
  }
  cd(d) { this.dir = d; this.paint(); }

  /* an entry in the viewer, over the list: < > for the others in its folder, download, extract, close (esc) */
  async peek(e) {
    const name = e.path.split('/').pop(), dir = e.path.slice(0, e.path.length - name.length);
    const same = this.all.filter(f => peekable(f) && f.path.startsWith(dir) && !f.path.slice(dir.length).includes('/')).sort((a, b) => collator.compare(a.path, b.path));
    const i = same.indexOf(e), prev = same[i - 1], next = same[i + 1];
    busy(true);
    let bytes;
    try { bytes = await e.read(); } finally { busy(false); }
    this.unpeek();
    const url = URL.createObjectURL(new Blob([bytes], { type: TYPES[extOf(name)] || '' }));
    const md = this.opts.md;
    const v = viewer({ name, raw: url, siblings: null, rawOf: null, md: md ? () => md(new TextDecoder().decode(bytes)) : null, mdOf: md || null });
    const kind = kindOf(name);
    const b = (text, title, fn, off) => h('button', { text, title, disabled: !!off, onclick: fn });
    const box = h('div', { class: 'arc-peek' },
      h('div', { class: 'toolbar' }, b('<', tx('previous (left arrow)'), () => this.peek(prev).catch(fail), !prev), b('>', tx('next (right arrow)'), () => this.peek(next).catch(fail), !next),
        h('span', { class: 'arc-peek-name', text: e.path }), h('span', { class: 'dim', text: fmtSize(e.size) }), h('span', { class: 'spacer' }),
        b(tx('download'), tx('download'), () => this.download(e).catch(fail)),
        this.opts.extract && !e.unsafe ? b(tx('extract'), tx('into the folder next to the archive'), () => this.extract([e])) : null,
        b(tx('close'), tx('close (esc)'), () => this.unpeek())),
      v.el);
    const me = {
      close: () => { document.removeEventListener('keydown', key, true); v.destroy(); URL.revokeObjectURL(url); box.remove(); PEEKS.splice(PEEKS.indexOf(me), 1); this.peeked = null; },
    };
    const key = ev => {
      if (PEEKS[PEEKS.length - 1] !== me || ev.ctrlKey || ev.metaKey || ev.altKey || isTyping(ev) || document.fullscreenElement) return;
      if (ev.key === 'Escape') { ev.preventDefault(); ev.stopPropagation(); this.unpeek(); }
      // arrows: the next picture; other kinds may use them themselves
      else if ((kind === 'image' || kind === 'img') && (ev.key === 'ArrowRight' || ev.key === 'ArrowLeft')) {
        ev.preventDefault();
        ev.stopPropagation();
        const to = ev.key === 'ArrowRight' ? next : prev;
        if (to) this.peek(to).catch(fail);
      }
    };
    document.addEventListener('keydown', key, true);
    PEEKS.push(me);
    this.peeked = me;
    this.host.appendChild(box);
  }
  unpeek() { if (this.peeked) this.peeked.close(); }

  async download(e) {
    const u = URL.createObjectURL(new Blob([await e.read()]));
    this.urls.push(u);
    const a = h('a', { href: u, download: e.path.split('/').pop() });
    document.body.appendChild(a);
    a.click();
    a.remove();
  }

  async extract(list) {
    list = list.filter(e => !e.unsafe && !e.locked && !isLink(e));
    if (!list.length) return;
    busy(true);
    try {
      const r = await this.opts.extract(list);
      toast([r.n || !r.kept ? txn(r.n, '{n} file extracted', '{n} files extracted') : '',
        r.kept ? txn(r.kept, '{n} file was there already and is kept as it was', '{n} files were there already and are kept as they were') : ''].filter(Boolean).join('; '));
    } catch (e) { fail(e); } finally { busy(false); }
  }

  /* pictures one after the other: arrows, click, esc */
  async show(pics, i) {
    const box = h('div', { class: 'arc-show', tabindex: '-1' });
    const img = h('img', { class: 'arc-pic', alt: '' }), cap = h('div', { class: 'arc-cap' });
    box.append(img, cap);
    document.body.appendChild(box);
    const go = async k => {
      if (k < 0 || k >= pics.length) return;
      i = k;
      const u = URL.createObjectURL(new Blob([await pics[i].read()]));
      if (img.src) URL.revokeObjectURL(img.src);
      img.src = u;
      cap.textContent = (i + 1) + ' / ' + pics.length + '  ' + pics[i].path;
    };
    const close = () => { document.removeEventListener('keydown', key, true); if (img.src) URL.revokeObjectURL(img.src); box.remove(); this.closeShow = null; };
    const key = e => {
      if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); }
      else if (e.key === 'ArrowRight' || e.key === ' ' || e.key === 'PageDown') { e.preventDefault(); e.stopPropagation(); go(i + 1); }
      else if (e.key === 'ArrowLeft' || e.key === 'PageUp') { e.preventDefault(); e.stopPropagation(); go(i - 1); }
    };
    document.addEventListener('keydown', key, true);
    box.addEventListener('click', e => (e.clientX < innerWidth / 3 ? go(i - 1) : e.clientX > innerWidth * 2 / 3 ? go(i + 1) : close()));
    this.closeShow = close;
    box.focus();
    await go(i);
  }

  destroy() { this.unpeek(); if (this.closeShow) this.closeShow(); this.urls.forEach(u => URL.revokeObjectURL(u)); }
}

return { readArchive, ArchiveView };
});
