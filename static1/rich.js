/* Rich text: the document editor (DocEditor) and the model the document formats share.
   Part rich. A format (rtf.js, odt.js, docx.js, msdoc.js) reads a file into blocks,
   Rich.html() turns them into the editor's HTML, and the formats that are written
   (rtf by its own walk; odt and docx) take Rich.flat(page), the page back as blocks.

   Blocks: a paragraph {runs, align ('' center right justify), li, fi, sb, sa (twips: left
   indent, first line, space before and after), list ('ul' 'ol' or none), level (0..),
   h (heading 1-6, 0 not)}; a rule {hr: true}; a table {rows: [{cells: [{blocks, span,
   rspan, w (twips)}], x (rtf: the cells' right edges)}]}.
   Runs: text {t (with \n for a line break, \t a tab), f: {b, i, u, s, sup, sub, font,
   size (half-points), color ('#rrggbb'), bg, href}}; a picture {img (a data: URL), w, h
   (px), href}. What a format cannot show is counted in lost: {what: n}. */
'use strict';

part('rich', () => {

function esc(s) { return String(s).replace(/[&<>"]/g, c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : '&quot;')); }
function hex2(n) { return (n < 16 ? '0' : '') + n.toString(16); }
function safeUrl(u) { return /^(https?:|mailto:)/i.test(u) ? u : null; }
function cssFont(n) { return "'" + n.replace(/['"<>;\\]/g, '') + "'"; }

const NAMED = { black: '#000000', white: '#ffffff', red: '#ff0000', green: '#008000', blue: '#0000ff', yellow: '#ffff00',
  gray: '#808080', grey: '#808080', silver: '#c0c0c0', maroon: '#800000', navy: '#000080', purple: '#800080',
  teal: '#008080', olive: '#808000', orange: '#ffa500', lime: '#00ff00', aqua: '#00ffff', fuchsia: '#ff00ff' };
function toRgb(c) {
  if (!c) return null;
  c = c.trim().toLowerCase();
  if (NAMED[c]) return NAMED[c];
  let m = /^#([0-9a-f]{6})$/.exec(c);
  if (m) return '#' + m[1];
  m = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/.exec(c);
  if (m) return '#' + m[1] + m[1] + m[2] + m[2] + m[3] + m[3];
  m = /^rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)(?:[\s,/]+([\d.]+))?\s*\)$/.exec(c);
  if (m) {
    if (m[4] !== undefined && parseFloat(m[4]) === 0) return null;
    return '#' + [m[1], m[2], m[3]].map(v => hex2(Math.min(255, +v))).join('');
  }
  return null;
}
function toPt(v, base) {
  if (!v) return null;
  const m = /^([\d.]+)(pt|px|em|rem|%)?$/.exec(v.trim());
  if (!m) return { 'xx-small': 7, 'x-small': 7.5, small: 10, medium: 12, large: 13.5, 'x-large': 18, 'xx-large': 24, 'xxx-large': 36 }[v.trim()] || null;
  const x = parseFloat(m[1]);
  switch (m[2]) {
    case 'pt': return x;
    case 'em': case 'rem': return x * base;
    case '%': return x * base / 100;
    default: return x * 0.75;
  }
}
function firstFamily(v) {
  const f = (v || '').split(',')[0].trim().replace(/^['"]|['"]$/g, '');
  return f && !/^(serif|sans-serif|monospace|inherit|initial|cursive|fantasy|system-ui)$/i.test(f) ? f : null;
}

/* ---------------- blocks to HTML ---------------- */
function runHtml(r, base, font) {
  if (r.img) {
    let x = '<img src="' + esc(r.img) + '" style="width:' + Math.round(r.w) + 'px;height:' + Math.round(r.h) + 'px" alt="">';
    if (r.href) x = '<a href="' + esc(r.href) + '">' + x + '</a>';
    return x;
  }
  const f = r.f || {};
  let x = esc(r.t).replace(/\n/g, '<br>');
  if (f.b) x = '<b>' + x + '</b>';
  if (f.i) x = '<i>' + x + '</i>';
  if (f.u) x = '<u>' + x + '</u>';
  if (f.s) x = '<s>' + x + '</s>';
  if (f.sup) x = '<sup>' + x + '</sup>';
  if (f.sub) x = '<sub>' + x + '</sub>';
  let s = '';
  if (f.font && (!font || f.font.trim().toLowerCase() !== font.toLowerCase())) s += 'font-family:' + cssFont(f.font.trim()) + ';';
  if (f.size && f.size !== (base || 24)) s += 'font-size:' + f.size / 2 + 'pt;';
  if (f.color) s += 'color:' + f.color + ';';
  if (f.bg) s += 'background-color:' + f.bg + ';';
  if (s) x = '<span style="' + esc(s) + '">' + x + '</span>';
  if (f.href) x = '<a href="' + esc(f.href) + '">' + x + '</a>';
  return x;
}

function paraStyle(b, list) {
  let s = '';
  if (b.align) s += 'text-align:' + b.align + ';';
  if (!list && b.li) s += 'margin-left:' + Math.round(b.li / 15) + 'px;';
  if (!list && b.fi) s += 'text-indent:' + Math.round(b.fi / 15) + 'px;';
  if (b.sb) s += 'margin-top:' + Math.round(b.sb / 15) + 'px;';
  if (b.sa) s += 'margin-bottom:' + Math.round(b.sa / 15) + 'px;';
  return s ? ' style="' + s + '"' : '';
}

/* base: the body's size in half-points, font its font: runs in them are written without */
function html(blocks, base, font) {
  let out = '';
  const lists = [];
  const closeLists = () => { while (lists.length) out += '</li></' + lists.pop() + '>'; };
  for (const b of blocks) {
    if (b.rows) {
      closeLists();
      out += '<table class="rich-table"><tbody>';
      for (const r of b.rows) {
        out += '<tr>';
        r.cells.forEach((c, ci) => {
          const x0 = ci && r.x ? r.x[ci - 1] : 0, x1 = r.x ? r.x[ci] : undefined;
          const w = c.w || (x1 !== undefined && x1 > x0 ? x1 - x0 : 0);
          out += '<td' + (c.span > 1 ? ' colspan="' + c.span + '"' : '') + (c.rspan > 1 ? ' rowspan="' + c.rspan + '"' : '') +
            (w ? ' style="width:' + Math.round(w / 15) + 'px"' : '') + '>' + (html(c.blocks || [], base, font) || '<p><br></p>') + '</td>';
        });
        out += '</tr>';
      }
      out += '</tbody></table>';
      continue;
    }
    if (b.hr) { closeLists(); out += '<hr>'; continue; }
    const inner = (b.runs || []).map(r => runHtml(r, base, font)).join('');
    if (b.list) {
      const target = (b.level || 0) + 1;
      if (lists.length > target) { while (lists.length > target) out += '</li></' + lists.pop() + '>'; out += '</li>'; }
      else if (lists.length === target) out += '</li>';
      if (lists.length === target && lists[target - 1] !== b.list) out += '</' + lists.pop() + '>';
      while (lists.length < target) { out += '<' + b.list + '>'; lists.push(b.list); }
      out += '<li' + paraStyle(b, true) + '>' + (inner || '<br>');
      continue;
    }
    closeLists();
    const tag = b.h ? 'h' + Math.min(6, b.h) : 'p';
    out += '<' + tag + paraStyle(b, false) + '>' + (inner || '<br>') + '</' + tag + '>';
  }
  closeLists();
  return out;
}

/* ---------------- the page back to blocks ---------------- */
const BLOCK = new Set(['p', 'div', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'ul', 'ol', 'li', 'blockquote', 'pre', 'table',
  'hr', 'section', 'article', 'header', 'footer', 'figure', 'center', 'address', 'dl', 'dt', 'dd', 'tr', 'td', 'th', 'tbody', 'thead', 'tfoot']);
const FONT_SIZE = { 1: 8, 2: 10, 3: 12, 4: 14, 5: 18, 6: 24, 7: 36 };

/* the formatting of el on top of f's (sizes in half-points) */
function fmtOf(el, f) {
  f = Object.assign({}, f);
  const t = el.tagName.toLowerCase();
  if (t === 'b' || t === 'strong') f.b = true;
  else if (t === 'i' || t === 'em' || t === 'cite') f.i = true;
  else if (t === 'u' || t === 'ins') f.u = true;
  else if (t === 's' || t === 'strike' || t === 'del') f.s = true;
  else if (t === 'sup') { f.sup = true; f.sub = false; }
  else if (t === 'sub') { f.sub = true; f.sup = false; }
  else if (t === 'code' || t === 'tt' || t === 'kbd' || t === 'samp') f.font = 'Courier New';
  else if (t === 'font') {
    const face = firstFamily(el.getAttribute('face'));
    if (face) f.font = face;
    const c = toRgb(el.getAttribute('color'));
    if (c) f.color = c;
    const sz = FONT_SIZE[el.getAttribute('size')];
    if (sz) f.size = sz * 2;
  }
  const s = el.style;
  if (s) {
    const fw = s.fontWeight;
    if (fw === 'bold' || fw === 'bolder' || +fw >= 600) f.b = true;
    else if (fw === 'normal' || (+fw && +fw < 600)) f.b = false;
    if (s.fontStyle === 'italic' || s.fontStyle === 'oblique') f.i = true;
    else if (s.fontStyle === 'normal') f.i = false;
    const td = s.textDecorationLine || s.textDecoration || '';
    if (td.includes('underline')) f.u = true;
    if (td.includes('line-through')) f.s = true;
    const pt = toPt(s.fontSize, (f.size || 24) / 2);
    if (pt) f.size = Math.round(pt * 2);
    const fam = firstFamily(s.fontFamily);
    if (fam) f.font = fam;
    if (s.color) { const c = toRgb(s.color); if (c) f.color = c; }
    if (s.backgroundColor) { const c = toRgb(s.backgroundColor); if (c) f.bg = c; }
    if (s.verticalAlign === 'super') { f.sup = true; f.sub = false; }
    if (s.verticalAlign === 'sub') { f.sub = true; f.sup = false; }
  }
  return f;
}

function blockPP(el, pp) {
  pp = Object.assign({}, pp);
  const s = el.style || {};
  const al = (s.textAlign || el.getAttribute('align') || '').toLowerCase();
  if (al === 'center' || al === 'right' || al === 'justify') pp.align = al;
  else if (al === 'left' || al === 'start') pp.align = '';
  const ml = s.marginLeft || s.paddingLeft;
  if (parseFloat(ml) > 0 && /px$/.test(ml)) pp.li = (pp.li || 0) + Math.round(parseFloat(ml) * 15);
  pp.fi = parseFloat(s.textIndent) && /px$/.test(s.textIndent) ? Math.round(parseFloat(s.textIndent) * 15) : 0;
  pp.sb = parseFloat(s.marginTop) > 0 && /px$/.test(s.marginTop) ? Math.round(parseFloat(s.marginTop) * 15) : 0;
  pp.sa = parseFloat(s.marginBottom) > 0 && /px$/.test(s.marginBottom) ? Math.round(parseFloat(s.marginBottom) * 15) : 0;
  return pp;
}

function imageRun(el, href) {
  const src = el.getAttribute('src') || '';
  if (!/^data:image\/(png|jpe?g|gif|svg\+xml|webp|bmp);base64,/i.test(src)) return null;
  const nw = el.naturalWidth || 0, nh = el.naturalHeight || 0;
  const w = parseFloat(el.style.width) || +el.getAttribute('width') || el.width || nw || 100;
  const h = parseFloat(el.style.height) || +el.getAttribute('height') || el.height || (nw ? w * nh / nw : 0) || w;
  return { img: src, w, h, href: href || undefined };
}

/* root: the editor's page (or any element with the HTML above) */
function flat(root) {
  const top = [];
  let out = top, para = null;
  const ensure = ctx => {
    if (!para) para = Object.assign({ runs: [], list: ctx.list ? ctx.list.type : null, level: ctx.list ? ctx.list.level : 0, h: ctx.h || 0 }, ctx.pp);
    return para;
  };
  const flush = () => {
    if (!para) return;
    const p = para;
    para = null;
    while (p.runs.length && p.runs[p.runs.length - 1].t === '\n') p.runs.pop();   // a trailing <br> is the browser's placeholder
    out.push(p);
  };
  const empty = ctx => { ensure(ctx); flush(); };

  function inline(node, ctx) {
    if (node.nodeType === 3) {
      const t = node.nodeValue;
      if (!t) return;
      if (!para && !/\S/.test(t)) return;   // whitespace between blocks is formatting
      ensure(ctx).runs.push({ t, f: ctx.f });
      return;
    }
    if (node.nodeType !== 1) return;
    const tag = node.tagName.toLowerCase();
    if (tag === 'br') { ensure(ctx).runs.push({ t: '\n', f: ctx.f }); return; }
    if (tag === 'img') { const r = imageRun(node, ctx.f.href); if (r) ensure(ctx).runs.push(r); return; }
    if (tag === 'script' || tag === 'style' || tag === 'template') return;
    if (BLOCK.has(tag)) { flush(); block(node, tag, ctx); flush(); return; }
    const f = fmtOf(node, ctx.f);
    if (tag === 'a') { const href = safeUrl(node.getAttribute('href') || ''); if (href) f.href = href; }
    for (const c of Array.from(node.childNodes)) inline(c, Object.assign({}, ctx, { f }));
  }

  function block(el, tag, ctx) {
    const before = out.length;
    const c = Object.assign({}, ctx, { f: fmtOf(el, ctx.f), pp: blockPP(el, ctx.pp) });
    if (tag === 'ul' || tag === 'ol') {
      const level = ctx.list ? ctx.list.level + 1 : 0;
      for (const ch of Array.from(el.childNodes)) {
        if (ch.nodeType === 1 && ch.tagName.toLowerCase() === 'li') {
          const li = Object.assign({}, c, { f: fmtOf(ch, c.f), pp: blockPP(ch, c.pp), list: { type: tag, level }, h: 0 });
          const b0 = out.length;
          for (const x of Array.from(ch.childNodes)) inline(x, li);
          flush();
          if (out.length === b0) empty(li);
        } else inline(ch, c);
      }
      flush();
      return;
    }
    if (tag === 'table') { table(el, c); return; }
    if (tag === 'hr') { out.push({ hr: true }); return; }
    if (/^h[1-6]$/.test(tag)) c.h = +tag[1];
    if (tag === 'blockquote') c.pp = Object.assign({}, c.pp, { li: (c.pp.li || 0) + 720 });
    if (tag === 'pre') c.f = Object.assign({}, c.f, { font: 'Courier New' });
    for (const x of Array.from(el.childNodes)) inline(x, c);
    flush();
    const leaf = !['div', 'section', 'article', 'header', 'footer', 'figure', 'center', 'tbody', 'thead', 'tfoot', 'tr', 'dl'].includes(tag);
    if (out.length === before && leaf) empty(c);
  }

  function table(el, ctx) {
    const rows = [];
    for (const tr of Array.from(el.querySelectorAll(':scope > tr, :scope > tbody > tr, :scope > thead > tr, :scope > tfoot > tr'))) {
      const cells = [];
      for (const td of Array.from(tr.children).filter(x => /^t[dh]$/i.test(x.tagName))) {
        const saved = out;
        out = [];
        const cc = { f: fmtOf(td, ctx.f), pp: { align: '', li: 0, fi: 0, sb: 0, sa: 0 }, list: null, h: 0 };
        if (/^th$/i.test(td.tagName)) cc.f.b = true;
        for (const x of Array.from(td.childNodes)) inline(x, cc);
        flush();
        if (!out.length) empty(cc);
        const w = parseFloat(td.style.width) || td.offsetWidth || 0;
        cells.push({ blocks: out, span: +td.getAttribute('colspan') || 1, rspan: +td.getAttribute('rowspan') || 1, w: w ? Math.round(w * 15) : 0 });
        out = saved;
      }
      if (cells.length) rows.push({ cells });
    }
    if (rows.length) out.push({ rows });
  }

  const ctx = { f: {}, pp: { align: '', li: 0, fi: 0, sb: 0, sa: 0 }, list: null, h: 0 };
  for (const c of Array.from(root.childNodes)) inline(c, ctx);
  flush();
  return top;
}

/* a data: URL as bytes and its type */
function dataBytes(url) {
  const m = /^data:([^;,]+)(;base64)?,(.*)$/s.exec(url || '');
  if (!m) return null;
  const bin = m[2] ? atob(m[3].replace(/\s+/g, '')) : decodeURIComponent(m[3]);
  const b = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) b[i] = bin.charCodeAt(i) & 0xFF;
  return { type: m[1].toLowerCase(), bytes: b };
}
/* bytes as a data: URL */
function dataUrl(type, bytes) {
  let s = '';
  for (let i = 0; i < bytes.length; i += 8192) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 8192));
  return 'data:' + type + ';base64,' + btoa(s);
}

const Rich = { esc, hex2, safeUrl, cssFont, toRgb, toPt, firstFamily, html, flat, dataBytes, dataUrl };

/* The editor: toolbar + a contenteditable "paper". doc = {html, font, lost}; opts = {readonly,
   onChange(dirty), format: {write(page, base) -> the file (or a promise of it), images: the
   picture types it keeps}} */
class DocEditor {
  constructor(host, doc, opts) {
    this.opts = opts || {};
    this.font = doc.font || 'Arial';
    this.size = doc.size || 12;   // the body's size, pt: runs at it are written without one
    this.lost = doc.lost || {};
    this.dirty = false;
    const ro = !!this.opts.readonly;
    const cmd = (c, v) => () => { this.page.focus(); document.execCommand(c, false, v); this.touch(); this.states(); };
    const b = (label, title, fn, cls) => {
      const el = h('button', { text: label, title, class: cls || null });
      el.addEventListener('mousedown', e => e.preventDefault());   // keep the selection in the page
      el.addEventListener('click', fn);
      return el;
    };
    this.bb = b('b', tx('bold (ctrl+b)'), cmd('bold'), 'fmt-b');
    this.bi = b('i', tx('italic (ctrl+i)'), cmd('italic'), 'fmt-i');
    this.bu = b('u', tx('underline (ctrl+u)'), cmd('underline'), 'fmt-u');
    this.bs = b('s', tx('strike through'), cmd('strikeThrough'), 'fmt-s');
    const blockSel = h('select', { title: tx('paragraph style') },
      h('option', { value: '', text: tx('paragraph') }),
      [['p', tx('body text')], ['h1', tx('heading 1')], ['h2', tx('heading 2')], ['h3', tx('heading 3')]].map(([v, t]) => h('option', { value: v, text: t })));
    blockSel.addEventListener('change', () => { if (blockSel.value) cmd('formatBlock', blockSel.value)(); blockSel.value = ''; });
    const fontSel = h('select', { title: tx('font') },
      h('option', { value: '', text: tx('font') }),
      [this.font, 'Arial', 'Calibri', 'Cambria', 'Courier New', 'Georgia', 'Liberation Serif', 'Times New Roman', 'Verdana']
        .filter((f, i, a) => a.indexOf(f) === i).map(f => h('option', { value: f, text: f })));
    fontSel.addEventListener('change', () => { if (fontSel.value) { cmd('fontName', fontSel.value)(); } fontSel.value = ''; });
    const sizeSel = h('select', { title: tx('size (pt)') }, h('option', { value: '', text: tx('size') }),
      [8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 36, 48].map(s => h('option', { value: s, text: s + 'pt' })));
    sizeSel.addEventListener('change', () => { if (sizeSel.value) this.fontSize(+sizeSel.value); sizeSel.value = ''; });
    const color = h('input', { type: 'color', value: '#c0473b', title: tx('text colour') });
    color.addEventListener('input', () => cmd('foreColor', color.value)());
    const mark = h('input', { type: 'color', value: '#ffff00', title: tx('highlight') });
    mark.addEventListener('input', () => cmd('hiliteColor', mark.value)());
    this.paper = h('input', { type: 'checkbox', checked: localStorage.getItem('osada.paper') !== '0' });
    this.paper.addEventListener('change', () => {
      localStorage.setItem('osada.paper', this.paper.checked ? '1' : '0');
      this.wrap.classList.toggle('paper', this.paper.checked);
    });
    const bar = h('div', { class: 'toolbar rich-bar' },
      ro ? null : [
        b(tx('undo'), tx('undo (ctrl+z)'), cmd('undo')), b(tx('redo'), tx('redo (ctrl+y)'), cmd('redo')), h('span', { class: 'sep' }),
        blockSel, this.bb, this.bi, this.bu, this.bs,
        b('x²', tx('superscript'), cmd('superscript')), b('x₂', tx('subscript'), cmd('subscript')), h('span', { class: 'sep' }),
        fontSel, sizeSel, color, b(tx('auto'), tx('automatic text colour'), () => this.autoColor()), mark,
        b(tx('clear'), tx('remove formatting'), cmd('removeFormat')), h('span', { class: 'sep' }),
        b(tx('left'), tx('align left'), cmd('justifyLeft')), b(tx('center'), tx('center'), cmd('justifyCenter')),
        b(tx('right'), tx('align right'), cmd('justifyRight')), b(tx('justify'), tx('justify'), cmd('justifyFull')), h('span', { class: 'sep' }),
        b(tx('• list'), tx('bullet list'), cmd('insertUnorderedList')), b(tx('1. list'), tx('numbered list'), cmd('insertOrderedList')),
        b('>>', tx('indent'), cmd('indent')), b('<<', tx('outdent'), cmd('outdent')), h('span', { class: 'sep' }),
        b(tx('link'), tx('link (ctrl+k)'), () => this.link()), b(tx('image'), tx('insert a picture'), () => this.image()),
        b(tx('table'), tx('insert a table'), () => this.table()), b(tx('rule'), tx('horizontal line'), cmd('insertHorizontalRule')),
      ],
      h('span', { class: 'spacer' }),
      h('label', { class: 'chk', title: tx('black on white, like print') }, this.paper, ' ' + tx('paper')));
    this.page = h('div', { class: 'rich-page', contenteditable: ro ? 'false' : 'true', spellcheck: true });
    this.page.style.fontFamily = cssFont(this.font) + ', sans-serif';
    this.page.style.fontSize = this.size + 'pt';
    this.page.innerHTML = doc.html || '<p><br></p>';
    this.wrap = h('div', { class: 'rich-wrap' + (this.paper.checked ? ' paper' : '') }, this.page);
    clear(host).append(bar, this.wrap);
    host.classList.add('rich-host');
    try {
      document.execCommand('defaultParagraphSeparator', false, 'p');
      document.execCommand('styleWithCSS', false, false);
    } catch (e) { /* old browser */ }
    this.page.addEventListener('input', () => this.touch());
    this.page.addEventListener('keydown', e => {
      if ((e.ctrlKey || e.metaKey) && e.key === 'k') { e.preventDefault(); this.link(); }
    });
    this.page.addEventListener('paste', e => this.paste(e));
    // links: ctrl+click opens, a plain click edits
    this.page.addEventListener('click', e => {
      const a = e.target.closest('a');
      if (a && (e.ctrlKey || e.metaKey)) { e.preventDefault(); window.open(a.href, '_blank', 'noopener'); }
    });
    this.onSel = () => this.states();
    document.addEventListener('selectionchange', this.onSel);
  }

  get images() { return (this.opts.format && this.opts.format.images) || ['image/png', 'image/jpeg']; }

  destroy() { document.removeEventListener('selectionchange', this.onSel); }

  touch() {
    if (this.dirty) return;
    this.dirty = true;
    if (this.opts.onChange) this.opts.onChange(true);
  }
  markSaved() { this.dirty = false; if (this.opts.onChange) this.opts.onChange(false); }

  states() {
    if (!this.page.contains(document.getSelection().anchorNode)) return;
    const q = c => { try { return document.queryCommandState(c); } catch (e) { return false; } };
    this.bb.classList.toggle('on', q('bold'));
    this.bi.classList.toggle('on', q('italic'));
    this.bu.classList.toggle('on', q('underline'));
    this.bs.classList.toggle('on', q('strikeThrough'));
  }

  /* the file, in the format it was opened with */
  text() { return this.opts.format.write(this.page, { font: this.font, size: this.size }); }

  /* execCommand only knows sizes 1-7; mark with 7, then set the real size */
  fontSize(pt) {
    this.page.focus();
    document.execCommand('fontSize', false, '7');
    for (const f of this.page.querySelectorAll('font[size="7"]')) {
      f.removeAttribute('size');
      f.style.fontSize = pt + 'pt';
    }
    this.touch();
  }

  /* black is written as "automatic"; drop it from the page too, or it is unreadable when dark */
  autoColor() {
    this.page.focus();
    document.execCommand('foreColor', false, '#000000');
    for (const f of this.page.querySelectorAll('font[color]')) {
      if (toRgb(f.getAttribute('color')) !== '#000000') continue;
      f.removeAttribute('color');
      if (!f.attributes.length) f.replaceWith(...f.childNodes);
    }
    for (const el of this.page.querySelectorAll('[style*="color"]')) {
      if (toRgb(el.style.color) === '#000000') el.style.color = '';
    }
    this.touch();
  }

  async link() {
    const sel = document.getSelection();
    const range = sel.rangeCount ? sel.getRangeAt(0).cloneRange() : null;
    const a = sel.anchorNode && sel.anchorNode.parentElement && sel.anchorNode.parentElement.closest('a');
    const url = await ask(tx('link'), tx('address (empty removes the link)'), a ? a.getAttribute('href') : 'https://', tx('set'));
    if (url === null) return;
    this.page.focus();
    if (range) { sel.removeAllRanges(); sel.addRange(range); }
    if (!url.trim()) { document.execCommand('unlink'); this.touch(); return; }
    if (!/^(https?:|mailto:)/i.test(url.trim())) { toast(tx('links must start with http://, https:// or mailto:'), 'err'); return; }
    if (range && range.collapsed) document.execCommand('insertHTML', false, '<a href="' + url.trim().replace(/"/g, '%22') + '">' + escHtml(url.trim()) + '</a>');
    else document.execCommand('createLink', false, url.trim());
    this.touch();
  }

  image() {
    const inp = h('input', { type: 'file', accept: this.images.join(',') });
    inp.addEventListener('change', () => { if (inp.files[0]) this.insertImage(inp.files[0]); });
    inp.click();
  }

  insertImage(file) {
    if (!this.images.includes(file.type)) { toast(tx('this document takes these pictures: {types}', { types: this.images.map(t => t.replace('image/', '')).join(', ') }), 'err'); return; }
    const r = new FileReader();
    r.onload = () => {
      const img = new Image();
      img.onload = () => {
        // no wider than the page
        const w = Math.min(img.naturalWidth, 640), hgt = Math.round(img.naturalHeight * w / img.naturalWidth);
        this.page.focus();
        document.execCommand('insertHTML', false, '<img src="' + r.result + '" style="width:' + w + 'px;height:' + hgt + 'px" alt="">');
        this.touch();
      };
      img.src = r.result;
    };
    r.readAsDataURL(file);
  }

  paste(e) {
    const items = e.clipboardData ? Array.from(e.clipboardData.items) : [];
    const img = items.find(i => i.kind === 'file' && this.images.includes(i.type));
    if (img && !e.clipboardData.getData('text/html')) { e.preventDefault(); this.insertImage(img.getAsFile()); }
  }

  async table() {
    const v = await ask(tx('table'), tx('columns x rows'), '3x3', tx('insert'));
    if (!v) return;
    const m = /^\s*(\d+)\s*[x×*, ]\s*(\d+)\s*$/.exec(v);
    if (!m || +m[1] < 1 || +m[2] < 1 || +m[1] > 30 || +m[2] > 500) { toast(tx('say it like 3x4 (columns x rows)'), 'err'); return; }
    const w = Math.floor(640 / +m[1]);
    let s = '<table class="rich-table"><tbody>';
    for (let r = 0; r < +m[2]; r++) {
      s += '<tr>';
      for (let c = 0; c < +m[1]; c++) s += '<td style="width:' + w + 'px"><br></td>';
      s += '</tr>';
    }
    s += '</tbody></table><p><br></p>';
    this.page.focus();
    document.execCommand('insertHTML', false, s);
    this.touch();
  }
}

return { Rich, DocEditor };
});
