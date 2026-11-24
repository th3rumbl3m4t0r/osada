/* Fonts (.ttf, .otf, .ttc, .woff, .woff2): the font itself, set in sizes, and what it says
   about itself (its name table: family, style, version, designer, licence). Part font. The
   browser draws it (FontFace, from the bytes); it is let go when the view closes. A woff2's
   tables are brotli-packed, so it is shown under its file name. */
'use strict';

part('font', () => {

/* sfnt tables of a font (ttf, otf, the first of a ttc, woff) -> Map tag -> bytes */
async function tables(b) {
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const tag = o => String.fromCharCode(b[o], b[o + 1], b[o + 2], b[o + 3]);
  const out = new Map();
  let at = 0;
  if (tag(0) === 'ttcf') at = dv.getUint32(12);
  if (tag(0) === 'wOFF') {
    const n = dv.getUint16(12);
    for (let i = 0; i < n; i++) {
      const e = 44 + i * 20, off = dv.getUint32(e + 4), len = dv.getUint32(e + 8), orig = dv.getUint32(e + 12);
      const raw = b.subarray(off, off + len);
      out.set(tag(e), len < orig ? new Uint8Array(await new Response(new Blob([raw]).stream().pipeThrough(new DecompressionStream('deflate'))).arrayBuffer()) : raw);
    }
    return out;
  }
  const n = dv.getUint16(at + 4);
  for (let i = 0; i < n; i++) { const e = at + 12 + i * 16; out.set(tag(e), b.subarray(dv.getUint32(e + 8), dv.getUint32(e + 8) + dv.getUint32(e + 12))); }
  return out;
}

/* the name table: id -> string, English (Windows 0x409, or Mac roman) first */
function names(t) {
  const out = {};
  if (!t || t.length < 6) return out;
  const dv = new DataView(t.buffer, t.byteOffset, t.byteLength);
  const n = dv.getUint16(2), str = dv.getUint16(4);
  const rank = (p, l) => (p === 3 && l === 0x409 ? 0 : p === 3 || p === 0 ? 1 : p === 1 && l === 0 ? 2 : 3);
  const best = {};
  for (let i = 0; i < n; i++) {
    const e = 6 + i * 12, p = dv.getUint16(e), l = dv.getUint16(e + 4), id = dv.getUint16(e + 6), len = dv.getUint16(e + 8), off = dv.getUint16(e + 10);
    const r = rank(p, l);
    if (best[id] !== undefined && best[id] <= r) continue;
    const s = t.subarray(str + off, str + off + len);
    if (p === 1) out[id] = String.fromCharCode(...s);
    else { let x = ''; for (let k = 0; k + 1 < s.length; k += 2) x += String.fromCharCode((s[k] << 8) | s[k + 1]); out[id] = x; }
    best[id] = r;
  }
  return out;
}

let seq = 0;
async function readFont(bytes, name) {
  const t = bytes[0] === 0x77 && bytes[1] === 0x4F && bytes[2] === 0x46 && bytes[3] === 0x32 ? new Map() : await tables(bytes);
  const nm = names(t.get('name'));
  const maxp = t.get('maxp');
  const family = 'osada-font-' + (++seq);
  const face = new FontFace(family, bytes.slice().buffer);
  await face.load();
  document.fonts.add(face);
  return {
    face, family,
    info: { family: nm[16] || nm[1] || stem(name || ''), style: nm[17] || nm[2] || '', full: nm[4] || '', version: (nm[5] || '').replace(/^Version\s*/i, ''), designer: nm[9] || nm[8] || '', copyright: nm[0] || '', license: nm[13] || '', licenseUrl: nm[14] || '',
      glyphs: maxp && maxp.length >= 6 ? (maxp[4] << 8) | maxp[5] : 0 },
  };
}

const SAMPLE = 'The quick brown fox jumps over the lazy dog. Příliš žluťoučký kůň úpěl ďábelské ódy.';
const CHARS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz 0123456789 .,:;!?¿¡\'"«»()[]{}&@#%*+-=/\\<>€$£¥ ÁČĎÉĚÍŇÓŘŠŤÚŮÝŽ áčďéěíňóřšťúůýž ÄÖÜß äöüß ÀÂÇÈÊËÎÏÔÙÛ àâçèêëîïôùû';

class FontView {
  constructor(host, f) {
    this.f = f;
    const i = f.info;
    const text = h('input', { type: 'text', value: localStorage.getItem('osada.font.sample') || SAMPLE, 'aria-label': tx('sample text'), class: 'font-input' });
    const sizes = h('div', { class: 'font-sizes' });
    const draw = () => sizes.replaceChildren(...[12, 16, 24, 36, 48, 72].map(px => h('div', { class: 'font-line' }, h('span', { class: 'dim', text: px + ' px' }),
      h('div', { style: 'font-family:"' + f.family + '";font-size:' + px + 'px', text: text.value || SAMPLE }))));
    text.addEventListener('input', () => { localStorage.setItem('osada.font.sample', text.value); draw(); });
    const row = (k, v) => (v ? h('tr', {}, h('td', { class: 'dim', text: k }), h('td', { text: v })) : null);
    clear(host).append(
      h('div', { class: 'font-head' }, h('div', { class: 'font-big', style: 'font-family:"' + f.family + '"', text: i.family }), h('div', { class: 'dim', text: [i.style, i.version && tx('version {v}', { v: i.version })].filter(Boolean).join(', ') })),
      text, sizes,
      h('div', { class: 'font-chars', style: 'font-family:"' + f.family + '"', text: CHARS }),
      h('table', { class: 'font-info' }, h('tbody', {}, row(tx('full name'), i.full), row(tx('designer'), i.designer), row(tx('glyphs'), i.glyphs ? String(i.glyphs) : ''), row('©', i.copyright), row(tx('licence'), i.license), row('', i.licenseUrl))));
    host.classList.add('font-host');
    draw();
  }
  destroy() { document.fonts.delete(this.f.face); }
}

return { readFont, FontView, FONT: { tables, names } };
});
