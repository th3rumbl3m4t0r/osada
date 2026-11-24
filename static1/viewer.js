/* The file viewer, used by the app (/view/…) and by public links (/s/…).
   src = {
     name,                   file name
     raw,                    url of the content
     siblings,               names in the same folder, or null (prev / next, subtitles)
     rawOf(name),            url of a sibling
     md(),                   Promise of the rendered HTML (markdown only), or null: shown as text
     mdOf(text),             the same for a text of its own (a file in an archive), or null
     save(name, bytes),      keeps something taken out of the file next to it, or null
     extract(entries),       writes an archive's entries into a folder next to it -> {n, kept}, or null
   }
   Returns {el, prev, next, destroy}; prev / next are sibling names or null. */
'use strict';

const TEXT_MAX = 4 << 20;
const CSV_MAX = 32 << 20;

/* a file's response, or an error that says why not; tells util.js whether home answered */
async function fetchFile(url, o) {
  let r;
  try { r = await fetch(url, Object.assign({ credentials: 'same-origin' }, o)); } catch (e) { setOffline(true); throw new Error(tx('server unreachable')); }
  heard(r);
  if (!r.ok) throw new Error(offline() ? tx('not kept on this device') : r.status + ' ' + r.statusText);
  return r;
}

/* the first `max` bytes as text; truncated when the file is longer. Past the
   browser's cache: Chromium keeps a range as a partial copy and later asks for
   only that length, which cuts a file that has grown since */
async function fetchText(url, max) {
  const r = await fetchFile(url, { cache: 'no-store', headers: { range: 'bytes=0-' + (max - 1) } });
  const total = /\/(\d+)$/.exec(r.headers.get('content-range') || '');
  return { text: await r.text(), truncated: !!total && +total[1] > max, etag: r.headers.get('etag') };
}

/* the whole file, for a part's view; refused past `max` bytes */
async function fetchBuf(url, max) {
  const r = await fetchFile(url, {});
  if (max && +r.headers.get('content-length') > max) throw new Error(tx('too big to show here (over {size}); download it', { size: fmtSize(max) }));
  return new Uint8Array(await r.arrayBuffer());
}

/* kinds a part shows (util.js loads it when such a file is opened): the part, the class of
   the box it shows in, and show(exports, box, src), which resolves to something with destroy() */
