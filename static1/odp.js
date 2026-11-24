/* OpenDocument presentations (.odp, .otp; drawings, .odg, read the same way): read into the
   deck model (slides.js) and written from it. The format presentations are edited in. Part
   odp, on zip, rich and slides.
   Read: the page size, each slide's frames (text boxes with their paragraphs, lists and
   formatting, pictures, tables), custom shapes, rectangles, ellipses and lines with their
   fill and stroke, groups, rotation, the background, and what the master page draws
   (shown behind the slide). Saving an .odp read here keeps its styles.xml (masters, page
   layout) as it was and writes the slides anew; a deck from elsewhere gets a plain one. */
'use strict';

part('odp', ({ ZIP, Rich, shade }) => {

const NS = {
  office: 'urn:oasis:names:tc:opendocument:xmlns:office:1.0', style: 'urn:oasis:names:tc:opendocument:xmlns:style:1.0',
  text: 'urn:oasis:names:tc:opendocument:xmlns:text:1.0', table: 'urn:oasis:names:tc:opendocument:xmlns:table:1.0',
  draw: 'urn:oasis:names:tc:opendocument:xmlns:drawing:1.0', fo: 'urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0',
  xlink: 'http://www.w3.org/1999/xlink', svg: 'urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0',
  presentation: 'urn:oasis:names:tc:opendocument:xmlns:presentation:1.0', meta: 'urn:oasis:names:tc:opendocument:xmlns:meta:1.0',
  dc: 'http://purl.org/dc/elements/1.1/', manifest: 'urn:oasis:names:tc:opendocument:xmlns:manifest:1.0',
};
const MIME = 'application/vnd.oasis.opendocument.presentation';
const IMG_TYPES = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', svg: 'image/svg+xml', bmp: 'image/bmp', webp: 'image/webp' };
const at = (el, ns, name) => (el ? el.getAttributeNS(NS[ns], name) : null);
const kids = el => (el ? Array.from(el.children) : []);
const is = (el, ns, name) => el.namespaceURI === NS[ns] && el.localName === name;
/* a length in px (96 dpi) */
function px(v) {
  const m = /^(-?[\d.]+)(cm|mm|in|pt|pc|px)?$/.exec((v || '').trim());
  return m ? parseFloat(m[1]) * { cm: 96 / 2.54, mm: 96 / 25.4, in: 96, pt: 4 / 3, pc: 16, px: 1 }[m[2] || 'px'] : 0;
}

async function parse(bytes) {
  const files = new Map(ZIP.read(bytes).map(f => [f.name, f]));
  const xml = async name => {
    const f = files.get(name);
    if (!f) return null;
    const d = new DOMParser().parseFromString(new TextDecoder().decode(await ZIP.inflate(f)), 'application/xml');
    if (d.getElementsByTagName('parsererror').length) throw new Error(name + ': broken XML');
    return d;
  };
  const content = await xml('content.xml');
  if (!content) throw new Error('not an OpenDocument presentation (no content.xml)');
  const stylesDoc = await xml('styles.xml');
  const lost = {};
  const lose = w => { lost[w] = (lost[w] || 0) + 1; };

  const faces = {};
  for (const d of [stylesDoc, content]) if (d) for (const ff of d.getElementsByTagNameNS(NS.style, 'font-face')) faces[at(ff, 'style', 'name')] = (at(ff, 'svg', 'font-family') || at(ff, 'style', 'name')).replace(/^['"]|['"]$/g, '');
  const styles = {}, lists = {};
  const readStyles = root => {
    for (const s of kids(root)) {
      if (is(s, 'style', 'style')) styles[at(s, 'style', 'family') + '/' + at(s, 'style', 'name')] = s;
      else if (is(s, 'text', 'list-style')) lists[at(s, 'style', 'name')] = s;
      else if (is(s, 'style', 'default-style')) styles[at(s, 'style', 'family') + '/'] = s;
    }
  };
  if (stylesDoc) for (const tag of ['styles', 'automatic-styles']) { const r = stylesDoc.getElementsByTagNameNS(NS.office, tag)[0]; if (r) readStyles(r); }
  { const r = content.getElementsByTagNameNS(NS.office, 'automatic-styles')[0]; if (r) readStyles(r); }

  const chain = (fam, name) => {
    const out = [];
    for (let s = styles[fam + '/' + name], n = 0; s && n < 20; n++) { out.unshift(s); const p = at(s, 'style', 'parent-style-name'); s = p ? styles[fam + '/' + p] : null; }
    if (styles[fam + '/']) out.unshift(styles[fam + '/']);
    return out;
  };
  const prop = (els, kind) => els.map(s => kids(s).find(x => is(x, 'style', kind))).filter(Boolean);
  const textF = (els, f) => {
    f = Object.assign({}, f);
    for (const p of prop(els, 'text-properties')) {
      const fw = at(p, 'fo', 'font-weight'); if (fw) f.b = fw === 'bold' || +fw >= 600;
      const fs = at(p, 'fo', 'font-style'); if (fs) f.i = fs === 'italic';
      const ul = at(p, 'style', 'text-underline-style'); if (ul) f.u = ul !== 'none';
      const lt = at(p, 'style', 'text-line-through-style'); if (lt) f.s = lt !== 'none';
      const c = at(p, 'fo', 'color'); if (c) f.color = c.toLowerCase();
      const sz = at(p, 'fo', 'font-size');
      if (sz) { const m = /^([\d.]+)(pt|%)$/.exec(sz); if (m) f.size = Math.round(m[2] === '%' ? (f.size || 36) * parseFloat(m[1]) / 100 : parseFloat(m[1]) * 2); }
      const fn = at(p, 'style', 'font-name') || at(p, 'fo', 'font-family'); if (fn) f.font = faces[fn] || fn.replace(/^['"]|['"]$/g, '');
      const pos = at(p, 'style', 'text-position');
      if (pos) { const x = /^super/.test(pos) ? 1 : /^sub/.test(pos) ? -1 : parseFloat(pos); f.sup = x > 0; f.sub = x < 0; }
    }
    return f;
  };
  const paraP = (els, b) => {
    for (const p of prop(els, 'paragraph-properties')) {
      const al = at(p, 'fo', 'text-align');
      if (al) b.align = al === 'center' ? 'center' : al === 'end' || al === 'right' ? 'right' : al === 'justify' ? 'justify' : '';
    }
    return b;
  };
  const graphic = els => {
    const g = {};
    for (const p of prop(els, 'graphic-properties')) {
      const fill = at(p, 'draw', 'fill');
      if (fill) g.fill = fill === 'solid' ? (at(p, 'draw', 'fill-color') || '#729fcf').toLowerCase() : fill === 'none' ? null : g.fill;
      else if (at(p, 'draw', 'fill-color') && g.fill === undefined) g.fillColor = at(p, 'draw', 'fill-color').toLowerCase();
      const st = at(p, 'draw', 'stroke');
      if (st) g.stroke = st !== 'none';
      if (at(p, 'svg', 'stroke-color')) g.strokeColor = at(p, 'svg', 'stroke-color').toLowerCase();
      if (at(p, 'svg', 'stroke-width')) g.strokeW = px(at(p, 'svg', 'stroke-width'));
      const va = at(p, 'draw', 'textarea-vertical-align');
      if (va) g.anchor = va === 'middle' ? 'ctr' : va === 'bottom' ? 'b' : 't';
      if (at(p, 'draw', 'fill-image-name')) g.fillImage = at(p, 'draw', 'fill-image-name');
    }
    return g;
  };

  // pictures by path
  const picCache = new Map();
  const pic = async href => {
    const p = (href || '').replace(/^\.\//, '');
    if (picCache.has(p)) return picCache.get(p);
    const t = IMG_TYPES[p.toLowerCase().split('.').pop()];
    const f = files.get(p);
    const r = f && t ? Rich.dataUrl(t, await ZIP.inflate(f)) : null;
    picCache.set(p, r);
    return r;
  };

  async function runs(el, f, out) {
    for (const n of Array.from(el.childNodes)) {
      if (n.nodeType === 3) { if (n.nodeValue) out.push({ t: n.nodeValue.replace(/\s+/g, ' '), f }); continue; }
      if (n.nodeType !== 1) continue;
      if (is(n, 'text', 's')) out.push({ t: ' '.repeat(+at(n, 'text', 'c') || 1), f });
      else if (is(n, 'text', 'tab')) out.push({ t: '\t', f });
      else if (is(n, 'text', 'line-break')) out.push({ t: '\n', f });
      else if (is(n, 'text', 'span')) await runs(n, textF(chain('text', at(n, 'text', 'style-name')), f), out);
      else if (is(n, 'text', 'a')) { const href = Rich.safeUrl(at(n, 'xlink', 'href') || ''); await runs(n, href ? Object.assign({}, f, { href }) : f, out); }
      else if (n.namespaceURI === NS.text && /^(page-number|date|time|title|sender-|author)/.test(n.localName)) out.push({ t: n.textContent, f });
      else await runs(n, f, out);
    }
  }
  async function paras(el, f, out, list, level) {
    for (const c of kids(el)) {
      if (is(c, 'text', 'p') || is(c, 'text', 'h')) {
        const ps = chain('paragraph', at(c, 'text', 'style-name'));
        const b = paraP(ps, { runs: [], align: '' });
        if (list) { b.list = list; b.level = level; }
        await runs(c, textF(ps, f), b.runs);
        if (b.runs.length) { b.runs[0].t = (b.runs[0].t || '').replace(/^ +/, ''); }
        out.push(b);
      } else if (is(c, 'text', 'list')) {
        const ls = lists[at(c, 'text', 'style-name')];
        const lv = ls && (kids(ls).find(x => +at(x, 'text', 'level') === (list ? level + 2 : 1)) || kids(ls)[0]);
        const type = lv && lv.localName === 'list-level-style-number' && at(lv, 'style', 'num-format') ? 'ol' : 'ul';
        for (const it of kids(c)) await paras(it, f, out, type, list ? level + 1 : 0);
      } else if (is(c, 'text', 'list-item') || is(c, 'text', 'list-header') || is(c, 'text', 'section')) await paras(c, f, out, list, level);
    }
  }

  // the text a frame of a presentation class starts from (size, colour)
  const classF = cls => {
    const map = { title: 'title', subtitle: 'subtitle', outline: 'outline1', notes: 'notes' };
    for (const fam of ['presentation/', 'graphic/']) {
      const k = Object.keys(styles).find(x => x.startsWith(fam) && x.toLowerCase().endsWith('-' + (map[cls] || cls)));
      if (k) return textF(chain(k.split('/')[0], k.split('/')[1]), { size: 36 });
    }
    return { size: cls === 'title' ? 88 : 36 };
  };

  const deg = rad => -rad * 180 / Math.PI;
  /* a transform's rotation (degrees, clockwise) and its translation */
  const transform = (t, w, hh) => {
    const rot = /rotate\s*\(\s*(-?[\d.e-]+)\s*\)/.exec(t || ''), tr = /translate\s*\(\s*(-?[\d.e-]+\w*)\s+(-?[\d.e-]+\w*)\s*\)/.exec(t || '');
    if (!rot || !tr) return null;
    const a = +rot[1], ox = px(tr[1]), oy = px(tr[2]);
    // the corner (ox, oy) is where the rotated shape's top left went: its centre is that corner plus the rotated half
    const cx = ox + (w / 2) * Math.cos(-a) - (hh / 2) * Math.sin(-a), cy = oy + (w / 2) * Math.sin(-a) + (hh / 2) * Math.cos(-a);
    return { x: cx - w / 2, y: cy - hh / 2, rot: deg(a) };
  };

  async function shapes(el, out, dx, dy) {
    for (const c of kids(el)) {
      if (c.namespaceURI !== NS.draw) continue;
      if (c.localName === 'g') { await shapes(c, out, dx, dy); continue; }
      if (at(c, 'presentation', 'placeholder') === 'true') continue;
      const w = px(at(c, 'svg', 'width')), hh = px(at(c, 'svg', 'height'));
      let x = px(at(c, 'svg', 'x')) + dx, y = px(at(c, 'svg', 'y')) + dy, rot = 0;
      const tf = transform(at(c, 'draw', 'transform'), w, hh);
      if (tf) { x = tf.x + dx; y = tf.y + dy; rot = tf.rot; }
      const cls = at(c, 'presentation', 'class');
      const g = graphic(chain('graphic', at(c, 'draw', 'style-name')).concat(chain('presentation', at(c, 'presentation', 'style-name'))));
      const sh = { x, y, w, h: hh, rot, geom: null, anchor: g.anchor || (cls === 'title' ? 'ctr' : 't'), ph: cls || null };
      if (g.fill) sh.fill = g.fill;
      if (g.stroke) sh.line = { color: g.strokeColor || '#3465a4', w: g.strokeW || 1.33 };
      const baseF = Object.assign(classF(cls), textF(chain('presentation', at(c, 'presentation', 'style-name')).concat(chain('graphic', at(c, 'draw', 'style-name'))), {}));
      sh.size = (baseF.size || 36) / 2;
      if (c.localName === 'frame') {
        const box = kids(c).find(x => is(x, 'draw', 'text-box')), img = kids(c).find(x => is(x, 'draw', 'image')), tbl = kids(c).find(x => is(x, 'table', 'table'));
        if (img && !box) {
          const alt = kids(c).filter(x => is(x, 'draw', 'image')).map(x => at(x, 'xlink', 'href'));
          let src = null;
          for (const a of alt) { src = await pic(a); if (src) break; }
          if (!src) { lose('pictures in other formats'); continue; }
          sh.img = src;
        } else if (tbl) {
          sh.table = { cols: [], rows: [] };
          for (const r of tbl.getElementsByTagNameNS(NS.table, 'table-row')) {
            const cells = [];
            for (const td of kids(r)) {
              if (!is(td, 'table', 'table-cell')) continue;
              const cp = [];
              await paras(td, baseF, cp);
              cells.push({ paras: cp, span: +at(td, 'table', 'number-columns-spanned') || 1, rspan: +at(td, 'table', 'number-rows-spanned') || 1 });
            }
            sh.table.rows.push({ cells });
          }
          sh.size = 14;
        } else if (box) {
          sh.paras = [];
          await paras(box, baseF, sh.paras);
        } else if (kids(c).some(x => is(x, 'draw', 'object') || is(x, 'draw', 'object-ole'))) { lose('charts and embedded objects'); continue; }
        else continue;
      } else if (c.localName === 'custom-shape' || c.localName === 'rect' || c.localName === 'ellipse' || c.localName === 'circle') {
        const geo = kids(c).find(x => is(x, 'draw', 'enhanced-geometry'));
        const type = c.localName === 'ellipse' || c.localName === 'circle' ? 'ellipse' : geo ? at(geo, 'draw', 'type') || 'rectangle' : 'rectangle';
        sh.geom = /ellipse|circle/.test(type) ? 'ellipse' : /round/.test(type) ? 'roundRect' : 'rect';
        if (!/^(rectangle|round-rectangle|ellipse|circle|rect|flowchart-process)$/.test(type) && geo) lose('shapes drawn as rectangles');
        if (g.fill === undefined && g.fillColor) sh.fill = g.fillColor;
        sh.paras = [];
        await paras(c, baseF, sh.paras);
        sh.anchor = g.anchor || 'ctr';
      } else if (c.localName === 'line') {
        const x1 = px(at(c, 'svg', 'x1')) + dx, y1 = px(at(c, 'svg', 'y1')) + dy, x2 = px(at(c, 'svg', 'x2')) + dx, y2 = px(at(c, 'svg', 'y2')) + dy;
        Object.assign(sh, { geom: 'line', x: Math.min(x1, x2), y: Math.min(y1, y2), w: Math.abs(x2 - x1), h: Math.abs(y2 - y1), flipH: x2 < x1 !== y2 < y1, line: { color: g.strokeColor || '#3465a4', w: g.strokeW || 1.33 } });
        delete sh.paras;
      } else { if (!/^(page-thumbnail|a)$/.test(c.localName)) lose('drawings'); continue; }
      out.push(sh);
    }
  }

  // the page size, the masters' backgrounds and what they draw
  let W = 1123, H = 794;
  const layouts = {};
  if (stylesDoc) for (const pl of stylesDoc.getElementsByTagNameNS(NS.style, 'page-layout')) {
    const p = kids(pl).find(x => is(x, 'style', 'page-layout-properties'));
    if (p) layouts[at(pl, 'style', 'name')] = { w: px(at(p, 'fo', 'page-width')), h: px(at(p, 'fo', 'page-height')) };
  }
  const bgOf = name => {
    const g = graphic(chain('drawing-page', name));
    return g.fill || null;
  };
  const masters = {};
  if (stylesDoc) for (const mp of stylesDoc.getElementsByTagNameNS(NS.style, 'master-page')) {
    const m = { bg: bgOf(at(mp, 'draw', 'style-name')), shapes: [] };
    await shapes(mp, m.shapes, 0, 0);
    const l = layouts[at(mp, 'style', 'page-layout-name')];
    if (l && l.w) { W = l.w; H = l.h; }
    masters[at(mp, 'style', 'name')] = m;
  }
  const deck = { w: Math.round(W), h: Math.round(H), slides: [], lost };
  // a drawing (.odg) is pages too
  const pres = content.getElementsByTagNameNS(NS.office, 'presentation')[0] || content.getElementsByTagNameNS(NS.office, 'drawing')[0];
  if (!pres) throw new Error('not an OpenDocument presentation (no content.xml)');
  for (const pg of kids(pres).filter(x => is(x, 'draw', 'page'))) {
    const master = masters[at(pg, 'draw', 'master-page-name')] || { bg: null, shapes: [] };
    const s = { bg: bgOf(at(pg, 'draw', 'style-name')) || master.bg, shapes: [], under: master.shapes, master: at(pg, 'draw', 'master-page-name'), name: at(pg, 'draw', 'name') };
    await shapes(pg, s.shapes, 0, 0);
    deck.slides.push(s);
  }
  // to write it back: the styles as they are, and the pictures they use
  if (stylesDoc) deck.keep = { styles: new TextDecoder().decode(await ZIP.inflate(files.get('styles.xml'))), files: [...files.values()].filter(f => /^Pictures\//.test(f.name)) };
  return deck;
}

/* ---------------- writing ---------------- */
const xe = s => String(s).replace(/[\x00-\x08\x0B\x0C\x0E-\x1F￾￿]/g, '').replace(/[&<>"]/g, c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : '&quot;'));
const cm = v => (v * 2.54 / 96).toFixed(3) + 'cm';
function textXml(t) {
  let out = '';
  for (const part of t.split(/(\t|\n| {2,})/)) {
    if (part === '\t') out += '<text:tab/>';
    else if (part === '\n') out += '<text:line-break/>';
    else if (/^ {2,}$/.test(part)) out += ' <text:s text:c="' + (part.length - 1) + '"/>';
    else out += xe(part);
  }
  return out;
}

/* deck -> .odp; flatten: what masters drew becomes part of each slide (a deck from elsewhere) */
async function write(deck, opts) {
  opts = opts || {};
  const keep = deck.keep && !opts.flatten ? deck.keep : null;
  const auto = [], gStyles = new Map(), pStyles = new Map(), tStyles = new Map(), dStyles = new Map(), pics = [], picOf = new Map();
  const style = (map, prefix, family, props, inner) => {
    const key = props;
    if (!map.has(key)) { const n = prefix + (map.size + 1); map.set(key, n); auto.push('<style:style style:name="' + n + '" style:family="' + family + '"' + (family === 'graphic' ? ' style:parent-style-name="standard"' : '') + '>' + inner + '</style:style>'); }
    return map.get(key);
  };
  const gStyle = sh => {
    let p = '';
    p += sh.fill ? ' draw:fill="solid" draw:fill-color="' + sh.fill + '"' : ' draw:fill="none"';
    p += sh.line && sh.line.color ? ' draw:stroke="solid" svg:stroke-color="' + sh.line.color + '" svg:stroke-width="' + cm(sh.line.w || 1.33) + '"' : ' draw:stroke="none"';
    p += ' draw:textarea-vertical-align="' + ({ ctr: 'middle', b: 'bottom' }[sh.anchor] || 'top') + '" draw:auto-grow-height="false" fo:padding-top="0.13cm" fo:padding-bottom="0.13cm" fo:padding-left="0.25cm" fo:padding-right="0.25cm" fo:wrap-option="wrap"';
    return style(gStyles, 'gr', 'graphic', p, '<style:graphic-properties' + p + '/>');
  };
  const pStyle = b => {
    const p = b.align ? ' fo:text-align="' + (b.align === 'right' ? 'end' : b.align) + '"' : ' fo:text-align="start"';
    return style(pStyles, 'P', 'paragraph', p, '<style:paragraph-properties' + p + '/>');
  };
  const tStyle = (f, size) => {
    let p = ' fo:font-size="' + ((f.size || size * 2) / 2) + 'pt"';
    if (f.b) p += ' fo:font-weight="bold"';
    if (f.i) p += ' fo:font-style="italic"';
    if (f.u) p += ' style:text-underline-style="solid" style:text-underline-width="auto" style:text-underline-color="font-color"';
    if (f.s) p += ' style:text-line-through-style="solid"';
    if (f.sup) p += ' style:text-position="super 58%"'; else if (f.sub) p += ' style:text-position="sub 58%"';
    if (f.color) p += ' fo:color="' + f.color + '"';
    if (f.font) p += ' fo:font-family="' + xe(/\s/.test(f.font) ? "'" + f.font + "'" : f.font) + '"';
    return style(tStyles, 'T', 'text', p, '<style:text-properties' + p + '/>');
  };
  const parasXml = (ps, size) => {
    let out = '';
    const open = [];
    const close = n => { while (open.length > n) { out += '</text:list-item></text:list>'; open.pop(); } };
    for (const b of ps || []) {
      let inner = '';
      for (const r of b.runs || []) {
        if (r.img) continue;
        let x = '<text:span text:style-name="' + tStyle(r.f || {}, size) + '">' + textXml(r.t || '') + '</text:span>';
        if (r.f && r.f.href) x = '<text:a xlink:type="simple" xlink:href="' + xe(r.f.href) + '">' + x + '</text:a>';
        inner += x;
      }
      const p = '<text:p text:style-name="' + pStyle(b) + '">' + inner + '</text:p>';
      if (b.list) {
        const lv = b.level || 0;
        close(lv + 1);
        if (open.length === lv + 1) out += '</text:list-item><text:list-item>';
        while (open.length < lv + 1) { out += '<text:list text:style-name="' + (b.list === 'ol' ? 'LNum' : 'LBul') + '"><text:list-item>'; open.push(b.list); }
        out += p;
      } else { close(0); out += p; }
    }
    close(0);
    return out;
  };
  const picPath = async src => {
    if (picOf.has(src)) return picOf.get(src);
    const d = Rich.dataBytes(src);
    if (!d) return null;
    const ext = { 'image/png': 'png', 'image/jpeg': 'jpg', 'image/gif': 'gif', 'image/svg+xml': 'svg', 'image/bmp': 'bmp', 'image/webp': 'webp' }[d.type] || 'png';
    const p = 'Pictures/s' + (pics.length + 1) + '.' + ext;
    pics.push({ path: p, bytes: d.bytes, type: d.type });
    picOf.set(src, p);
    return p;
  };
  const place = sh => {
    if (!sh.rot) return 'svg:x="' + cm(sh.x) + '" svg:y="' + cm(sh.y) + '"';
    // rotated about the centre: where the top left corner goes
    const a = -sh.rot * Math.PI / 180, cx = sh.x + sh.w / 2, cy = sh.y + sh.h / 2;
    const ox = cx - (sh.w / 2) * Math.cos(a) + (sh.h / 2) * Math.sin(a), oy = cy - (sh.w / 2) * Math.sin(a) - (sh.h / 2) * Math.cos(a);
    return 'draw:transform="rotate (' + a.toFixed(6) + ') translate (' + cm(ox) + ' ' + cm(oy) + ')"';
  };
  const shapeXml = async sh => {
    const size = 'svg:width="' + cm(Math.max(1, sh.w)) + '" svg:height="' + cm(Math.max(1, sh.h)) + '" ';
    const cls = sh.ph === 'title' || sh.ph === 'ctrTitle' ? ' presentation:class="title"' : sh.ph === 'subTitle' ? ' presentation:class="subtitle"' : '';
    if (sh.geom === 'line') {
      const x1 = sh.flipH ? sh.x + sh.w : sh.x, x2 = sh.flipH ? sh.x : sh.x + sh.w;
      return '<draw:line draw:style-name="' + gStyle(Object.assign({}, sh, { fill: null })) + '" svg:x1="' + cm(x1) + '" svg:y1="' + cm(sh.y) + '" svg:x2="' + cm(x2) + '" svg:y2="' + cm(sh.y + sh.h) + '"/>';
    }
    if (sh.img) {
      const p = await picPath(sh.img);
      return p ? '<draw:frame draw:style-name="' + gStyle({ anchor: 't' }) + '" ' + size + place(sh) + '><draw:image xlink:href="' + p + '" xlink:type="simple" xlink:show="embed" xlink:actuate="onLoad"/></draw:frame>' : '';
    }
    if (sh.table) {
      let x = '<table:table>';
      const ncols = Math.max(...sh.table.rows.map(r => r.cells.reduce((a, c) => a + (c.span || 1), 0)), 1);
      x += '<table:table-column table:number-columns-repeated="' + ncols + '"/>';
      for (const r of sh.table.rows) {
        x += '<table:table-row>';
        for (const c of r.cells) {
          x += '<table:table-cell' + (c.span > 1 ? ' table:number-columns-spanned="' + c.span + '"' : '') + (c.rspan > 1 ? ' table:number-rows-spanned="' + c.rspan + '"' : '') + '>' +
            (parasXml(c.paras, sh.size || 14) || '<text:p/>') + '</table:table-cell>';
          for (let k = 1; k < (c.span || 1); k++) x += '<table:covered-table-cell/>';
        }
        x += '</table:table-row>';
      }
      return '<draw:frame draw:style-name="' + gStyle({ anchor: 't' }) + '" ' + size + place(sh) + '>' + x + '</table:table></draw:frame>';
    }
    if (sh.geom) {
      const type = { ellipse: 'ellipse', roundRect: 'round-rectangle' }[sh.geom] || 'rectangle';
      return '<draw:custom-shape draw:style-name="' + gStyle(sh) + '" ' + size + place(sh) + '>' + parasXml(sh.paras, sh.size || 18) +
        '<draw:enhanced-geometry svg:viewBox="0 0 21600 21600" draw:type="' + type + '"/></draw:custom-shape>';
    }
    return '<draw:frame draw:style-name="' + gStyle(sh) + '" ' + size + place(sh) + cls + '><draw:text-box>' + parasXml(sh.paras, sh.size || 18) + '</draw:text-box></draw:frame>';
  };
  let pages = '';
  for (const [i, s] of deck.slides.entries()) {
    const dp = style(dStyles, 'dp', 'drawing-page', s.bg || '', '<style:drawing-page-properties presentation:background-visible="true" presentation:background-objects-visible="true"' +
      (s.bg ? ' draw:fill="solid" draw:fill-color="' + s.bg + '"' : '') + '/>');
    let x = '';
    const list = (keep ? [] : s.under || []).concat(s.shapes);
    for (const sh of list) x += await shapeXml(sh);
    pages += '<draw:page draw:name="' + xe(s.name || 'page' + (i + 1)) + '" draw:style-name="' + dp + '" draw:master-page-name="' + xe(keep && s.master ? s.master : 'Default') + '">' + x + '</draw:page>';
  }
  const listStyle = (name, numbered) => '<text:list-style style:name="' + name + '">' + Array.from({ length: 9 }, (_, i) => {
    const props = '<style:list-level-properties text:space-before="' + (0.6 * i).toFixed(2) + 'cm" text:min-label-width="0.6cm"/>';
    return numbered
      ? '<text:list-level-style-number text:level="' + (i + 1) + '" style:num-suffix="." style:num-format="1">' + props + '</text:list-level-style-number>'
      : '<text:list-level-style-bullet text:level="' + (i + 1) + '" text:bullet-char="' + ['●', '–', '•'][i % 3] + '">' + props + '<style:text-properties fo:font-family="OpenSymbol" style:use-window-font-color="true" fo:font-size="45%"/></text:list-level-style-bullet>';
  }).join('') + '</text:list-style>';
  const nsDecl = Object.entries(NS).filter(([k]) => k !== 'manifest').map(([k, v]) => 'xmlns:' + k + '="' + v + '"').join(' ');
  const head = '<?xml version="1.0" encoding="UTF-8"?>\n';
  const content = head + '<office:document-content ' + nsDecl + ' office:version="1.3"><office:automatic-styles>' + auto.join('') + listStyle('LBul', false) + listStyle('LNum', true) +
    '</office:automatic-styles><office:body><office:presentation>' + pages + '</office:presentation></office:body></office:document-content>';
  const styles = keep ? keep.styles : head + '<office:document-styles ' + nsDecl + ' office:version="1.3"><office:styles>' +
    '<style:default-style style:family="graphic"><style:graphic-properties svg:stroke-color="#3465a4" draw:fill-color="#729fcf"/><style:paragraph-properties style:text-autospace="ideograph-alpha"/><style:text-properties fo:font-size="18pt" fo:font-family="\'Liberation Sans\'" fo:language="en" fo:country="GB"/></style:default-style>' +
    '<style:style style:name="standard" style:family="graphic"><style:graphic-properties draw:stroke="none" draw:fill="none"/><style:text-properties fo:font-size="18pt"/></style:style>' +
    '</office:styles><office:automatic-styles><style:page-layout style:name="PM1"><style:page-layout-properties fo:margin-top="0cm" fo:margin-bottom="0cm" fo:margin-left="0cm" fo:margin-right="0cm" fo:page-width="' + cm(deck.w) + '" fo:page-height="' + cm(deck.h) + '" style:print-orientation="landscape"/></style:page-layout>' +
    '<style:style style:name="Mdp1" style:family="drawing-page"><style:drawing-page-properties draw:background-size="full" draw:fill="solid" draw:fill-color="#ffffff"/></style:style></office:automatic-styles>' +
    '<office:master-styles><style:master-page style:name="Default" style:page-layout-name="PM1" draw:style-name="Mdp1"/></office:master-styles></office:document-styles>';
  const meta = head + '<office:document-meta ' + nsDecl + ' office:version="1.3"><office:meta><meta:generator>osada</meta:generator></office:meta></office:document-meta>';
  const keptPics = keep ? keep.files.filter(f => !pics.some(p => p.path === f.name)) : [];
  const manifest = head + '<manifest:manifest xmlns:manifest="' + NS.manifest + '" manifest:version="1.3">' +
    '<manifest:file-entry manifest:full-path="/" manifest:version="1.3" manifest:media-type="' + MIME + '"/>' +
    ['content.xml', 'styles.xml', 'meta.xml'].map(n => '<manifest:file-entry manifest:full-path="' + n + '" manifest:media-type="text/xml"/>').join('') +
    pics.concat(keptPics.map(f => ({ path: f.name, type: IMG_TYPES[f.name.toLowerCase().split('.').pop()] || 'application/octet-stream' })))
      .map(p => '<manifest:file-entry manifest:full-path="' + p.path + '" manifest:media-type="' + p.type + '"/>').join('') + '</manifest:manifest>';
  const files = [await ZIP.add('mimetype', MIME, true), await ZIP.add('content.xml', content), await ZIP.add('styles.xml', styles),
    await ZIP.add('meta.xml', meta), await ZIP.add('META-INF/manifest.xml', manifest)];
  for (const p of pics) files.push(await ZIP.add(p.path, p.bytes, true));
  for (const f of keptPics) files.push(Object.assign({}, f, { name: f.name }));
  return ZIP.write(files);
}

/* an empty presentation: one title slide, 16:9 */
const blank = () => write({ w: 1280, h: 720, slides: [{ bg: null, shapes: [
  { x: 96, y: 200, w: 1088, h: 160, ph: 'title', anchor: 'ctr', size: 44, paras: [{ runs: [], align: 'center' }] },
  { x: 96, y: 390, w: 1088, h: 120, ph: 'subTitle', anchor: 't', size: 24, paras: [{ runs: [], align: 'center' }] },
] }] });

const ODP = { parse, write, blank, MIME };
/* for the editor (slides.js): how it saves */
const ODP_FORMAT = { write: deck => write(deck) };
return { ODP, ODP_FORMAT, readDeck: parse };
});
