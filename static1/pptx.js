/* PowerPoint presentations (.pptx, .pptm, .potx, .ppsx): read into the deck model (slides.js)
   to be shown and edited as .odp, and written from it as the "export .pptx" of an .odp. Part
   pptx, on zip, rich and slides. Read: the slide size; each slide's shapes with the
   geometry their placeholders inherit from layout and master; text with the formatting it
   inherits from the master's text styles down to the run (size, bold, italic, underline,
   colour through the theme, font, bullets and levels, alignment); pictures, tables, groups,
   lines, solid and theme fills, backgrounds, and what master and layout draw (shown
   behind each slide). Charts, diagrams, media and animations are counted as lost. */
'use strict';

part('pptx', ({ ZIP, Rich, shade }) => {

const A = 'http://schemas.openxmlformats.org/drawingml/2006/main';
const P = 'http://schemas.openxmlformats.org/presentationml/2006/main';
const R = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships';
const PR = 'http://schemas.openxmlformats.org/package/2006/relationships';
const MC = 'http://schemas.openxmlformats.org/markup-compatibility/2006';
const EMU = 9525;   // per px at 96 dpi
const IMG_TYPES = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', svg: 'image/svg+xml', bmp: 'image/bmp', webp: 'image/webp' };
const PRST = { black: '#000000', white: '#ffffff', red: '#ff0000', green: '#008000', blue: '#0000ff', yellow: '#ffff00', gray: '#808080', darkGray: '#a9a9a9', lightGray: '#d3d3d3' };

const kid = (el, ns, name) => (el ? Array.from(el.children).find(x => x.namespaceURI === ns && x.localName === name) || null : null);
const kidsOf = (el, ns, name) => (el ? Array.from(el.children).filter(x => x.namespaceURI === ns && (!name || x.localName === name)) : []);
const attr = (el, n) => (el ? el.getAttribute(n) : null);
function partPath(base, target) {
  if (target.startsWith('/')) return target.slice(1);
  const out = base.split('/').filter(Boolean);
  out.pop();
  for (const s of target.split('/')) { if (s === '..') out.pop(); else if (s !== '.') out.push(s); }
  return out.join('/');
}

async function parse(bytes) {
  const files = new Map(ZIP.read(bytes).map(f => [f.name, f]));
  const cache = new Map();
  const xml = async name => {
    if (cache.has(name)) return cache.get(name);
    const f = files.get(name);
    let d = null;
    if (f) {
      d = new DOMParser().parseFromString(new TextDecoder().decode(await ZIP.inflate(f)), 'application/xml');
      if (d.getElementsByTagName('parsererror').length) throw new Error(name + ': broken XML');
    }
    cache.set(name, d);
    return d;
  };
  const relsOf = async part => {
    const d = await xml(part.replace(/[^/]*$/, '') + '_rels/' + part.split('/').pop() + '.rels');
    const out = {};
    if (d) for (const r of d.getElementsByTagNameNS(PR, 'Relationship')) out[r.getAttribute('Id')] = { target: partPath(part, r.getAttribute('Target')), raw: r.getAttribute('Target'), external: r.getAttribute('TargetMode') === 'External', type: (r.getAttribute('Type') || '').split('/').pop() };
    return out;
  };
  const lost = {};
  const lose = w => { lost[w] = (lost[w] || 0) + 1; };
  let main = 'ppt/presentation.xml';
  const root = await relsOf('');
  for (const id in root) if (root[id].type === 'officeDocument') main = root[id].target;
  const pres = await xml(main);
  if (!pres) throw new Error('not a PowerPoint presentation (no presentation.xml)');
  const pr = pres.documentElement, prels = await relsOf(main);
  const sz = kid(pr, P, 'sldSz');
  const deck = { w: sz ? Math.round(+attr(sz, 'cx') / EMU) : 1280, h: sz ? Math.round(+attr(sz, 'cy') / EMU) : 720, slides: [], lost };
  const macros = [...files.keys()].some(n => /vbaProject\.bin$/i.test(n));
  if (macros) deck.macros = true;

  // the theme of a master: its colours and fonts
  const themes = new Map();
  const themeOf = async masterPath => {
    if (themes.has(masterPath)) return themes.get(masterPath);
    const mr = await relsOf(masterPath);
    let th = null;
    for (const id in mr) if (mr[id].type === 'theme') th = await xml(mr[id].target);
    const t = { colors: {}, major: 'Calibri Light', minor: 'Calibri' };
    if (th) {
      const cs = th.getElementsByTagNameNS(A, 'clrScheme')[0];
      for (const c of cs ? Array.from(cs.children) : []) {
        const v = c.firstElementChild;
        t.colors[c.localName] = v ? '#' + (attr(v, 'lastClr') || attr(v, 'val') || '000000').toLowerCase() : '#000000';
      }
      const fs = th.getElementsByTagNameNS(A, 'fontScheme')[0];
      const lat = tag => { const f = fs && fs.getElementsByTagNameNS(A, tag)[0]; const l = f && kid(f, A, 'latin'); return l && attr(l, 'typeface'); };
      t.major = lat('majorFont') || t.major;
      t.minor = lat('minorFont') || t.minor;
    }
    themes.set(masterPath, t);
    return t;
  };

  // a colour element (a:srgbClr, a:schemeClr...) with its changes
  const colorOf = (el, ctx) => {
    if (!el) return null;
    let hex = null;
    if (el.localName === 'srgbClr') hex = '#' + (attr(el, 'val') || '000000').toLowerCase();
    else if (el.localName === 'sysClr') hex = '#' + (attr(el, 'lastClr') || '000000').toLowerCase();
    else if (el.localName === 'prstClr') hex = PRST[attr(el, 'val')] || '#000000';
    else if (el.localName === 'scrgbClr') hex = '#' + ['r', 'g', 'b'].map(k => Math.round(Math.min(100000, +attr(el, k)) / 100000 * 255).toString(16).padStart(2, '0')).join('');
    else if (el.localName === 'schemeClr') {
      let v = attr(el, 'val');
      if (v === 'phClr' && ctx.ph) return ctx.ph;
      v = ctx.map[v] || v;
      hex = ctx.theme.colors[v] || null;
    }
    if (!hex) return null;
    const m = n => { const x = kid(el, A, n); return x ? +attr(x, 'val') / 100000 : undefined; };
    if (kid(el, A, 'alpha') && m('alpha') < 0.05) return null;
    return m('lumMod') !== undefined || m('lumOff') !== undefined || m('tint') !== undefined || m('shade') !== undefined ? shade(hex, m('lumMod'), m('lumOff'), m('tint'), m('shade')) : hex;
  };
  const colorIn = (el, ctx) => (el ? colorOf(Array.from(el.children).find(x => /Clr$/.test(x.localName)) || null, ctx) : null);
  // a fill: solid, the first stop of a gradient, none; undefined when not said
  const fillOf = (spPr, ctx) => {
    if (!spPr) return undefined;
    if (kid(spPr, A, 'noFill')) return null;
    const s = kid(spPr, A, 'solidFill');
    if (s) return colorIn(s, ctx);
    const g = kid(spPr, A, 'gradFill');
    if (g) { const gs = g.getElementsByTagNameNS(A, 'gs')[0]; return gs ? colorIn(gs, ctx) : undefined; }
    if (kid(spPr, A, 'pattFill')) { const f = kid(kid(spPr, A, 'pattFill'), A, 'fgClr'); return colorIn(f, ctx); }
    return undefined;
  };

  // text: the properties a level has (a:lvl1pPr... in a list style), and a run's
  const runF = (rPr, f, ctx) => {
    if (!rPr) return f;
    f = Object.assign({}, f);
    if (attr(rPr, 'sz')) f.size = Math.round(+attr(rPr, 'sz') / 50);
    if (attr(rPr, 'b') !== null) f.b = /^(1|true)$/.test(attr(rPr, 'b'));
    if (attr(rPr, 'i') !== null) f.i = /^(1|true)$/.test(attr(rPr, 'i'));
    if (attr(rPr, 'u') !== null) f.u = attr(rPr, 'u') !== 'none';
    if (attr(rPr, 'strike') !== null) f.s = attr(rPr, 'strike') !== 'noStrike';
    if (attr(rPr, 'baseline') !== null) { const b = +attr(rPr, 'baseline'); f.sup = b > 0; f.sub = b < 0; }
    const fill = kid(rPr, A, 'solidFill');
    if (fill) f.color = colorIn(fill, ctx);
    const lat = kid(rPr, A, 'latin');
    if (lat) { const tf = attr(lat, 'typeface'); f.font = tf === '+mj-lt' ? ctx.theme.major : tf === '+mn-lt' ? ctx.theme.minor : tf; }
    return f;
  };
  const lvlProps = (lst, n, base, ctx) => {
    const lv = lst && kid(lst, A, 'lvl' + n + 'pPr');
    if (!lv) return base;
    const b = Object.assign({}, base);
    if (attr(lv, 'algn')) b.align = { ctr: 'center', r: 'right', just: 'justify', dist: 'justify' }[attr(lv, 'algn')] || '';
    if (kid(lv, A, 'buNone')) b.bullet = null;
    else if (kid(lv, A, 'buAutoNum')) b.bullet = 'ol';
    else if (kid(lv, A, 'buChar') || kid(lv, A, 'buBlip')) b.bullet = 'ul';
    b.f = runF(kid(lv, A, 'defRPr'), b.f, ctx);
    return b;
  };

  // shapes and their placeholders: {type, idx} -> the shape on layout or master
  const phKey = sp => { const ph = sp && kid(kid(kid(sp, P, 'nvSpPr') || kid(sp, P, 'nvPicPr') || kid(sp, P, 'nvGraphicFramePr'), P, 'nvPr'), P, 'ph'); return ph ? { type: attr(ph, 'type') || 'body', idx: attr(ph, 'idx') } : null; };
  const findPh = (tree, key) => {
    if (!tree || !key) return null;
    const all = Array.from(tree.getElementsByTagNameNS(P, 'sp'));
    return all.find(s => { const k = phKey(s); return k && key.idx !== null && k.idx === key.idx; }) ||
      all.find(s => { const k = phKey(s); return k && k.type === key.type; }) ||
      (key.type === 'ctrTitle' ? all.find(s => { const k = phKey(s); return k && k.type === 'title'; }) : null) ||
      (key.type === 'subTitle' || key.type === 'obj' ? all.find(s => { const k = phKey(s); return k && k.type === 'body'; }) : null);
  };
  const xfrmOf = el => {
    const x = el && kid(el, A, 'xfrm');
    if (!x) return null;
    const off = kid(x, A, 'off'), ext = kid(x, A, 'ext');
    if (!off || !ext) return null;
    return { x: +attr(off, 'x') / EMU, y: +attr(off, 'y') / EMU, w: +attr(ext, 'cx') / EMU, h: +attr(ext, 'cy') / EMU, rot: (+attr(x, 'rot') || 0) / 60000,
      flipH: attr(x, 'flipH') === '1', flipV: attr(x, 'flipV') === '1', chOff: kid(x, A, 'chOff'), chExt: kid(x, A, 'chExt') };
  };

  const picCache = new Map();
  const picture = async (rels, rid) => {
    const r = rels[rid];
    if (!r || r.external) { lose('linked pictures'); return null; }
    if (picCache.has(r.target)) return picCache.get(r.target);
    const t = IMG_TYPES[r.target.toLowerCase().split('.').pop()], f = files.get(r.target);
    const v = t && f ? Rich.dataUrl(t, await ZIP.inflate(f)) : null;
    if (!v) lose('pictures in other formats');
    picCache.set(r.target, v);
    return v;
  };

  // the text body of a shape, with what it inherits (lists: master text style, master and layout placeholder, own)
  const textOf = async (txBody, inh, ctx, rels) => {
    const own = kid(txBody, A, 'lstStyle');
    const paras = [];
    for (const p of kidsOf(txBody, A, 'p')) {
      const pPr = kid(p, A, 'pPr'), lvl = pPr && attr(pPr, 'lvl') ? +attr(pPr, 'lvl') : 0;
      let st = { align: '', bullet: null, f: { size: 36 } };
      for (const l of inh.lists.concat([own])) st = lvlProps(l, lvl + 1, st, ctx);
      // the paragraph's own properties
      if (pPr) {
        if (attr(pPr, 'algn')) st.align = { ctr: 'center', r: 'right', just: 'justify', dist: 'justify' }[attr(pPr, 'algn')] || '';
        if (kid(pPr, A, 'buNone')) st.bullet = null;
        else if (kid(pPr, A, 'buAutoNum')) st.bullet = 'ol';
        else if (kid(pPr, A, 'buChar')) st.bullet = 'ul';
        st.f = runF(kid(pPr, A, 'defRPr'), st.f, ctx);
      }
      const b = { runs: [], align: st.align || '' };
      if (st.bullet) { b.list = st.bullet; b.level = lvl; }
      for (const r of Array.from(p.children)) {
        if (r.namespaceURI !== A) continue;
        if (r.localName === 'r' || r.localName === 'fld') {
          const rPr = kid(r, A, 'rPr');
          let f = runF(rPr, st.f, ctx);
          const hl = rPr && kid(rPr, A, 'hlinkClick');
          if (hl && rels[hl.getAttributeNS(R, 'id')] && rels[hl.getAttributeNS(R, 'id')].external) { const u = Rich.safeUrl(rels[hl.getAttributeNS(R, 'id')].raw); if (u) f = Object.assign({}, f, { href: u }); }
          const t = kid(r, A, 't');
          if (t) b.runs.push({ t: t.textContent, f });
        } else if (r.localName === 'br') b.runs.push({ t: '\n', f: st.f });
      }
      if (!b.runs.length) b.size = st.f.size;
      paras.push(b);
    }
    // a run's size inherits; the shape's size is the commonest one
    const sizes = paras.flatMap(p => p.runs.map(r => r.f.size)).filter(Boolean);
    const common = sizes.length ? sizes.sort((x, y) => sizes.filter(v => v === y).length - sizes.filter(v => v === x).length)[0] : 36;
    paras.forEach(p => delete p.size);
    return { paras, size: common / 2 };
  };

  // a slide, layout or master's shape tree
  async function tree(spTree, ctx, rels, out, layer, tf) {
    for (const el of Array.from(spTree ? spTree.children : [])) {
      if (el.namespaceURI === MC && el.localName === 'AlternateContent') {
        const alt = kid(el, MC, 'Fallback') || kid(el, MC, 'Choice');
        if (alt) await tree(alt, ctx, rels, out, layer, tf);
        continue;
      }
      if (el.namespaceURI !== P) continue;
      const key = phKey(el);
      // on layouts and masters, placeholders only give geometry and text style
      if (layer !== 'slide' && key) continue;
      const tfx = g => (tf ? { x: tf.x + (g.x - tf.cx) * tf.kx, y: tf.y + (g.y - tf.cy) * tf.ky, w: g.w * tf.kx, h: g.h * tf.ky, rot: g.rot, flipH: g.flipH, flipV: g.flipV } : g);
      if (el.localName === 'grpSp') {
        const g = xfrmOf(kid(el, P, 'grpSpPr'));
        if (!g) { await tree(el, ctx, rels, out, layer, tf); continue; }
        const o = tfx(g);
        const cx = g.chOff ? +attr(g.chOff, 'x') / EMU : g.x, cy = g.chOff ? +attr(g.chOff, 'y') / EMU : g.y;
        const cw = g.chExt ? +attr(g.chExt, 'cx') / EMU : g.w, ch = g.chExt ? +attr(g.chExt, 'cy') / EMU : g.h;
        await tree(el, ctx, rels, out, layer, { x: o.x, y: o.y, cx, cy, kx: cw ? o.w / cw : 1, ky: ch ? o.h / ch : 1 });
        continue;
      }
      if (el.localName === 'sp' || el.localName === 'cxnSp') {
        const spPr = kid(el, P, 'spPr');
        const lay = key ? findPh(ctx.layoutTree, key) : null, mas = key ? findPh(ctx.masterTree, key) || findPh(ctx.masterTree, { type: key.type === 'title' || key.type === 'ctrTitle' ? 'title' : 'body', idx: null }) : null;
        let g = xfrmOf(spPr) || xfrmOf(lay && kid(lay, P, 'spPr')) || xfrmOf(mas && kid(mas, P, 'spPr'));
        if (!g) continue;
        g = tfx(g);
        const geomEl = kid(spPr, A, 'prstGeom');
        const prst = geomEl ? attr(geomEl, 'prst') : kid(spPr, A, 'custGeom') ? 'cust' : null;
        const style = kid(el, P, 'style');
        const sh = { x: g.x, y: g.y, w: g.w, h: g.h, rot: g.rot, flipH: g.flipH, flipV: g.flipV, geom: null, ph: key ? key.type : null };
        let fill = fillOf(spPr, ctx);
        if (fill === undefined && style) { const fr = kid(style, A, 'fillRef'); if (fr && +attr(fr, 'idx') > 0) fill = colorIn(fr, ctx); }
        if (fill) sh.fill = fill;
        const ln = kid(spPr, A, 'ln');
        let lc = ln && !kid(ln, A, 'noFill') ? colorIn(kid(ln, A, 'solidFill'), ctx) : null;
        if (!lc && style && !(ln && kid(ln, A, 'noFill'))) { const lr = kid(style, A, 'lnRef'); if (lr && +attr(lr, 'idx') > 0) lc = colorIn(lr, ctx); }
        if (lc) sh.line = { color: lc, w: ln && attr(ln, 'w') ? Math.max(0.5, +attr(ln, 'w') / EMU) : 1.33 };
        if (el.localName === 'cxnSp' || /line|Connector/i.test(prst || '')) { sh.geom = 'line'; sh.line = sh.line || { color: '#000000', w: 1.33 }; delete sh.fill; out.push(sh); continue; }
        if (prst === 'ellipse') sh.geom = 'ellipse';
        else if (prst === 'roundRect') sh.geom = 'roundRect';
        else if (prst && prst !== 'rect' && (sh.fill || sh.line)) { sh.geom = 'rect'; lose('shapes drawn as rectangles'); }
        else if (sh.fill || sh.line) sh.geom = 'rect';
        const txBody = kid(el, P, 'txBody');
        if (txBody && txBody.getElementsByTagNameNS(A, 't').length) {
          const bodyPr = kid(txBody, A, 'bodyPr') || kid(kid(lay, P, 'txBody'), A, 'bodyPr') || kid(kid(mas, P, 'txBody'), A, 'bodyPr');
          const anc = attr(bodyPr, 'anchor') || attr(kid(kid(lay, P, 'txBody'), A, 'bodyPr'), 'anchor') || attr(kid(kid(mas, P, 'txBody'), A, 'bodyPr'), 'anchor');
          sh.anchor = anc === 'ctr' ? 'ctr' : anc === 'b' ? 'b' : sh.geom ? 'ctr' : 't';
          const kind = !key ? 'other' : key.type === 'title' || key.type === 'ctrTitle' ? 'title' : 'body';
          const lists = [ctx.txStyles[kind], kid(kid(mas, P, 'txBody'), A, 'lstStyle'), kid(kid(lay, P, 'txBody'), A, 'lstStyle')];
          let tctx = ctx;
          if (style) { const fr = kid(style, A, 'fontRef'); const c = fr && colorIn(fr, ctx); if (c) tctx = Object.assign({}, ctx, { textColor: c }); }
          const t = await textOf(txBody, { lists }, tctx, rels);
          if (tctx.textColor) t.paras.forEach(p => p.runs.forEach(r => { if (!r.f.color) r.f = Object.assign({}, r.f, { color: tctx.textColor }); }));
          sh.paras = t.paras;
          sh.size = t.size;
        }
        if (!sh.paras && !sh.geom) continue;
        out.push(sh);
      } else if (el.localName === 'pic') {
        const g0 = xfrmOf(kid(el, P, 'spPr')) || xfrmOf(kid(findPh(ctx.layoutTree, key), P, 'spPr'));
        if (!g0) continue;
        const g = tfx(g0);
        const blip = el.getElementsByTagNameNS(A, 'blip')[0];
        const src = blip ? await picture(rels, blip.getAttributeNS(R, 'embed')) : null;
        if (el.getElementsByTagNameNS(A, 'videoFile').length || el.getElementsByTagNameNS(A, 'audioFile').length) lose('video and sound');
        if (src) out.push({ x: g.x, y: g.y, w: g.w, h: g.h, rot: g.rot, flipH: g.flipH, flipV: g.flipV, img: src });
      } else if (el.localName === 'graphicFrame') {
        // a graphic frame keeps its place in p:xfrm
        const xf = kid(el, P, 'xfrm'), off = kid(xf, A, 'off'), ext = kid(xf, A, 'ext');
        const g = off && ext ? tfx({ x: +attr(off, 'x') / EMU, y: +attr(off, 'y') / EMU, w: +attr(ext, 'cx') / EMU, h: +attr(ext, 'cy') / EMU, rot: 0 }) : null;
        const tbl = el.getElementsByTagNameNS(A, 'tbl')[0];
        if (!tbl || !g) { lose('charts and diagrams'); continue; }
        const cols = Array.from(tbl.getElementsByTagNameNS(A, 'gridCol')).map(c => +attr(c, 'w') / EMU);
        const rows = [];
        for (const tr of kidsOf(tbl, A, 'tr')) {
          const cells = [];
          for (const tc of kidsOf(tr, A, 'tc')) {
            if (attr(tc, 'hMerge') === '1' || attr(tc, 'vMerge') === '1') continue;
            const t = kid(tc, A, 'txBody') ? await textOf(kid(tc, A, 'txBody'), { lists: [ctx.txStyles.other] }, ctx, rels) : { paras: [] };
            const fill = fillOf(kid(tc, A, 'tcPr'), ctx);
            cells.push({ paras: t.paras, span: +attr(tc, 'gridSpan') || 1, rspan: +attr(tc, 'rowSpan') || 1, fill: fill || undefined });
          }
          rows.push({ cells });
        }
        out.push({ x: g.x, y: g.y, w: g.w, h: g.h, rot: 0, table: { cols, rows }, size: 14 });
      }
    }
  }

  // backgrounds: the slide's, else the layout's, else the master's
  const bgOf = async (cSld, ctx, rels) => {
    const bg = kid(cSld, P, 'bg');
    if (!bg) return undefined;
    const pr = kid(bg, P, 'bgPr');
    if (pr) {
      const f = fillOf(pr, ctx);
      if (f) return { color: f };
      const bf = kid(pr, A, 'blipFill');
      if (bf) { const b = kid(bf, A, 'blip'); const src = b && await picture(rels, b.getAttributeNS(R, 'embed')); if (src) return { img: src }; }
      return undefined;
    }
    const ref = kid(bg, P, 'bgRef');
    return ref ? { color: colorIn(ref, ctx) } : undefined;
  };

  for (const sid of kidsOf(kid(pr, P, 'sldIdLst'), P, 'sldId')) {
    const rel = prels[sid.getAttributeNS(R, 'id')];
    if (!rel) continue;
    const sp = rel.target, sx = await xml(sp);
    if (!sx) continue;
    if (attr(sx.documentElement, 'show') === '0') lose('hidden slides (shown here)');
    const srels = await relsOf(sp);
    let layoutPath = null;
    for (const id in srels) if (srels[id].type === 'slideLayout') layoutPath = srels[id].target;
    const lx = layoutPath && await xml(layoutPath), lrels = layoutPath ? await relsOf(layoutPath) : {};
    let masterPath = null;
    for (const id in lrels) if (lrels[id].type === 'slideMaster') masterPath = lrels[id].target;
    const mx = masterPath && await xml(masterPath), mrels = masterPath ? await relsOf(masterPath) : {};
    const theme = await themeOf(masterPath || '');
    const map = {};
    const cm = mx && kid(mx.documentElement, P, 'clrMap');
    for (const a of cm ? Array.from(cm.attributes) : []) map[a.name] = a.value;
    const ov = kid(kid(sx.documentElement, P, 'clrMapOvr'), A, 'overrideClrMapping');
    for (const a of ov ? Array.from(ov.attributes) : []) map[a.name] = a.value;
    const ts = mx && kid(mx.documentElement, P, 'txStyles');
    const ctx = {
      theme, map,
      layoutTree: lx ? lx.getElementsByTagNameNS(P, 'spTree')[0] : null,
      masterTree: mx ? mx.getElementsByTagNameNS(P, 'spTree')[0] : null,
      txStyles: { title: kid(ts, P, 'titleStyle'), body: kid(ts, P, 'bodyStyle'), other: kid(ts, P, 'otherStyle') },
    };
    const sCsld = kid(sx.documentElement, P, 'cSld');
    const bg = await bgOf(sCsld, ctx, srels) || (lx && await bgOf(kid(lx.documentElement, P, 'cSld'), ctx, lrels)) || (mx && await bgOf(kid(mx.documentElement, P, 'cSld'), ctx, mrels)) || { color: '#ffffff' };
    const slide = { bg: bg.color || null, shapes: [], under: [] };
    if (bg.img) slide.under.push({ x: 0, y: 0, w: deck.w, h: deck.h, img: bg.img });
    if (mx && attr(lx && lx.documentElement, 'showMasterSp') !== '0' && attr(sx.documentElement, 'showMasterSp') !== '0') await tree(ctx.masterTree, ctx, mrels, slide.under, 'master');
    if (lx && attr(sx.documentElement, 'showMasterSp') !== '0') await tree(ctx.layoutTree, ctx, lrels, slide.under, 'layout');
    await tree(sCsld && kid(sCsld, P, 'spTree'), ctx, srels, slide.shapes, 'slide');
    if (sx.getElementsByTagNameNS(P, 'timing').length && sx.getElementsByTagNameNS(P, 'par').length) lost.animations = 1;
    deck.slides.push(slide);
  }
  return deck;
}

/* ---------------- writing ---------------- */
const xe = s => String(s).replace(/[\x00-\x08\x0B\x0C\x0E-\x1F￾￿]/g, '').replace(/[&<>"]/g, c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : '&quot;'));
const emu = v => Math.round(v * EMU);
const NSX = 'xmlns:a="' + A + '" xmlns:r="' + R + '" xmlns:p="' + P + '"';
const HEAD = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n';
const MIME_EXT = { 'image/png': 'png', 'image/jpeg': 'jpeg', 'image/gif': 'gif', 'image/bmp': 'bmp', 'image/svg+xml': 'svg', 'image/webp': 'webp' };

const THEME = HEAD + '<a:theme ' + NSX + ' name="osada"><a:themeElements><a:clrScheme name="osada">' +
  '<a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>' +
  '<a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1>' +
  '<a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4>' +
  '<a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink>' +
  '<a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="osada"><a:majorFont><a:latin typeface="Calibri Light"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont>' +
  '<a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="osada">' +
  '<a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst>' +
  '<a:lnStyleLst><a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="12700"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="19050"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst>' +
  '<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>' +
  '<a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst>' +
  '</a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>';

const lvlStyles = (size0, bullets) => Array.from({ length: 9 }, (_, i) => '<a:lvl' + (i + 1) + 'pPr marL="' + (bullets ? 228600 * (i + 1) : 0) + '" indent="' + (bullets ? -228600 : 0) +
  '" algn="l" defTabSz="914400" rtl="0" eaLnBrk="1" latinLnBrk="0" hangingPunct="1">' + (bullets ? '<a:buFont typeface="Arial"/><a:buChar char="•"/>' : '<a:buNone/>') +
  '<a:defRPr sz="' + Math.max(1000, size0 - i * 200) + '" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/><a:cs typeface="+mn-cs"/></a:defRPr></a:lvl' + (i + 1) + 'pPr>').join('');
const GROUP = '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>';
const MASTER = HEAD + '<p:sldMaster ' + NSX + '><p:cSld><p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg><p:spTree>' + GROUP + '</p:spTree></p:cSld>' +
  '<p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/>' +
  '<p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle>' + lvlStyles(4400, false).replace('<a:latin typeface="+mn-lt"/>', '<a:latin typeface="+mj-lt"/>') +
  '</p:titleStyle><p:bodyStyle>' + lvlStyles(2800, true) + '</p:bodyStyle><p:otherStyle>' + lvlStyles(1800, false) + '</p:otherStyle></p:txStyles></p:sldMaster>';
const LAYOUT = HEAD + '<p:sldLayout ' + NSX + ' type="blank" preserve="1"><p:cSld name="Blank"><p:spTree>' + GROUP + '</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>';

async function write(deck) {
  const media = [];
  const mediaOf = new Map();
  const slides = [];
  for (const s of deck.slides) {
    const rels = ['<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>'];
    const rel = (type, target, ext) => { const id = 'rId' + (rels.length + 1); rels.push('<Relationship Id="' + id + '" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/' + type + '" Target="' + xe(target) + '"' + (ext ? ' TargetMode="External"' : '') + '/>'); return id; };
    const links = new Map();
    const linkId = u => { if (!links.has(u)) links.set(u, rel('hyperlink', u, true)); return links.get(u); };
    const imageId = src => {
      const d = Rich.dataBytes(src);
      if (!d || !MIME_EXT[d.type]) return null;
      let name = mediaOf.get(src);
      if (!name) { name = 'image' + (media.length + 1) + '.' + MIME_EXT[d.type]; media.push({ name, bytes: d.bytes }); mediaOf.set(src, name); }
      return rel('image', '../media/' + name);
    };
    let id = 2, sp = '';
    const xfrm = sh => '<a:xfrm' + (sh.rot ? ' rot="' + Math.round(sh.rot * 60000) + '"' : '') + (sh.flipH ? ' flipH="1"' : '') + (sh.flipV ? ' flipV="1"' : '') +
      '><a:off x="' + emu(sh.x) + '" y="' + emu(sh.y) + '"/><a:ext cx="' + emu(Math.max(1, sh.w)) + '" cy="' + emu(Math.max(1, sh.h)) + '"/></a:xfrm>';
    const fillX = c => (c ? '<a:solidFill><a:srgbClr val="' + c.slice(1).toUpperCase() + '"/></a:solidFill>' : '<a:noFill/>');
    const lnX = l => (l && l.color ? '<a:ln w="' + emu(l.w || 1.33) + '">' + fillX(l.color) + '</a:ln>' : '<a:ln><a:noFill/></a:ln>');
    const parasX = (ps, size) => (ps && ps.length ? ps : [{ runs: [] }]).map(p => {
      const pPr = '<a:pPr' + (p.list ? ' marL="' + 342900 * ((p.level || 0) + 1) + '" indent="-342900"' : ' marL="0" indent="0"') + (p.level ? ' lvl="' + Math.min(8, p.level) + '"' : '') +
        (p.align ? ' algn="' + { center: 'ctr', right: 'r', justify: 'just' }[p.align] + '"' : '') + '>' +
        (p.list === 'ol' ? '<a:buFont typeface="+mj-lt"/><a:buAutoNum type="arabicPeriod"/>' : p.list === 'ul' ? '<a:buFont typeface="Arial"/><a:buChar char="•"/>' : '<a:buNone/>') + '</a:pPr>';
      let rs = '';
      for (const r of p.runs || []) {
        if (r.img) continue;
        for (const part of (r.t || '').split('\n').map((x, i) => [x, i])) {
          if (part[1] > 0) rs += '<a:br><a:rPr lang="en-US" sz="' + Math.round(((r.f && r.f.size) || size * 2) * 50) + '"/></a:br>';
          if (!part[0]) continue;
          const f = r.f || {};
          rs += '<a:r><a:rPr lang="en-US" sz="' + Math.round((f.size || size * 2) * 50) + '"' + (f.b ? ' b="1"' : '') + (f.i ? ' i="1"' : '') + (f.u ? ' u="sng"' : '') +
            (f.s ? ' strike="sngStrike"' : '') + (f.sup ? ' baseline="30000"' : f.sub ? ' baseline="-25000"' : '') + ' dirty="0">' + (f.color ? fillX(f.color) : '') +
            (f.font ? '<a:latin typeface="' + xe(f.font) + '"/>' : '') + (f.href ? '<a:hlinkClick r:id="' + linkId(f.href) + '"/>' : '') + '</a:rPr><a:t>' + xe(part[0].replace(/\t/g, '    ')) + '</a:t></a:r>';
        }
      }
      return '<a:p>' + pPr + rs + '<a:endParaRPr lang="en-US" sz="' + Math.round(size * 100) + '" dirty="0"/></a:p>';
    }).join('');
    const body = sh => '<p:txBody><a:bodyPr wrap="square" lIns="91440" tIns="45720" rIns="91440" bIns="45720" rtlCol="0" anchor="' + ({ ctr: 'ctr', b: 'b' }[sh.anchor] || 't') + '"><a:noAutofit/></a:bodyPr><a:lstStyle/>' + parasX(sh.paras, sh.size || 18) + '</p:txBody>';
    for (const sh of (s.under || []).concat(s.shapes)) {
      const n = id++;
      if (sh.img) {
        const rid = imageId(sh.img);
        if (!rid) continue;
        sp += '<p:pic><p:nvPicPr><p:cNvPr id="' + n + '" name="Picture ' + n + '"/><p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="' + rid +
          '"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>' + xfrm(sh) + '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>';
      } else if (sh.geom === 'line') {
        sp += '<p:cxnSp><p:nvCxnSpPr><p:cNvPr id="' + n + '" name="Line ' + n + '"/><p:cNvCxnSpPr/><p:nvPr/></p:nvCxnSpPr><p:spPr>' + xfrm(sh) + '<a:prstGeom prst="line"><a:avLst/></a:prstGeom>' + lnX(sh.line) + '</p:spPr></p:cxnSp>';
      } else if (sh.table) {
        const cols = sh.table.cols && sh.table.cols.length ? sh.table.cols : [];
        const ncols = Math.max(cols.length, ...sh.table.rows.map(r => r.cells.reduce((a, c) => a + (c.span || 1), 0)));
        const cw = Array.from({ length: ncols }, (_, i) => cols[i] || sh.w / ncols);
        let t = '<a:tbl><a:tblPr firstRow="1" bandRow="1"/><a:tblGrid>' + cw.map(w => '<a:gridCol w="' + emu(w) + '"/>').join('') + '</a:tblGrid>';
        const rh = sh.h / Math.max(1, sh.table.rows.length);
        const below = [];   // vertical merges still running, by column
        for (const row of sh.table.rows) {
          t += '<a:tr h="' + emu(rh) + '">';
          let col = 0;
          const cells = row.cells.slice();
          const emit = c => '<a:tc' + (c.span > 1 ? ' gridSpan="' + c.span + '"' : '') + (c.rspan > 1 ? ' rowSpan="' + c.rspan + '"' : '') + '><a:txBody><a:bodyPr/><a:lstStyle/>' + parasX(c.paras, sh.size || 14) +
            '</a:txBody><a:tcPr>' + (c.fill ? fillX(c.fill) : '') + '</a:tcPr></a:tc>';
          while (col < ncols) {
            if (below[col] && below[col].left > 0) { below[col].left--; t += '<a:tc vMerge="1"' + (below[col].span > 1 ? ' gridSpan="' + below[col].span + '"' : '') + '><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr lang="en-US"/></a:p></a:txBody><a:tcPr/></a:tc>'; for (let k = 1; k < below[col].span; k++) t += '<a:tc hMerge="1"><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr lang="en-US"/></a:p></a:txBody><a:tcPr/></a:tc>'; col += below[col].span; continue; }
            const c = cells.shift() || { paras: [], span: 1, rspan: 1 };
            t += emit(c);
            for (let k = 1; k < (c.span || 1); k++) t += '<a:tc hMerge="1"><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr lang="en-US"/></a:p></a:txBody><a:tcPr/></a:tc>';
            if (c.rspan > 1) below[col] = { left: c.rspan - 1, span: c.span || 1 };
            col += c.span || 1;
          }
          t += '</a:tr>';
        }
        t += '</a:tbl>';
        sp += '<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="' + n + '" name="Table ' + n + '"/><p:cNvGraphicFramePr><a:graphicFrameLocks noGrp="1"/></p:cNvGraphicFramePr><p:nvPr/></p:nvGraphicFramePr>' +
          '<p:xfrm><a:off x="' + emu(sh.x) + '" y="' + emu(sh.y) + '"/><a:ext cx="' + emu(sh.w) + '" cy="' + emu(sh.h) + '"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table">' + t + '</a:graphicData></a:graphic></p:graphicFrame>';
      } else {
        const prst = { ellipse: 'ellipse', roundRect: 'roundRect' }[sh.geom] || 'rect';
        sp += '<p:sp><p:nvSpPr><p:cNvPr id="' + n + '" name="' + (sh.geom ? 'Shape ' : 'TextBox ') + n + '"/><p:cNvSpPr' + (sh.geom ? '' : ' txBox="1"') + '/><p:nvPr/></p:nvSpPr><p:spPr>' + xfrm(sh) +
          '<a:prstGeom prst="' + prst + '"><a:avLst/></a:prstGeom>' + fillX(sh.fill) + lnX(sh.line) + '</p:spPr>' + body(sh) + '</p:sp>';
      }
    }
    const bg = s.bg ? '<p:bg><p:bgPr><a:solidFill><a:srgbClr val="' + s.bg.slice(1).toUpperCase() + '"/></a:solidFill><a:effectLst/></p:bgPr></p:bg>' : '';
    slides.push({
      xml: HEAD + '<p:sld ' + NSX + '><p:cSld>' + bg + '<p:spTree>' + GROUP + sp + '</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>',
      rels: HEAD + '<Relationships xmlns="' + PR + '">' + rels.join('') + '</Relationships>',
    });
  }
  const n = slides.length;
  const presXml = HEAD + '<p:presentation ' + NSX + ' saveSubsetFonts="1"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>' +
    slides.map((s, i) => '<p:sldId id="' + (256 + i) + '" r:id="rId' + (10 + i) + '"/>').join('') + '</p:sldIdLst><p:sldSz cx="' + emu(deck.w) + '" cy="' + emu(deck.h) + '"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>';
  const presRels = HEAD + '<Relationships xmlns="' + PR + '">' +
    '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/>' +
    '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="theme/theme1.xml"/>' +
    '<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/presProps" Target="presProps.xml"/>' +
    '<Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/viewProps" Target="viewProps.xml"/>' +
    '<Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/tableStyles" Target="tableStyles.xml"/>' +
    slides.map((s, i) => '<Relationship Id="rId' + (10 + i) + '" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide' + (i + 1) + '.xml"/>').join('') + '</Relationships>';
  const ct = 'application/vnd.openxmlformats-officedocument.presentationml.';
  const types = HEAD + '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>' +
    '<Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="jpeg" ContentType="image/jpeg"/><Default Extension="gif" ContentType="image/gif"/>' +
    '<Default Extension="bmp" ContentType="image/bmp"/><Default Extension="svg" ContentType="image/svg+xml"/><Default Extension="webp" ContentType="image/webp"/>' +
    '<Override PartName="/ppt/presentation.xml" ContentType="' + ct + 'presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="' + ct + 'slideMaster+xml"/>' +
    '<Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="' + ct + 'slideLayout+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>' +
    '<Override PartName="/ppt/presProps.xml" ContentType="' + ct + 'presProps+xml"/><Override PartName="/ppt/viewProps.xml" ContentType="' + ct + 'viewProps+xml"/>' +
    '<Override PartName="/ppt/tableStyles.xml" ContentType="' + ct + 'tableStyles+xml"/>' +
    slides.map((s, i) => '<Override PartName="/ppt/slides/slide' + (i + 1) + '.xml" ContentType="' + ct + 'slide+xml"/>').join('') +
    '<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>';
  const rootRels = HEAD + '<Relationships xmlns="' + PR + '"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>' +
    '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>' +
    '<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>';
  const now = new Date().toISOString().replace(/\.\d+Z$/, 'Z');
  const core = HEAD + '<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">' +
    '<dcterms:created xsi:type="dcterms:W3CDTF">' + now + '</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">' + now + '</dcterms:modified></cp:coreProperties>';
  const app = HEAD + '<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>osada</Application><Slides>' + n + '</Slides></Properties>';
  const files = [await ZIP.add('[Content_Types].xml', types), await ZIP.add('_rels/.rels', rootRels), await ZIP.add('docProps/core.xml', core), await ZIP.add('docProps/app.xml', app),
    await ZIP.add('ppt/presentation.xml', presXml), await ZIP.add('ppt/_rels/presentation.xml.rels', presRels),
    await ZIP.add('ppt/slideMasters/slideMaster1.xml', MASTER),
    await ZIP.add('ppt/slideMasters/_rels/slideMaster1.xml.rels', HEAD + '<Relationships xmlns="' + PR + '"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>'),
    await ZIP.add('ppt/slideLayouts/slideLayout1.xml', LAYOUT),
    await ZIP.add('ppt/slideLayouts/_rels/slideLayout1.xml.rels', HEAD + '<Relationships xmlns="' + PR + '"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>'),
    await ZIP.add('ppt/theme/theme1.xml', THEME),
    await ZIP.add('ppt/presProps.xml', HEAD + '<p:presentationPr ' + NSX + '/>'),
    await ZIP.add('ppt/viewProps.xml', HEAD + '<p:viewPr ' + NSX + '><p:normalViewPr><p:restoredLeft sz="15620"/><p:restoredTop sz="94660"/></p:normalViewPr><p:gridSpacing cx="76200" cy="76200"/></p:viewPr>'),
    await ZIP.add('ppt/tableStyles.xml', HEAD + '<a:tblStyleLst xmlns:a="' + A + '" def="{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}"/>')];
  for (const [i, s] of slides.entries()) {
    files.push(await ZIP.add('ppt/slides/slide' + (i + 1) + '.xml', s.xml));
    files.push(await ZIP.add('ppt/slides/_rels/slide' + (i + 1) + '.xml.rels', s.rels));
  }
  for (const m of media) files.push(await ZIP.add('ppt/media/' + m.name, m.bytes, true));
  return ZIP.write(files);
}

const PPTX = { parse, write };
return { PPTX, readDeck: parse };
});
