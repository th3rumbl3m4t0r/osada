/* Compound files (OLE2, MS-CFB): the container of the old Office formats (xls, doc, ppt,
   pub) and of Outlook's msg. Part cfb. CFB.read(bytes) gives every stream and storage by
   path ('Workbook', 'Quill/QuillSub/CONTENTS'), names compared without case as the
   format has it; get(path) is a stream's bytes. Chains that loop or leave the file are
   refused rather than followed. */
'use strict';

part('cfb', () => {

const CFB = {
  SIG: [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1],

  is(b) { return b && b.length >= 512 && CFB.SIG.every((x, i) => b[i] === x); },

  read(b) {
    if (!CFB.is(b)) throw new Error('not an OLE2 compound file');
    const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
    const u16 = o => dv.getUint16(o, true), u32 = o => dv.getUint32(o, true);
    const ss = 1 << u16(30), mss = 1 << u16(32);
    if (ss !== 512 && ss !== 4096) throw new Error('compound file: unknown sector size');
    const nSect = Math.floor((b.length - ss) / ss) + 1;
    const at = s => ss + s * ss;   // sector 0 follows the header (a whole sector in v4)
    const END = 0xFFFFFFFE;

    // the FAT's own sectors: 109 in the header, the rest in the DIFAT chain
    const fatSects = [];
    for (let i = 0; i < 109; i++) { const s = u32(76 + i * 4); if (s < nSect) fatSects.push(s); }
    for (let s = u32(68), n = 0; s < nSect && n < nSect; n++) {
      for (let i = 0; i < ss / 4 - 1; i++) { const x = u32(at(s) + i * 4); if (x < nSect) fatSects.push(x); }
      s = u32(at(s) + ss - 4);
    }
    const fat = new Uint32Array(fatSects.length * ss / 4);
    fatSects.forEach((s, i) => { if (at(s) + ss <= b.length) fat.set(new Uint32Array(b.buffer.slice(b.byteOffset + at(s), b.byteOffset + at(s) + ss)), i * ss / 4); });

    const chain = (start, table) => {
      const out = [], seen = new Set();
      for (let s = start; s !== END && s < table.length; s = table[s]) {
        if (seen.has(s)) throw new Error('compound file: a sector chain loops');
        seen.add(s);
        out.push(s);
      }
      return out;
    };
    const bigStream = (start, size) => {
      const out = new Uint8Array(size === undefined ? 0 : size);
      const secs = chain(start, fat);
      if (size === undefined) {
        const all = new Uint8Array(secs.length * ss);
        secs.forEach((s, i) => { if (at(s) < b.length) all.set(b.subarray(at(s), Math.min(at(s) + ss, b.length)), i * ss); });
        return all;
      }
      let p = 0;
      for (const s of secs) {
        if (p >= size) break;
        const n = Math.min(ss, size - p);
        if (at(s) + n > b.length) throw new Error('compound file: a stream goes past the end');
        out.set(b.subarray(at(s), at(s) + n), p);
        p += n;
      }
      if (p < size) throw new Error('compound file: a stream is cut short');
      return out;
    };

    // the directory: 128-byte entries
    const dir = bigStream(u32(48));
    const ddv = new DataView(dir.buffer, dir.byteOffset, dir.byteLength);
    const ents = [];
    for (let o = 0; o + 128 <= dir.length; o += 128) {
      const nl = ddv.getUint16(o + 64, true);
      let name = '';
      for (let i = 0; i + 2 < nl && i < 64; i += 2) name += String.fromCharCode(ddv.getUint16(o + i, true));
      ents.push({
        name, type: dir[o + 66], left: ddv.getUint32(o + 68, true), right: ddv.getUint32(o + 72, true),
        child: ddv.getUint32(o + 76, true), start: ddv.getUint32(o + 116, true),
        // v3 files may have junk in the high half
        size: ss === 512 ? ddv.getUint32(o + 120, true) : ddv.getUint32(o + 120, true) + ddv.getUint32(o + 124, true) * 4294967296,
      });
    }
    if (!ents.length || ents[0].type !== 5) throw new Error('compound file: no root entry');

    // small streams live in the mini stream (the root's), in 64-byte sectors
    const cutoff = u32(56);
    const miniFat = u32(60) < nSect ? new Uint32Array(bigStream(u32(60)).buffer) : new Uint32Array(0);
    let mini = null;
    const miniStream = (start, size) => {
      if (!mini) mini = bigStream(ents[0].start, ents[0].size);
      const out = new Uint8Array(size);
      let p = 0;
      for (const s of chain(start, miniFat)) {
        if (p >= size) break;
        const n = Math.min(mss, size - p);
        if (s * mss + n > mini.length) throw new Error('compound file: a small stream goes past its container');
        out.set(mini.subarray(s * mss, s * mss + n), p);
        p += n;
      }
      if (p < size) throw new Error('compound file: a small stream is cut short');
      return out;
    };

    // paths: each storage's children are a tree of siblings
    const byPath = new Map(), list = [];
    const walk = (id, prefix, depth, seen) => {
      if (id >= ents.length || seen.has(id) || depth > 64) return;
      seen.add(id);
      const e = ents[id];
      walk(e.left, prefix, depth + 1, seen);
      const path = prefix + e.name;
      const item = { name: e.name, path, dir: e.type === 1, size: e.size, id };
      list.push(item);
      byPath.set(path.toLowerCase(), item);
      if (e.type === 1) walk(e.child, path + '/', depth + 1, seen);
      walk(e.right, prefix, depth + 1, seen);
    };
    walk(ents[0].child, '', 0, new Set());

    return {
      list,
      has(path) { return byPath.has(path.toLowerCase()); },
      /* a stream's bytes, or null */
      get(path) {
        const it = byPath.get(path.toLowerCase());
        if (!it || it.dir) return null;
        const e = ents[it.id];
        if (e.size === 0) return new Uint8Array(0);
        return e.size < cutoff ? miniStream(e.start, e.size) : bigStream(e.start, e.size);
      },
      /* the streams and storages right under a storage ('' is the root) */
      children(path) {
        const p = path ? path.toLowerCase() + '/' : '';
        return list.filter(x => x.path.toLowerCase().startsWith(p) && !x.path.slice(p.length).includes('/'));
      },
    };
  },
};

return { CFB };
});
