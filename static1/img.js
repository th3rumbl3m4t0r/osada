/* Pictures browsers do not show: TIFF (every page; uncompressed, LZW, PackBits, Deflate, and
   JPEG strips with their shared tables; grey, RGB, palette, CMYK; 8 and 16 bits), Photoshop's
   PSD and PSB (the merged picture; RGB, grey, CMYK), and camera raw files (CR2, CR3, CRW, NEF,
   ARW, DNG, ORF, RW2, PEF, RAF, SRW, X3F, 3FR, IIQ…: the largest JPEG preview inside, upright,
   with camera, lens and exposure). Part img. read(bytes, name) -> {pages: [canvas or blob
   URL], note}. CCITT fax compression is not read. */
'use strict';

part('img', () => {

async function inflate(b, raw) {
  return new Uint8Array(await new Response(new Blob([b]).stream().pipeThrough(new DecompressionStream(raw ? 'deflate-raw' : 'deflate'))).arrayBuffer());
}

/* TIFF's LZW: codes MSB first, 9 to 12 bits, a code earlier than plain LZW */
function lzw(src, want) {
  const out = new Uint8Array(want);
  let o = 0, bit = 0, width = 9, next = 258, prev = -1;
  const tab = new Array(4096);
  for (let i = 0; i < 256; i++) tab[i] = [i];
  const read = () => {
    let v = 0;
    for (let i = 0; i < width; i++) { const b = src[bit >> 3]; if (b === undefined) return 257; v = (v << 1) | ((b >> (7 - (bit & 7))) & 1); bit++; }
    return v;
  };
  while (o < want) {
    const c = read();
    if (c === 257) break;
    if (c === 256) { width = 9; next = 258; prev = -1; continue; }
    let e;
    if (c < next && tab[c]) e = tab[c];
    else if (prev >= 0) e = tab[prev].concat(tab[prev][0]);
    else break;
    for (const x of e) { if (o < want) out[o++] = x; }
    if (prev >= 0 && next < 4096) tab[next++] = tab[prev].concat(e[0]);
    prev = c;
    if (next + 1 >= (1 << width) && width < 12) width++;
  }
  return out;
}
function packbits(src, want) {
  const out = new Uint8Array(want);
  let o = 0, i = 0;
  while (o < want && i < src.length) {
    const n = (src[i++] << 24) >> 24;
    if (n >= 0) { for (let k = 0; k <= n && o < want; k++) out[o++] = src[i++]; }
    else if (n !== -128) { const v = src[i++]; for (let k = 0; k <= -n && o < want; k++) out[o++] = v; }
  }
  return out;
}

/* the IFDs of a TIFF-like file: [{tag: [values]}], with the reader */
function ifds(b) {
  const le = b[0] === 0x49;
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const u16 = o => dv.getUint16(o, le), u32 = o => dv.getUint32(o, le);
  const SIZE = { 1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 6: 1, 7: 1, 8: 2, 9: 4, 10: 8, 11: 4, 12: 8, 13: 4 };
  const val = (t, o) => (t === 3 ? u16(o) : t === 4 || t === 13 ? u32(o) : t === 5 ? u32(o) / (u32(o + 4) || 1) : t === 8 ? dv.getInt16(o, le) : t === 9 ? dv.getInt32(o, le) : b[o]);
  const read = (at, seen) => {
    if (!at || at + 2 > b.length || seen.has(at)) return null;
    seen.add(at);
    const n = u16(at), tags = {};
    for (let i = 0; i < n && at + 2 + i * 12 + 12 <= b.length; i++) {
      const e = at + 2 + i * 12, tag = u16(e), type = u16(e + 2), cnt = u32(e + 4), sz = (SIZE[type] || 1) * cnt;
      const vo = sz <= 4 ? e + 8 : u32(e + 8);
      if (vo + sz > b.length || cnt > 1 << 24) continue;
      const vals = [];
      for (let k = 0; k < Math.min(cnt, 65536 * 4); k++) vals.push(val(type, vo + k * (SIZE[type] || 1)));
      tags[tag] = vals;
    }
    return { tags, next: u32(at + 2 + n * 12) };
  };
  return { le, u16, u32, read };
}

async function tiffPages(b) {
  const t = ifds(b), pages = [], seen = new Set();
  let at = t.u32(4);
  const notes = new Set();
  for (let guard = 0; at && guard < 500; guard++) {
    const d = t.read(at, seen);
    if (!d) break;
    at = d.next;
    const T = d.tags, g = (k, def) => (T[k] ? T[k][0] : def);
    if (g(254, 0) & 1) continue;   // a reduced copy of another page
    try { pages.push(await tiffPage(b, T, g, t.le)); } catch (e) { notes.add(txErr(e.message)); }
  }
  return { pages, note: [...notes].join('; ') };
}

async function tiffPage(b, T, g, le) {
  const w = g(256, 0), hh = g(257, 0), comp = g(259, 1), photo = g(262, 1), spp = g(277, 1), bps = (T[258] || [1])[0];
  const planar = g(284, 1), pred = g(317, 1), extra = T[338] ? T[338].length : 0;
  if (!w || !hh || w * hh > 120e6) throw new Error('picture size not readable');
  if (comp === 3 || comp === 4 || comp === 2) throw new Error('CCITT fax pages are not shown');
  const tiled = !!T[322];
  const offs = T[tiled ? 324 : 273] || [], cnts = T[tiled ? 325 : 279] || [];
  const tw = tiled ? g(322) : w, th = tiled ? g(323) : g(278, hh);
  const canvas = document.createElement('canvas');
  canvas.width = w;
  canvas.height = hh;
  const ctx = canvas.getContext('2d');
  if (comp === 7) {
    // each strip or tile a JPEG; the shared tables go in front of each
    const tables = T[347] ? new Uint8Array(T[347]) : null;
    const across = Math.ceil(w / tw);
    for (let i = 0; i < offs.length; i++) {
      let part = b.subarray(offs[i], offs[i] + cnts[i]);
      if (tables && tables.length > 4) { const j = new Uint8Array(tables.length - 2 + part.length - 2); j.set(tables.subarray(0, tables.length - 2)); j.set(part.subarray(2), tables.length - 2); part = j; }
      const bmp = await createImageBitmap(new Blob([part], { type: 'image/jpeg' }));
      ctx.drawImage(bmp, (i % across) * tw, Math.floor(i / across) * th);
      bmp.close();
    }
    return canvas;
  }
  if (comp !== 1 && comp !== 5 && comp !== 8 && comp !== 32946 && comp !== 32773) throw new Error(tx('a TIFF compression osada does not read ({n})', { n: comp }));
  if (bps !== 1 && bps !== 4 && bps !== 8 && bps !== 16) throw new Error(tx('{n}-bit TIFF pages are not shown', { n: bps }));
  if (planar !== 1 && spp > 1) throw new Error('planar TIFF pages are not shown');
  const img = ctx.createImageData(w, hh), px = img.data;
  const map = T[320];
  const across = Math.ceil(w / tw);
  for (let i = 0; i < offs.length; i++) {
    const tx0 = tiled ? (i % across) * tw : 0, ty0 = tiled ? Math.floor(i / across) * th : i * th;
    const rows = Math.min(th, hh - ty0), cols = tiled ? tw : w;
    if (rows <= 0) continue;
    const rowBytes = Math.ceil(cols * spp * bps / 8), want = rowBytes * (tiled ? th : rows);
    const src = b.subarray(offs[i], offs[i] + (cnts[i] || want));
    let data = comp === 5 ? lzw(src, want) : comp === 32773 ? packbits(src, want) : comp === 8 || comp === 32946 ? await inflate(src) : src;
    if (pred === 2 && bps === 8) for (let r = 0; r < rows; r++) for (let x = spp; x < cols * spp; x++) data[r * rowBytes + x] = (data[r * rowBytes + x] + data[r * rowBytes + x - spp]) & 0xFF;
    const sample = (r, x, s) => {
      if (bps === 8) return data[r * rowBytes + x * spp + s];
      if (bps === 16) { const o = r * rowBytes + (x * spp + s) * 2; return le ? data[o + 1] : data[o]; }   // the high byte
      const bit = (x * spp + s) * bps, byte = data[r * rowBytes + (bit >> 3)];
      return bps === 1 ? (byte >> (7 - (bit & 7))) & 1 ? 255 : 0 : ((byte >> (4 - (bit & 7))) & 15) * 17;
    };
    for (let r = 0; r < rows; r++) {
      const y = ty0 + r;
      for (let x = 0; x < cols && tx0 + x < w; x++) {
        const p = (y * w + tx0 + x) * 4;
        let R, G, B, Aa = 255;
        if (photo === 2) { R = sample(r, x, 0); G = sample(r, x, 1); B = sample(r, x, 2); if (extra) Aa = sample(r, x, 3); }
        else if (photo === 3 && map) { const ix = bps === 8 ? data[r * rowBytes + x] : bps === 4 ? (data[r * rowBytes + (x >> 1)] >> (x & 1 ? 0 : 4)) & 15 : sample(r, x, 0) ? 1 : 0, n = map.length / 3; R = map[ix] >> 8; G = map[n + ix] >> 8; B = map[2 * n + ix] >> 8; }
        else if (photo === 5) { const c = sample(r, x, 0), m = sample(r, x, 1), yy = sample(r, x, 2), k = sample(r, x, 3); R = (255 - c) * (255 - k) / 255; G = (255 - m) * (255 - k) / 255; B = (255 - yy) * (255 - k) / 255; }
        else { let v = sample(r, x, 0); if (photo === 0) v = 255 - v; R = G = B = v; if (extra && spp > 1) Aa = sample(r, x, 1); }
        px[p] = R; px[p + 1] = G; px[p + 2] = B; px[p + 3] = Aa;
      }
    }
  }
  ctx.putImageData(img, 0, 0);
  return canvas;
}

/* Photoshop: the merged picture at the end of the file */
async function psd(b) {
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  if (dv.getUint32(0) !== 0x38425053) throw new Error('not a Photoshop file');
  const ch = dv.getUint16(12), hh = dv.getUint32(14), w = dv.getUint32(18), depth = dv.getUint16(22), mode = dv.getUint16(24);
  if (depth !== 8 && depth !== 16) throw new Error(tx('{n}-bit Photoshop files are not shown', { n: depth }));
  let o = 26;
  o += 4 + dv.getUint32(o);   // colour mode data
  o += 4 + dv.getUint32(o);   // resources
  const big = dv.getUint16(4) === 2;   // PSB: wider lengths
  o += big ? 8 + Number(dv.getBigUint64(o)) : 4 + dv.getUint32(o);   // layers
  const comp = dv.getUint16(o);
  o += 2;
  const bpc = depth / 8, plane = w * hh * bpc, chans = [];
  if (comp === 0) for (let c = 0; c < ch; c++) chans.push(b.subarray(o + c * plane, o + (c + 1) * plane));
  else if (comp === 1) {
    const counts = [];
    for (let i = 0; i < ch * hh; i++) counts.push(big ? dv.getUint32(o + i * 4) : dv.getUint16(o + i * 2));
    let p = o + ch * hh * (big ? 4 : 2);
    for (let c = 0; c < ch; c++) {
      const out = new Uint8Array(w * hh * bpc);
      for (let r = 0; r < hh; r++) { const n = counts[c * hh + r]; out.set(packbits(b.subarray(p, p + n), w * bpc), r * w * bpc); p += n; }
      chans.push(out);
    }
  } else throw new Error('this Photoshop compression is not read');
  const canvas = document.createElement('canvas');
  canvas.width = w;
  canvas.height = hh;
  const ctx = canvas.getContext('2d'), img = ctx.createImageData(w, hh), px = img.data;
  const at = (c, i) => (chans[c] ? chans[c][i * bpc] : 255);
  for (let i = 0; i < w * hh; i++) {
    let R, G, B;
    if (mode === 3) { R = at(0, i); G = at(1, i); B = at(2, i); }
    else if (mode === 4) { const k = at(3, i); R = at(0, i) * k / 255; G = at(1, i) * k / 255; B = at(2, i) * k / 255; }   // stored inverted: 255 is no ink
    else { R = G = B = at(0, i); }
    px[i * 4] = R; px[i * 4 + 1] = G; px[i * 4 + 2] = B;
    px[i * 4 + 3] = mode === 3 && ch > 3 ? at(3, i) : 255;
  }
  ctx.putImageData(img, 0, 0);
  return canvas;
}

/* camera raw: the largest JPEG inside that a browser draws (in the TIFF structure, else
   anywhere in the file; not the lossless JPEG some keep the sensor data in), turned the way
   the camera says when the JPEG does not say itself, and what the camera wrote about the shot */
const str = v => (v ? String.fromCharCode(...v).replace(/\0[\s\S]*$/, '').trim() : '');
function exifOf(b) {
  // a TIFF structure: IFD0 and its EXIF IFD
  try {
    const t = ifds(b), seen = new Set(), d0 = t.read(t.u32(4), seen);
    if (!d0) return null;
    const e = d0.tags[34665] ? t.read(d0.tags[34665][0], seen) : null;
    return { ifd0: d0.tags, exif: e ? e.tags : {} };
  } catch (x) { return null; }
}
/* the TIFF in a JPEG's APP1 Exif segment */
function jpegExif(b) {
  for (let j = 2; j + 4 < b.length && b[j] === 0xFF;) {
    const m = b[j + 1], n = (b[j + 2] << 8) | b[j + 3];
    if (m === 0xE1 && String.fromCharCode(b[j + 4], b[j + 5], b[j + 6], b[j + 7]) === 'Exif') return exifOf(b.subarray(j + 10, j + 2 + n));
    if (m === 0xDA) break;
    j += 2 + n;
  }
  return null;
}
/* the SOF a JPEG starts with: 0xC0..0xC2 a browser draws, 0xC3 (lossless) it does not */
function sofOf(b, i) {
  for (let j = i + 2; j + 3 < b.length && b[j] === 0xFF;) {
    const m = b[j + 1];
    if (m >= 0xC0 && m <= 0xCF && m !== 0xC4 && m !== 0xC8 && m !== 0xCC) return m;
    if (m === 0xDA) return 0;
    j += 2 + ((b[j + 2] << 8) | b[j + 3]);
  }
  return 0;
}
function say(x) {
  if (!x) return '';
  const T = x.ifd0, E = x.exif, n = (t, k) => (t[k] ? t[k][0] : 0);
  const make = str(T[271]), model = str(T[272]);
  const cam = model.toLowerCase().startsWith(make.toLowerCase().split(' ')[0]) ? model : [make, model].filter(Boolean).join(' ');
  const exp = n(E, 33434), f = n(E, 33437), iso = n(E, 34855), mm = n(E, 37386);
  const when = str(E[36867]) || str(T[306]);
  return [cam, str(E[42036]), exp ? (exp < 1 ? '1/' + Math.round(1 / exp) : Math.round(exp * 10) / 10) + ' s' : '', f ? 'f/' + Math.round(f * 10) / 10 : '',
    iso ? 'ISO ' + iso : '', mm ? Math.round(mm) + ' mm' : '', when.replace(/^(\d{4}):(\d\d):(\d\d)/, '$1-$2-$3')].filter(Boolean).join(' \u00b7 ');
}
async function upright(blob, o) {
  const bm = await createImageBitmap(blob);
  const side = o >= 5, k = Math.min(1, 6000 / Math.max(bm.width, bm.height));
  const w = Math.round(bm.width * k), hh = Math.round(bm.height * k);
  const canvas = document.createElement('canvas');
  canvas.width = side ? hh : w;
  canvas.height = side ? w : hh;
  const ctx = canvas.getContext('2d');
  const M = { 2: [-1, 0, 0, 1, w, 0], 3: [-1, 0, 0, -1, w, hh], 4: [1, 0, 0, -1, 0, hh], 5: [0, 1, 1, 0, 0, 0], 6: [0, 1, -1, 0, hh, 0], 7: [0, -1, -1, 0, hh, w], 8: [0, -1, 1, 0, 0, w] }[o];
  if (M) ctx.setTransform(...M);
  ctx.drawImage(bm, 0, 0, w, hh);
  bm.close();
  return canvas;
}
async function rawPreview(b) {
  const found = [];
  let info = null;
  if ((b[0] === 0x49 && b[1] === 0x49) || (b[0] === 0x4D && b[1] === 0x4D)) {
    info = exifOf(b);
    try {
      const t = ifds(b), seen = new Set(), queue = [t.u32(4)];
      while (queue.length && seen.size < 100) {
        const d = t.read(queue.shift(), seen);
        if (!d) continue;
        const T = d.tags;
        if (T[513] && T[514]) found.push([T[513][0], T[514][0]]);
        if (T[259] && (T[259][0] === 6 || T[259][0] === 7) && T[273] && T[279] && T[273].length === 1) found.push([T[273][0], T[279][0]]);
        for (const k of [330, 34665]) for (const v of T[k] || []) queue.push(v);
        if (d.next) queue.push(d.next);
      }
    } catch (e) { /* the scan below */ }
  }
  // Canon CR3: IFD0 and EXIF as TIFFs in the CMT1 / CMT2 boxes
  if (!info) {
    for (let i = 0; i + 12 < Math.min(b.length, 1 << 20); i++) {
      if (b[i] !== 0x43 || b[i + 1] !== 0x4D || b[i + 2] !== 0x54 || b[i + 3] !== 0x31) continue;
      const one = exifOf(b.subarray(i + 4));
      const j = b.indexOf(0x43, i + 4);
      let two = null;
      for (let k = j; k >= 0 && k + 12 < b.length && k < i + 65536; k = b.indexOf(0x43, k + 1)) if (b[k + 1] === 0x4D && b[k + 2] === 0x54 && b[k + 3] === 0x32) { two = exifOf(b.subarray(k + 4)); break; }
      if (one) info = { ifd0: one.ifd0, exif: two ? two.ifd0 : one.exif };
      break;
    }
  }
  const ok = ([o, n]) => o + n <= b.length && b[o] === 0xFF && b[o + 1] === 0xD8 && sofOf(b, o) >= 0xC0 && sofOf(b, o) <= 0xC2;
  let best = found.filter(ok).sort((x, y) => y[1] - x[1])[0];
  if (!best || best[1] < 20000) {
    // anywhere: SOI..EOI runs, the largest
    for (let i = 0; i + 3 < b.length; i++) {
      if (b[i] !== 0xFF || b[i + 1] !== 0xD8 || b[i + 2] !== 0xFF) continue;
      const sof = sofOf(b, i);
      if (sof < 0xC0 || sof > 0xC2) continue;
      let j = i + 2;
      while (j + 3 < b.length) {
        if (b[j] !== 0xFF) break;
        const m = b[j + 1];
        if (m === 0xDA) {
          let k = j + 2;
          while (k + 1 < b.length && !(b[k] === 0xFF && b[k + 1] === 0xD9)) k++;
          if (k + 1 < b.length && (!best || k + 2 - i > best[1])) best = [i, k + 2 - i];
          i = k;
          break;
        }
        j += 2 + ((b[j + 2] << 8) | b[j + 3]);
      }
    }
  }
  if (!best) throw new Error('no preview picture in this raw file');
  const jpeg = b.subarray(best[0], best[0] + best[1]), own = jpegExif(jpeg);
  if (!info) info = own;
  const blob = new Blob([jpeg], { type: 'image/jpeg' });
  // a JPEG with its own orientation is turned by the browser
  const o = own && own.ifd0[274] ? 1 : info && info.ifd0[274] ? info.ifd0[274][0] : 1;
  return { page: o > 1 && o <= 8 ? await upright(blob, o) : URL.createObjectURL(blob), info: say(info) };
}

async function read(bytes, name) {
  const ext = (name || '').toLowerCase().split('.').pop();
  if (ext === 'psd' || ext === 'psb') return { pages: [await psd(bytes)] };
  if (ext === 'tif' || ext === 'tiff') return tiffPages(bytes);
  const r = await rawPreview(bytes);
  return { pages: [r.page], note: tx('the preview picture the camera put in the file') + (r.info ? ': ' + r.info : '') };
}

/* the view: one page at a time, fitted; a click: actual size */
class ImgView {
  constructor(host, r) {
    this.urls = r.pages.filter(p => typeof p === 'string');
    this.pages = r.pages;
    this.i = 0;
    this.box = h('div', { class: 'img-box' });
    this.count = h('span', { class: 'dim' });
    const b = (t, title, fn) => h('button', { text: t, title, onclick: fn });
    const bar = r.pages.length > 1 ? h('div', { class: 'toolbar' }, b('<', tx('previous page'), () => this.go(this.i - 1)), this.count, b('>', tx('next page'), () => this.go(this.i + 1))) : null;
    clear(host).append(bar || '', r.note ? h('div', { class: 'viewer-note dim', text: r.note }) : '', this.box);
    host.classList.add('img-host');
    this.box.addEventListener('click', () => this.box.classList.toggle('actual'));
    this.go(0);
  }
  go(i) {
    if (i < 0 || i >= this.pages.length) return;
    this.i = i;
    const p = this.pages[i];
    this.box.replaceChildren(typeof p === 'string' ? h('img', { src: p, alt: '' }) : p);
    this.count.textContent = (i + 1) + ' / ' + this.pages.length;
  }
  destroy() { this.urls.forEach(u => URL.revokeObjectURL(u)); }
}

return { IMG: { read, lzw, packbits }, ImgView, readImg: read };
});
