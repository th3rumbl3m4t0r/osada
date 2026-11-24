/* Word 97-2003 documents (.doc, .dot): read into the editor's blocks (rich.js), to be shown
   and edited as .odt. Part msdoc, on cfb and rich. The text comes from the piece table;
   paragraphs (alignment, indents, spacing, list, heading by style or outline level) from
   the PAPX pages and the stylesheet; bold, italic, underline, strike, super- and
   subscript, size, colour, highlight and font from the CHPX pages; tables from the cell
   and row marks; fields as their results (HYPERLINK as a link); inline PNG, JPEG and DIB
   pictures from the Data stream. Word 6/95 files give their text. Headers and footers,
   footnotes, comments and floating drawings are counted as lost. */
'use strict';

part('msdoc', ({ CFB, Rich }) => {

const u16 = (d, o) => d[o] | (d[o + 1] << 8);
const i16 = (d, o) => (u16(d, o) << 16) >> 16;
const u32 = (d, o) => (d[o] | (d[o + 1] << 8) | (d[o + 2] << 16) | (d[o + 3] << 24)) >>> 0;
const ICO = [null, '#000000', '#0000ff', '#00ffff', '#00ff00', '#ff00ff', '#ff0000', '#ffff00', '#ffffff', '#000080', '#008080', '#008000',
  '#800080', '#800000', '#808000', '#808080', '#c0c0c0'];

/* a grpprl as [sprm, operand offset, operand length] */
function sprms(d, o, end) {
  const out = [];
  while (o + 2 <= end) {
    const s = u16(d, o), spra = s >> 13;
    let p = o + 2, n;
    if (spra === 6) {
      if (s === 0xD608 || s === 0xC615) { n = u16(d, p) - 1; p += 2; if (s === 0xC615 && d[p - 2] === 255) n = 0; }
      else { n = d[p]; p += 1; }
    } else n = [1, 1, 2, 4, 2, 2, 0, 3][spra];
    if (p + n > end) break;
    out.push([s, p, n]);
    o = p + n;
  }
  return out;
}

const decoders = {};
function decode(bytes, enc) {
  try { return (decoders[enc] || (decoders[enc] = new TextDecoder(enc))).decode(bytes); } catch (e) { return String.fromCharCode.apply(null, Array.from(bytes)); }
}

function parse(bytes) {
  const cf = CFB.read(bytes);
  const wd = cf.get('WordDocument');
  if (!wd || (u16(wd, 0) !== 0xA5EC && u16(wd, 0) !== 0xA5DC)) throw new Error('not a Word document (no WordDocument stream)');
  const lost = {};
  const lose = what => { lost[what] = (lost[what] || 0) + 1; };
  const nFib = u16(wd, 2), flags = u16(wd, 0x0A);
  if (flags & 0x0100) throw new Error('password-protected documents cannot be opened here');
  const macros = cf.list.some(x => /^Macros$/i.test(x.name));
  if (nFib < 0x00C1) return old(wd, lost, macros);

  const tbl = cf.get(flags & 0x0200 ? '1Table' : '0Table');
  if (!tbl) throw new Error('Word document without its table stream');
  const data = cf.get('Data');
  const ccpText = u32(wd, 0x4C);
  if (u32(wd, 0x54)) lose('headers and footers');
  if (u32(wd, 0x50) || u32(wd, 0x60)) lose('footnotes');
  if (u32(wd, 0x5C)) lose('comments');
  const fc = i => [u32(wd, 0x9A + i * 8), u32(wd, 0x9E + i * 8)];

  // the piece table: which bytes hold which characters
  const [fcClx, lcbClx] = fc(33);
  let p = fcClx;
  while (p < fcClx + lcbClx && tbl[p] === 0x01) p += 3 + u16(tbl, p + 1);
  if (tbl[p] !== 0x02) throw new Error('Word document: no piece table');
  const lcb = u32(tbl, p + 1), n = (lcb - 4) / 12, base = p + 5;
  const pieces = [];
  for (let i = 0; i < n; i++) {
    const cp0 = u32(tbl, base + i * 4), cp1 = u32(tbl, base + (i + 1) * 4);
    const raw = u32(tbl, base + (n + 1) * 4 + i * 8 + 2), comp = !!(raw & 0x40000000);
    pieces.push({ cp0, cp1, comp, fc: comp ? (raw & 0x3FFFFFFF) / 2 : raw });
  }
  // the main text, and each character's place in the file (for its formatting)
  const text = [], fcs = [];
  for (const pc of pieces) {
    if (pc.cp0 >= ccpText) break;
    const end = Math.min(pc.cp1, ccpText);
    const len = end - pc.cp0;
    if (pc.comp) {
      const s = decode(wd.subarray(pc.fc, pc.fc + len), 'windows-1252');
      for (let i = 0; i < len; i++) { text.push(s[i] || ''); fcs.push(pc.fc + i); }
    } else {
      for (let i = 0; i < len; i++) { text.push(String.fromCharCode(u16(wd, pc.fc + i * 2))); fcs.push(pc.fc + i * 2); }
    }
  }

  // formatting runs by file offset, from the FKP pages
  const fkpRuns = (i, papx) => {
    const [o, l] = fc(i), runs = [];
    if (!l) return runs;
    const cnt = (l - 4) / 8;
    for (let k = 0; k < cnt; k++) {
      const pn = u32(tbl, o + (cnt + 1) * 4 + k * 4) & 0x3FFFFF, pg = pn * 512;
      if (pg + 512 > wd.length) continue;
      const crun = wd[pg + 511];
      for (let j = 0; j < crun; j++) {
        const a = u32(wd, pg + j * 4), b = u32(wd, pg + (j + 1) * 4);
        if (papx) {
          const bo = wd[pg + (crun + 1) * 4 + j * 13] * 2;
          let cb = wd[pg + bo], q = pg + bo + 1, size;
          if (cb === 0) { cb = wd[q]; q++; size = cb * 2; } else size = cb * 2 - 1;
          runs.push({ a, b, istd: u16(wd, q), g: [q + 2, q + size] });
        } else {
          const bo = wd[pg + (crun + 1) * 4 + j] * 2;
          runs.push({ a, b, g: bo ? [pg + bo + 1, pg + bo + 1 + wd[pg + bo]] : null });
        }
      }
    }
    return runs.sort((x, y) => x.a - y.a);
  };
  const chpx = fkpRuns(12, false), papx = fkpRuns(13, true);
  const find = (runs, f) => {
    let lo = 0, hi = runs.length - 1;
    while (lo <= hi) { const m = (lo + hi) >> 1; if (f < runs[m].a) hi = m - 1; else if (f >= runs[m].b) lo = m + 1; else return runs[m]; }
    return null;
  };

  // fonts
  const fonts = [];
  {
    const [o, l] = fc(15);
    if (l > 4) {
      const cnt = u16(tbl, o);
      let q = o + 4;
      for (let k = 0; k < cnt && q < o + l; k++) {
        const cb = tbl[q];
        let s = '';
        for (let c = q + 1 + 39; c + 1 < q + 1 + cb; c += 2) { const ch = u16(tbl, c); if (!ch) break; s += String.fromCharCode(ch); }
        fonts.push(s);
        q += cb + 1;
      }
    }
  }

  // character and paragraph properties from a grpprl, on top of what is given
  const charProps = (g, f, styleF) => {
    if (!g) return f;
    f = Object.assign({}, f);
    const tog = (v, cur, st) => (v === 0 ? false : v === 1 ? true : v === 0x80 ? !!st : v === 0x81 ? !st : cur);
    for (const [s, o] of sprms(g.d, g[0], g[1])) {
      const d = g.d, v = d[o];
      switch (s) {
        case 0x0835: f.b = tog(v, f.b, styleF && styleF.b); break;
        case 0x0836: f.i = tog(v, f.i, styleF && styleF.i); break;
        case 0x0837: case 0x2A53: f.s = tog(v, f.s, styleF && styleF.s); break;
        case 0x2A3E: f.u = v !== 0; break;
        case 0x4A43: f.size = u16(d, o); break;
        case 0x2A42: f.color = v > 1 ? ICO[v] || null : null; break;
        case 0x6870: f.color = d[o + 3] ? null : '#' + [d[o], d[o + 1], d[o + 2]].map(x => x.toString(16).padStart(2, '0')).join(''); if (f.color === '#000000') f.color = null; break;
        case 0x2A48: f.sup = v === 1; f.sub = v === 2; break;
        case 0x4A4F: f.font = fonts[u16(d, o)] || f.font; break;
        case 0x2A0C: f.bg = v ? ICO[v] || null : null; break;
        case 0x083C: f.hidden = tog(v, f.hidden, styleF && styleF.hidden); break;
        case 0x0800: f.del = v !== 0; break;
        case 0x0855: f.spec = v !== 0; break;
        case 0x6A03: f.pic = u32(d, o); break;
        case 0x0806: f.data = v !== 0; break;
      }
    }
    return f;
  };
  const paraProps = (g, pp) => {
    if (!g) return pp;
    pp = Object.assign({}, pp);
    for (const [s, o] of sprms(g.d, g[0], g[1])) {
      const d = g.d;
      switch (s) {
        case 0x2403: case 0x2461: pp.align = ['', 'center', 'right', 'justify', 'justify', 'justify'][d[o]] || ''; break;
        case 0x840F: case 0x845E: pp.li = i16(d, o); break;
        case 0x8411: case 0x8460: pp.fi = i16(d, o); break;
        case 0xA413: pp.sb = u16(d, o); break;
        case 0xA414: pp.sa = u16(d, o); break;
        case 0x460B: pp.ilfo = i16(d, o); break;
        case 0x260A: pp.ilvl = d[o]; break;
        case 0x2416: pp.inTable = d[o] !== 0; break;
        case 0x2417: pp.ttp = d[o] !== 0; break;
        case 0x6649: pp.itap = u32(d, o); if (pp.itap) pp.inTable = true; break;
        case 0x664A: break;
        case 0x2640: pp.outline = d[o]; break;
        case 0xD608: {
          // the row: its cells' edges, and how each is merged (TC80 flags)
          const itc = d[o];
          pp.edges = [];
          pp.merge = [];
          for (let c = 0; c <= itc; c++) pp.edges.push(i16(d, o + 1 + c * 2));
          for (let c = 0; c < itc; c++) {
            const fl = o + 1 + (itc + 1) * 2 + c * 20 < g[1] ? u16(d, o + 1 + (itc + 1) * 2 + c * 20) : 0;
            pp.merge.push({ h: !!(fl & 2), v: !!(fl & 0x20) && !(fl & 0x40) });
          }
          break;
        }
      }
    }
    return pp;
  };

  // styles: istd -> {f, pp, h}
  const styles = [];
  let defFont = null;
  {
    const [o, l] = fc(1);
    if (l > 2) {
      const cbStshi = u16(tbl, o), cstd = u16(tbl, o + 2), cbBase = u16(tbl, o + 4);
      defFont = cbStshi >= 18 ? fonts[u16(tbl, o + 2 + 12)] : null;
      const raw = [];
      let q = o + 2 + cbStshi;
      for (let k = 0; k < cstd && q + 2 <= o + l; k++) {
        const cb = u16(tbl, q);
        q += 2;
        if (!cb) { raw.push(null); continue; }
        const s = q, sti = u16(tbl, s) & 0x0FFF, stk = tbl[s + 2] & 0x0F, istdBase = u16(tbl, s + 2) >> 4, cupx = tbl[s + 4] & 0x0F;
        let r = s + cbBase;
        const nlen = u16(tbl, r);
        let name = '';
        for (let c = 0; c < nlen; c++) name += String.fromCharCode(u16(tbl, r + 2 + c * 2));
        r += 2 + (nlen + 1) * 2;
        const upx = [];
        for (let u = 0; u < cupx && r + 2 <= s + cb; u++) {
          if ((r - s) & 1) r++;
          const ul = u16(tbl, r);
          upx.push([r + 2, r + 2 + ul]);
          r += 2 + ul;
        }
        raw.push({ sti, stk, istdBase, name, upx });
        q += cb;
      }
      const T = d => ({ d: tbl, 0: d[0], 1: d[1] });
      const res = (i, depth) => {
        if (styles[i]) return styles[i];
        const s = raw[i];
        if (!s || depth > 20) return { f: { size: 20 }, pp: {}, h: 0 };
        const b = s.istdBase !== 0x0FFF && s.istdBase !== i ? res(s.istdBase, depth + 1) : { f: { size: 20 }, pp: {}, h: 0 };
        let f = b.f, pp = b.pp;
        if (s.stk === 1 && s.upx[0]) pp = paraProps(T([s.upx[0][0] + 2, s.upx[0][1]]), pp);   // the papx upx begins with the istd
        const cu = s.stk === 1 ? s.upx[1] : s.stk === 2 ? s.upx[0] : null;
        if (cu) f = charProps(T(cu), f, b.f);
        const h = s.sti >= 1 && s.sti <= 9 ? s.sti : /^heading \d$/i.test(s.name) ? +s.name.slice(-1) : b.h;
        return (styles[i] = { f, pp, h, name: s.name });
      };
      raw.forEach((s, i) => res(i, 0));
    }
  }
  const styleOf = i => styles[i] || styles[0] || { f: { size: 20 }, pp: {}, h: 0 };

  // lists: ilfo -> lsid -> the number format of a level
  const lists = new Map(), lfos = [];
  {
    const [o, l] = fc(73);
    if (l > 2) {
      const cnt = u16(tbl, o);
      const lstf = [];
      for (let k = 0; k < cnt; k++) {
        const q = o + 2 + k * 28;
        lstf.push({ lsid: u32(tbl, q), simple: !!(tbl[q + 26] & 1) });
      }
      let q = o + 2 + cnt * 28;
      for (const ls of lstf) {
        const lv = [];
        for (let k = 0; k < (ls.simple ? 1 : 9) && q + 28 <= tbl.length; k++) {
          lv.push(tbl[q + 4]);
          q += 28 + tbl[q + 25] + tbl[q + 24];
          q += 2 + u16(tbl, q) * 2;
        }
        lists.set(ls.lsid, lv);
      }
    }
    const [o2, l2] = fc(74);
    if (l2 > 4) { const cnt = u32(tbl, o2); for (let k = 0; k < cnt; k++) lfos.push(u32(tbl, o2 + 4 + k * 16)); }
  }
  const listType = (ilfo, ilvl) => {
    const lv = lists.get(lfos[ilfo - 1]);
    const nfc = lv ? lv[Math.min(ilvl || 0, lv.length - 1)] : 0x17;
    return nfc === 0x17 || nfc === 0xFF ? 'ul' : 'ol';
  };

  // a BLIP record (an OfficeArt picture) at d[r]: PNG and JPEG as they are, a DIB as a BMP
  const blip = (d, r) => {
    if (!d || r + 8 > d.length) return null;
    const inst = u16(d, r) >> 4, type = u16(d, r + 2), len = u32(d, r + 4), body = r + 8, end = Math.min(body + len, d.length);
    const two = { 0xF01E: 0x6E1, 0xF01D: 0x46B, 0xF02A: 0x6E3, 0xF01F: 0x7A9 }[type] === inst;
    const img = d.subarray(body + 17 + (two ? 16 : 0), end);
    if (type === 0xF01E) return { type: 'image/png', bytes: img };
    if (type === 0xF01D || type === 0xF02A) return { type: 'image/jpeg', bytes: img };
    if (type === 0xF01F) {
      const bmp = new Uint8Array(14 + img.length), hs = u32(img, 0), bpp = u16(img, 14), clr = u32(img, 32) || (bpp <= 8 ? 1 << bpp : 0);
      bmp.set([0x42, 0x4D]);
      new DataView(bmp.buffer).setUint32(2, bmp.length, true);
      new DataView(bmp.buffer).setUint32(10, 14 + hs + clr * 4, true);
      bmp.set(img, 14);
      return { type: 'image/bmp', bytes: bmp };
    }
    if (type >= 0xF018 && type <= 0xF117) lose('pictures in other formats');
    return null;
  };

  // floating pictures: the drawing marks' anchors (PlcSpaMom) -> shape -> picture (BLIP store)
  const shapes = new Map(), bses = [], anchors = new Map();
  {
    const [o, l] = fc(50);
    if (l > 8) {
      let shape = null;
      const walk = (from, to, depth) => {
        for (let r = from; r + 8 <= to && depth < 10;) {
          const ver = u16(tbl, r) & 0xF, inst = u16(tbl, r) >> 4, type = u16(tbl, r + 2), len = u32(tbl, r + 4), body = r + 8;
          if (type === 0xF007) {
            const cbName = tbl[body + 33];
            bses.push(len > 36 + cbName ? { d: tbl, at: body + 36 + cbName } : u32(tbl, body + 20) ? { d: wd, at: u32(tbl, body + 28) } : null);
          } else if (type === 0xF00A && shape) shape.spid = u32(tbl, body);
          else if ((type === 0xF00B || type === 0xF122) && shape) {
            for (let k = 0; k < inst; k++) if ((u16(tbl, body + k * 6) & 0x3FFF) === 0x104) shape.pib = u32(tbl, body + k * 6 + 2);
          } else if (ver === 0xF) {
            const outer = shape;
            if (type === 0xF004) shape = {};
            walk(body, Math.min(body + len, to), depth + 1);
            if (type === 0xF004) { if (shape.spid && shape.pib) shapes.set(shape.spid, shape.pib); shape = outer; }
          }
          r = body + len;
        }
      };
      // the store's container, then each drawing behind a one-byte label
      let r = o;
      const end = o + l;
      if (u16(tbl, r + 2) === 0xF000) { walk(r, r + 8 + u32(tbl, r + 4), 0); r += 8 + u32(tbl, r + 4); }
      while (r + 9 <= end) { r++; const len = u32(tbl, r + 4); walk(r, Math.min(r + 8 + len, end), 0); r += 8 + len; }
    }
    const [o2, l2] = fc(40);
    if (l2 > 4) {
      const cnt = (l2 - 4) / 30;
      for (let k = 0; k < cnt; k++) {
        const q = o2 + (cnt + 1) * 4 + k * 26;
        anchors.set(u32(tbl, o2 + k * 4), { spid: u32(tbl, q), w: (i16(tbl, q + 12) - i16(tbl, q + 4)) / 15, h: (i16(tbl, q + 16) - i16(tbl, q + 8)) / 15 });
      }
    }
  }
  const floating = cp => {
    const a = anchors.get(cp), pib = a && shapes.get(a.spid), b = pib && bses[pib - 1];
    const img = b && blip(b.d, b.at);
    return img ? { img: Rich.dataUrl(img.type, img.bytes), w: Math.abs(a.w) || 200, h: Math.abs(a.h) || 150 } : null;
  };

  // an inline picture from the Data stream
  const picture = at => {
    if (!data || at + 0x44 > data.length) return null;
    const lcbP = u32(data, at), cbHeader = u16(data, at + 4), mm = u16(data, at + 6);
    const w = u16(data, at + 28) * (u16(data, at + 32) || 1000) / 1000 / 15, hh = u16(data, at + 30) * (u16(data, at + 34) || 1000) / 1000 / 15;
    let q = at + cbHeader;
    if (mm === 0x66) q += 1 + data[q];
    const end = Math.min(at + lcbP, data.length);
    let found = null;
    const scan = (from, to, depth) => {
      let r = from;
      while (r + 8 <= to && !found && depth < 6) {
        const ver = u16(data, r) & 0xF, type = u16(data, r + 2), len = u32(data, r + 4);
        const body = r + 8;
        if (ver === 0xF && type !== 0xF007) scan(body, Math.min(body + len, to), depth + 1);
        else if (type === 0xF007) scan(body + 36 + data[body + 33], Math.min(body + len, to), depth + 1);
        else if (type >= 0xF018 && type <= 0xF117) found = blip(data, r);
        r = body + len;
      }
    };
    scan(q, end, 0);
    return found ? { img: Rich.dataUrl(found.type, found.bytes), w: w || 200, h: hh || 150 } : null;
  };

  // paragraphs: the text up to each paragraph mark or cell mark
  const blocks = [];
  let rows = null, row = null, cell = null;
  const target = () => (cell ? cell.blocks : blocks);
  let fieldStack = [];
  let start = 0;
  for (let i = 0; i <= text.length; i++) {
    const ch = text[i];
    if (i < text.length && ch !== '\r' && ch !== '\x07') continue;
    if (i === text.length && start >= text.length) break;
    // the paragraph's properties are at its mark
    const markFc = fcs[Math.min(i, fcs.length - 1)];
    const pr = find(papx, markFc);
    const st = styleOf(pr ? pr.istd : 0);
    const pp = pr ? paraProps({ d: wd, 0: pr.g[0], 1: pr.g[1] }, st.pp) : st.pp;
    const runs = [];
    let cur = null;
    const fOf = new Map();   // a CHPX run's formatting here: {all (with the special marks), clean}
    for (let k = start; k < i; k++) {
      const c = text[k];
      const cr = find(chpx, fcs[k]);
      if (!fOf.has(cr)) {
        const all = cr && cr.g ? charProps({ d: wd, 0: cr.g[0], 1: cr.g[1] }, st.f, st.f) : st.f;
        const clean = Object.assign({}, all);
        for (const x of ['spec', 'pic', 'data', 'del', 'hidden']) delete clean[x];
        fOf.set(cr, { all, clean });
      }
      const { all: f, clean } = fOf.get(cr);
      if (c === '\x13') { fieldStack.push({ instr: '', result: false }); continue; }
      if (c === '\x14') { if (fieldStack.length) fieldStack[fieldStack.length - 1].result = true; continue; }
      if (c === '\x15') { fieldStack.pop(); cur = null; continue; }
      const top = fieldStack[fieldStack.length - 1];
      if (top && !top.result) { top.instr += c; continue; }
      if (f.hidden || f.del) continue;
      let href = null;
      for (const fs of fieldStack) { const m = /HYPERLINK\s+"([^"]+)"/i.exec(fs.instr); if (m) href = Rich.safeUrl(m[1]); }
      let t = c;
      if (f.spec || c === '\x01' || c === '\x08') {
        if (c === '\x01' && f.pic !== undefined) { const pic = picture(f.pic); if (pic) { runs.push(Object.assign(pic, href ? { href } : {})); cur = null; } }
        else if (c === '\x08') { const pic = floating(k); if (pic) { runs.push(Object.assign(pic, href ? { href } : {})); cur = null; } else lose('floating drawings'); }
        else if (c === '\x02') { runs.push({ t: '*', f: Object.assign({}, clean, { sup: true }) }); cur = null; }
        continue;
      }
      if (c === '\x0B') t = '\n';
      else if (c === '\x0C' || c === '\x0E') t = '\n';
      else if (c === '\x1E') t = '‑';
      else if (c === '\x1F' || c < ' ' && c !== '\t') continue;
      if (cur && cur.base === clean && cur.href === href) cur.t += t;
      else { cur = { t, f: href ? Object.assign({}, clean, { href }) : clean, base: clean, href }; runs.push(cur); }
    }
    for (const r of runs) if ('base' in r) { delete r.base; delete r.href; }
    const b = { runs, align: pp.align || '', li: pp.li || 0, fi: pp.fi || 0, sb: pp.sb || 0, sa: pp.sa || 0,
      h: st.h && st.h <= 6 ? st.h : pp.outline !== undefined && pp.outline < 6 ? pp.outline + 1 : 0 };
    if (pp.ilfo > 0) { b.list = listType(pp.ilfo, pp.ilvl); b.level = pp.ilvl || 0; b.li = 0; b.fi = 0; }
    if (pp.inTable) {
      if (!rows) { rows = []; }
      if (ch === '\x07' && pp.ttp) {
        // the row's end: where its cells' edges are, and their merges
        if (row) {
          if (cell) row.cells.push(cell);
          if (pp.edges) { row.edges = pp.edges; row.merge = pp.merge; }
          rows.push(row);
        }
        row = null;
        cell = null;
      } else {
        if (!row) row = { cells: [] };
        if (!cell) cell = { blocks: [], span: 1, rspan: 1 };
        cell.blocks.push(b);
        if (ch === '\x07') { row.cells.push(cell); cell = null; }
      }
    } else {
      if (rows) { if (row) { if (cell) row.cells.push(cell); rows.push(row); } if (rows.length) blocks.push({ rows }); rows = null; row = null; cell = null; }
      blocks.push(b);
    }
    start = i + 1;
  }
  if (rows) { if (row) { if (cell) row.cells.push(cell); rows.push(row); } if (rows.length) blocks.push({ rows }); }
  for (const b of blocks) if (b.rows) layout(b);
  const body = styleOf(0);
  const font = body.f.font || defFont || fonts[0] || 'Times New Roman';
  return { html: Rich.html(blocks, body.f.size || 20, font), font, size: (body.f.size || 20) / 2, lost, blocks, macros };
}

/* a table's cells over one grid: the edges of all rows make the columns; a cell spans the
   columns between its edges, merges fold into the cell left of or above them */
function layout(t) {
  const xs = [...new Set(t.rows.flatMap(r => r.edges || []))].sort((a, b) => a - b);
  const above = [];
  for (const r of t.rows) {
    const out = [];
    let col = 0;
    r.cells.forEach((c, i) => {
      const e = r.edges, m = r.merge && r.merge[i];
      const a = e && e[i] !== undefined ? xs.indexOf(e[i]) : col, z = e && e[i + 1] !== undefined ? xs.indexOf(e[i + 1]) : a + 1;
      const span = Math.max(1, z - a);
      if (m && m.h && out.length) { out[out.length - 1].span += span; out[out.length - 1].w += e ? e[i + 1] - e[i] : 0; col = a + span; return; }
      if (m && m.v && above[a]) { above[a].rspan++; col = a + span; return; }
      c.span = span;
      c.w = e && e[i + 1] !== undefined ? e[i + 1] - e[i] : 0;
      out.push(c);
      for (let k = a; k < a + span; k++) above[k] = c;
      col = a + span;
    });
    r.cells = out;
    delete r.edges;
    delete r.merge;
  }
}

/* Word 6 and 95: the text, paragraph by paragraph */
function old(wd, lost, macros) {
  lost['formatting (Word 6/95)'] = 1;
  const fcMin = u32(wd, 0x18), fcMac = u32(wd, 0x1C);
  const s = decode(wd.subarray(fcMin, Math.min(fcMac, wd.length)), 'windows-1252');
  const blocks = s.split(/[\r\x07]/).map(p => ({ runs: [{ t: p.replace(/\x0B/g, '\n').replace(/[\x00-\x08\x0E-\x1F\x13-\x15]/g, ''), f: {} }] }));
  return { html: Rich.html(blocks, 24), font: 'Times New Roman', size: 12, lost, blocks, macros };
}

const DOC = { parse };
return { DOC, readDoc: async b => parse(b) };
});
