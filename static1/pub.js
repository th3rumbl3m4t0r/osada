/* Publisher (.pub): shown, not edited. Part pub, on cfb and rich. What a preview can give
   without Publisher's layout engine: the text of every frame, in order (the TEXT chunk of
   Quill/QuillSub/CONTENTS), and the pictures (PNG, JPEG, DIB from the BLIP store in
   Escher/EscherStm, their data in EscherDelayStm), shown in the document view. */
'use strict';

part('pub', ({ CFB, Rich }) => {

const u16 = (d, o) => d[o] | (d[o + 1] << 8);
const u32 = (d, o) => (d[o] | (d[o + 1] << 8) | (d[o + 2] << 16) | (d[o + 3] << 24)) >>> 0;

function parse(bytes) {
  const cf = CFB.read(bytes);
  const q = cf.get('Quill/QuillSub/CONTENTS');
  if (!q || String.fromCharCode(...q.subarray(0, 4)) !== 'CHNK') throw new Error('not a Publisher file (no Quill contents)');
  const lost = { 'page layout (the text and pictures are shown in order)': 1 };
  // the chunk directory: 24-byte entries from 0x20, each [?, name, index, ?, name, offset, length]
  let text = '';
  const n = u16(q, 0x0C);
  for (let i = 0; i < n && 0x20 + i * 24 + 24 <= q.length; i++) {
    const e = 0x20 + i * 24;
    if (String.fromCharCode(...q.subarray(e + 2, e + 6)) !== 'TEXT') continue;
    const off = u32(q, e + 16), len = u32(q, e + 20);
    for (let k = off; k + 1 < Math.min(off + len, q.length); k += 2) text += String.fromCharCode(u16(q, k));
  }
  const blocks = [];
  for (const p of text.split('\r')) {
    const t = p.replace(/\n/g, '').replace(/[\x00-\x08\x0B-\x1F]/g, '');
    if (t.trim() || (blocks.length && blocks[blocks.length - 1].runs.length)) blocks.push({ runs: t ? [{ t, f: {} }] : [] });
  }
  // pictures: the store's entries, their data embedded or in the delay stream
  const es = cf.get('Escher/EscherStm'), delay = cf.get('Escher/EscherDelayStm');
  const pics = [];
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
    lost['pictures in other formats'] = (lost['pictures in other formats'] || 0) + 1;
    return null;
  };
  const walk = (d, from, to, depth) => {
    for (let r = from; r + 8 <= Math.min(to, d.length) && depth < 8;) {
      const ver = u16(d, r) & 0xF, type = u16(d, r + 2), len = u32(d, r + 4), body = r + 8;
      if (type === 0xF007) {
        const cbName = d[body + 33];
        const p = len > 36 + cbName ? blip(d, body + 36 + cbName) : u32(d, body + 20) ? blip(delay, u32(d, body + 28)) : null;
        if (p) pics.push(p);
      } else if (ver === 0xF) walk(d, body, body + len, depth + 1);
      r = body + len;
    }
  };
  if (es) walk(es, 0, es.length, 0);
  for (const p of pics) {
    // the picture's own size, from its header, scaled to the page
    let w = 0, hh = 0;
    const b = p.bytes;
    if (p.type === 'image/png' && b.length > 24) { w = (b[16] << 24 | b[17] << 16 | b[18] << 8 | b[19]) >>> 0; hh = (b[20] << 24 | b[21] << 16 | b[22] << 8 | b[23]) >>> 0; }
    else if (p.type === 'image/bmp') { w = u32(b, 18); hh = Math.abs(u32(b, 22) | 0); }
    else if (p.type === 'image/jpeg') for (let o = 2; o + 9 < b.length;) { if (b[o] !== 0xFF) break; const m = b[o + 1], l = (b[o + 2] << 8) | b[o + 3]; if (m >= 0xC0 && m <= 0xC3) { hh = (b[o + 5] << 8) | b[o + 6]; w = (b[o + 7] << 8) | b[o + 8]; break; } o += 2 + l; }
    const k = w ? Math.min(1, 600 / w) : 1;
    blocks.push({ runs: [{ img: Rich.dataUrl(p.type, p.bytes), w: (w || 300) * k, h: (hh || 200) * k }] });
  }
  return { html: Rich.html(blocks, 24, 'Arial'), font: 'Arial', size: 12, lost, blocks };
}

return { PUB: { parse }, readPub: async b => parse(b) };
});
