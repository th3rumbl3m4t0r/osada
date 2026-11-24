/* BZip2: .bz2 / .tar.bz2 files, and 7z's BZip2 coder. Part bzip2. Blocks of Huffman-coded
   move-to-front symbols, the Burrows-Wheeler transform undone, then the runs of four. */
'use strict';

part('bzip2', () => {

const bad = () => { throw new Error('damaged bzip2 data'); };

function decode(b) {
  // bits, most significant first; up to 31 kept in buf
  let p = 0, buf = 0, cnt = 0;
  const fill = () => { while (cnt <= 23) { buf = ((buf << 8) | (p < b.length ? b[p] : 0)) & 0x7FFFFFFF; p++; cnt += 8; } };
  const bits = n => {
    if (cnt < n) fill();
    if (p > b.length + 4) bad();
    cnt -= n;
    return (buf >>> cnt) & ((1 << n) - 1);
  };
  const parts = [];
  let total = 0;
  while (p + 4 <= b.length && b[p] === 0x42 && b[p + 1] === 0x5A && b[p + 2] === 0x68) {
    const level = b[p + 3] - 0x30;
    if (level < 1 || level > 9) bad();
    p += 4; buf = 0; cnt = 0;
    const max = level * 100000, tt = new Uint32Array(max);
    let out = new Uint8Array(max * 2), o = 0;
    const put = v => { if (o >= out.length) { const n = new Uint8Array(out.length * 2); n.set(out); out = n; } out[o++] = v; };
    for (;;) {
      const m1 = bits(24), m2 = bits(24);
      if (m1 === 0x177245 && m2 === 0x385090) { bits(16); bits(16); p -= cnt >> 3; break; }   // the bytes read ahead are the next stream's
      if (m1 !== 0x314159 || m2 !== 0x265359) bad();
      bits(16); bits(16);   // the block's CRC
      if (bits(1)) throw new Error('randomised bzip2 blocks are not read');
      const orig = bits(24);
      // the bytes used
      const used = [], in16 = bits(16);
      for (let i = 0; i < 16; i++) if (in16 & (0x8000 >> i)) { const w = bits(16); for (let j = 0; j < 16; j++) if (w & (0x8000 >> j)) used.push(i * 16 + j); }
      if (!used.length) bad();
      const alpha = used.length + 2, groups = bits(3), nsel = bits(15);
      if (groups < 2 || groups > 6 || nsel < 1) bad();
      const order = [0, 1, 2, 3, 4, 5].slice(0, groups), sel = new Uint8Array(nsel);
      for (let i = 0; i < nsel; i++) {
        let j = 0;
        while (bits(1)) if (++j >= groups) bad();
        const v = order[j];
        order.splice(j, 1);
        order.unshift(v);
        sel[i] = v;
      }
      // the Huffman tables: limit, base, perm per group
      const tables = [];
      for (let t = 0; t < groups; t++) {
        const len = new Uint8Array(alpha);
        let cur = bits(5);
        for (let i = 0; i < alpha; i++) {
          for (;;) { if (cur < 1 || cur > 20) bad(); if (!bits(1)) break; cur += bits(1) ? -1 : 1; }
          len[i] = cur;
        }
        let minL = 32, maxL = 0;
        for (const l of len) { if (l < minL) minL = l; if (l > maxL) maxL = l; }
        const perm = [], base = new Int32Array(25), limit = new Int32Array(25);
        for (let l = minL; l <= maxL; l++) for (let j = 0; j < alpha; j++) if (len[j] === l) perm.push(j);
        for (let j = 0; j < alpha; j++) base[len[j] + 1]++;
        for (let l = 1; l < 25; l++) base[l] += base[l - 1];
        let vec = 0;
        for (let l = minL; l <= maxL; l++) { vec += base[l + 1] - base[l]; limit[l] = vec - 1; vec <<= 1; }
        for (let l = minL + 1; l <= maxL; l++) base[l] = ((limit[l - 1] + 1) << 1) - base[l];
        tables.push({ minL, maxL, perm, base, limit });
      }
      // the symbols: runs (RUNA, RUNB) and move-to-front positions
      const mtf = Uint8Array.from(used, (_, i) => i), counts = new Int32Array(256);
      let n = 0, g = -1, left = 0, T = null;
      const sym = () => {
        if (left === 0) { if (++g >= nsel) bad(); T = tables[sel[g]]; left = 50; }
        left--;
        // the longest code's bits, then the length that fits
        if (cnt < T.maxL) fill();
        const w = (buf >>> (cnt - T.maxL)) & ((1 << T.maxL) - 1);
        let zn = T.minL, zv = w >>> (T.maxL - zn);
        while (zv > T.limit[zn]) { if (++zn > T.maxL) bad(); zv = w >>> (T.maxL - zn); }
        cnt -= zn;
        return T.perm[zv - T.base[zn]];
      };
      let s = sym();
      while (s !== alpha - 1) {
        if (s <= 1) {
          let run = 0, k = 1;
          while (s <= 1) { run += (s + 1) * k; k <<= 1; if (k > (1 << 24)) bad(); s = sym(); }
          const c = used[mtf[0]];
          if (n + run > max) bad();
          counts[c] += run;
          tt.fill(c, n, n + run);
          n += run;
          continue;
        }
        const i = s - 1, v = mtf[i];
        for (let k = i; k > 0; k--) mtf[k] = mtf[k - 1];
        mtf[0] = v;
        const c = used[v];
        if (n >= max) bad();
        counts[c]++;
        tt[n++] = c;
        s = sym();
      }
      if (orig >= n) bad();
      // the Burrows-Wheeler transform, undone
      const start = new Int32Array(256);
      for (let i = 1; i < 256; i++) start[i] = start[i - 1] + counts[i - 1];
      for (let i = 0; i < n; i++) { const c = tt[i] & 0xFF; tt[start[c]++] |= i << 8; }
      // runs of four, then a count
      if (o + n * 2 > out.length) { const x = new Uint8Array(Math.max(out.length * 2, o + n * 2)); x.set(out.subarray(0, o)); out = x; }
      let pos = tt[orig] >>> 8, last = -1, run = 0;
      for (let k = 0; k < n; k++) {
        pos = tt[pos];
        const c = pos & 0xFF;
        pos >>>= 8;
        if (run === 4) { for (let r = 0; r < c; r++) put(last); run = 0; continue; }
        if (o < out.length) out[o++] = c; else put(c);
        if (c === last) run++; else { last = c; run = 1; }
      }
    }
    parts.push(out.subarray(0, o));
    total += o;
  }
  if (!parts.length) bad();
  if (parts.length === 1) return parts[0];
  const all = new Uint8Array(total);
  let o = 0;
  for (const x of parts) { all.set(x, o); o += x.length; }
  return all;
}

return { BZIP2: { decode } };
});