const PART_VIEWS = {
  csv: {
    part: 'csv', cls: 'csv-host',
    async show(x, host, src) {
      const r = await fetchText(src.raw, CSV_MAX);
      if (r.truncated) throw new Error(tx('too big for the table view (over {size}); download it', { size: fmtSize(CSV_MAX) }));
      return new x.CsvGrid(host, r.text, { readonly: true, name: src.name });
    },
  },
  sheet: {
    part: sheetPart, cls: 'csv-host',
    async show(x, host, src) {
      const book = await x.openBook(await fetchBuf(src.raw, CSV_MAX));
      if (book.macros) host.before(h('div', { class: 'viewer-note warn', text: tx('this file has macros: they are not run here, and not kept in an .ods made from it') }));
      return x.SheetGrid.open(host, book, { readonly: true });
    },
  },
  rtf: { part: 'rtf', cls: 'rich-view', show: async (x, host, src) => new x.DocEditor(host, x.RTF.parse(x.RTF.bytesToString(await fetchBuf(src.raw))), { readonly: true }) },
  mail: {
    part: n => (/^(msg|oft)$/.test(extOf(n)) ? 'msg' : 'mail'), cls: 'mail-view',
    async show(x, host, src) {
      const msg = await x.readMail(await fetchBuf(src.raw, 64 << 20));
      if (msg.lost && Object.keys(msg.lost).length) host.before(h('div', { class: 'viewer-note dim', text: tx('not shown here: {what}', { what: Object.keys(msg.lost).map(k => tx(k)).join(', ') }) }));
      return new x.MailView(host, msg, { save: src.save || null });
    },
  },
  pub: {
    part: 'pub', cls: 'rich-view',
    async show(x, host, src) {
      const doc = await x.readPub(await fetchBuf(src.raw));
      host.before(h('div', { class: 'viewer-note dim', text: tx('a Publisher file: its text and pictures, in order; the page layout is not shown. not shown here: {what}', { what: Object.keys(doc.lost).filter(k => !k.startsWith('page layout')).map(k => tx(k)).join(', ') || '-' }) }));
      return new x.DocEditor(host, doc, { readonly: true });
    },
  },
  slides: {
    part: slidesPart, cls: 'slides-view',
    async show(x, host, src) {
      const deck = await x.readDeck(await fetchBuf(src.raw));
      const lost = Object.keys(deck.lost || {});
      if (lost.length) host.before(h('div', { class: 'viewer-note dim', text: tx('not shown here: {what}', { what: lost.map(k => tx(k)).join(', ') }) }));
      if (deck.macros) host.before(h('div', { class: 'viewer-note warn', text: tx('this file has macros: they are not run here, and not kept in an .odp made from it') }));
      return new x.SlideView(host, deck);
    },
  },
  doc: {
    part: docPart, cls: 'rich-view',
    async show(x, host, src) {
      const doc = await x.readDoc(await fetchBuf(src.raw));
      const lost = Object.keys(doc.lost || {});
      if (lost.length) host.before(h('div', { class: 'viewer-note dim', text: tx('not shown here: {what}', { what: lost.map(k => tx(k)).join(', ') }) }));
      if (doc.macros) host.before(h('div', { class: 'viewer-note warn', text: tx('this file has macros: they are not run here, and not kept in an .odt made from it') }));
      return new x.DocEditor(host, doc, { readonly: true });
    },
  },
  archive: {
    part: n => (/^(7z|cb7)$/.test(extOf(n)) ? 'sevenz' : /^(rar|cbr)$/.test(extOf(n)) ? 'rar' : 'archive'), cls: 'part-view',
    show: async (x, host, src) => new x.ArchiveView(host, await x.readArchive(await fetchBuf(src.raw, 512 << 20), src.name), { name: src.name, extract: src.extract || null, md: src.mdOf || null }),
  },
  img: { part: 'img', cls: 'part-view', show: async (x, host, src) => new x.ImgView(host, await x.readImg(await fetchBuf(src.raw, 512 << 20), src.name)) },
  book: { part: 'epub', cls: 'part-view', show: async (x, host, src) => new x.BookView(host, await x.readBook(await fetchBuf(src.raw, 256 << 20))) },
  cal: {
    part: 'cal', cls: 'part-view',
    async show(x, host, src) {
      const t = new TextDecoder().decode(await fetchBuf(src.raw, 64 << 20));
      return /^vc(f|ard)$/.test(extOf(src.name)) ? new x.CardView(host, x.readCards(t)) : new x.CalView(host, x.readCal(t));
    },
  },
  font: { part: 'font', cls: 'part-view', show: async (x, host, src) => new x.FontView(host, await x.readFont(await fetchBuf(src.raw, 64 << 20), src.name)) },
  iwork: { part: 'iwork', cls: 'part-view', show: async (x, host, src) => new x.IworkView(host, await x.readIwork(await fetchBuf(src.raw, 512 << 20))) },
};

