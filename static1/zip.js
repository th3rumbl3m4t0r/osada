/* Zip archives: reading the directory, inflating an entry, writing a new archive.
   No zip64, no encryption: what office files use. Part zip. */
'use strict';

part('zip', () => {

/* ---- zip (no zip64, no encryption: what office files use) ---- */
const ZIP = {
  table: null,
  crc(b) {
    let t = ZIP.table;
    if (!t) {
      t = ZIP.table = new Uint32Array(256);
      for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xEDB88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; }
    }
    let c = 0xFFFFFFFF;
    for (let i = 0; i < b.length; i++) c = t[(c ^ b[i]) & 0xFF] ^ (c >>> 8);
    return (c ^ 0xFFFFFFFF) >>> 0;
  },

  /* the entries, from the central directory; data stays compressed */
  read(b) {
    const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
    let e = -1;
    for (let i = b.length - 22; i >= Math.max(0, b.length - 65557); i--) if (dv.getUint32(i, true) === 0x06054b50) { e = i; break; }
    if (e < 0) throw new Error('not a zip file (no zip directory)');
    const n = dv.getUint16(e + 10, true);
    let p = dv.getUint32(e + 16, true);
    if (n === 0xFFFF || p === 0xFFFFFFFF) throw new Error('zip64 files are not supported');
    const dec = new TextDecoder(), out = [];
    for (let k = 0; k < n; k++) {
      if (dv.getUint32(p, true) !== 0x02014b50) throw new Error('broken zip directory');
      const nl = dv.getUint16(p + 28, true), xl = dv.getUint16(p + 30, true), cl = dv.getUint16(p + 32, true);
      const f = {
        made: dv.getUint16(p + 4, true), flags: dv.getUint16(p + 8, true), method: dv.getUint16(p + 10, true),
        time: dv.getUint16(p + 12, true), date: dv.getUint16(p + 14, true), crc: dv.getUint32(p + 16, true),
        csize: dv.getUint32(p + 20, true), size: dv.getUint32(p + 24, true),
        iattr: dv.getUint16(p + 36, true), eattr: dv.getUint32(p + 38, true),
        name: dec.decode(b.subarray(p + 46, p + 46 + nl)),
      };
      if (f.flags & 1) throw new Error('encrypted files are not supported');
      const lo = dv.getUint32(p + 42, true);
      if (dv.getUint32(lo, true) !== 0x04034b50) throw new Error('broken zip entry ' + f.name);
      const start = lo + 30 + dv.getUint16(lo + 26, true) + dv.getUint16(lo + 28, true);
      f.data = b.subarray(start, start + f.csize);
      out.push(f);
      p += 46 + nl + xl + cl;
    }
    return out;
  },

  async inflate(f) {
    if (f.method === 0) return f.data;
    if (f.method !== 8) throw new Error(f.name + ': unknown compression');
    return new Uint8Array(await new Response(new Blob([f.data]).stream().pipeThrough(new DecompressionStream('deflate-raw'))).arrayBuffer());
  },

  /* now, as DOS time and date */
  stamp() {
    const d = new Date();
    return { time: (d.getHours() << 11) | (d.getMinutes() << 5) | (d.getSeconds() >> 1), date: ((d.getFullYear() - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate() };
  },
  /* old with new content */
  async entry(old, raw) {
    const data = new Uint8Array(await new Response(new Blob([raw]).stream().pipeThrough(new CompressionStream('deflate-raw'))).arrayBuffer());
    return Object.assign({}, old, { method: 8, data, crc: ZIP.crc(raw), csize: data.length, size: raw.length }, ZIP.stamp());
  },
  /* a new entry, deflated; store: kept as it is (an ods mimetype must be) */
  async add(name, raw, store) {
    if (typeof raw === 'string') raw = new TextEncoder().encode(raw);
    const f = { made: 20, flags: /^[\x20-\x7e]*$/.test(name) ? 0 : 0x800, iattr: 0, eattr: 0, name };
    if (!store) return ZIP.entry(f, raw);
    return Object.assign(f, { method: 0, data: raw, crc: ZIP.crc(raw), csize: raw.length, size: raw.length }, ZIP.stamp());
  },

  /* sizes go in the headers, so no data descriptors (flag bit 3) */
  write(files) {
    const enc = new TextEncoder(), names = files.map(f => enc.encode(f.name));
    let size = 22;
    files.forEach((f, i) => { size += 76 + 2 * names[i].length + f.data.length; });
    const out = new Uint8Array(size), dv = new DataView(out.buffer), offs = [];
    const head = (p, f, i) => {
      dv.setUint16(p, f.flags & ~8, true); dv.setUint16(p + 2, f.method, true);
      dv.setUint16(p + 4, f.time, true); dv.setUint16(p + 6, f.date, true); dv.setUint32(p + 8, f.crc, true);
      dv.setUint32(p + 12, f.csize, true); dv.setUint32(p + 16, f.size, true); dv.setUint16(p + 20, names[i].length, true);
    };
    let p = 0;
    files.forEach((f, i) => {
      offs.push(p);
      dv.setUint32(p, 0x04034b50, true); dv.setUint16(p + 4, 20, true); head(p + 6, f, i);
      out.set(names[i], p + 30);
      out.set(f.data, p + 30 + names[i].length);
      p += 30 + names[i].length + f.data.length;
    });
    const cd = p;
    files.forEach((f, i) => {
      dv.setUint32(p, 0x02014b50, true); dv.setUint16(p + 4, f.made, true); dv.setUint16(p + 6, 20, true); head(p + 8, f, i);
      dv.setUint16(p + 36, f.iattr, true); dv.setUint32(p + 38, f.eattr, true); dv.setUint32(p + 42, offs[i], true);
      out.set(names[i], p + 46);
      p += 46 + names[i].length;
    });
    dv.setUint32(p, 0x06054b50, true); dv.setUint16(p + 8, files.length, true); dv.setUint16(p + 10, files.length, true);
    dv.setUint32(p + 12, p - cd, true); dv.setUint32(p + 16, cd, true);
    return out;
  },
};

return { ZIP };
});
