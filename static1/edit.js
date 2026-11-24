/* The editors: text and markdown, the edit page, the sheet page, "edit as .ods". Part edit (util.js): loaded when the page opens, it
   uses the app's helpers (app.js, util.js) as globals. */
'use strict';

part('edit', () => {

/* ---- text encodings: UTF-8, else a single-byte legacy one, kept on save ---- */
const LEGACY = ['windows-1250', 'windows-1252', 'windows-1251', 'iso-8859-2', 'iso-8859-15', 'ibm852', 'koi8-r', 'macintosh'];
function guessLegacy() {
  const l = (navigator.language || '').slice(0, 2).toLowerCase();
  if (['cs', 'sk', 'pl', 'hu', 'sl', 'hr', 'ro', 'bs'].includes(l)) return 'windows-1250';
  if (['ru', 'uk', 'bg', 'be', 'mk', 'sr'].includes(l)) return 'windows-1251';
  return 'windows-1252';
}
function decodeBytes(bytes, enc) {
  if (!enc) {
    try { return { text: new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes), enc: 'utf-8' }; } catch (e) { enc = guessLegacy(); }
  }
  return { text: new TextDecoder(enc, { ignoreBOM: true }).decode(bytes), enc };
}
const encMaps = {};
function encodeText(s, enc) {
  if (enc === 'utf-8') return { bytes: new TextEncoder().encode(s), lost: 0 };
  let m = encMaps[enc];
  if (!m) {
    const d = new TextDecoder(enc);
    m = encMaps[enc] = new Map();
    for (let b = 255; b >= 0; b--) { const c = d.decode(new Uint8Array([b])); if (c.length === 1 && c !== '�') m.set(c, b); }
  }
  const out = [];
  let lost = 0;
  for (const ch of s) {
    const b = m.get(ch);
    if (b === undefined) { out.push(63); lost++; } else out.push(b);
  }
  return { bytes: new Uint8Array(out), lost };
}

/* for the editors: the whole file as it is now, never a copy from the browser's
   cache (which Chromium can piece together from an older, shorter range);
   anything but a full 200 would be saved back cut short */
async function fetchBytes(url, max) {
  const r = await fetch(url, { credentials: 'same-origin', cache: 'no-store' });
  if (!r.ok) {
    if (r.status === 401 && window.onUnauthorized) window.onUnauthorized();
    throw new Error(r.status + ' ' + r.statusText);
  }
  if (r.status !== 200) throw new Error(tx('the file came back incomplete ({status}); open it again', { status: r.status }));
  const b = new Uint8Array(await r.arrayBuffer());
  if (b.length > max) throw new Error(tx('too big to edit here (over {size})', { size: fmtSize(max) }));
  return { bytes: b, etag: r.headers.get('etag') };
}

/* ---- text / markdown editor ---- */
function taReplace(ta, from, to, text, s0, s1) {
  ta.focus();
  ta.setSelectionRange(from, to);
  if (!document.execCommand('insertText', false, text)) {   // keeps the browser's undo; the fallback does not
    ta.setRangeText(text, from, to, 'end');
    ta.dispatchEvent(new Event('input'));
  }
  ta.setSelectionRange(s0, s1);
}
function lineRange(ta) {
  const v = ta.value;
  const s = v.lastIndexOf('\n', ta.selectionStart - 1) + 1;
  let endAt = ta.selectionEnd;
  if (endAt > ta.selectionStart && v[endAt - 1] === '\n') endAt--;   // whole lines selected: not the next one
  let e = v.indexOf('\n', endAt);
  if (e < 0) e = v.length;
  return [s, e];
}
function mdWrap(ta, a, b, ph) {
  const s = ta.selectionStart, e = ta.selectionEnd, v = ta.value;
  if (v.slice(s - a.length, s) === a && v.slice(e, e + b.length) === b) {   // already wrapped: unwrap
    taReplace(ta, s - a.length, e + b.length, v.slice(s, e), s - a.length, e - a.length);
    return;
  }
  const t = v.slice(s, e) || ph;
  taReplace(ta, s, e, a + t + b, s + a.length, s + a.length + t.length);
}
function mdLines(ta, prefix, strip, numbered) {
  const [s, e] = lineRange(ta);
  const lines = ta.value.slice(s, e).split('\n');
  const all = !numbered && lines.every(l => l.startsWith(prefix));
  const out = lines.map((l, i) => all ? l.slice(prefix.length)
    : (numbered ? (i + 1) + '. ' : prefix) + (strip ? l.replace(strip, '') : l)).join('\n');
  taReplace(ta, s, e, out, s, s + out.length);
}
function mdInsert(ta, text) {
  const s = ta.selectionStart, e = ta.selectionEnd;
  taReplace(ta, s, e, text, s + text.length, s + text.length);
}
const LIST_RE = /^([-*+]|\d+\.)\s+(\[[ xX]\]\s+)?/;
const MD_TOOLS = [
  ['b', 'bold (ctrl+b)', ta => mdWrap(ta, '**', '**', tx('bold'))],
  ['i', 'italic (ctrl+i)', ta => mdWrap(ta, '*', '*', tx('italic'))],
  ['s', 'strike through', ta => mdWrap(ta, '~~', '~~', tx('text'))],
  ['code', 'code', ta => (ta.value.slice(ta.selectionStart, ta.selectionEnd).includes('\n') ? mdWrap(ta, '```\n', '\n```', tx('code')) : mdWrap(ta, '`', '`', tx('code')))],
  null,
  ['h1', 'heading 1', ta => mdLines(ta, '# ', /^#{1,6}\s+/)],
  ['h2', 'heading 2', ta => mdLines(ta, '## ', /^#{1,6}\s+/)],
  ['h3', 'heading 3', ta => mdLines(ta, '### ', /^#{1,6}\s+/)],
  ['quote', 'quote', ta => mdLines(ta, '> ')],
  null,
  ['• list', 'bullet list', ta => mdLines(ta, '- ', LIST_RE)],
  ['1. list', 'numbered list', ta => mdLines(ta, '', LIST_RE, true)],
  ['[ ] task', 'task list', ta => mdLines(ta, '- [ ] ', LIST_RE)],
  null,
  ['link', 'link (ctrl+k)', ta => mdWrap(ta, '[', '](https://)', tx('text'))],
  ['reference', 'reference to a wiki article (ctrl+e): find one, or name one yet to be written', (ta, ed) => ed.reference().catch(fail)],
  ['image', 'image: a relative path shows a file from this folder', ta => mdWrap(ta, '![', '](picture.png)', tx('description'))],
  ['table', 'table', ta => mdInsert(ta, '\n| column | column |\n| ------ | ------ |\n| cell   | cell   |\n')],
  ['rule', 'horizontal rule', ta => mdInsert(ta, '\n---\n')],
];

class TextEditor {
  constructor(host, text, opts) {
    this.opts = opts;
    this.md = !!opts.md;
    this.eol = text.includes('\r\n') ? '\r\n' : '\n';
    text = text.replace(/\r\n/g, '\n');
    this.ta = h('textarea', { class: 'src', spellcheck: this.md, autocapitalize: 'off' });
    this.ta.value = text;
    this.saved = this.ta.value;
    this.wasDirty = false;
    this.ta.addEventListener('input', () => { this.changed(); if (this.md) this.preview(); });
    this.ta.addEventListener('keydown', e => this.keys(e));
    const st = () => { cancelAnimationFrame(this.raf); this.raf = requestAnimationFrame(() => this.status()); };
    this.ta.addEventListener('keyup', st);
    this.ta.addEventListener('click', st);
    const wrapOn = localStorage.getItem('osada.wrap') !== '0';
    this.ta.classList.toggle('nowrap', !wrapOn);
    const wrapBox = h('input', { type: 'checkbox', checked: wrapOn });
    wrapBox.addEventListener('change', () => { this.ta.classList.toggle('nowrap', !wrapBox.checked); localStorage.setItem('osada.wrap', wrapBox.checked ? '1' : '0'); });
    const tools = [];
    if (this.md) {
      for (const t of MD_TOOLS) {
        if (!t) { tools.push(h('span', { class: 'sep' })); continue; }
        const b = h('button', { text: tx(t[0]), title: tx(t[1]) });
        b.addEventListener('mousedown', e => e.preventDefault());
        b.addEventListener('click', () => t[2](this.ta, this));
        tools.push(b);
      }
      this.mode = localStorage.getItem('osada.mdmode') || (innerWidth > 900 ? 'split' : 'edit');
      this.modeBtns = ['edit', 'split', 'preview'].map(m => { const b = btn(tx(m), null, () => this.setMode(m)); b.dataset.mode = m; return b; });
      tools.push(h('span', { class: 'spacer' }), h('span', { class: 'seg' }, this.modeBtns));
    } else tools.push(h('span', { class: 'spacer' }));
    tools.push(h('label', { class: 'chk', title: tx('wrap long lines') }, wrapBox, ' ' + tx('wrap')));
    this.prev = h('div', { class: 'md-body md-preview' });
    this.split = h('div', { class: 'md-split' }, this.ta, this.md ? this.prev : null);
    this.ta.addEventListener('scroll', () => {
      if (!this.md || this.mode !== 'split') return;
      const r = this.ta.scrollTop / Math.max(1, this.ta.scrollHeight - this.ta.clientHeight);
      this.prev.scrollTop = r * (this.prev.scrollHeight - this.prev.clientHeight);
    });
    clear(host).append(h('div', { class: 'toolbar' }, tools), this.split);
    if (this.md) { this.setMode(this.mode); this.preview(true); }
  }

  setMode(m) {
    this.mode = m;
    localStorage.setItem('osada.mdmode', m);
    this.split.className = 'md-split mode-' + m;
    this.modeBtns.forEach(b => b.classList.toggle('on', b.dataset.mode === m));
    if (m !== 'preview') this.ta.focus();
  }

  preview(now) {
    clearTimeout(this.pt);
    this.pt = setTimeout(async () => {
      try {
        const r = await api('POST', '/api/md', { text: this.ta.value, img_base: this.opts.imgBase, link_base: this.opts.linkBase });
        this.prev.innerHTML = r.html;
      } catch (e) { /* keep the last preview */ }
    }, now ? 0 : 250);
  }

  keys(e) {
    const ta = this.ta, mod = e.ctrlKey || e.metaKey;
    if (this.md && mod && !e.altKey) {
      const k = e.key.toLowerCase();
      if (k === 'b') { e.preventDefault(); mdWrap(ta, '**', '**', tx('bold')); return; }
      if (k === 'i') { e.preventDefault(); mdWrap(ta, '*', '*', tx('italic')); return; }
      if (k === 'k') { e.preventDefault(); mdWrap(ta, '[', '](https://)', tx('text')); return; }
      if (k === 'e') { e.preventDefault(); this.reference().catch(fail); return; }
    }
    if (e.key === 'Tab' && !mod && !e.altKey) {
      e.preventDefault();
      const ind = this.md ? '  ' : '\t';
      const multi = ta.value.slice(ta.selectionStart, ta.selectionEnd).includes('\n');
      if (e.shiftKey || multi) {
        const [s, en] = lineRange(ta);
        const out = ta.value.slice(s, en).split('\n').map(l => (e.shiftKey ? l.replace(this.md ? /^ {1,2}/ : /^\t/, '') : ind + l)).join('\n');
        taReplace(ta, s, en, out, s, s + out.length);
      } else mdInsert(ta, ind);
      return;
    }
    // markdown lists go on with Enter; Enter on an empty item ends the list
    if (this.md && e.key === 'Enter' && !e.shiftKey && !mod && !e.altKey && ta.selectionStart === ta.selectionEnd) {
      const v = ta.value, p = ta.selectionStart, ls = v.lastIndexOf('\n', p - 1) + 1, line = v.slice(ls, p);
      const m = /^(\s*)([-*+] \[[ xX]\] |[-*+] |(\d+)([.)]) |> )/.exec(line);
      if (!m) return;
      e.preventDefault();
      if (line.length === m[0].length) { taReplace(ta, ls, p, '', ls, ls); return; }
      let mk = m[2];
      if (m[3]) mk = (+m[3] + 1) + m[4] + ' ';
      else mk = mk.replace(/\[[xX]\]/, '[ ]');
      const ins = '\n' + m[1] + mk;
      taReplace(ta, p, p, ins, p + ins.length, p + ins.length);
    }
  }

  /* the selection becomes a reference to a wiki article picked in wiki.js; in the wiki
     itself by its address alone (opts.refBase ''), elsewhere as /wiki/<address> */
  async reference() {
    const ta = this.ta, s = ta.selectionStart, e = ta.selectionEnd, sel = ta.value.slice(s, e).trim();
    const r = await withPart('wiki', x => x.refPicker(sel));
    if (!r) { ta.focus(); return; }
    const md = '[' + (sel || r.title).replace(/[[\]]/g, '') + '](' + (this.opts.refBase !== undefined ? this.opts.refBase : '/wiki/') + r.id + ')';
    taReplace(ta, s, e, md, s + md.length, s + md.length);
  }

  get dirty() { return this.ta.value !== this.saved; }
  changed() {
    const d = this.dirty;
    if (d !== this.wasDirty) { this.wasDirty = d; this.opts.onChange(d); }
  }
  markSaved(text) { this.saved = text; this.changed(); }
  text() { return this.eol === '\n' ? this.ta.value : this.ta.value.replace(/\n/g, '\r\n'); }
  status() {
    const v = this.ta.value, p = this.ta.selectionStart;
    let line = 1;
    for (let i = v.indexOf('\n'); i >= 0 && i < p; i = v.indexOf('\n', i + 1)) line++;
    const col = p - v.lastIndexOf('\n', p - 1);
    this.opts.onStatus(tx('line {line}, col {col}', { line, col }) + ' | ' + txn(v.length, '{n} char', '{n} chars') + (this.eol === '\r\n' ? ' | crlf' : ''));
  }
  focus() { if (!this.md || this.mode !== 'preview') this.ta.focus(); }
  destroy() { clearTimeout(this.pt); }
}

/* ---- the edit page: md / text / csv / rtf / odt / odp, saved with If-Match ---- */
async function editPage(loc, path, parts) {
  const ls = await api('GET', U.ls(loc, path));
  if (!ls.file) return { redirect: U.files(loc, path) };
  const name = ls.file.name, kind = editorOf(name), dir = parentPath(path);
  if (!kind) return { redirect: U.view(loc, path) };
  if (!ls.write) return { redirect: U.view(loc, path) };
  if (kind === 'sheet') return sheetPage(loc, path, ls, parts);
  const max = kind === 'md' || kind === 'text' ? 8 << 20 : 32 << 20;
  let { bytes, etag } = await fetchBytes(U.raw(loc, path), max);
  // documents (rtf, odt) in the document editor (rich.js), presentations (odp) in the slide editor (slides.js), the rest as text
  const rich = kind === 'rtf' || kind === 'odt' || kind === 'odp';
  const x = rich || kind === 'csv' ? await parts.hold(kind) : {};
  const doc = kind === 'odt' ? await x.ODT.parse(bytes) : kind === 'rtf' ? x.RTF.parse(x.RTF.bytesToString(bytes)) : kind === 'odp' ? await x.ODP.parse(bytes) : null;

  const host = h('div', { class: 'edit-host edit-' + kind });
  const mark = h('span', { class: 'dirty', text: '' });
  const saveBtn = btn(tx('save'), tx('save (ctrl+s)'), () => save(), 'primary');
  const encSel = h('select', { title: tx('text encoding of the file') });
  const note = h('div', { class: 'note warn', hidden: true });
  let ed = null, enc = 'utf-8', bom = false, statusText = '', lostOk = false, saving = false;

  const onChange = d => { mark.textContent = d ? '* ' + tx('modified') : ''; saveBtn.disabled = !d; };
  const onStatus = t => { statusText = t; setStatus((!rich ? enc + (bom ? ' bom' : '') + ' | ' : '') + t); };

  function load(forceEnc) {
    if (ed && ed.destroy) ed.destroy();
    if (rich) {
      ed = kind === 'odp' ? new x.SlideEditor(host, doc, { onChange, format: x.ODP_FORMAT })
        : new x.DocEditor(host, doc, { onChange, format: kind === 'rtf' ? x.RTF_FORMAT : x.ODT_FORMAT });
      const lost = Object.keys(ed.lost);
      if (lost.length) { note.hidden = false; note.textContent = tx('this file has {what}: the editor does not keep them, saving drops them.', { what: lost.map(k => tx(k)).join(', ') }); }
      onStatus(kind);
      return;
    }
    const d = decodeBytes(bytes, forceEnc);
    enc = d.enc;
    let text = d.text;
    bom = text.startsWith('\uFEFF');
    if (bom) text = text.slice(1);
    encSel.replaceChildren(...['utf-8'].concat(LEGACY).map(x => h('option', { value: x, text: x })));
    encSel.value = enc;
    if (kind === 'csv') ed = new x.CsvGrid(host, text, { name, onChange, onStatus });
    else ed = new TextEditor(host, text, { md: kind === 'md', onChange, onStatus, imgBase: '/api/raw/' + loc + encP(dir) + '/', linkBase: '/view/' + loc + encP(dir) + '/' });
    if (ed.status) ed.status();
    onChange(false);
    if (!forceEnc && enc !== 'utf-8') toast(tx('not UTF-8: opened as {enc}, pick another encoding if it looks wrong', { enc }), 'warn');
  }
  encSel.addEventListener('change', async () => {
    if (ed.dirty && !(await confirmBox(tx('encoding'), tx('reload the file as {enc}? the changes are lost.', { enc: encSel.value }), tx('reload'), true))) { encSel.value = enc; return; }
    load(encSel.value);
  });

  async function save(force) {
    if (saving || (!ed.dirty && !force)) return;
    const lost = rich ? Object.keys(ed.lost) : [];
    if (lost.length && !lostOk) {
      if (!(await confirmBox(tx('save'), tx('saving drops the {what} in this file. save anyway?', { what: lost.map(k => tx(k)).join(', ') }), tx('save'), true))) return;
      lostOk = true;
    }
    const snap = ed.ta ? ed.ta.value : null;   // what "saved" means if typing goes on during the upload
    const mark = () => (ed.ta ? ed.markSaved(snap) : ed.markSaved());
    let body = await ed.text();
    if (!rich) {
      const r = encodeText((bom ? '\uFEFF' : '') + body, enc);
      if (r.lost && !(await confirmBox(tx('encoding'), tx('{n} characters cannot be stored in {enc} and would become "?". save anyway? (or switch the encoding to utf-8)', { n: r.lost, enc }), tx('save'), true))) return;
      body = r.bytes;
    }
    saving = true;
    saveBtn.disabled = true;
    busy(true);
    let conflict = false;
    try {
      const r = await putRaw(U.raw(loc, path), body, { 'if-match': force ? '*' : etag });
      etag = r.etag;
      bytes = body instanceof Uint8Array ? body : new TextEncoder().encode(body);
      mark();
      toast(tx('saved'));
    } catch (e) {
      if (e.status === 412) conflict = true; else fail(e);
    } finally {
      saving = false;
      busy(false);
      saveBtn.disabled = !ed.dirty;
    }
    if (!conflict) return;
    const how = await dialog({
      title: tx('conflict'), body: h('div', { class: 'dlg-text', text: tx('{name} was changed by someone else since you opened it.', { name }) }),
      buttons: [{ label: tx('cancel'), value: null }, { label: tx('save as a copy'), value: 'copy' }, { label: tx('overwrite theirs'), value: 'over', danger: true, primary: true }],
    });
    if (how === 'over') return save(true);
    if (how !== 'copy') return;
    const ext = extOf(name);
    const copy = await dialogAsk(tx('save as a copy'), tx('name'), tx('{name} (copy)', { name: stem(name) }) + (ext ? '.' + ext : ''), tx('save'), async v => {
      const bad = checkName(v);
      if (bad) return bad;
      await putRaw(U.raw(loc, joinPath(dir, v)) + '?mode=new', body);
    });
    if (copy) {
      mark();
      navigate(U.edit(loc, joinPath(dir, copy)), true);
    }
  }

  /* a copy as docx: downloaded, or saved next to it */
  async function exportDocx() {
    const to = stem(name) + '.docx';
    const how = await dialog({
      title: tx('export as docx'), body: h('div', { class: 'dlg-text', text: tx('a copy of {name} as it is now, in the Word format.', { name }) }),
      buttons: [{ label: tx('cancel'), value: null }, { label: tx('save as {name}', { name: to }), value: 'save' }, { label: tx('download'), value: 'dl', primary: true }],
    });
    if (!how) return;
    const out = await withPart('docx', ({ DOCX, Rich }) => DOCX.write(Rich.flat(ed.page), { font: ed.font, size: ed.size }));
    await keepCopy(how, loc, dir, to, out, 'application/vnd.openxmlformats-officedocument.wordprocessingml.document');
  }

  /* a copy as pptx: downloaded, or saved next to it */
  async function exportPptx() {
    const to = stem(name) + '.pptx';
    const how = await dialog({
      title: tx('export as pptx'), body: h('div', { class: 'dlg-text', text: tx('a copy of {name} as it is now, in the PowerPoint format.', { name }) }),
      buttons: [{ label: tx('cancel'), value: null }, { label: tx('save as {name}', { name: to }), value: 'save' }, { label: tx('download'), value: 'dl', primary: true }],
    });
    if (!how) return;
    const out = await withPart('pptx', ({ PPTX }) => PPTX.write(ed.deck()));
    await keepCopy(how, loc, dir, to, out, 'application/vnd.openxmlformats-officedocument.presentationml.presentation');
  }

  const el = h('section', { class: 'win editwin' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), crumbs(loc, ls.root_name, path, false), mark,
      h('span', { class: 'spacer' }), kind === 'odp' ? btn(tx('export .pptx'), tx('a copy in the PowerPoint format'), () => exportPptx().catch(fail))
        : rich ? btn(tx('export .docx'), tx('a copy in the Word format'), () => exportDocx().catch(fail)) : encSel, saveBtn,
      btn(tx('view'), tx('view'), () => navigate(U.view(loc, path))),
      btn(tx('close'), tx('back to the folder'), () => navigate(splitPath(path).length ? U.files(loc, dir) : '/shared'))),
    note, host);
  load();
  return {
    el, cls: 'fill', title: name,
    status: '',
    dirty: () => !!(ed && ed.dirty),
    save: () => save(),
    destroy: () => { if (ed && ed.destroy) ed.destroy(); },
    shown: () => { onStatus(statusText); if (ed.focus) ed.focus(); else (ed.page || ed.scroll).focus(); },
  };
}

/* ---- the sheet page: ods, saved on its own every few seconds ---- */
const SHEET_MAX = 32 << 20;
const SHEET_TICK = 5000;
async function sheetPage(loc, path, ls, parts) {
  const name = ls.file.name, dir = parentPath(path);
  let { bytes, etag } = await fetchBytes(U.raw(loc, path), SHEET_MAX);
  const { SheetGrid } = await parts.hold('sheet');
  const host = h('div', { class: 'edit-host edit-sheet' });
  const mark = h('span', { class: 'dirty', text: '' });
  let grid = null, statusText = '', saving = false, job = Promise.resolve(), failed = '';

  const show = () => { mark.textContent = saving ? tx('saving…') : grid && grid.dirty ? '* ' + tx('modified') : ''; };
  const onStatus = t => { statusText = t; setStatus(t); };
  // one thing at a time; the same error again is not shown again
  const run = f => (job = job.then(f).then(() => { failed = ''; }, e => { if (e.message !== failed) fail(e); failed = e.message; }));

  /* their file, with the cells changed here on top */
  async function reload(msg) {
    const cur = await fetchBytes(U.raw(loc, path), SHEET_MAX);
    await grid.rebase(cur.bytes);
    etag = cur.etag;
    toast(msg, 'warn');
  }

  async function save() {
    for (let tries = 0; grid.dirty; tries++) {
      const out = await grid.pack();
      saving = true;
      show();
      try {
        etag = (await putRaw(U.raw(loc, path), out.bytes, { 'if-match': etag })).etag;
        grid.stored(out);
        return;
      } catch (e) {
        if (e.status !== 412 || tries >= 2) throw e;
      } finally {
        saving = false;
        show();
      }
      await reload(tx('changed elsewhere meanwhile: your edits went on top of that version'));
    }
  }

  async function tick() {
    if (grid.dirty) return save();
    if (document.hidden) return;
    const l = await api('GET', U.ls(loc, path));
    if (l.file && l.file.etag !== etag) await reload(tx('changed elsewhere: reloaded'));
  }

  /* a copy as xlsx: downloaded, or saved next to it */
  async function exportXlsx() {
    const to = stem(name) + '.xlsx';
    const how = await dialog({
      title: tx('export as xlsx'), body: h('div', { class: 'dlg-text', text: tx('a copy of {name} as it is now, in the Excel format.', { name }) }),
      buttons: [{ label: tx('cancel'), value: null }, { label: tx('save as {name}', { name: to }), value: 'save' }, { label: tx('download'), value: 'dl', primary: true }],
    });
    if (!how) return;
    await keepCopy(how, loc, dir, to, await grid.xlsx(), 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet');
  }

  grid = await SheetGrid.open(host, bytes, { name, onChange: show, onStatus });
  bytes = null;
  const timer = setInterval(() => run(tick), SHEET_TICK);
  const vis = () => { if (document.hidden) run(save); };
  document.addEventListener('visibilitychange', vis);
  const leave = url => run(save).then(() => navigate(url));

  const el = h('section', { class: 'win editwin' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), crumbs(loc, ls.root_name, path, false), mark,
      h('span', { class: 'spacer' }), h('span', { class: 'dim', text: tx('saves by itself') }),
      btn(tx('export .xlsx'), tx('a copy in the Excel format'), () => exportXlsx().catch(fail)),
      btn(tx('view'), tx('view'), () => leave(U.view(loc, path))),
      btn(tx('close'), tx('back to the folder'), () => leave(splitPath(path).length ? U.files(loc, dir) : '/shared'))),
    host);
  return {
    el, cls: 'fill', title: name,
    status: '',
    dirty: () => grid.dirty,
    save: () => run(save),
    destroy: () => { clearInterval(timer); document.removeEventListener('visibilitychange', vis); grid.destroy(); },
    shown: () => { onStatus(statusText); grid.scroll.focus(); },
  };
}


/* an export: downloaded (how 'dl'), or saved next to the file as `to` */
async function keepCopy(how, loc, dir, to, bytes, type) {
  if (how === 'dl') {
    const href = URL.createObjectURL(new Blob([bytes], { type }));
    const a = h('a', { href, download: to });
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(href), 60000);
    return;
  }
  const url = U.raw(loc, joinPath(dir, to));
  try {
    await putRaw(url + '?mode=new', bytes);
  } catch (e) {
    if (e.status !== 409) throw e;
    if (!(await confirmBox(tx('already there'), tx('{name} exists. replace it?', { name: to }), tx('replace'), true))) return;
    await putRaw(url, bytes);
  }
  toast(tx('saved {name}', { name: to }));
}

/* a docx or doc is shown, not edited: an .odt made from it is */
async function editAsOdt(loc, path, name) {
  const to = joinPath(parentPath(path), stem(name) + '.odt'), url = U.raw(loc, to);
  const { bytes } = await fetchBytes(U.raw(loc, path), 64 << 20);
  const [r, o] = await Promise.all([hold(docPart(name)), hold('odt')]);
  let doc, out;
  try {
    doc = await r.readDoc(bytes);
    out = await o.ODT.write(doc.blocks, { font: doc.font, size: doc.size });
  } finally {
    release(docPart(name));
    release('odt');
  }
  try {
    await putRaw(url + '?mode=new', out);
  } catch (e) {
    if (e.status !== 409) throw e;
    const v = await dialog({
      title: tx('already there'), body: h('div', { class: 'dlg-text', text: tx('{name} exists.', { name: stem(name) + '.odt' }) }),
      buttons: [{ label: tx('cancel'), value: null }, { label: tx('replace it'), value: 'replace', danger: true }, { label: tx('edit that one'), value: 'open', primary: true }],
    });
    if (!v) return;
    if (v === 'replace') await putRaw(url, out);
  }
  const lost = Object.keys(doc.lost || {});
  if (lost.length || doc.macros) toast(tx('not carried into the .odt: {what}', { what: lost.map(k => tx(k)).concat(doc.macros ? [tx('macros')] : []).join(', ') }), 'warn');
  navigate(U.edit(loc, to));
}

/* a pptx or ppt is shown, not edited: an .odp made from it is (what the masters draw becomes part of each slide) */
async function editAsOdp(loc, path, name) {
  const to = joinPath(parentPath(path), stem(name) + '.odp'), url = U.raw(loc, to);
  const { bytes } = await fetchBytes(U.raw(loc, path), 128 << 20);
  const [r, o] = await Promise.all([hold(slidesPart(name)), hold('odp')]);
  let deck, out;
  try {
    deck = await r.readDeck(bytes);
    out = await o.ODP.write(deck, { flatten: true });
  } finally {
    release(slidesPart(name));
    release('odp');
  }
  try {
    await putRaw(url + '?mode=new', out);
  } catch (e) {
    if (e.status !== 409) throw e;
    const v = await dialog({
      title: tx('already there'), body: h('div', { class: 'dlg-text', text: tx('{name} exists.', { name: stem(name) + '.odp' }) }),
      buttons: [{ label: tx('cancel'), value: null }, { label: tx('replace it'), value: 'replace', danger: true }, { label: tx('edit that one'), value: 'open', primary: true }],
    });
    if (!v) return;
    if (v === 'replace') await putRaw(url, out);
  }
  const lost = Object.keys(deck.lost || {});
  if (lost.length || deck.macros) toast(tx('not carried into the .odp: {what}', { what: lost.map(k => tx(k)).concat(deck.macros ? [tx('macros')] : []).join(', ') }), 'warn');
  navigate(U.edit(loc, to));
}

/* an xlsx is shown, not edited: an .ods made from it is */
async function editAsOds(loc, path, name) {
  const to = joinPath(parentPath(path), stem(name) + '.ods'), url = U.raw(loc, to);
  const { bytes } = await fetchBytes(U.raw(loc, path), SHEET_MAX);
  let macros = false;
  const out = await withPart(sheetPart(name), async ({ openBook, OdsOut, Calc }) => {
    const book = await openBook(bytes);
    macros = !!book.macros;
    return OdsOut.write(book, new Calc(book));
  });
  if (macros) toast(tx('the macros of {name} were left out', { name }), 'warn');
  try {
    await putRaw(url + '?mode=new', out);
  } catch (e) {
    if (e.status !== 409) throw e;
    const v = await dialog({
      title: tx('already there'), body: h('div', { class: 'dlg-text', text: tx('{name} exists.', { name: stem(name) + '.ods' }) }),
      buttons: [{ label: tx('cancel'), value: null }, { label: tx('replace it'), value: 'replace', danger: true }, { label: tx('edit that one'), value: 'open', primary: true }],
    });
    if (!v) return;
    if (v === 'replace') await putRaw(url, out);
  }
  navigate(U.edit(loc, to));
}

return { editPage, sheetPage, editAsOds, editAsOdt, editAsOdp, TextEditor, fetchBytes, decodeBytes, encodeText };
});