function viewer(src) {
  const kind = kindOf(src.name);
  const el = h('div', { class: 'viewer viewer-' + kind });
  const blobs = [];
  // a part's view (PART_VIEWS), and the parts it holds
  let media = null, shown = null, gone = false;
  const parts = holder();

  function note(msg) { return h('div', { class: 'viewer-note dim', text: msg }); }

  if (kind === 'video' || kind === 'audio') {
    media = h(kind, { controls: true, preload: 'metadata', src: src.raw });
    media.addEventListener('error', () => {
      el.appendChild(note(offline() ? tx('not kept on this device') : tx('the browser cannot play this file (container or codec); download it instead')));
    });
    if (kind === 'video' && src.siblings) {
      // movie.mkv takes movie.vtt, movie.srt, movie.en.srt, movie.cs.vtt …
      const base = stem(src.name);
      src.siblings.filter(n => (extOf(n) === 'vtt' || extOf(n) === 'srt') && stem(n).startsWith(base)).forEach(n => {
        const lang = stem(n).slice(base.length).replace(/^[.\-_ ]+/, '') || tx('subtitles');
        const t = h('track', { kind: 'subtitles', label: lang, srclang: /^[a-z]{2,3}$/i.test(lang) ? lang : null });
        if (extOf(n) === 'vtt') t.src = src.rawOf(n);
        else {
          fetch(src.rawOf(n), { credentials: 'same-origin' }).then(r => r.text()).then(s => {
            const u = URL.createObjectURL(new Blob([srtToVtt(s)], { type: 'text/vtt' }));
            blobs.push(u);
            t.src = u;
          });
        }
        media.appendChild(t);
      });
    }
    el.appendChild(media);
  } else if (kind === 'image') {
    const img = h('img', { src: src.raw, alt: src.name, title: tx('click: actual size / fit') });
    img.addEventListener('click', () => el.classList.toggle('actual'));
    img.addEventListener('error', () => { if (offline()) img.replaceWith(note(tx('not kept on this device'))); });
    el.appendChild(img);
  } else if (kind === 'pdf') {
    el.appendChild(h('iframe', { src: src.raw, title: src.name }));
  } else if (kind === 'md' && src.md) {
    const body = h('div', { class: 'md-body' }, note(tx('rendering…')));
    el.appendChild(body);
    src.md().then(html => { body.innerHTML = html; }, e => {
      if (!offline()) { clear(body).appendChild(note(txErr(e.message))); return; }
      // rendering needs home: the text as it is
      fetchText(src.raw, TEXT_MAX).then(r => { body.replaceWith(h('pre', { class: 'text-view', text: r.text })); },
        e2 => { clear(body).appendChild(note(txErr(e2.message))); });
    });
  } else if (PART_VIEWS[kind]) {
    const pv = PART_VIEWS[kind], host = h('div', { class: pv.cls }, note(tx('loading…')));
    el.appendChild(host);
    parts.hold(typeof pv.part === 'function' ? pv.part(src.name) : pv.part)
      .then(x => pv.show(x, host, src))
      .then(v => { if (gone) v.destroy(); else shown = v; }, e => { if (!gone) clear(host).appendChild(note(txErr(e.message))); });
  } else if (kind === 'text' || kind === 'md') {
    const pre = h('pre', { class: 'text-view' }, note(tx('loading…')));
    el.appendChild(pre);
    fetchFile(src.raw, { cache: 'no-store', headers: { range: 'bytes=0-' + (TEXT_MAX - 1) } })
      .then(r => r.text())
      .then(t => {
        pre.textContent = t;
        if (t.length >= TEXT_MAX - 4) el.appendChild(note(tx('only the first {size} are shown', { size: fmtSize(TEXT_MAX) })));
      }, e => { clear(pre).appendChild(note(txErr(e.message))); });
  } else {
    el.appendChild(note(tx('no preview for this kind of file')));
  }

  // prev / next among files of the same kind
  let prev = null, next = null;
  if (src.siblings && (kind === 'image' || kind === 'video' || kind === 'audio')) {
    const same = src.siblings.filter(n => kindOf(n) === kind).sort(nameCmp);
    const i = same.indexOf(src.name);
    if (i > 0) prev = same[i - 1];
    if (i >= 0 && i < same.length - 1) next = same[i + 1];
  }

  return {
    el, prev, next,
    destroy() {
      if (media) { media.pause(); media.removeAttribute('src'); media.load(); }
      blobs.forEach(u => URL.revokeObjectURL(u));
      gone = true;
      if (shown) shown.destroy();
      parts.done();
    },
  };
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });
function nameCmp(a, b) { return collator.compare(a, b); }
