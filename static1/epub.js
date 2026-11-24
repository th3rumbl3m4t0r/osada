/* E-books (.epub): read in a sandboxed frame, chapter by chapter. Part epub, on zip. The
   container gives the package; its spine the chapters in order, its nav document (EPUB 3)
   or NCX (EPUB 2) the contents. A chapter is shown with its stylesheets, pictures and fonts
   taken from the book (as data), no script, nothing from outside; arrows go on. */
'use strict';

part('epub', ({ ZIP }) => {

const TYPES = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', svg: 'image/svg+xml', webp: 'image/webp', css: 'text/css',
  ttf: 'font/ttf', otf: 'font/otf', woff: 'font/woff', woff2: 'font/woff2' };
const resolve = (base, href) => {
  const out = base.split('/').slice(0, -1);
  for (const s of decodeURIComponent(href.split('#')[0]).split('/')) { if (s === '..') out.pop(); else if (s && s !== '.') out.push(s); }
  return out.join('/');
};

async function readBook(bytes) {
  const files = new Map(ZIP.read(bytes).map(f => [f.name, f]));
  const text = async n => { const f = files.get(n); return f ? new TextDecoder().decode(await ZIP.inflate(f)) : null; };
  const xml = async n => { const t = await text(n); return t ? new DOMParser().parseFromString(t, 'application/xml') : null; };
  const cont = await xml('META-INF/container.xml');
  const rf = cont && cont.getElementsByTagName('rootfile')[0];
  const opfPath = rf ? rf.getAttribute('full-path') : [...files.keys()].find(n => n.endsWith('.opf'));
  const opf = opfPath && await xml(opfPath);
  if (!opf) throw new Error('not an EPUB (no package)');
  const meta = tag => { const e = opf.getElementsByTagNameNS('http://purl.org/dc/elements/1.1/', tag)[0]; return e ? e.textContent.trim() : ''; };
  const items = {};
  for (const it of opf.getElementsByTagName('item')) items[it.getAttribute('id')] = { href: resolve(opfPath, it.getAttribute('href')), type: it.getAttribute('media-type'), props: it.getAttribute('properties') || '' };
  const spine = [...opf.getElementsByTagName('itemref')].filter(r => r.getAttribute('linear') !== 'no').map(r => items[r.getAttribute('idref')]).filter(Boolean).map(i => i.href);
  // the contents: nav (EPUB 3) or NCX (EPUB 2)
  const toc = [];
  const nav = Object.values(items).find(i => /\bnav\b/.test(i.props));
  if (nav) {
    const d = new DOMParser().parseFromString(await text(nav.href) || '', 'application/xhtml+xml');
    const n = [...d.getElementsByTagName('nav')].find(x => /toc/.test(x.getAttribute('epub:type') || x.getAttributeNS('http://www.idpf.org/2007/ops', 'type') || 'toc')) || d.getElementsByTagName('nav')[0];
    if (n) for (const a of n.getElementsByTagName('a')) { let depth = 0; for (let p = a.parentElement; p && p !== n; p = p.parentElement) if (p.tagName.toLowerCase() === 'ol') depth++; toc.push({ label: a.textContent.trim(), href: resolve(nav.href, a.getAttribute('href') || ''), depth: Math.max(0, depth - 1) }); }
  } else {
    const ncxId = (opf.getElementsByTagName('spine')[0] || { getAttribute: () => null }).getAttribute('toc');
    const ncx = ncxId && items[ncxId] ? await xml(items[ncxId].href) : null;
    if (ncx) {
      const walk = (el, depth) => { for (const np of [...el.children].filter(x => x.localName === 'navPoint')) { const lab = np.getElementsByTagName('text')[0], ct = np.getElementsByTagName('content')[0]; if (lab && ct) toc.push({ label: lab.textContent.trim(), href: resolve(items[ncxId].href, ct.getAttribute('src')), depth }); walk(np, depth + 1); } };
      const map = ncx.getElementsByTagName('navMap')[0];
      if (map) walk(map, 0);
    }
  }
  if (!spine.length) throw new Error('this EPUB has no chapters');
  return { title: meta('title'), author: meta('creator'), spine, toc, files, text };
}

const dataUrl = async (book, path) => {
  const f = book.files.get(path);
  if (!f) return null;
  const b = await ZIP.inflate(f);
  let s = '';
  for (let i = 0; i < b.length; i += 8192) s += String.fromCharCode.apply(null, b.subarray(i, i + 8192));
  return 'data:' + (TYPES[path.toLowerCase().split('.').pop()] || 'application/octet-stream') + ';base64,' + btoa(s);
};
/* url(...) in a stylesheet: what the book has, as data */
async function cssUrls(book, css, base) {
  const parts = [];
  let last = 0;
  for (const m of css.matchAll(/url\(\s*(['"]?)([^'")]+)\1\s*\)/g)) {
    parts.push(css.slice(last, m.index));
    const u = /^(data:|https?:)/.test(m[2]) ? null : await dataUrl(book, resolve(base, m[2]));
    parts.push(u ? 'url("' + u + '")' : 'none');
    last = m.index + m[0].length;
  }
  parts.push(css.slice(last));
  return parts.join('');
}

/* a chapter as a document of its own */
async function chapterDoc(book, path, size) {
  const src = await book.text(path);
  if (src === null) return '<p>missing: ' + escHtml(path) + '</p>';
  let d = new DOMParser().parseFromString(src, 'application/xhtml+xml');
  if (d.getElementsByTagName('parsererror').length) d = new DOMParser().parseFromString(src, 'text/html');
  for (const s of [...d.querySelectorAll('script, iframe, object, embed, form')]) s.remove();
  let css = '';
  for (const l of [...d.querySelectorAll('link[rel~="stylesheet"]')]) { const p = resolve(path, l.getAttribute('href') || ''); const t = await book.text(p); if (t) css += await cssUrls(book, t, p) + '\n'; l.remove(); }
  for (const st of [...d.querySelectorAll('style')]) { css += await cssUrls(book, st.textContent, path) + '\n'; st.remove(); }
  for (const el of [...d.querySelectorAll('img[src], image, source[src], video[poster]')]) {
    const attr = el.hasAttribute('src') ? 'src' : el.localName === 'video' ? 'poster' : null;
    const href = attr ? el.getAttribute(attr) : el.getAttribute('href') || el.getAttributeNS('http://www.w3.org/1999/xlink', 'href');
    if (!href || /^data:/.test(href)) continue;
    // nothing from outside
    if (/^[a-z]+:/i.test(href)) { el.removeAttribute(attr || 'href'); continue; }
    const u = await dataUrl(book, resolve(path, href));
    if (!u) continue;
    if (attr) el.setAttribute(attr, u); else { el.setAttribute('href', u); el.setAttributeNS('http://www.w3.org/1999/xlink', 'xlink:href', u); }
  }
  for (const a of [...d.querySelectorAll('a[href]')]) if (!/^(https?:|mailto:)/.test(a.getAttribute('href'))) a.removeAttribute('href');
  for (const el of [...d.querySelectorAll('[style]')]) el.setAttribute('style', await cssUrls(book, el.getAttribute('style'), path));
  const body = d.body || d.getElementsByTagName('body')[0];
  return '<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src \'none\'; img-src data:; style-src \'unsafe-inline\'; font-src data:"><base target="_blank">' +
    '<style>' + css.replace(/<\/style/gi, '') + '</style><style>html{font-size:' + size + '%}body{max-width:42em;margin:1.5em auto;padding:0 1.2em;line-height:1.5;font-family:Georgia,serif;color:#1a1a1a;background:#fbfaf6}img,svg{max-width:100%;height:auto}</style></head><body>' +
    (body ? body.innerHTML : '') + '</body></html>';
}

class BookView {
  constructor(host, book) {
    this.book = book;
    this.i = 0;
    this.size = +(localStorage.getItem('osada.epub.size') || 100);
    const b = (t, title, fn) => h('button', { text: t, title, onclick: fn });
    this.sel = h('select', { title: tx('contents') });
    const opts = book.toc.length ? book.toc : book.spine.map((s, i) => ({ label: tx('chapter {n}', { n: i + 1 }), href: s, depth: 0 }));
    this.sel.append(...opts.map(t => h('option', { value: t.href, text: '  '.repeat(t.depth) + t.label })));
    this.sel.addEventListener('change', () => { const k = this.book.spine.indexOf(this.sel.value.split('#')[0]); if (k >= 0) this.go(k); });
    this.count = h('span', { class: 'dim' });
    this.frame = h('iframe', { class: 'book-frame', sandbox: 'allow-popups allow-popups-to-escape-sandbox', title: book.title || tx('book') });
    clear(host).append(h('div', { class: 'toolbar book-bar' }, h('b', { text: book.title || '' }), book.author ? h('span', { class: 'dim', text: book.author }) : null,
      h('span', { class: 'spacer' }), this.sel, b('<', tx('previous chapter (left arrow)'), () => this.go(this.i - 1)), this.count, b('>', tx('next chapter (right arrow)'), () => this.go(this.i + 1)),
      b('A-', tx('smaller text'), () => this.zoom(-10)), b('A+', tx('larger text'), () => this.zoom(10))), this.frame);
    host.classList.add('book-host');
    this.onKey = e => {
      if (e.defaultPrevented || isTyping(e) || e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === 'ArrowRight') { e.preventDefault(); this.go(this.i + 1); } else if (e.key === 'ArrowLeft') { e.preventDefault(); this.go(this.i - 1); }
    };
    document.addEventListener('keydown', this.onKey);
    this.go(0);
  }
  async go(i) {
    if (i < 0 || i >= this.book.spine.length) return;
    this.i = i;
    this.count.textContent = (i + 1) + ' / ' + this.book.spine.length;
    const at = [...this.sel.options].findIndex(o => o.value.split('#')[0] === this.book.spine[i]);
    if (at >= 0) this.sel.selectedIndex = at;
    this.frame.srcdoc = await chapterDoc(this.book, this.book.spine[i], this.size);
  }
  zoom(by) { this.size = Math.min(200, Math.max(60, this.size + by)); localStorage.setItem('osada.epub.size', String(this.size)); this.go(this.i); }
  destroy() { document.removeEventListener('keydown', this.onKey); }
}

return { readBook, BookView };
});
