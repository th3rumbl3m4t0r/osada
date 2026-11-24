/* PowerPoint 97-2003 presentations (.ppt, .pps, .pot): read into the deck model (slides.js), to
   be shown and edited as .odp. Part msppt, on cfb, rich and slides. The latest edit through
   the persist directory; each slide's drawing: shapes with their anchors (in groups too),
   fill and line colours (through the colour scheme), rotation; text from the shape or the
   slide's outline, with bullets, alignment, bold, italic, underline, size, colour and font
   from StyleTextPropAtom; pictures from the Pictures stream; the master's background and
   decorations. Other shapes become rectangles; charts and embedded objects are lost. */
'use strict';

part('msppt', ({ CFB, Rich }) => {

const u16 = (d, o) => d[o] | (d[o + 1] << 8);
const i16 = (d, o) => (u16(d, o) << 16) >> 16;
const u32 = (d, o) => (d[o] | (d[o + 1] << 8) | (d[o + 2] << 16) | (d[o + 3] << 24)) >>> 0;
const i32 = (d, o) => d[o] | (d[o + 1] << 8) | (d[o + 2] << 16) | (d[o + 3] << 24);
const head = (d, o) => ({ ver: u16(d, o) & 0xF, inst: u16(d, o) >> 4, type: u16(d, o + 2), len: u32(d, o + 4), o, body: o + 8, end: o + 8 + u32(d, o + 4) });
function recs(d, from, to) {
  const out = [];
  for (let o = from; o + 8 <= Math.min(to, d.length);) { const r = head(d, o); out.push(r); o = r.end; }
  return out;
}
const kidsOf = (d, r, type) => recs(d, r.body, r.end).filter(x => type === undefined || x.type === type);
const kidOf = (d, r, type) => kidsOf(d, r, type)[0] || null;
const hex = (r, g, b) => '#' + [r, g, b].map(x => x.toString(16).padStart(2, '0')).join('');
const MU = 6;   // master units (576 per inch) per px (96 per inch)

function parse(bytes) {
  const cf = CFB.read(bytes);
  const doc = cf.get('PowerPoint Document');
  if (!doc) throw new Error('not a PowerPoint presentation (no PowerPoint Document stream)');
  const cu = cf.get('Current User'), pics = cf.get('Pictures');
  const lost = {};
  const lose = w => { lost[w] = (lost[w] || 0) + 1; };
  if (cu && u32(cu, 12) === 0xF3D1C4DF) throw new Error('password-protected presentations cannot be opened here');

  // the persist directory: persist id -> offset, newest edit first
  const where = new Map();
  let docRef = 0;
  {
    let edit = cu && cu.length >= 20 ? u32(cu, 16) : -1;
    const seen = new Set();
    while (edit >= 0 && edit + 8 <= doc.length && !seen.has(edit)) {
      seen.add(edit);
      const r = head(doc, edit);
      if (r.type !== 0x0FF5) break;
      if (!docRef) docRef = u32(doc, r.body + 16);
      const pd = head(doc, u32(doc, r.body + 12));
      for (let o = pd.body; o + 4 <= pd.end;) {
        const e = u32(doc, o), id0 = e & 0xFFFFF, n = e >>> 20;
        for (let k = 0; k < n; k++) if (!where.has(id0 + k)) where.set(id0 + k, u32(doc, o + 4 + k * 4));
        o += 4 + n * 4;
      }
      const prev = u32(doc, r.body + 8);
      edit = prev ? prev : -1;
    }
  }
  if (!where.has(docRef)) throw new Error('PowerPoint document: no document container');
  const D = head(doc, where.get(docRef));
  const da = kidOf(doc, D, 0x03E9);
  const deck = { w: da ? Math.round(i32(doc, da.body) / MU) : 960, h: da ? Math.round(i32(doc, da.body + 4) / MU) : 720, slides: [], lost };

  // fonts
  const fonts = [];
  const env = kidOf(doc, D, 0x03F2), fc = env && kidOf(doc, env, 0x07D5);
  if (fc) for (const fe of kidsOf(doc, fc, 0x0FB7)) { let s = ''; for (let k = 0; k < 32; k++) { const c = u16(doc, fe.body + k * 2); if (!c) break; s += String.fromCharCode(c); } fonts.push(s); }

  // pictures: the BLIP store
  const bses = [];
  const dg = kidOf(doc, D, 0x040B);
  const walkStore = (d, from, to) => {
    for (const r of recs(d, from, to)) {
      if (r.type === 0xF007) {
        const cbName = d[r.body + 33];
        bses.push(r.len > 36 + cbName ? { d, at: r.body + 36 + cbName } : pics ? { d: pics, at: u32(d, r.body + 28) } : null);
      } else if (r.ver === 0xF) walkStore(d, r.body, r.end);
    }
  };
  if (dg) walkStore(doc, dg.body, dg.end);
  const blip = (d, r0) => {
    if (!d || r0 + 8 > d.length) return null;
    const r = head(d, r0), two = { 0xF01E: 0x6E1, 0xF01D: 0x46B, 0xF02A: 0x6E3, 0xF01F: 0x7A9 }[r.type] === r.inst;
    const img = d.subarray(r.body + 17 + (two ? 16 : 0), Math.min(r.end, d.length));
    if (r.type === 0xF01E) return Rich.dataUrl('image/png', img);
    if (r.type === 0xF01D || r.type === 0xF02A) return Rich.dataUrl('image/jpeg', img);
    if (r.type === 0xF01F) {
      const bmp = new Uint8Array(14 + img.length), hs = u32(img, 0), bpp = u16(img, 14), clr = u32(img, 32) || (bpp <= 8 ? 1 << bpp : 0);
      bmp.set([0x42, 0x4D]);
      new DataView(bmp.buffer).setUint32(2, bmp.length, true);
      new DataView(bmp.buffer).setUint32(10, 14 + hs + clr * 4, true);
      bmp.set(img, 14);
      return Rich.dataUrl('image/bmp', bmp);
    }
    lose('pictures in other formats');
    return null;
  };
  const picture = pib => { const b = bses[pib - 1]; return b ? blip(b.d, b.at) : null; };

  // slides and masters in their lists: persist id, slide id, and the texts that go with them
  const lists = { 0: [], 1: [] };
  for (const sl of kidsOf(doc, D, 0x0FF0)) {
    if (sl.inst !== 0 && sl.inst !== 1) continue;
    let cur = null;
    for (const r of recs(doc, sl.body, sl.end)) {
      if (r.type === 0x03F3) { cur = { ref: u32(doc, r.body), id: u32(doc, r.body + 12), texts: [] }; lists[sl.inst].push(cur); }
      else if (cur && r.type === 0x0F9F) cur.texts.push({ type: u32(doc, r.body), text: '', style: null });
      else if (cur && cur.texts.length && (r.type === 0x0FA0 || r.type === 0x0FA8)) cur.texts[cur.texts.length - 1].text = textOf(r);
      else if (cur && cur.texts.length && r.type === 0x0FA1) cur.texts[cur.texts.length - 1].style = r;
    }
  }
  function textOf(r) {
    if (r.type === 0x0FA0) { let s = ''; for (let k = r.body; k + 1 < r.end; k += 2) s += String.fromCharCode(u16(doc, k)); return s; }
    return new TextDecoder('windows-1252').decode(doc.subarray(r.body, r.end));
  }

  // a colour: RGB, or an index into the scheme
  const colorRef = (v, scheme) => {
    const r = v & 0xFF, g = (v >> 8) & 0xFF, b = (v >> 16) & 0xFF, fl = v >>> 24;
    if (fl & 0x08) return scheme[r] || null;
    if (fl & 0x10) return null;
    return hex(r, g, b);
  };
  const schemeOf = cont => {
    const cs = cont && kidsOf(doc, cont, 0x07F0).find(x => x.inst === 1);
    if (!cs) return null;
    return Array.from({ length: 8 }, (_, k) => hex(doc[cs.body + k * 4], doc[cs.body + k * 4 + 1], doc[cs.body + k * 4 + 2]));
  };

  /* StyleTextPropAtom: paragraph and character runs over the text */
  function styled(text, st, scheme, type) {
    const paras = [];
    const lines = text.replace(/\r$/, '').split('\r');
    const n = text.length + 1;
    const pr = [], cr = [];
    if (st) {
      let o = st.body;
      try {
        for (let got = 0; got < n && o + 6 <= st.end;) {
          const cnt = u32(doc, o), lvl = u16(doc, o + 4), m = u32(doc, o + 6);
          o += 10;
          const p = { cnt, lvl };
          if (m & 0x000F) { p.bullet = !!(u16(doc, o) & 1); o += 2; }
          if (m & 0x0080) o += 2;
          if (m & 0x0010) o += 2;
          if (m & 0x0040) o += 2;
          if (m & 0x0020) o += 4;
          if (m & 0x0800) { p.align = u16(doc, o); o += 2; }
          if (m & 0x1000) o += 2;
          if (m & 0x2000) o += 2;
          if (m & 0x4000) o += 2;
          if (m & 0x0100) o += 2;
          if (m & 0x0400) o += 2;
          if (m & 0x8000) o += 2;
          if (m & 0x100000) { const c = u16(doc, o); o += 2 + c * 4; }
          if (m & 0x10000) o += 2;
          if (m & 0xE0000) o += 2;
          if (m & 0x200000) o += 2;
          pr.push(p);
          got += cnt;
        }
        for (let got = 0; got < n && o + 8 <= st.end;) {
          const cnt = u32(doc, o), m = u32(doc, o + 4);
          o += 8;
          const c = { cnt };
          if (m & 0xFFFF) { const fs = u16(doc, o); c.b = !!(fs & 1); c.i = !!(fs & 2); c.u = !!(fs & 4); o += 2; }
          if (m & 0x10000) { c.font = fonts[u16(doc, o)]; o += 2; }
          if (m & 0x200000) o += 2;
          if (m & 0x400000) o += 2;
          if (m & 0x800000) o += 2;
          if (m & 0x20000) { c.size = u16(doc, o); o += 2; }
          if (m & 0x40000) { const idx = doc[o + 3]; c.color = idx === 0xFE ? hex(doc[o], doc[o + 1], doc[o + 2]) : scheme[idx] || null; o += 4; }
          if (m & 0x80000) { const pos = i16(doc, o); c.sup = pos > 0; c.sub = pos < 0; o += 2; }
          if (m & 0x1000000) o += 2;
          if (m & 0x2000000) o += 2;
          if (m & 0x4000000) o += 4;
          cr.push(c);
          got += cnt;
        }
      } catch (e) { /* past the record: what was read is kept */ }
    }
    // walk the text: paragraph runs give the paragraph, character runs the formatting
    let at = 0, pi = 0, pLeft = pr[0] ? pr[0].cnt : Infinity, ci = 0, cLeft = cr[0] ? cr[0].cnt : Infinity;
    for (const line of lines) {
      const p = pr[pi] || {};
      const b = { runs: [], align: ['', 'center', 'right', 'justify'][p.align] || '' };
      if (p.bullet && type !== 0 && type !== 6) { b.list = 'ul'; b.level = p.lvl || 0; }
      let cur = null;
      for (const ch of line) {
        const c = cr[ci] || {};
        const f = { b: c.b, i: c.i, u: c.u, size: c.size ? c.size * 2 : undefined, color: c.color, font: c.font, sup: c.sup, sub: c.sub };
        if (cur && cur.ci === ci) cur.t += ch === '\x0B' ? '\n' : ch;
        else { cur = { t: ch === '\x0B' ? '\n' : ch, f, ci }; b.runs.push(cur); }
        at++;
        if (--cLeft <= 0 && ci < cr.length - 1) { ci++; cLeft = cr[ci].cnt; }
        if (--pLeft <= 0 && pi < pr.length - 1) { pi++; pLeft = pr[pi].cnt; }
      }
      // the paragraph mark
      at++;
      if (--cLeft <= 0 && ci < cr.length - 1) { ci++; cLeft = cr[ci].cnt; }
      if (--pLeft <= 0 && pi < pr.length - 1) { pi++; pLeft = pr[pi].cnt; }
      b.runs.forEach(r => { delete r.ci; for (const k in r.f) if (r.f[k] === undefined || r.f[k] === false) delete r.f[k]; });
      paras.push(b);
    }
    return paras;
  }

  const SIZE = { 0: 44, 6: 44, 1: 28, 5: 32, 7: 24, 8: 20 };   // by text type: title, centre title, body, centre body, half and quarter body
  /* the shapes of a drawing (a slide's or a master's); placeholders: skipped for masters */
  function drawing(cont, slideTexts, scheme, master) {
    const out = [], under = [];
    let bg = null;
    const ppd = kidOf(doc, cont, 0x040C);
    const dgc = ppd && kidOf(doc, ppd, 0xF002);
    if (!dgc) return { shapes: out, bg };
    const shape = (sc, tf) => {
      let fsp = null, opt = {}, anchor = null, child = null, ph = null, tb = null, grp = null;
      for (const r of recs(doc, sc.body, sc.end)) {
        if (r.type === 0xF00A) fsp = { type: r.inst, flags: u32(doc, r.body + 4) };
        else if (r.type === 0xF00B || r.type === 0xF122) for (let k = 0; k < r.inst; k++) { const id = u16(doc, r.body + k * 6) & 0x3FFF; opt[id] = u32(doc, r.body + k * 6 + 2); }
        else if (r.type === 0xF010) anchor = r.len >= 16 ? [i32(doc, r.body), i32(doc, r.body + 4), i32(doc, r.body + 8), i32(doc, r.body + 12)] : [i16(doc, r.body), i16(doc, r.body + 2), i16(doc, r.body + 4), i16(doc, r.body + 6)];
        else if (r.type === 0xF00F) child = [i32(doc, r.body), i32(doc, r.body + 4), i32(doc, r.body + 8), i32(doc, r.body + 12)];
        else if (r.type === 0xF009) grp = [i32(doc, r.body), i32(doc, r.body + 4), i32(doc, r.body + 8), i32(doc, r.body + 12)];
        else if (r.type === 0xF011) { const p = kidOf(doc, r, 0x0BC3); if (p) ph = doc[p.body + 4]; }
        else if (r.type === 0xF00D) tb = r;
      }
      // anchors: [top, left, right, bottom] on the slide, [left, top, right, bottom] in a group
      let x, y, w, hh;
      if (child && tf) { x = tf.x + (child[0] - tf.cx) * tf.kx; y = tf.y + (child[1] - tf.cy) * tf.ky; w = (child[2] - child[0]) * tf.kx; hh = (child[3] - child[1]) * tf.ky; }
      else if (anchor) { x = anchor[1] / MU; y = anchor[0] / MU; w = (anchor[2] - anchor[1]) / MU; hh = (anchor[3] - anchor[0]) / MU; }
      return { fsp, opt, x, y, w, h: hh, ph, tb, grp };
    };
    const walk = (spgr, tf, list) => {
      const items = recs(doc, spgr.body, spgr.end);
      let groupTf = tf;
      items.forEach((r, idx) => {
        if (r.type === 0xF003) { walk(r, groupTf, list); return; }
        if (r.type !== 0xF004) return;
        const s = shape(r, tf);
        if (!s.fsp) return;
        if (idx === 0 && s.grp) {
          // the group's own shape: how its children's coordinates map onto the slide
          if (s.x !== undefined && s.grp[2] !== s.grp[0] && s.grp[3] !== s.grp[1]) groupTf = { x: s.x, y: s.y, cx: s.grp[0], cy: s.grp[1], kx: s.w / (s.grp[2] - s.grp[0]), ky: s.h / (s.grp[3] - s.grp[1]) };
          else if (!tf) groupTf = { x: 0, y: 0, cx: 0, cy: 0, kx: 1 / MU, ky: 1 / MU };
          return;
        }
        if (s.fsp.flags & 0x400) {   // the background
          if (s.opt[0x0180] === 3 && s.opt[0x0186]) { const src = picture(s.opt[0x0186]); if (src) list.unshift({ x: 0, y: 0, w: deck.w, h: deck.h, img: src }); }
          else if (s.opt[0x0181] !== undefined) bg = colorRef(s.opt[0x0181], scheme);
          return;
        }
        if (s.fsp.flags & 0x08 || s.x === undefined) return;
        if (master && s.ph !== null && s.ph !== undefined) return;
        const sh = { x: s.x, y: s.y, w: s.w, h: s.h, rot: s.opt[0x0004] ? ((s.opt[0x0004] | 0) / 65536) : 0, flipH: !!(s.fsp.flags & 0x40), flipV: !!(s.fsp.flags & 0x80), geom: null };
        // fill and line: as the shape says when it says (its "use" bit), else drawn shapes have them and placeholders and text boxes not
        const drawn = (s.ph === null || s.ph === undefined) && s.fsp.type !== 202 && s.fsp.type !== 75 && s.fsp.type !== 0;
        const fb = s.opt[0x01BF], lb = s.opt[0x01FF];
        const filled = fb !== undefined && fb & 0x100000 ? !!(fb & 0x10) : drawn;
        const lined = lb !== undefined && lb & 0x80000 ? !!(lb & 0x08) : drawn;
        if (s.opt[0x0104] && s.fsp.type === 75) { sh.img = picture(s.opt[0x0104]); if (!sh.img) return; list.push(sh); return; }
        if (s.fsp.type === 20 || s.fsp.flags & 0x100) {
          sh.geom = 'line';
          sh.line = { color: colorRef(s.opt[0x01C0] === undefined ? 0x08000001 : s.opt[0x01C0], scheme) || '#000000', w: s.opt[0x01CB] ? s.opt[0x01CB] / 9525 : 1.33 };
          list.push(sh);
          return;
        }
        if (filled) sh.fill = colorRef(s.opt[0x0181] === undefined ? 0x08000004 : s.opt[0x0181], scheme);
        if (lined) sh.line = { color: colorRef(s.opt[0x01C0] === undefined ? 0x08000001 : s.opt[0x01C0], scheme) || '#000000', w: s.opt[0x01CB] ? s.opt[0x01CB] / 9525 : 1.33 };
        if (s.fsp.type === 3) sh.geom = 'ellipse';
        else if (s.fsp.type === 2) sh.geom = 'roundRect';
        else if (sh.fill || sh.line) { sh.geom = 'rect'; if (s.fsp.type !== 1 && s.fsp.type !== 202) lose('shapes drawn as rectangles'); }
        // the text: in the shape, or the slide's outline by index
        let text = null, style = null, type = 4;
        if (s.tb) {
          for (const r of recs(doc, s.tb.body, s.tb.end)) {
            if (r.type === 0x0F9F) type = u32(doc, r.body);
            else if (r.type === 0x0FA0 || r.type === 0x0FA8) text = textOf(r);
            else if (r.type === 0x0FA1) style = r;
            else if (r.type === 0x0F9E && slideTexts) { const t = slideTexts[i32(doc, r.body)]; if (t) { text = t.text; style = t.style; type = t.type; } }
          }
        }
        if (text !== null && text.replace(/[\r\s]/g, '')) {
          sh.paras = styled(text, style, scheme, type);
          sh.size = SIZE[type] || 18;
          sh.ph = type === 0 || type === 6 ? 'title' : type === 1 || type === 5 ? 'body' : null;
          sh.anchor = { 1: 'ctr', 2: 'b', 4: 'ctr', 5: 'b' }[s.opt[0x0087]] || (type === 0 || type === 6 ? 'ctr' : 't');
          const def = scheme[type === 0 || type === 6 ? 3 : 1];
          sh.paras.forEach(p => p.runs.forEach(r => { if (!r.f.color && def) r.f.color = def; }));
        } else if (!sh.geom) return;
        list.push(sh);
      });
    };
    const top = kidOf(doc, dgc, 0xF003);
    if (top) walk(top, null, out);
    for (const r of recs(doc, dgc.body, dgc.end)) if (r.type === 0xF004) { const s = shape(r, null); if (s.fsp && s.fsp.flags & 0x400 && s.opt[0x0181] !== undefined) bg = colorRef(s.opt[0x0181], scheme); }
    return { shapes: out, bg };
  }

  // masters by slide id
  const masters = new Map();
  for (const m of lists[1]) {
    if (!where.has(m.ref)) continue;
    const mc = head(doc, where.get(m.ref));
    if (mc.type !== 0x03F8) continue;
    const scheme = schemeOf(mc) || ['#ffffff', '#000000', '#808080', '#000000', '#bbe0e3', '#333399', '#009999', '#99cc00'];
    const dr = drawing(mc, null, scheme, true);
    masters.set(m.id, { scheme, bg: dr.bg || scheme[0], shapes: dr.shapes });
  }
  for (const sl of lists[0]) {
    if (!where.has(sl.ref)) continue;
    const sc = head(doc, where.get(sl.ref));
    if (sc.type !== 0x03EE) continue;
    const sa = kidOf(doc, sc, 0x03EF);
    const flags = sa ? u16(doc, sa.body + 20) : 7;
    const master = sa ? masters.get(u32(doc, sa.body + 12)) : null;
    const scheme = (!(flags & 2) && schemeOf(sc)) || (master && master.scheme) || ['#ffffff', '#000000', '#808080', '#000000', '#bbe0e3', '#333399', '#009999', '#99cc00'];
    const dr = drawing(sc, sl.texts, scheme, false);
    const s = { bg: (!(flags & 4) && dr.bg) || (master && master.bg) || scheme[0] || '#ffffff', shapes: dr.shapes, under: master && flags & 1 ? master.shapes : [] };
    const outline = sl.texts.filter(t => t.text.replace(/[\r\s]/g, ''));
    if (!s.shapes.length && outline.length) {
      // a slide whose text lives only in the outline
      outline.forEach((t, i) => s.shapes.push({ x: 60, y: 40 + i * 140, w: deck.w - 120, h: 120, paras: styled(t.text, t.style, scheme, t.type), size: SIZE[t.type] || 18, anchor: 't' }));
    }
    deck.slides.push(s);
  }
  if (cf.list.some(x => /^(VBA|Macros|_VBA_PROJECT)$/i.test(x.name))) deck.macros = true;
  return deck;
}

const PPT = { parse };
return { PPT, readDeck: async b => parse(b) };
});
