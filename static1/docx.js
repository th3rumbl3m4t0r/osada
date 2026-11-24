/* Word documents (.docx, .docm, .dotx, .dotm): read into the editor's blocks (rich.js) to be
   shown and edited as .odt, and written from them as the "export .docx" of an .odt. Part
   docx, on zip and rich. Read: headings (by style or outline level), paragraphs and their
   alignment, indents and spacing; run formatting through the style chain (bold, italic,
   underline, strike, super- and subscript, fonts, sizes, colours, highlight); lists by
   their numbering (bullets or numbers); links; tables with merged cells and widths;
   pictures (DrawingML and VML); tracked insertions (deletions are left out); field results;
   text boxes' text. Footnotes, comments, headers and footers are counted as lost. Macros
   (a .docm's vbaProject.bin) are never read: macros. */
'use strict';

part('docx', ({ ZIP, Rich }) => {

const W = 'http://schemas.openxmlformats.org/wordprocessingml/2006/main';
const R = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships';
const PR = 'http://schemas.openxmlformats.org/package/2006/relationships';
const A = 'http://schemas.openxmlformats.org/drawingml/2006/main';
const WP = 'http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing';
const PIC = 'http://schemas.openxmlformats.org/drawingml/2006/picture';
const V = 'urn:schemas-microsoft-com:vml';
const MC = 'http://schemas.openxmlformats.org/markup-compatibility/2006';
const IMG_TYPES = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', svg: 'image/svg+xml', bmp: 'image/bmp', webp: 'image/webp' };
const HIGHLIGHT = { yellow: '#ffff00', green: '#00ff00', cyan: '#00ffff', magenta: '#ff00ff', blue: '#0000ff', red: '#ff0000', darkBlue: '#000080',
  darkCyan: '#008080', darkGreen: '#008000', darkMagenta: '#800080', darkRed: '#800000', darkYellow: '#808000', darkGray: '#808080',
  lightGray: '#c0c0c0', black: '#000000', white: '#ffffff' };

const wa = (el, name) => (el ? el.getAttributeNS(W, name) : null);
const wkid = (el, name) => (el ? Array.from(el.children).find(x => x.namespaceURI === W && x.localName === name) || null : null);
const kids = el => (el ? Array.from(el.children) : []);
const on = el => !!el && !/^(0|false|off|none)$/.test(wa(el, 'val') || '');

function partPath(base, target) {
  if (target.startsWith('/')) return target.slice(1);
  const out = base.split('/').filter(Boolean);
  for (const s of target.split('/')) { if (s === '..') out.pop(); else if (s !== '.') out.push(s); }
  return out.join('/');
}

/* ---------------- reading ---------------- */
async function parse(bytes) {
  const files = new Map(ZIP.read(bytes).map(f => [f.name, f]));
  const xml = async name => {
    const f = files.get(name);
    if (!f) return null;
    const d = new DOMParser().parseFromString(new TextDecoder().decode(await ZIP.inflate(f)), 'application/xml');
    if (d.getElementsByTagName('parsererror').length) throw new Error(name + ': broken XML');
    return d;
  };
  const rels = async base => {
    const out = {};
    const d = await xml(base.replace(/[^/]*$/, '') + '_rels/' + base.split('/').pop() + '.rels');
    if (d) for (const r of d.getElementsByTagNameNS(PR, 'Relationship')) out[r.getAttribute('Id')] = { target: r.getAttribute('Target'), external: r.getAttribute('TargetMode') === 'External', type: r.getAttribute('Type') || '' };
    return out;
  };
  let main = 'word/document.xml';
  const root = await rels('');
  for (const id in root) if (/\/officeDocument$/.test(root[id].type)) main = partPath('', root[id].target);
  const doc = await xml(main);
  if (!doc) throw new Error('not a Word document (no document.xml)');
  const lost = {};
  const lose = what => { lost[what] = (lost[what] || 0) + 1; };
  const docRels = await rels(main);
  const dir = main.replace(/[^/]*$/, '');
  const byType = t => { for (const id in docRels) if (docRels[id].type.endsWith('/' + t)) return partPath(dir, docRels[id].target); return null; };
  const macros = [...files.keys()].some(n => /vbaProject\.bin$/i.test(n));

  // the theme's fonts, for runs that name a theme font
  const theme = {};
  const th = byType('theme') && await xml(byType('theme'));
  if (th) {
    const lat = (tag) => { const el = th.getElementsByTagNameNS(A, tag)[0]; const l = el && el.getElementsByTagNameNS(A, 'latin')[0]; return l && l.getAttribute('typeface'); };
    theme.major = lat('majorFont');
    theme.minor = lat('minorFont');
  }

  // styles: id -> element; the defaults
  const st = byType('styles') && await xml(byType('styles'));
  const styles = {};
  let defR = null, defP = null, normal = null;
  if (st) {
    for (const s of st.getElementsByTagNameNS(W, 'style')) {
      styles[wa(s, 'styleId')] = s;
      if (wa(s, 'type') === 'paragraph' && /^(1|true)$/.test(wa(s, 'default') || '')) normal = wa(s, 'styleId');
    }
    const dd = st.getElementsByTagNameNS(W, 'docDefaults')[0];
    defR = dd && wkid(wkid(dd, 'rPrDefault'), 'rPr');
    defP = dd && wkid(wkid(dd, 'pPrDefault'), 'pPr');
  }

  const runProps = (rPr, f) => {
    if (!rPr) return f;
    f = Object.assign({}, f);
    for (const c of kids(rPr)) {
      if (c.namespaceURI !== W) continue;
      switch (c.localName) {
        case 'b': f.b = on(c); break;
        case 'i': f.i = on(c); break;
        case 'u': f.u = on(c); break;
        case 'strike': case 'dstrike': f.s = on(c); break;
        case 'vertAlign': f.sup = wa(c, 'val') === 'superscript'; f.sub = wa(c, 'val') === 'subscript'; break;
        case 'color': { const v = wa(c, 'val'); f.color = v && /^[0-9a-f]{6}$/i.test(v) && v !== '000000' ? '#' + v.toLowerCase() : null; break; }
        case 'sz': f.size = +wa(c, 'val') || f.size; break;
        case 'highlight': f.bg = HIGHLIGHT[wa(c, 'val')] || null; break;
        case 'shd': { const v = wa(c, 'fill'); if (v && /^[0-9a-f]{6}$/i.test(v) && v.toUpperCase() !== 'FFFFFF') f.bg = '#' + v.toLowerCase(); break; }
        case 'rFonts': {
          const v = wa(c, 'ascii') || wa(c, 'hAnsi') || (wa(c, 'asciiTheme') && (/major/.test(wa(c, 'asciiTheme')) ? theme.major : theme.minor));
          if (v) f.font = v;
          break;
        }
        case 'vanish': f.hidden = on(c); break;
      }
    }
    return f;
  };
  const paraProps = (pPr, b) => {
    if (!pPr) return b;
    b = Object.assign({}, b);
    for (const c of kids(pPr)) {
      if (c.namespaceURI !== W) continue;
      switch (c.localName) {
        case 'jc': { const v = wa(c, 'val'); b.align = v === 'center' ? 'center' : v === 'right' || v === 'end' ? 'right' : v === 'both' || v === 'distribute' ? 'justify' : ''; break; }
        case 'ind': {
          const l = wa(c, 'left') || wa(c, 'start');
          if (l !== null) b.li = +l || 0;
          if (wa(c, 'firstLine') !== null) b.fi = +wa(c, 'firstLine') || 0;
          if (wa(c, 'hanging') !== null) b.fi = -(+wa(c, 'hanging') || 0);
          break;
        }
        case 'spacing':
          if (wa(c, 'before') !== null) b.sb = +wa(c, 'before') || 0;
          if (wa(c, 'after') !== null) b.sa = +wa(c, 'after') || 0;
          break;
        case 'outlineLvl': { const v = +wa(c, 'val'); if (v < 9) b.h = v + 1; break; }
        case 'numPr': { const id = wkid(c, 'numId'), lv = wkid(c, 'ilvl'); if (id) b.num = { id: wa(id, 'val'), lvl: lv ? +wa(lv, 'val') || 0 : b.num ? b.num.lvl : 0 }; break; }
      }
    }
    return b;
  };
  // a style with its bases: {f, p (paragraph props), h}
  const memo = new Map();
  const style = (id, depth) => {
    if (memo.has(id)) return memo.get(id);
    const s = styles[id];
    let r;
    if (!s || (depth || 0) > 20) r = { f: runProps(defR, { size: 20 }), p: paraProps(defP, {}) };
    else {
      const base = style(wa(wkid(s, 'basedOn'), 'val') || '', (depth || 0) + 1);
      r = { f: runProps(wkid(s, 'rPr'), base.f), p: paraProps(wkid(s, 'pPr'), base.p) };
      const name = (wa(wkid(s, 'name'), 'val') || '').toLowerCase();
      const m = /^heading (\d)$/.exec(name) || /^heading(\d)$/i.exec(id);
      if (m) r.p.h = +m[1];
      if (name === 'title') r.p.h = r.p.h || 1;
    }
    memo.set(id, r);
    return r;
  };

  // lists: numId -> abstract levels -> bullet or number
  const numbering = byType('numbering') && await xml(byType('numbering'));
  const abstracts = {}, nums = {};
  if (numbering) {
    for (const a of numbering.getElementsByTagNameNS(W, 'abstractNum')) {
      const lv = { single: wa(wkid(a, 'multiLevelType'), 'val') === 'singleLevel' };
      for (const l of a.getElementsByTagNameNS(W, 'lvl')) {
        const ind = wkid(wkid(l, 'pPr'), 'ind');
        lv[+wa(l, 'ilvl')] = wa(wkid(l, 'numFmt'), 'val') || 'decimal';
        lv['ind' + wa(l, 'ilvl')] = ind ? +(wa(ind, 'left') || wa(ind, 'start') || 0) : 0;
      }
      abstracts[wa(a, 'abstractNumId')] = lv;
    }
    for (const n of numbering.getElementsByTagNameNS(W, 'num')) {
      const lv = Object.assign({}, abstracts[wa(wkid(n, 'abstractNumId'), 'val')] || {});
      for (const o of kids(n).filter(x => x.localName === 'lvlOverride')) { const l = wkid(o, 'lvl'); if (l) lv[+wa(o, 'ilvl')] = wa(wkid(l, 'numFmt'), 'val') || lv[+wa(o, 'ilvl')]; }
      lv.single = (abstracts[wa(wkid(n, 'abstractNumId'), 'val')] || {}).single;
      nums[wa(n, 'numId')] = lv;
    }
  }
  const listOf = num => {
    if (!num || num.id === '0' || !nums[num.id]) return null;
    const fmt = nums[num.id][num.lvl] || 'decimal';
    return fmt === 'bullet' || fmt === 'none' ? 'ul' : 'ol';
  };

  const image = async (rid, w, h) => {
    const r = docRels[rid];
    if (!r || r.external) { lose('linked pictures'); return null; }
    const p = partPath(dir, r.target), f = files.get(p);
    const t = IMG_TYPES[p.toLowerCase().split('.').pop()];
    if (!f || !t) { lose('pictures in other formats'); return null; }
    return { img: Rich.dataUrl(t, await ZIP.inflate(f)), w: w || 200, h: h || 150 };
  };

  const blocks = [];
  // a field is begun, its result shown: [{inResult}] for fields that nest
  let fields = [];

  async function runsOf(el, f, out, extra, href) {
    for (const c of kids(el)) {
      if (c.namespaceURI === MC && c.localName === 'AlternateContent') {
        const alt = kids(c).find(x => x.localName === 'Choice' && x.getElementsByTagNameNS(PIC, 'pic').length) || kids(c).find(x => x.localName === 'Fallback') || kids(c)[0];
        if (alt) await runsOf(alt, f, out, extra, href);
        continue;
      }
      if (c.namespaceURI !== W) continue;
      switch (c.localName) {
        case 'r': await run(c, f, out, extra, href); break;
        case 'hyperlink': {
          const r = docRels[c.getAttributeNS(R, 'id')];
          const url = r && r.external ? Rich.safeUrl(r.target) : null;
          await runsOf(c, f, out, extra, url || href);
          break;
        }
        case 'ins': case 'smartTag': case 'customXml': case 'fldSimple': case 'dir': case 'bdo': await runsOf(c, f, out, extra, href); break;
        case 'sdt': await runsOf(wkid(c, 'sdtContent'), f, out, extra, href); break;
        case 'del': case 'moveFrom': break;
        case 'moveTo': await runsOf(c, f, out, extra, href); break;
        case 'commentRangeStart': lose('comments'); break;
      }
    }
  }

  async function run(r, pf, out, extra, href) {
    const rPr = wkid(r, 'rPr');
    const cs = rPr && wkid(rPr, 'rStyle');
    let f = cs ? runProps(wkid(styles[wa(cs, 'val')], 'rPr'), pf) : pf;
    f = runProps(rPr, f);
    if (href) f = Object.assign({}, f, { href });
    const hidden = f.hidden || fields.some(x => !x.result);
    for (const c of kids(r)) {
      if (c.namespaceURI === MC && c.localName === 'AlternateContent') {
        const alt = kids(c).find(x => x.localName === 'Choice' && x.getElementsByTagNameNS(PIC, 'pic').length) || kids(c).find(x => x.localName === 'Fallback') || kids(c)[0];
        if (alt) for (const k of kids(alt)) await runPiece(k, f, out, extra, hidden);
        continue;
      }
      await runPiece(c, f, out, extra, hidden);
    }
  }

  async function runPiece(c, f, out, extra, hidden) {
    if (c.namespaceURI === W) {
      switch (c.localName) {
        case 'fldChar': {
          const t = wa(c, 'fldCharType');
          if (t === 'begin') fields.push({ result: false });
          else if (t === 'separate' && fields.length) fields[fields.length - 1].result = true;
          else if (t === 'end') fields.pop();
          return;
        }
        case 't': if (!hidden) out.push({ t: c.textContent, f }); return;
        case 'tab': case 'ptab': if (!hidden) out.push({ t: '\t', f }); return;
        case 'br': case 'cr': if (!hidden && wa(c, 'type') !== 'page' && wa(c, 'type') !== 'column') out.push({ t: '\n', f }); return;
        case 'noBreakHyphen': if (!hidden) out.push({ t: '‑', f }); return;
        case 'softHyphen': return;
        case 'sym': {
          const ch = parseInt(wa(c, 'char') || '', 16);
          if (!hidden && ch) out.push({ t: String.fromCharCode(ch >= 0xF000 ? ch - 0xF000 : ch), f: Object.assign({}, f, { font: wa(c, 'font') || f.font }) });
          return;
        }
        case 'footnoteReference': case 'endnoteReference': lose('footnotes'); out.push({ t: '*', f: Object.assign({}, f, { sup: true }) }); return;
        case 'commentReference': return;
        case 'drawing': if (!hidden) await drawing(c, f, out, extra); return;
        case 'pict': case 'object': if (!hidden) await vml(c, f, out, extra); return;
      }
    }
  }

  async function drawing(el, f, out, extra) {
    for (const holder of el.getElementsByTagNameNS(WP, '*')) {
      if (holder.localName !== 'inline' && holder.localName !== 'anchor') continue;
      if (holder.localName === 'anchor') lose('picture positions');
      const ext = holder.getElementsByTagNameNS(WP, 'extent')[0];
      const w = ext ? +ext.getAttribute('cx') / 9525 : 0, h = ext ? +ext.getAttribute('cy') / 9525 : 0;
      const blip = holder.getElementsByTagNameNS(A, 'blip')[0];
      if (blip) {
        const r = await image(blip.getAttributeNS(R, 'embed'), w, h);
        if (r) out.push(Object.assign(r, f.href ? { href: f.href } : {}));
        continue;
      }
      const box = holder.getElementsByTagNameNS(W, 'txbxContent')[0];
      if (box) { lose('text box positions'); await body(box, extra); continue; }
      lose('drawings');
    }
  }

  async function vml(el, f, out, extra) {
    const box = el.getElementsByTagNameNS(W, 'txbxContent')[0];
    if (box) { lose('text box positions'); await body(box, extra); return; }
    const im = el.getElementsByTagNameNS(V, 'imagedata')[0];
    if (!im) { lose('drawings'); return; }
    const shape = im.parentElement, css = (shape && shape.getAttribute('style')) || '';
    const len = k => { const m = new RegExp(k + ':\\s*([\\d.]+)(pt|px|in|cm)?').exec(css); return m ? parseFloat(m[1]) * ({ pt: 4 / 3, px: 1, in: 96, cm: 96 / 2.54 }[m[2] || 'px']) : 0; };
    const r = await image(im.getAttributeNS(R, 'id'), len('width'), len('height'));
    if (r) out.push(r);
  }

  async function para(p, out) {
    const pPr = wkid(p, 'pPr');
    const ps = style((pPr && wa(wkid(pPr, 'pStyle'), 'val')) || normal || '');
    const props = paraProps(pPr, ps.p);
    const extra = [];
    const b = { runs: [], align: props.align || '', li: props.li || 0, fi: props.fi || 0, sb: props.sb || 0, sa: props.sa || 0, h: props.h && props.h <= 6 ? props.h : 0 };
    const list = listOf(props.num);
    if (list) {
      // single-level lists (List Bullet, List Bullet 2…) are nested by their indent alone: ranked when the body is read
      const lv = nums[props.num.id];
      b.list = list;
      b.level = Math.min(8, props.num.lvl);
      if (lv && lv.single) b.ind = props.li || lv['ind' + props.num.lvl] || 0;
      b.li = 0;
      b.fi = 0;
    }
    await runsOf(p, ps.f, b.runs, extra);
    out.push(b);
    out.push(...extra);
  }

  async function table(tbl, out) {
    const grid = kids(wkid(tbl, 'tblGrid')).filter(x => x.localName === 'gridCol').map(g => +wa(g, 'w') || 0);
    const rows = [];
    const above = [];   // per grid column: the cell a vertical merge continues
    for (const tr of kids(tbl).filter(x => x.namespaceURI === W && (x.localName === 'tr' || x.localName === 'sdt'))) {
      const trs = tr.localName === 'sdt' ? kids(wkid(tr, 'sdtContent')).filter(x => x.localName === 'tr') : [tr];
      for (const row of trs) {
        const cells = [];
        let col = 0;
        const gb = wkid(wkid(row, 'trPr'), 'gridBefore');
        if (gb) col += +wa(gb, 'val') || 0;
        for (const tc of kids(row).filter(x => x.namespaceURI === W && (x.localName === 'tc' || x.localName === 'sdt'))) {
          const cell = tc.localName === 'sdt' ? wkid(wkid(tc, 'sdtContent'), 'tc') : tc;
          if (!cell) continue;
          const pr = wkid(cell, 'tcPr');
          const span = +wa(wkid(pr, 'gridSpan'), 'val') || 1;
          const vm = wkid(pr, 'vMerge');
          if (vm && wa(vm, 'val') !== 'restart' && above[col]) {
            above[col].rspan++;
            col += span;
            continue;
          }
          const cb = [];
          await body(cell, cb);
          let w = 0;
          for (let i = 0; i < span; i++) w += grid[col + i] || 0;
          const c = { blocks: cb.length ? cb : [{ runs: [] }], span, rspan: 1, w };
          cells.push(c);
          for (let i = 0; i < span; i++) above[col + i] = vm ? c : null;
          col += span;
        }
        rows.push({ cells });
      }
    }
    if (rows.length) out.push({ rows });
  }

  async function body(el, out) {
    for (const c of kids(el)) {
      if (c.namespaceURI !== W) {
        if (c.namespaceURI === MC && c.localName === 'AlternateContent') { const alt = kids(c).find(x => x.localName === 'Fallback') || kids(c)[0]; if (alt) await body(alt, out); }
        continue;
      }
      if (c.localName === 'p') await para(c, out);
      else if (c.localName === 'tbl') await table(c, out);
      else if (c.localName === 'sdt') await body(wkid(c, 'sdtContent'), out);
      else if (c.localName === 'customXml' || c.localName === 'ins' || c.localName === 'moveTo') await body(c, out);
      else if (c.localName === 'sectPr') { if (wkid(c, 'headerReference') || wkid(c, 'footerReference')) lose('headers and footers'); }
    }
  }

  await body(doc.getElementsByTagNameNS(W, 'body')[0], blocks);
  // single-level lists: their indents, deepest last, are the levels
  const all = [], walk = bs => bs.forEach(b => { if (b.rows) b.rows.forEach(r => r.cells.forEach(c => walk(c.blocks))); else if (b.ind !== undefined) all.push(b); });
  walk(blocks);
  const inds = [...new Set(all.map(b => b.ind))].sort((x, y) => x - y);
  for (const b of all) { b.level = Math.min(8, Math.max(b.level, inds.indexOf(b.ind))); delete b.ind; }
  for (const sp of doc.getElementsByTagNameNS(W, 'sectPr')) if (sp.parentElement && sp.parentElement.localName === 'pPr' && (wkid(sp, 'headerReference') || wkid(sp, 'footerReference'))) lose('headers and footers');
  const base = style(normal || '');
  const font = base.f.font || theme.minor || 'Calibri';
  return { html: Rich.html(blocks, base.f.size || 20, font), font, size: (base.f.size || 20) / 2, lost, blocks, macros };
}

/* ---------------- writing ---------------- */
const xe = s => String(s).replace(/[\x00-\x08\x0B\x0C\x0E-\x1F￾￿]/g, '').replace(/[&<>"]/g, c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : '&quot;'));
const MIME_EXT = { 'image/png': 'png', 'image/jpeg': 'jpeg', 'image/gif': 'gif', 'image/bmp': 'bmp', 'image/svg+xml': 'svg', 'image/webp': 'webp' };

/* blocks (Rich.flat of a page) as a .docx; base = {font, size (pt)} */
async function write(blocks, base) {
  base = Object.assign({ font: 'Calibri', size: 11 }, base || {});
  const rels = [], media = [];
  const nums = [];
  let picId = 0;
  const rel = (type, target, external) => {
    const id = 'rId' + (rels.length + 10);
    rels.push('<Relationship Id="' + id + '" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/' + type + '" Target="' + xe(target) + '"' + (external ? ' TargetMode="External"' : '') + '/>');
    return id;
  };
  const linkIds = new Map();
  const linkId = url => { if (!linkIds.has(url)) linkIds.set(url, rel('hyperlink', url, true)); return linkIds.get(url); };

  const rPr = (f, link) => {
    let s = '';
    if (link) s += '<w:rStyle w:val="Hyperlink"/>';
    if (f.font && f.font.toLowerCase() !== base.font.toLowerCase()) s += '<w:rFonts w:ascii="' + xe(f.font) + '" w:hAnsi="' + xe(f.font) + '" w:cs="' + xe(f.font) + '"/>';
    if (f.b) s += '<w:b/><w:bCs/>';
    if (f.i) s += '<w:i/><w:iCs/>';
    if (f.s) s += '<w:strike/>';
    if (f.color && f.color !== '#000000') s += '<w:color w:val="' + f.color.slice(1).toUpperCase() + '"/>';
    if (f.size && Math.abs(f.size / 2 - base.size) > 0.01) s += '<w:sz w:val="' + Math.round(f.size) + '"/><w:szCs w:val="' + Math.round(f.size) + '"/>';
    if (f.u) s += '<w:u w:val="single"/>';
    if (f.bg && f.bg !== '#ffffff') s += '<w:shd w:val="clear" w:color="auto" w:fill="' + f.bg.slice(1).toUpperCase() + '"/>';
    if (f.sup) s += '<w:vertAlign w:val="superscript"/>';
    else if (f.sub) s += '<w:vertAlign w:val="subscript"/>';
    return s ? '<w:rPr>' + s + '</w:rPr>' : '';
  };
  const textRun = (t, f, link) => {
    let x = '';
    for (const part of t.split(/(\t|\n)/)) {
      if (part === '\t') x += '<w:tab/>';
      else if (part === '\n') x += '<w:br/>';
      else if (part) x += '<w:t xml:space="preserve">' + xe(part) + '</w:t>';
    }
    return x ? '<w:r>' + rPr(f, link) + x + '</w:r>' : '';
  };
  const picRun = r => {
    const d = Rich.dataBytes(r.img);
    if (!d || !MIME_EXT[d.type]) return '';
    picId++;
    const name = 'image' + picId + '.' + MIME_EXT[d.type];
    media.push({ path: 'word/media/' + name, bytes: d.bytes });
    const id = rel('image', 'media/' + name);
    const cx = Math.round(r.w * 9525), cy = Math.round(r.h * 9525);
    return '<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="' + cx + '" cy="' + cy + '"/><wp:docPr id="' + picId + '" name="Picture ' + picId +
      '"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect="1"/></wp:cNvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="' + picId +
      '" name="' + name + '"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="' + id + '"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="' + cx + '" cy="' + cy +
      '"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>';
  };
  const runsXml = runs => {
    let out = '';
    for (let i = 0; i < runs.length; i++) {
      const r = runs[i], href = r.img ? r.href : r.f && r.f.href;
      if (href) {
        // the link's runs together
        let inner = '';
        while (i < runs.length && (runs[i].img ? runs[i].href : runs[i].f && runs[i].f.href) === href) {
          inner += runs[i].img ? picRun(runs[i]) : textRun(runs[i].t || '', runs[i].f || {}, true);
          i++;
        }
        i--;
        out += '<w:hyperlink r:id="' + linkId(href) + '" w:history="1">' + inner + '</w:hyperlink>';
        continue;
      }
      out += r.img ? picRun(r) : textRun(r.t || '', r.f || {}, false);
    }
    return out;
  };
  const pXml = (b, numId, border) => {
    let pp = '';
    if (b.h) pp += '<w:pStyle w:val="Heading' + Math.min(6, b.h) + '"/>';
    else if (b.list) pp += '<w:pStyle w:val="ListParagraph"/>';
    if (b.list) pp += '<w:numPr><w:ilvl w:val="' + Math.min(8, b.level || 0) + '"/><w:numId w:val="' + numId + '"/></w:numPr>';
    if (border) pp += '<w:pBdr><w:bottom w:val="single" w:sz="6" w:space="1" w:color="auto"/></w:pBdr>';
    if (b.sb || b.sa) pp += '<w:spacing' + (b.sb ? ' w:before="' + b.sb + '"' : '') + (b.sa ? ' w:after="' + b.sa + '"' : '') + '/>';
    if (!b.list && (b.li || b.fi)) pp += '<w:ind' + (b.li ? ' w:left="' + b.li + '"' : '') + (b.fi > 0 ? ' w:firstLine="' + b.fi + '"' : b.fi < 0 ? ' w:hanging="' + -b.fi + '"' : '') + '/>';
    if (b.align) pp += '<w:jc w:val="' + (b.align === 'justify' ? 'both' : b.align) + '"/>';
    return '<w:p>' + (pp ? '<w:pPr>' + pp + '</w:pPr>' : '') + runsXml(b.runs || []) + '</w:p>';
  };
  const tblXml = t => {
    // the grid, as in odt.js: cells spanned from above take their columns
    const grid = [];
    let ncols = 0;
    t.rows.forEach((row, ri) => {
      grid[ri] = grid[ri] || [];
      let col = 0;
      for (const cell of row.cells) {
        while (grid[ri][col]) col++;
        for (let dr = 0; dr < (cell.rspan || 1); dr++) {
          grid[ri + dr] = grid[ri + dr] || [];
          for (let dc = 0; dc < (cell.span || 1); dc++) grid[ri + dr][col + dc] = dr === 0 ? (dc === 0 ? cell : 'span') : (dc === 0 ? { cont: cell } : 'span');
        }
        col += cell.span || 1;
      }
      ncols = Math.max(ncols, grid[ri].length);
    });
    const widths = [];
    t.rows.forEach((row, ri) => row.cells.forEach(cell => {
      const col = grid[ri].indexOf(cell);
      if (col >= 0 && (cell.span || 1) === 1 && cell.w && !widths[col]) widths[col] = cell.w;
    }));
    const total = 9638, known = widths.filter(Boolean).reduce((a, b) => a + b, 0), unknown = ncols - widths.filter(Boolean).length;
    const fill = unknown ? Math.max(567, Math.round((total - known) / unknown)) : 0;
    for (let c = 0; c < ncols; c++) widths[c] = widths[c] || fill;
    let x = '<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="0" w:type="auto"/><w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/></w:tblPr><w:tblGrid>' +
      widths.map(w => '<w:gridCol w:w="' + w + '"/>').join('') + '</w:tblGrid>';
    grid.forEach(gr => {
      x += '<w:tr>';
      for (let c = 0; c < ncols; c++) {
        const g = gr[c];
        if (g === 'span') continue;
        const cell = g && (g.cont || g);
        const span = cell ? cell.span || 1 : 1;
        let w = 0;
        for (let i = 0; i < span; i++) w += widths[c + i] || 0;
        let pr = '<w:tcW w:w="' + w + '" w:type="dxa"/>' + (span > 1 ? '<w:gridSpan w:val="' + span + '"/>' : '');
        if (g && g.cont) pr += '<w:vMerge/>';
        else if (cell && cell.rspan > 1) pr += '<w:vMerge w:val="restart"/>';
        const inner = !cell || g.cont ? '<w:p/>' : blocksXml(cell.blocks && cell.blocks.length ? cell.blocks : [{ runs: [] }], true);
        x += '<w:tc><w:tcPr>' + pr + '</w:tcPr>' + inner + '</w:tc>';
      }
      x += '</w:tr>';
    });
    return x + '</w:tbl>';
  };
  /* lists: a num per level and kind for as long as a list goes on; a new numbered one starts at 1 */
  function blocksXml(bs, inCell) {
    let out = '', cur = [];
    for (const b of bs) {
      if (b.rows) { out += tblXml(b); cur = []; continue; }
      if (b.hr) { out += pXml({ runs: [] }, 0, true); cur = []; continue; }
      if (b.list) {
        const lv = Math.min(8, b.level || 0);
        if (!cur[lv] || cur[lv].type !== b.list) {
          nums.push({ type: b.list, level: lv });
          cur[lv] = { type: b.list, id: nums.length };
        }
        cur.length = lv + 1;   // a deeper list after this item starts anew
        out += pXml(b, cur[lv].id);
        continue;
      }
      cur = [];
      out += pXml(b);
    }
    // a cell must end with a paragraph
    if (inCell && !/<\/w:p>$|<w:p\/>$/.test(out)) out += '<w:p/>';
    return out;
  }

  const body = blocksXml(blocks.length ? blocks : [{ runs: [] }]);
  const NSD = 'xmlns:w="' + W + '" xmlns:r="' + R + '" xmlns:wp="' + WP + '" xmlns:a="' + A + '" xmlns:pic="' + PIC + '"';
  const head = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n';
  const document = head + '<w:document ' + NSD + '><w:body>' + body +
    '<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" w:header="709" w:footer="709" w:gutter="0"/></w:sectPr></w:body></w:document>';
  const hsize = [0, 40, 32, 28, 26, 24, 24];
  const styles = head + '<w:styles xmlns:w="' + W + '"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="' + xe(base.font) + '" w:hAnsi="' + xe(base.font) + '" w:cs="' + xe(base.font) +
    '" w:eastAsia="' + xe(base.font) + '"/><w:sz w:val="' + Math.round(base.size * 2) + '"/><w:szCs w:val="' + Math.round(base.size * 2) + '"/><w:lang w:val="en-GB"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>' +
    '<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>' +
    [1, 2, 3, 4, 5, 6].map(n => '<w:style w:type="paragraph" w:styleId="Heading' + n + '"><w:name w:val="heading ' + n + '"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="240" w:after="120"/><w:outlineLvl w:val="' + (n - 1) + '"/></w:pPr><w:rPr><w:b/><w:bCs/><w:sz w:val="' + hsize[n] + '"/><w:szCs w:val="' + hsize[n] + '"/></w:rPr></w:style>').join('') +
    '<w:style w:type="paragraph" w:styleId="ListParagraph"><w:name w:val="List Paragraph"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="34"/><w:qFormat/><w:pPr><w:ind w:left="720"/><w:contextualSpacing/></w:pPr></w:style>' +
    '<w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"><w:name w:val="Default Paragraph Font"/><w:uiPriority w:val="1"/><w:semiHidden/><w:unhideWhenUsed/></w:style>' +
    '<w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/><w:unhideWhenUsed/><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style>' +
    '<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="108" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style>' +
    '<w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:basedOn w:val="TableNormal"/><w:uiPriority w:val="39"/><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideV w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr></w:style>' +
    '</w:styles>';
  const lvls = numbered => Array.from({ length: 9 }, (_, i) => '<w:lvl w:ilvl="' + i + '"><w:start w:val="1"/><w:numFmt w:val="' + (numbered ? ['decimal', 'lowerLetter', 'lowerRoman'][i % 3] : 'bullet') +
    '"/><w:lvlText w:val="' + (numbered ? '%' + (i + 1) + '.' : ['•', 'o', '▪'][i % 3]) + '"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="' + (720 * (i + 1)) + '" w:hanging="360"/></w:pPr>' +
    (numbered || i % 3 === 0 || i % 3 === 2 ? '' : '<w:rPr><w:rFonts w:ascii="Courier New" w:hAnsi="Courier New" w:cs="Courier New" w:hint="default"/></w:rPr>') + '</w:lvl>').join('');
  const numbering = head + '<w:numbering xmlns:w="' + W + '">' +
    '<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="hybridMultilevel"/>' + lvls(false) + '</w:abstractNum>' +
    '<w:abstractNum w:abstractNumId="1"><w:multiLevelType w:val="hybridMultilevel"/>' + lvls(true) + '</w:abstractNum>' +
    nums.map((n, i) => '<w:num w:numId="' + (i + 1) + '"><w:abstractNumId w:val="' + (n.type === 'ol' ? 1 : 0) + '"/>' +
      (n.type === 'ol' ? '<w:lvlOverride w:ilvl="' + n.level + '"><w:startOverride w:val="1"/></w:lvlOverride>' : '') + '</w:num>').join('') +
    '</w:numbering>';
  const settings = head + '<w:settings xmlns:w="' + W + '"><w:defaultTabStop w:val="708"/><w:characterSpacingControl w:val="doNotCompress"/><w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="15"/></w:compat></w:settings>';
  const docRels = head + '<Relationships xmlns="' + PR + '">' +
    '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>' +
    '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>' +
    '<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>' +
    rels.join('') + '</Relationships>';
  const types = head + '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">' +
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>' +
    '<Default Extension="png" ContentType="image/png"/><Default Extension="jpeg" ContentType="image/jpeg"/><Default Extension="gif" ContentType="image/gif"/>' +
    '<Default Extension="bmp" ContentType="image/bmp"/><Default Extension="svg" ContentType="image/svg+xml"/><Default Extension="webp" ContentType="image/webp"/>' +
    '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>' +
    '<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>' +
    '<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>' +
    '<Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>' +
    '<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>' +
    '<Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>';
  const rootRels = head + '<Relationships xmlns="' + PR + '"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>' +
    '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>' +
    '<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>';
  const now = new Date().toISOString().replace(/\.\d+Z$/, 'Z');
  const core = head + '<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">' +
    '<dcterms:created xsi:type="dcterms:W3CDTF">' + now + '</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">' + now + '</dcterms:modified></cp:coreProperties>';
  const app = head + '<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>osada</Application></Properties>';
  const files = [await ZIP.add('[Content_Types].xml', types), await ZIP.add('_rels/.rels', rootRels), await ZIP.add('word/document.xml', document),
    await ZIP.add('word/styles.xml', styles), await ZIP.add('word/numbering.xml', numbering), await ZIP.add('word/settings.xml', settings),
    await ZIP.add('word/_rels/document.xml.rels', docRels), await ZIP.add('docProps/core.xml', core), await ZIP.add('docProps/app.xml', app)];
  for (const m of media) files.push(await ZIP.add(m.path, m.bytes, true));
  return ZIP.write(files);
}

const DOCX = { parse, write };
return { DOCX, readDoc: parse };
});
