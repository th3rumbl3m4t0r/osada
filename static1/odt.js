/* OpenDocument text (.odt, .ott): read into the editor's blocks (rich.js), and written from
   them. The format documents are edited in. Part odt, on zip and rich. What is read:
   headings, paragraphs and their alignment, indents and spacing; bold, italic, underline,
   strike, super- and subscript, fonts, sizes, colours, highlight, through the style
   chains; links; lists, bulleted or numbered as their list style says; tables with
   merged cells and column widths; pictures (PNG, JPEG, GIF, SVG). Footnotes, comments,
   headers and footers, text box positions and other pictures are counted as lost. */
'use strict';

part('odt', ({ ZIP, Rich }) => {

const NS = {
  office: 'urn:oasis:names:tc:opendocument:xmlns:office:1.0',
  style: 'urn:oasis:names:tc:opendocument:xmlns:style:1.0',
  text: 'urn:oasis:names:tc:opendocument:xmlns:text:1.0',
  table: 'urn:oasis:names:tc:opendocument:xmlns:table:1.0',
  draw: 'urn:oasis:names:tc:opendocument:xmlns:drawing:1.0',
  fo: 'urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0',
  xlink: 'http://www.w3.org/1999/xlink',
  svg: 'urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0',
  meta: 'urn:oasis:names:tc:opendocument:xmlns:meta:1.0',
  dc: 'http://purl.org/dc/elements/1.1/',
  manifest: 'urn:oasis:names:tc:opendocument:xmlns:manifest:1.0',
  loext: 'urn:org:documentfoundation:names:experimental:office:xmlns:loext:1.0',
};
const MIME = 'application/vnd.oasis.opendocument.text';
const IMG_TYPES = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', svg: 'image/svg+xml', webp: 'image/webp', bmp: 'image/bmp' };

const at = (el, ns, name) => (el ? el.getAttributeNS(NS[ns], name) : null);
const kids = el => (el ? Array.from(el.children) : []);
const is = (el, ns, name) => el.namespaceURI === NS[ns] && el.localName === name;

/* a length (2.5cm, 1in, 12pt, 0.3mm, 10px) in twips */
function twips(v) {
  const m = /^(-?[\d.]+)(cm|mm|in|pt|pc|px)?$/.exec((v || '').trim());
  if (!m) return 0;
  const x = parseFloat(m[1]);
  return Math.round(x * { cm: 567, mm: 56.7, in: 1440, pt: 20, pc: 240, px: 15 }[m[2] || 'pt']);
}

async function xmlOf(files, name) {
  const f = files.get(name);
  if (!f) return null;
  const doc = new DOMParser().parseFromString(new TextDecoder().decode(await ZIP.inflate(f)), 'application/xml');
  if (doc.getElementsByTagName('parsererror').length) throw new Error(name + ': broken XML');
  return doc;
}

/* ---------------- reading ---------------- */
async function parse(bytes) {
  const files = new Map(ZIP.read(bytes).map(f => [f.name, f]));
  const content = await xmlOf(files, 'content.xml');
  if (!content) throw new Error('not an OpenDocument text (no content.xml)');
  const stylesDoc = await xmlOf(files, 'styles.xml');
  const lost = {};
  const lose = what => { lost[what] = (lost[what] || 0) + 1; };

  // fonts: style:font-name to a family
  const faces = {};
  for (const d of [stylesDoc, content]) {
    if (!d) continue;
    for (const ff of d.getElementsByTagNameNS(NS.style, 'font-face')) faces[at(ff, 'style', 'name')] = (at(ff, 'svg', 'font-family') || at(ff, 'style', 'name')).replace(/^['"]|['"]$/g, '');
  }
  // styles by family and name; list styles by name
  const styles = { paragraph: {}, text: {}, 'table-column': {}, 'table-cell': {} }, defaults = {}, lists = {};
  const readStyles = root => {
    if (!root) return;
    for (const s of kids(root)) {
      if (is(s, 'style', 'style')) {
        const fam = at(s, 'style', 'family');
        if (!styles[fam]) styles[fam] = {};
        styles[fam][at(s, 'style', 'name')] = s;
      } else if (is(s, 'style', 'default-style')) defaults[at(s, 'style', 'family')] = s;
      else if (is(s, 'text', 'list-style')) lists[at(s, 'style', 'name')] = s;
    }
  };
  if (stylesDoc) {
    readStyles(stylesDoc.getElementsByTagNameNS(NS.office, 'styles')[0]);
    readStyles(stylesDoc.getElementsByTagNameNS(NS.office, 'automatic-styles')[0]);
    if (stylesDoc.getElementsByTagNameNS(NS.style, 'header').length) lose('headers and footers');
    else if (stylesDoc.getElementsByTagNameNS(NS.style, 'footer').length) lose('headers and footers');
  }
  readStyles(content.getElementsByTagNameNS(NS.office, 'automatic-styles')[0]);

  const textProps = (el, f) => {
    const p = kids(el).find(x => is(x, 'style', 'text-properties'));
    if (!p) return f;
    f = Object.assign({}, f);
    const fw = at(p, 'fo', 'font-weight');
    if (fw) f.b = fw === 'bold' || +fw >= 600;
    const fs = at(p, 'fo', 'font-style');
    if (fs) f.i = fs === 'italic' || fs === 'oblique';
    const ul = at(p, 'style', 'text-underline-style');
    if (ul) f.u = ul !== 'none';
    const lt = at(p, 'style', 'text-line-through-style');
    if (lt) f.s = lt !== 'none';
    const pos = at(p, 'style', 'text-position');
    if (pos) {
      const v = /^(super|sub|0%?|-?[\d.]+%)/.exec(pos.trim());
      const x = v ? (v[1] === 'super' ? 1 : v[1] === 'sub' ? -1 : parseFloat(v[1])) : 0;
      f.sup = x > 0;
      f.sub = x < 0;
    }
    const col = at(p, 'fo', 'color');
    if (col) f.color = /^#[0-9a-f]{6}$/i.test(col) && col.toLowerCase() !== '#000000' ? col.toLowerCase() : null;
    if (at(p, 'style', 'use-window-font-color') === 'true') f.color = null;
    const bg = at(p, 'fo', 'background-color');
    if (bg) f.bg = /^#[0-9a-f]{6}$/i.test(bg) ? bg.toLowerCase() : null;
    const sz = at(p, 'fo', 'font-size');
    if (sz) {
      const m = /^([\d.]+)(pt|%)$/.exec(sz);
      if (m) f.size = Math.round(m[2] === '%' ? (f.size || 24) * parseFloat(m[1]) / 100 : parseFloat(m[1]) * 2);
    }
    const fn = at(p, 'style', 'font-name') || at(p, 'fo', 'font-family');
    if (fn) f.font = faces[fn] || fn.replace(/^['"]|['"]$/g, '');
    return f;
  };
  const paraProps = (el, pp) => {
    const p = kids(el).find(x => is(x, 'style', 'paragraph-properties'));
    if (!p) return pp;
    pp = Object.assign({}, pp);
    const al = at(p, 'fo', 'text-align');
    if (al) pp.align = al === 'center' ? 'center' : al === 'end' || al === 'right' ? 'right' : al === 'justify' ? 'justify' : '';
    if (p.hasAttributeNS(NS.fo, 'margin-left')) pp.li = twips(at(p, 'fo', 'margin-left'));
    if (p.hasAttributeNS(NS.fo, 'text-indent')) pp.fi = twips(at(p, 'fo', 'text-indent'));
    if (p.hasAttributeNS(NS.fo, 'margin-top')) pp.sb = twips(at(p, 'fo', 'margin-top'));
    if (p.hasAttributeNS(NS.fo, 'margin-bottom')) pp.sa = twips(at(p, 'fo', 'margin-bottom'));
    return pp;
  };
  // a style with everything it inherits: {f, pp, list (list style name), h}
  const memo = new Map();
  const resolve = (fam, name, depth) => {
    const key = fam + '/' + name;
    if (memo.has(key)) return memo.get(key);
    const el = styles[fam] && styles[fam][name];
    let r;
    if (!el || (depth || 0) > 20) {
      const d = defaults[fam];
      r = { f: d ? textProps(d, { size: 24 }) : { size: 24 }, pp: d ? paraProps(d, {}) : {}, list: null, h: 0 };
    } else {
      const parent = at(el, 'style', 'parent-style-name');
      const base = resolve(fam, parent || '', (depth || 0) + 1);
      r = { f: textProps(el, base.f), pp: paraProps(el, base.pp), list: at(el, 'style', 'list-style-name') || base.list, h: base.h };
      const ol = at(el, 'style', 'default-outline-level');
      if (ol) r.h = +ol;
    }
    memo.set(key, r);
    return r;
  };
  const listType = (name, level) => {
    const ls = lists[name];
    if (!ls) return 'ul';
    const lv = kids(ls).find(x => +at(x, 'text', 'level') === level + 1) || kids(ls)[Math.min(level, kids(ls).length - 1)];
    if (!lv) return 'ul';
    return lv.localName === 'list-level-style-number' && (at(lv, 'style', 'num-format') || '') !== '' ? 'ol' : 'ul';
  };

  async function picture(frame) {
    const img = kids(frame).find(x => is(x, 'draw', 'image'));
    const w = twips(at(frame, 'svg', 'width')) / 15, hgt = twips(at(frame, 'svg', 'height')) / 15;
    const href = img && at(img, 'xlink', 'href');
    if (href && files.has(href.replace(/^\.\//, ''))) {
      const ext = href.toLowerCase().split('.').pop();
      if (IMG_TYPES[ext]) return { img: Rich.dataUrl(IMG_TYPES[ext], await ZIP.inflate(files.get(href.replace(/^\.\//, '')))), w: w || 200, h: hgt || 150 };
    }
    // a newer image format with a PNG next to it
    const alt = kids(frame).filter(x => is(x, 'draw', 'image')).map(x => at(x, 'xlink', 'href')).find(x => x && /\.(png|jpe?g|gif)$/i.test(x) && files.has(x));
    if (alt) return { img: Rich.dataUrl(IMG_TYPES[alt.toLowerCase().split('.').pop()], await ZIP.inflate(files.get(alt))), w: w || 200, h: hgt || 150 };
    lose('pictures in other formats');
    return null;
  }

  const blocks = [];
  async function runs(el, f, out, extra) {
    for (const n of Array.from(el.childNodes)) {
      if (n.nodeType === 3) { if (n.nodeValue) out.push({ t: n.nodeValue.replace(/\s+/g, ' '), f }); continue; }
      if (n.nodeType !== 1) continue;
      if (is(n, 'text', 's')) { out.push({ t: ' '.repeat(Math.max(1, +at(n, 'text', 'c') || 1)), f }); continue; }
      if (is(n, 'text', 'tab')) { out.push({ t: '\t', f }); continue; }
      if (is(n, 'text', 'line-break')) { out.push({ t: '\n', f }); continue; }
      if (is(n, 'text', 'span')) { await runs(n, spanFmt(at(n, 'text', 'style-name'), f), out, extra); continue; }
      if (is(n, 'text', 'a')) {
        const href = Rich.safeUrl(at(n, 'xlink', 'href') || '');
        await runs(n, href ? Object.assign({}, f, { href }) : f, out, extra);
        continue;
      }
      if (is(n, 'text', 'note')) { lose('footnotes'); const c = kids(n).find(x => is(x, 'text', 'note-citation')); if (c) out.push({ t: c.textContent, f: Object.assign({}, f, { sup: true }) }); continue; }
      if (is(n, 'office', 'annotation') || is(n, 'office', 'annotation-end')) { if (is(n, 'office', 'annotation')) lose('comments'); continue; }
      if (is(n, 'draw', 'frame')) {
        if (kids(n).some(x => is(x, 'draw', 'text-box'))) {
          lose('text box positions');
          extra.push(...await body(kids(n).find(x => is(x, 'draw', 'text-box')), []));
          continue;
        }
        const r = await picture(n);
        if (r) out.push(Object.assign(r, f.href ? { href: f.href } : {}));
        continue;
      }
      if (is(n, 'draw', 'a')) { await runs(n, f, out, extra); continue; }
      if (n.namespaceURI === NS.draw) { lose('drawings'); continue; }
      // bookmarks, reference marks, fields: their text, if any
      await runs(n, f, out, extra);
    }
  }
  // a span's style on top of f, its parents first
  const spanFmt = (name, f, depth) => {
    const el = styles.text[name];
    if (!el || (depth || 0) > 10) return f;
    const parent = at(el, 'style', 'parent-style-name');
    return textProps(el, parent ? spanFmt(parent, f, (depth || 0) + 1) : f);
  };

  async function para(el, ctx, out) {
    const name = at(el, 'text', 'style-name');
    const st = resolve('paragraph', name || '');
    const extra = [];
    const b = Object.assign({ runs: [] }, st.pp, { list: ctx.list || null, level: ctx.level || 0, h: is(el, 'text', 'h') ? +at(el, 'text', 'outline-level') || 1 : 0 });
    await runs(el, st.f, b.runs, extra);
    // runs: base size 12pt is the editor's; one run per text node is fine
    if (b.runs.length) {
      b.runs[0].t = b.runs[0].t && b.runs[0].t.replace(/^ +/, '');
      const last = b.runs[b.runs.length - 1];
      if (last.t) last.t = last.t.replace(/ +$/, '');
    }
    out.push(b);
    out.push(...extra);
  }

  async function list(el, ctx, out) {
    const name = at(el, 'text', 'style-name') || ctx.listStyle;
    const level = ctx.inList ? ctx.level + 1 : 0;
    for (const it of kids(el)) {
      if (!is(it, 'text', 'list-item') && !is(it, 'text', 'list-header')) continue;
      for (const c of kids(it)) {
        if (is(c, 'text', 'list')) await list(c, Object.assign({}, ctx, { inList: true, level, listStyle: name }), out);
        else if (is(c, 'text', 'p') || is(c, 'text', 'h')) {
          const t = listType(name || resolve('paragraph', at(c, 'text', 'style-name') || '').list, level);
          await para(c, { list: is(it, 'text', 'list-header') ? null : t, level }, out);
        } else await body(c, out, ctx);
      }
    }
  }

  async function table(el, out) {
    const widths = [];
    const rows = [];
    const walk = async node => {
      for (const c of kids(node)) {
        if (is(c, 'table', 'table-column')) {
          const st = styles['table-column'][at(c, 'table', 'style-name')];
          const p = st && kids(st).find(x => is(x, 'style', 'table-column-properties'));
          const w = p ? twips(at(p, 'style', 'column-width')) : 0;
          for (let i = 0, n = Math.min(+at(c, 'table', 'number-columns-repeated') || 1, 256); i < n; i++) widths.push(w);
        } else if (is(c, 'table', 'table-row')) {
          const cells = [];
          let col = 0;
          for (const td of kids(c)) {
            if (is(td, 'table', 'covered-table-cell')) { col += +at(td, 'table', 'number-columns-repeated') || 1; continue; }
            if (!is(td, 'table', 'table-cell')) continue;
            const span = +at(td, 'table', 'number-columns-spanned') || 1;
            const cb = [];
            await body(td, cb, {});
            let w = 0;
            for (let i = 0; i < span; i++) w += widths[col + i] || 0;
            cells.push({ blocks: cb.length ? cb : [{ runs: [] }], span, rspan: +at(td, 'table', 'number-rows-spanned') || 1, w });
            col += span;
          }
          const rep = Math.min(+at(c, 'table', 'number-rows-repeated') || 1, 1000);
          for (let i = 0; i < rep; i++) rows.push({ cells });
        } else if (c.namespaceURI === NS.table) await walk(c);   // header rows, row groups, columns groups
      }
    };
    await walk(el);
    if (rows.length) out.push({ rows });
  }

  async function body(el, out, ctx) {
    ctx = ctx || {};
    for (const c of kids(el)) {
      if (is(c, 'text', 'p') || is(c, 'text', 'h')) await para(c, {}, out);
      else if (is(c, 'text', 'list')) await list(c, ctx, out);
      else if (is(c, 'table', 'table')) await table(c, out);
      else if (is(c, 'text', 'section') || is(c, 'text', 'index-body') || /^(table-of-content|alphabetical-index|illustration-index|bibliography|user-index|object-index|table-index)$/.test(c.localName)) await body(c, out, ctx);
      else if (is(c, 'text', 'sequence-decls') || is(c, 'text', 'variable-decls') || is(c, 'office', 'forms') || is(c, 'text', 'tracked-changes') || is(c, 'text', 'soft-page-break')) continue;
      else if (is(c, 'draw', 'frame')) { const r = []; const extra = []; await runs({ childNodes: [c] }, {}, r, extra); if (r.length) out.push({ runs: r }); out.push(...extra); }
    }
  }

  const text = content.getElementsByTagNameNS(NS.office, 'text')[0];
  if (!text) throw new Error('not an OpenDocument text (no office:text)');
  await body(text, blocks);
  const std = resolve('paragraph', 'Standard');
  const font = std.f.font || (defaults.paragraph && resolve('paragraph', '').f.font) || 'Liberation Serif';
  return { html: Rich.html(blocks, std.f.size || 24, font), font, size: (std.f.size || 24) / 2, lost, blocks };
}

/* ---------------- writing ---------------- */
const xe = s => String(s).replace(/[\x00-\x08\x0B\x0C\x0E-\x1F￾￿]/g, '').replace(/[&<>"]/g, c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : '&quot;'));
const cm = tw => (tw / 567).toFixed(3) + 'cm';

/* text as ODF wants it: runs of spaces, tabs and line breaks as elements */
function textXml(t) {
  let out = '';
  for (const part of t.split(/(\t|\n| {2,})/)) {
    if (part === '\t') out += '<text:tab/>';
    else if (part === '\n') out += '<text:line-break/>';
    else if (/^ {2,}$/.test(part)) out += ' <text:s text:c="' + (part.length - 1) + '"/>';
    else out += xe(part);
  }
  return out.replace(/^ /, '<text:s/>');
}

/* blocks (Rich.flat of the page) as an .odt; base = {font, size (pt)} */
async function write(blocks, base) {
  base = Object.assign({ font: 'Liberation Serif', size: 12 }, base || {});
  const pStyles = new Map(), tStyles = new Map(), fonts = new Set([base.font]), pics = [];
  let autoStyles = '', tableN = 0;
  const pStyle = (b, parent, border) => {
    const props = [];
    if (b.align) props.push('fo:text-align="' + (b.align === 'right' ? 'end' : b.align) + '"');
    if (b.li && !b.list) props.push('fo:margin-left="' + cm(b.li) + '"');
    if (b.fi && !b.list) props.push('fo:text-indent="' + cm(b.fi) + '"');
    if (b.sb) props.push('fo:margin-top="' + cm(b.sb) + '"');
    if (b.sa) props.push('fo:margin-bottom="' + cm(b.sa) + '"');
    if (border) props.push('fo:border-bottom="0.5pt solid #000000" fo:padding-bottom="0.05cm"');
    const key = parent + '|' + props.join(' ');
    if (!props.length) return parent;
    if (!pStyles.has(key)) {
      const name = 'P' + (pStyles.size + 1);
      pStyles.set(key, name);
      autoStyles += '<style:style style:name="' + name + '" style:family="paragraph" style:parent-style-name="' + parent + '"><style:paragraph-properties ' + props.join(' ') + '/></style:style>';
    }
    return pStyles.get(key);
  };
  const tStyle = f => {
    const props = [];
    if (f.b) props.push('fo:font-weight="bold" style:font-weight-asian="bold" style:font-weight-complex="bold"');
    if (f.i) props.push('fo:font-style="italic" style:font-style-asian="italic" style:font-style-complex="italic"');
    if (f.u) props.push('style:text-underline-style="solid" style:text-underline-width="auto" style:text-underline-color="font-color"');
    if (f.s) props.push('style:text-line-through-style="solid" style:text-line-through-type="single"');
    if (f.sup) props.push('style:text-position="super 58%"');
    else if (f.sub) props.push('style:text-position="sub 58%"');
    if (f.font && f.font.toLowerCase() !== base.font.toLowerCase()) { fonts.add(f.font); props.push('style:font-name="' + xe(f.font) + '"'); }
    if (f.size && Math.abs(f.size / 2 - base.size) > 0.01) props.push('fo:font-size="' + f.size / 2 + 'pt" style:font-size-asian="' + f.size / 2 + 'pt" style:font-size-complex="' + f.size / 2 + 'pt"');
    if (f.color && f.color !== '#000000') props.push('fo:color="' + f.color + '"');
    if (f.bg && f.bg !== '#ffffff') props.push('fo:background-color="' + f.bg + '"');
    if (!props.length) return null;
    const key = props.join(' ');
    if (!tStyles.has(key)) {
      const name = 'T' + (tStyles.size + 1);
      tStyles.set(key, name);
      autoStyles += '<style:style style:name="' + name + '" style:family="text"><style:text-properties ' + key + '/></style:style>';
    }
    return tStyles.get(key);
  };
  const runXml = r => {
    if (r.img) {
      const d = Rich.dataBytes(r.img);
      if (!d) return '';
      const ext = { 'image/png': 'png', 'image/jpeg': 'jpg', 'image/gif': 'gif', 'image/svg+xml': 'svg', 'image/webp': 'webp', 'image/bmp': 'bmp' }[d.type] || 'png';
      const path = 'Pictures/' + (pics.length + 1) + '.' + ext;
      pics.push({ path, bytes: d.bytes, type: d.type });
      const w = (r.w * 2.54 / 96).toFixed(3) + 'cm', hh = (r.h * 2.54 / 96).toFixed(3) + 'cm';
      let x = '<draw:frame draw:style-name="fr1" draw:name="Picture' + pics.length + '" text:anchor-type="as-char" svg:width="' + w + '" svg:height="' + hh +
        '" draw:z-index="0"><draw:image xlink:href="' + path + '" xlink:type="simple" xlink:show="embed" xlink:actuate="onLoad"/></draw:frame>';
      if (r.href) x = '<draw:a xlink:type="simple" xlink:href="' + xe(r.href) + '">' + x + '</draw:a>';
      return x;
    }
    const f = r.f || {};
    let x = textXml(r.t || '');
    const st = tStyle(f);
    if (st) x = '<text:span text:style-name="' + st + '">' + x + '</text:span>';
    if (f.href) x = '<text:a xlink:type="simple" xlink:href="' + xe(f.href) + '">' + x + '</text:a>';
    return x;
  };
  // runs with the same formatting and link go together
  const parXml = (b, tag) => {
    let inner = '';
    for (const r of b.runs || []) inner += runXml(r);
    const parent = b.h ? 'Heading_20_' + Math.min(6, b.h) : b.list ? 'List_20_Paragraph' : 'Standard';
    const st = pStyle(b, parent);
    return tag === 'h' || b.h
      ? '<text:h text:style-name="' + st + '" text:outline-level="' + Math.min(6, b.h || 1) + '">' + inner + '</text:h>'
      : '<text:p text:style-name="' + st + '">' + inner + '</text:p>';
  };

  function tableXml(t) {
    tableN++;
    const name = 'Table' + tableN;
    // the grid: rows spanned from above take their columns
    const grid = [], ncols = { n: 0 };
    t.rows.forEach((row, ri) => {
      grid[ri] = grid[ri] || [];
      let col = 0;
      for (const cell of row.cells) {
        while (grid[ri][col]) col++;
        for (let dr = 0; dr < (cell.rspan || 1); dr++) {
          grid[ri + dr] = grid[ri + dr] || [];
          for (let dc = 0; dc < (cell.span || 1); dc++) grid[ri + dr][col + dc] = dr === 0 && dc === 0 ? cell : 'covered';
        }
        col += cell.span || 1;
      }
      ncols.n = Math.max(ncols.n, grid[ri].length);
    });
    const widths = [];
    t.rows.forEach((row, ri) => {
      let col = 0;
      for (const cell of row.cells) {
        while (grid[ri][col] && grid[ri][col] !== cell) col++;
        if ((cell.span || 1) === 1 && cell.w && !widths[col]) widths[col] = cell.w;
        col += cell.span || 1;
      }
    });
    const total = 9638;   // 17 cm of text width, in twips
    const known = widths.filter(Boolean).reduce((a, b) => a + b, 0), unknown = ncols.n - widths.filter(Boolean).length;
    const fill = unknown ? Math.max(567, (total - known) / unknown) : 0;
    let cols = '';
    for (let c = 0; c < ncols.n; c++) {
      const cs = name + '.C' + (c + 1);
      autoStyles += '<style:style style:name="' + cs + '" style:family="table-column"><style:table-column-properties style:column-width="' + cm(widths[c] || fill) + '"/></style:style>';
      cols += '<table:table-column table:style-name="' + cs + '"/>';
    }
    autoStyles += '<style:style style:name="' + name + '" style:family="table"><style:table-properties style:width="' + cm(Math.min(total, (widths.reduce((a, b) => a + (b || 0), 0)) + fill * unknown)) + '" table:align="left"/></style:style>';
    let rows = '';
    grid.forEach(gr => {
      rows += '<table:table-row>';
      for (let c = 0; c < ncols.n; c++) {
        const cell = gr[c];
        if (cell === 'covered') { rows += '<table:covered-table-cell/>'; continue; }
        if (!cell) { rows += '<table:table-cell table:style-name="' + name + '.A1" office:value-type="string"><text:p text:style-name="Table_20_Contents"/></table:table-cell>'; continue; }
        rows += '<table:table-cell table:style-name="' + name + '.A1" office:value-type="string"' +
          (cell.span > 1 ? ' table:number-columns-spanned="' + cell.span + '"' : '') + (cell.rspan > 1 ? ' table:number-rows-spanned="' + cell.rspan + '"' : '') + '>' +
          blocksXml(cell.blocks && cell.blocks.length ? cell.blocks : [{ runs: [] }]) + '</table:table-cell>';
      }
      rows += '</table:table-row>';
    });
    autoStyles += '<style:style style:name="' + name + '.A1" style:family="table-cell"><style:table-cell-properties fo:padding="0.097cm" fo:border="0.5pt solid #000000"/></style:style>';
    return '<table:table table:name="' + name + '" table:style-name="' + name + '">' + cols + rows + '</table:table>';
  }

  function blocksXml(bs) {
    let out = '';
    const open = [];   // list types, one per level
    const close = n => { while (open.length > n) { out += '</text:list-item></text:list>'; open.pop(); } };
    for (const b of bs) {
      if (b.rows) { close(0); out += tableXml(b); continue; }
      if (b.hr) { close(0); out += '<text:p text:style-name="' + pStyle({}, 'Standard', true) + '"/>'; continue; }
      if (b.list) {
        const level = b.level || 0;
        close(level + 1);
        if (open.length === level + 1 && open[level] !== b.list) close(level);
        if (open.length === level + 1) out += '</text:list-item><text:list-item>';
        while (open.length < level + 1) {
          const lt = open.length === level ? b.list : 'ul';
          out += '<text:list text:style-name="' + (lt === 'ol' ? 'LNum' : 'LBul') + '"><text:list-item>';
          open.push(lt);
        }
        out += parXml(b);
        continue;
      }
      close(0);
      out += parXml(b);
    }
    close(0);
    return out;
  }

  const bodyXml = blocksXml(blocks.length ? blocks : [{ runs: [] }]);
  const listStyle = (name, numbered) => '<text:list-style style:name="' + name + '">' + Array.from({ length: 10 }, (_, i) => {
    const lvl = i + 1, ind = (0.635 * (i + 1)).toFixed(3);
    const props = '<style:list-level-properties text:list-level-position-and-space-mode="label-alignment"><style:list-level-label-alignment text:label-followed-by="listtab" text:list-tab-stop-position="' + ind + 'cm" fo:text-indent="-0.635cm" fo:margin-left="' + ind + 'cm"/></style:list-level-properties>';
    return numbered
      ? '<text:list-level-style-number text:level="' + lvl + '" text:style-name="Numbering_20_Symbols" style:num-suffix="." style:num-format="1">' + props + '</text:list-level-style-number>'
      : '<text:list-level-style-bullet text:level="' + lvl + '" text:style-name="Bullet_20_Symbols" text:bullet-char="' + ['•', '◦', '▪'][i % 3] + '">' + props + '</text:list-level-style-bullet>';
  }).join('') + '</text:list-style>';
  const fontDecls = '<office:font-face-decls>' + [...fonts].map(f => '<style:font-face style:name="' + xe(f) + '" svg:font-family="' + xe(/\s/.test(f) ? "'" + f + "'" : f) + '"/>').join('') + '</office:font-face-decls>';
  const nsDecl = 'xmlns:office="' + NS.office + '" xmlns:style="' + NS.style + '" xmlns:text="' + NS.text + '" xmlns:table="' + NS.table +
    '" xmlns:draw="' + NS.draw + '" xmlns:fo="' + NS.fo + '" xmlns:xlink="' + NS.xlink + '" xmlns:svg="' + NS.svg + '" xmlns:meta="' + NS.meta + '" xmlns:dc="' + NS.dc + '"';
  const head = '<?xml version="1.0" encoding="UTF-8"?>\n';
  const content = head + '<office:document-content ' + nsDecl + ' office:version="1.3">' + fontDecls +
    '<office:automatic-styles>' + autoStyles +
    '<style:style style:name="fr1" style:family="graphic" style:parent-style-name="Graphics"><style:graphic-properties style:vertical-pos="top" style:vertical-rel="baseline" style:horizontal-pos="center" style:horizontal-rel="paragraph"/></style:style>' +
    '</office:automatic-styles><office:body><office:text>' + bodyXml + '</office:text></office:body></office:document-content>';
  const hsize = [0, 20, 16, 14, 13, 12, 12];
  const styles = head + '<office:document-styles ' + nsDecl + ' office:version="1.3">' + fontDecls +
    '<office:styles>' +
    '<style:default-style style:family="paragraph"><style:paragraph-properties fo:orphans="2" fo:widows="2"/><style:text-properties style:font-name="' + xe(base.font) + '" fo:font-size="' + base.size + 'pt" fo:language="en" fo:country="GB"/></style:default-style>' +
    '<style:default-style style:family="graphic"/>' +
    '<style:style style:name="Standard" style:family="paragraph" style:class="text"/>' +
    '<style:style style:name="Heading" style:family="paragraph" style:parent-style-name="Standard" style:next-style-name="Standard" style:class="text"><style:paragraph-properties fo:margin-top="0.423cm" fo:margin-bottom="0.212cm" fo:keep-with-next="always"/><style:text-properties fo:font-weight="bold"/></style:style>' +
    [1, 2, 3, 4, 5, 6].map(n => '<style:style style:name="Heading_20_' + n + '" style:display-name="Heading ' + n + '" style:family="paragraph" style:parent-style-name="Heading" style:next-style-name="Standard" style:default-outline-level="' + n + '" style:class="text"><style:text-properties fo:font-size="' + hsize[n] + 'pt" fo:font-weight="bold"/></style:style>').join('') +
    '<style:style style:name="List_20_Paragraph" style:display-name="List Paragraph" style:family="paragraph" style:parent-style-name="Standard" style:class="list"/>' +
    '<style:style style:name="Table_20_Contents" style:display-name="Table Contents" style:family="paragraph" style:parent-style-name="Standard" style:class="extra"/>' +
    '<style:style style:name="Graphics" style:family="graphic"><style:graphic-properties text:anchor-type="as-char" svg:y="0cm" style:vertical-pos="top" style:vertical-rel="baseline"/></style:style>' +
    '<style:style style:name="Bullet_20_Symbols" style:display-name="Bullet Symbols" style:family="text"/>' +
    '<style:style style:name="Numbering_20_Symbols" style:display-name="Numbering Symbols" style:family="text"/>' +
    listStyle('LBul', false) + listStyle('LNum', true) +
    '</office:styles><office:automatic-styles><style:page-layout style:name="pm1"><style:page-layout-properties fo:page-width="21cm" fo:page-height="29.7cm" style:print-orientation="portrait" fo:margin-top="2cm" fo:margin-bottom="2cm" fo:margin-left="2cm" fo:margin-right="2cm"/></style:page-layout></office:automatic-styles>' +
    '<office:master-styles><style:master-page style:name="Standard" style:page-layout-name="pm1"/></office:master-styles></office:document-styles>';
  const meta = head + '<office:document-meta ' + nsDecl + ' office:version="1.3"><office:meta><meta:generator>osada</meta:generator><dc:date>' + new Date().toISOString().replace(/\.\d+Z$/, '') + '</dc:date></office:meta></office:document-meta>';
  const manifest = head + '<manifest:manifest xmlns:manifest="' + NS.manifest + '" manifest:version="1.3">' +
    '<manifest:file-entry manifest:full-path="/" manifest:version="1.3" manifest:media-type="' + MIME + '"/>' +
    '<manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>' +
    '<manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/>' +
    '<manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/>' +
    pics.map(p => '<manifest:file-entry manifest:full-path="' + p.path + '" manifest:media-type="' + p.type + '"/>').join('') +
    '</manifest:manifest>';
  const files = [await ZIP.add('mimetype', MIME, true), await ZIP.add('content.xml', content), await ZIP.add('styles.xml', styles),
    await ZIP.add('meta.xml', meta), await ZIP.add('META-INF/manifest.xml', manifest)];
  for (const p of pics) files.push(await ZIP.add(p.path, p.bytes, true));
  return ZIP.write(files);
}

/* for the editor (rich.js): how it saves, the pictures ODT keeps */
const ODT_FORMAT = { write: (page, base) => write(Rich.flat(page), base), images: ['image/png', 'image/jpeg', 'image/gif', 'image/svg+xml'] };
const ODT = { parse, write, MIME, blank: () => write([{ runs: [] }]) };

return { ODT, ODT_FORMAT, readDoc: parse };
});
