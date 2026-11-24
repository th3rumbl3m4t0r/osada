/* RAR archives (RAR 4 and RAR 5): read into the archive view's entries. Part rar, on archive;
   the ppmd part is held while a RAR 4 file with text compression is unpacked. Unpacked: stored
   files, RAR 2.9-4 compression (LZ with its standard filters: E8, E8E9, delta, RGB, audio,
   Itanium; and PPMd blocks) and RAR 5 compression (LZ, filters delta, E8, E8E9, ARM). The
   dictionary is one growing array per file: in a solid archive a file is unpacked after the
   ones before it, keeping the dictionary's worth of their bytes. RAR 2.x compression (LZ and
   its audio mode) is read too. Not read: encrypted files or headers, archives in several
   volumes, and RAR 1.5 compression. */
'use strict';

part('rar', ({ readArchive: others, ZIP }) => {

const damaged = () => { throw new Error('this RAR file is damaged'); };
const decUtf8 = new TextDecoder('utf-8', { fatal: true }), decOem = new TextDecoder('ibm866');
const SPREAD = 'this file is spread over several volumes of the archive; osada reads single-volume archives';

/* ---- bits, most significant first; the input is padded with zeros ---- */
class Bits {
  constructor(b) { this.b = b; this.a = 0; this.bit = 0; }
  get16() { const b = this.b, a = this.a; return (((b[a] << 16) | (b[a + 1] << 8) | b[a + 2]) >>> (8 - this.bit)) & 0xFFFF; }
  get32() { const b = this.b, a = this.a; return ((((b[a] << 24) | (b[a + 1] << 16) | (b[a + 2] << 8) | b[a + 3]) << this.bit) | (b[a + 4] >>> (8 - this.bit))) >>> 0; }
  add(n) { n += this.bit; this.a += n >> 3; this.bit = n & 7; }
  align() { if (this.bit) { this.a++; this.bit = 0; } }
}
const padded = b => { const p = new Uint8Array(b.length + 64); p.set(b); return p; };

/* ---- Huffman tables, as RAR builds them: lengths up to 15, a quick table for short codes ---- */
const QB = 9;
class Huff {
  constructor(size) {
    this.size = size;
    this.len = new Uint32Array(17); this.pos = new Uint32Array(17); this.num = new Uint16Array(size);
    this.qlen = new Uint8Array(1 << QB); this.qnum = new Uint16Array(1 << QB);
  }
  make(lengths, off) {
    const n = this.size, count = new Uint32Array(16);
    for (let i = 0; i < n; i++) count[lengths[off + i] & 15]++;
    count[0] = 0;
    this.num.fill(0);
    this.pos[0] = 0; this.len[0] = 0;
    let upper = 0;
    for (let i = 1; i < 16; i++) {
      upper += count[i];
      this.len[i] = upper * 2 ** (16 - i);
      upper *= 2;
      this.pos[i] = this.pos[i - 1] + count[i - 1];
    }
    this.len[16] = 0x10000; this.pos[16] = this.pos[15] + count[15];
    const cp = this.pos.slice();
    for (let i = 0; i < n; i++) { const l = lengths[off + i] & 15; if (l) this.num[cp[l]++] = i; }
    let l = 1;
    for (let code = 0; code < 1 << QB; code++) {
      const bf = code << (16 - QB);
      while (l < 16 && bf >= this.len[l]) l++;
      this.qlen[code] = l;
      const p = this.pos[l] + ((bf - this.len[l - 1]) >>> (16 - l));
      this.qnum[code] = p < n ? this.num[p] : 0;
    }
  }
  decode(bits) {
    const bf = bits.get16() & 0xFFFE;
    if (bf < this.len[QB]) { const c = bf >>> (16 - QB); bits.add(this.qlen[c]); return this.qnum[c]; }
    let l = 15;
    for (let i = QB + 1; i < 15; i++) if (bf < this.len[i]) { l = i; break; }
    bits.add(l);
    const p = this.pos[l] + ((bf - this.len[l - 1]) >>> (16 - l));
    return p < this.size ? this.num[p] : 0;
  }
}

/* the bit lengths of the table of tables, then the tables (shared by RAR 3 and 5) */
function readLengths(bits, n, old) {
  const bl = new Uint8Array(20);
  for (let i = 0; i < 20; i++) {
    const l = bits.get16() >>> 12;
    bits.add(4);
    if (l === 15) {
      let z = bits.get16() >>> 12;
      bits.add(4);
      if (z === 0) bl[i] = 15;
      else { z += 2; while (z-- > 0 && i < 20) bl[i++] = 0; i--; }
    } else bl[i] = l;
  }
  const bd = new Huff(20);
  bd.make(bl, 0);
  const t = new Uint8Array(n);
  for (let i = 0; i < n;) {
    if (bits.a > bits.b.length - 8) return null;
    const x = bd.decode(bits);
    if (x < 16) { t[i] = old ? (x + old[i]) & 15 : x; i++; }
    else if (x < 18) {
      let k;
      if (x === 16) { k = (bits.get16() >>> 13) + 3; bits.add(3); } else { k = (bits.get16() >>> 9) + 11; bits.add(7); }
      if (i === 0) return null;
      while (k-- > 0 && i < n) { t[i] = t[i - 1]; i++; }
    } else {
      let k;
      if (x === 18) { k = (bits.get16() >>> 13) + 3; bits.add(3); } else { k = (bits.get16() >>> 9) + 11; bits.add(7); }
      while (k-- > 0 && i < n) t[i++] = 0;
    }
  }
  return t;
}

const u32 = (d, i) => (d[i] | (d[i + 1] << 8) | (d[i + 2] << 16) | (d[i + 3] << 24)) >>> 0;
const put32 = (d, i, v) => { d[i] = v; d[i + 1] = v >>> 8; d[i + 2] = v >>> 16; d[i + 3] = v >>> 24; };
function e8(d, n, fileOffset, e9, wrap) {
  for (let cur = 0; cur + 4 < n;) {
    const c = d[cur++];
    if (c === 0xE8 || (e9 && c === 0xE9)) {
      const offset = wrap ? (cur + fileOffset) % 0x1000000 : (cur + fileOffset) >>> 0, addr = u32(d, cur);
      if (addr & 0x80000000) { if (((addr + offset) & 0x80000000) === 0) put32(d, cur, addr + 0x1000000); }
      else if (((addr - 0x1000000) & 0x80000000) !== 0) put32(d, cur, addr - offset);
      cur += 4;
    }
  }
  return d;
}

/* the dictionary: one array per file, with the end of the one before it (solid); what is
   written from it (filters run) goes to out, at the moments RAR's own window would be written */
class Window {
  constructor() { this.w = new Uint8Array(0); this.ptr = 0; }
  start(size, keep, slack) {
    const k = Math.min(keep, this.ptr), w = new Uint8Array(k + size + slack);
    w.set(this.w.subarray(this.ptr - k, this.ptr), 0);
    this.w = w; this.ptr = k; this.base = k; this.end = k + size;
    this.out = new Uint8Array(size); this.o = 0; this.wr = k;
  }
  put(d, from, n) {
    const k = Math.min(n, this.out.length - this.o);
    if (k > 0) this.out.set(d.subarray(from, from + k), this.o);
    this.o += n;
  }
  done() { if (this.o < this.out.length) damaged(); return this.out; }
  copy(len, dist) {
    const w = this.w;
    let p = this.ptr, s = p - dist;
    if (s < 0 || dist === 0) damaged();
    if (p + len > w.length) damaged();
    while (len--) w[p++] = w[s++];
    this.ptr = p;
  }
}

/* ---------------- RAR 2.9 - 4 ---------------- */
const NC = 299, DC = 60, LDC = 17, RC = 28, TABLE = NC + DC + LDC + RC;
const LDECODE = [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224];
const LBITS = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5];
const DDECODE = [], DBITS = [];
{ let d = 0; [4, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 14, 0, 12].forEach((n, b) => { for (let j = 0; j < n; j++) { DDECODE.push(d); DBITS.push(b); d += 2 ** b; } }); }
const SDDECODE = [0, 4, 8, 16, 32, 64, 128, 192], SDBITS = [2, 2, 3, 4, 5, 6, 6, 6];
// the standard filters, known by their code's length and CRC
const STD = { '53,ad576887': 'e8', '57,3cd7e57e': 'e8e9', '120,3769893f': 'itanium', '29,0e06077d': 'delta', '149,1c2c5dc8': 'rgb', '216,bc85e701': 'audio' };

function vmData(v) {
  const d = v.get16();
  switch (d & 0xC000) {
    case 0: v.add(6); return (d >>> 10) & 0xF;
    case 0x4000:
      if ((d & 0x3C00) === 0) { v.add(14); return (0xFFFFFF00 | ((d >>> 2) & 0xFF)) >>> 0; }
      v.add(10); return (d >>> 6) & 0xFF;
    case 0x8000: { v.add(2); const x = v.get16(); v.add(16); return x; }
    default: { v.add(2); const hi = v.get16(); v.add(16); const lo = v.get16(); v.add(16); return (hi * 65536 + lo) >>> 0; }
  }
}

function itaniumBits(d, pos, n) { const a = pos >>> 3; return ((u32(d, a) >>> (pos & 7)) & (0xFFFFFFFF >>> (32 - n))) >>> 0; }
function itaniumSet(d, v, pos, n) {
  const a = pos >>> 3, b = pos & 7;
  let and = ~(((0xFFFFFFFF >>> (32 - n)) << b) >>> 0);
  v = (v << b) >>> 0;
  for (let i = 0; i < 4; i++) { d[a + i] = (d[a + i] & and) | v; and = (and >>> 8) | 0xFF000000; v >>>= 8; }
}

/* a RAR 3 filter over a block: its output */
function run3(f, data, fileOffset) {
  const r = f.r, n = r[4] >>> 0, out = () => new Uint8Array(n);
  switch (f.type) {
    case 'e8': case 'e8e9':
      if (n > 0x40000 || n < 4) return data;
      return e8(data, n, fileOffset, f.type === 'e8e9', false);
    case 'itanium': {
      if (n > 0x40000 || n < 21) return data;
      const masks = [4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4, 0, 0];
      let off = fileOffset >>> 4;
      for (let cur = 0; cur < n - 21; cur += 16, off++) {
        const t = (data[cur] & 0x1F) - 0x10;
        if (t < 0 || !masks[t]) continue;
        for (let i = 0; i <= 2; i++) {
          if (!(masks[t] & (1 << i))) continue;
          const sp = i * 41 + 5;
          if (itaniumBits(data.subarray(cur), sp + 37, 4) === 5) {
            const o = itaniumBits(data.subarray(cur), sp + 13, 20);
            itaniumSet(data.subarray(cur), (o - off) & 0xFFFFF, sp + 13, 20);
          }
        }
      }
      return data;
    }
    case 'delta': {
      const ch = r[0];
      if (n > 0x20000 || ch > 1024 || ch === 0) return data;
      const o = out();
      let s = 0;
      for (let c = 0; c < ch; c++) { let prev = 0; for (let d = c; d < n; d += ch) o[d] = prev = (prev - data[s++]) & 0xFF; }
      return o;
    }
    case 'rgb': {
      const width = r[0] - 3, posR = r[1];
      if (n > 0x20000 || n < 3 || width > n || posR > 2) return data;
      const o = out();
      let s = 0;
      for (let c = 0; c < 3; c++) {
        let prev = 0;
        for (let i = c; i < n; i += 3) {
          let pred = prev;
          if (i >= width + 3) {
            const up = o[i - width], ul = o[i - width - 3];
            pred = prev + up - ul;
            const pa = Math.abs(pred - prev), pb = Math.abs(pred - up), pc = Math.abs(pred - ul);
            pred = pa <= pb && pa <= pc ? prev : pb <= pc ? up : ul;
          }
          o[i] = prev = (pred - data[s++]) & 0xFF;
        }
      }
      for (let i = posR; i < n - 2; i += 3) { const g = o[i + 1]; o[i] = (o[i] + g) & 0xFF; o[i + 2] = (o[i + 2] + g) & 0xFF; }
      return o;
    }
    case 'audio': {
      const ch = r[0];
      if (n > 0x20000 || ch > 128 || ch === 0) return data;
      const o = out();
      let s = 0;
      const sb = x => (x << 24) >> 24;
      for (let c = 0; c < ch; c++) {
        let prev = 0, prevDelta = 0, d1 = 0, d2 = 0, d3, k1 = 0, k2 = 0, k3 = 0;
        const dif = [0, 0, 0, 0, 0, 0, 0];
        for (let i = c, count = 0; i < n; i += ch, count++) {
          d3 = d2; d2 = prevDelta - d1; d1 = prevDelta;
          let pred = (8 * prev + k1 * d1 + k2 * d2 + k3 * d3) >>> 0;
          pred = (pred >>> 3) & 0xFF;
          const cur = data[s++];
          pred = (pred - cur) >>> 0;
          o[i] = pred;
          prevDelta = sb(pred - prev);
          prev = pred & 0xFF;
          const d = sb(cur) * 8;
          dif[0] += Math.abs(d); dif[1] += Math.abs(d - d1); dif[2] += Math.abs(d + d1); dif[3] += Math.abs(d - d2);
          dif[4] += Math.abs(d + d2); dif[5] += Math.abs(d - d3); dif[6] += Math.abs(d + d3);
          if ((count & 0x1F) === 0) {
            let min = dif[0], at = 0;
            dif[0] = 0;
            for (let j = 1; j < 7; j++) { if (dif[j] < min) { min = dif[j]; at = j; } dif[j] = 0; }
            if (at === 1) { if (k1 >= -16) k1--; } else if (at === 2) { if (k1 < 16) k1++; } else if (at === 3) { if (k2 >= -16) k2--; }
            else if (at === 4) { if (k2 < 16) k2++; } else if (at === 5) { if (k3 >= -16) k3--; } else if (at === 6) { if (k3 < 16) k3++; }
          }
        }
      }
      return o;
    }
    default: return data;
  }
}

class Unpack3 {
  constructor() {
    this.win = new Window();
    this.ld = new Huff(NC); this.dd = new Huff(DC); this.ldd = new Huff(LDC); this.rd = new Huff(RC);
    this.reset();
  }
  reset() {
    this.old = [0, 0, 0, 0]; this.lastLength = 0; this.prevLow = 0; this.lowRep = 0;
    this.oldTable = new Uint8Array(TABLE); this.tablesRead = false;
    this.escChar = 2; this.ppmBlock = false;
    this.filters = []; this.oldLens = []; this.lastFilter = 0;
  }
  readTables() {
    const bits = this.bits;
    bits.align();
    const bf = bits.get16();
    if (bf & 0x8000) { this.ppmBlock = true; return this.ppmInit(); }
    this.ppmBlock = false;
    this.prevLow = 0; this.lowRep = 0;
    if (!(bf & 0x4000)) this.oldTable.fill(0);
    bits.add(2);
    const t = readLengths(bits, TABLE, this.oldTable);
    if (!t) return false;
    this.tablesRead = true;
    this.ld.make(t, 0); this.dd.make(t, NC); this.ldd.make(t, NC + DC); this.rd.make(t, NC + DC + LDC);
    this.oldTable = t;
    return true;
  }
  ppmInit() {
    const bits = this.bits, next = () => bits.b[bits.a++] || 0;
    const flags = next(), reset = flags & 0x20;
    let mb = 0;
    if (reset) mb = next(); else if (!this.ppm) return false;
    if (flags & 0x40) this.escChar = next();
    this.rc = new this.PPMD.RangeRar(next);
    if (reset) {
      let order = (flags & 0x1F) + 1;
      if (order > 16) order = 16 + (order - 16) * 3;
      if (order === 1) { this.ppm = null; return false; }
      this.ppm = new this.PPMD.Model((mb + 1) << 20, true);
      this.ppm.init(order);
    }
    return true;
  }
  ppmChar() { const c = this.ppm.decode(this.rc); return c < 0 ? -1 : c; }
  addVMCode(first, code) {
    const v = new Bits(padded(code));
    let pos;
    if (first & 0x80) { pos = vmData(v); if (pos === 0) { this.filters = []; this.oldLens = []; this.stack = []; } else pos--; }
    else pos = this.lastFilter;
    if (pos > this.filters.length || pos > this.oldLens.length) return false;
    this.lastFilter = pos;
    const isNew = pos === this.filters.length;
    let filter;
    if (isNew) { if (pos > 8192) return false; filter = { type: null }; this.filters.push(filter); this.oldLens.push(0); }
    else filter = this.filters[pos];
    this.stack = this.stack.filter(Boolean);
    if (this.stack.length > 8192) return false;
    const sf = { r: [0, 0, 0, 0, 0, 0, 0] };
    this.stack.push(sf);
    let start = vmData(v);
    if (first & 0x40) start += 258;
    sf.start = this.win.ptr + start;
    if (first & 0x20) { sf.len = vmData(v); this.oldLens[pos] = sf.len; } else sf.len = pos < this.oldLens.length ? this.oldLens[pos] : 0;
    sf.r[4] = sf.len;
    if (first & 0x10) {
      const mask = v.get16() >>> 9;
      v.add(7);
      for (let i = 0; i < 7; i++) if (mask & (1 << i)) sf.r[i] = vmData(v);
    }
    if (isNew) {
      const size = vmData(v);
      if (size >= 0x10000 || size === 0 || v.a + size > code.length) return false;
      const vm = new Uint8Array(size);
      for (let i = 0; i < size; i++) { vm[i] = v.get16() >>> 8; v.add(8); }
      let x = 0;
      for (let i = 1; i < size; i++) x ^= vm[i];
      filter.type = x === vm[0] ? STD[size + ',' + ZIP.crc(vm).toString(16).padStart(8, '0')] || null : null;
    }
    sf.type = filter.type;
    return true;
  }
  readVMCode() {
    const bits = this.bits, first = bits.get16() >>> 8;
    bits.add(8);
    let len = (first & 7) + 1;
    if (len === 7) { len = (bits.get16() >>> 8) + 7; bits.add(8); }
    else if (len === 8) { len = bits.get16(); bits.add(16); }
    if (len === 0) return false;
    const code = new Uint8Array(len);
    for (let i = 0; i < len; i++) { code[i] = bits.get16() >>> 8; bits.add(8); }
    return this.addVMCode(first, code);
  }
  readVMCodePPM() {
    const first = this.ppmChar();
    if (first < 0) return false;
    let len = (first & 7) + 1;
    if (len === 7) { const b1 = this.ppmChar(); if (b1 < 0) return false; len = b1 + 7; }
    else if (len === 8) { const b1 = this.ppmChar(), b2 = this.ppmChar(); if (b1 < 0 || b2 < 0) return false; len = b1 * 256 + b2; }
    if (len === 0) return false;
    const code = new Uint8Array(len);
    for (let i = 0; i < len; i++) { const c = this.ppmChar(); if (c < 0) return false; code[i] = c; }
    return this.addVMCode(first, code);
  }
  insertOld(d) { this.old[3] = this.old[2]; this.old[2] = this.old[1]; this.old[1] = this.old[0]; this.old[0] = d; }
  /* one file: its packed bytes, the size it unpacks to, solid (after the one before) */
  file(packed, size, solid, dict) {
    const win = this.win;
    if (!solid) this.reset();
    this.stack = [];
    win.start(size, solid ? dict : 0, 0x10000);
    const bits = this.bits = new Bits(padded(packed)), limit = packed.length + 8, w = win.w;
    this.winSize = Math.max(dict, 0x40000);
    if ((!solid || !this.tablesRead) && !this.readTables()) { this.write(); return win.done(); }
    const end = win.end + 0x10000 - 300;
    for (;;) {
      if (bits.a > limit || win.ptr > end) break;
      if (win.ptr - win.wr > this.winSize - 260) this.write();
      if (this.ppmBlock) {
        const ch = this.ppmChar();
        if (ch < 0) { this.ppm = null; this.ppmBlock = false; break; }
        if (ch === this.escChar) {
          const nc = this.ppmChar();
          if (nc === 0) { if (!this.readTables()) break; continue; }
          if (nc < 0 || nc === 2) break;
          if (nc === 3) { if (!this.readVMCodePPM()) break; continue; }
          if (nc === 4) {
            let dist = 0, len = 0, bad = false;
            for (let i = 0; i < 4 && !bad; i++) { const c = this.ppmChar(); if (c < 0) bad = true; else if (i === 3) len = c; else dist = dist * 256 + c; }
            if (bad) break;
            win.copy(len + 32, dist + 2);
            continue;
          }
          if (nc === 5) { const l = this.ppmChar(); if (l < 0) break; win.copy(l + 4, 1); continue; }
        }
        w[win.ptr++] = ch;
        continue;
      }
      const n = this.ld.decode(bits);
      if (n < 256) { w[win.ptr++] = n; continue; }
      if (n >= 271) {
        let k = n - 271, len = LDECODE[k] + 3, b = LBITS[k];
        if (b > 0) { len += bits.get16() >>> (16 - b); bits.add(b); }
        const dn = this.dd.decode(bits);
        let dist = DDECODE[dn] + 1;
        b = DBITS[dn];
        if (b > 0) {
          if (dn > 9) {
            if (b > 4) { dist += (bits.get16() >>> (20 - b)) << 4; bits.add(b - 4); }
            if (this.lowRep > 0) { this.lowRep--; dist += this.prevLow; }
            else {
              const low = this.ldd.decode(bits);
              if (low === 16) { this.lowRep = 15; dist += this.prevLow; } else { dist += low; this.prevLow = low; }
            }
          } else { dist += bits.get16() >>> (16 - b); bits.add(b); }
        }
        if (dist >= 0x2000) { len++; if (dist >= 0x40000) len++; }
        this.insertOld(dist);
        this.lastLength = len;
        win.copy(len, dist);
        continue;
      }
      if (n === 256) {
        // end of block: "1" new tables here; "00" next file; "01" next file, new tables there
        const bf = bits.get16();
        let newTable, newFile = false;
        if (bf & 0x8000) { newTable = true; bits.add(1); } else { newFile = true; newTable = !!(bf & 0x4000); bits.add(2); }
        this.tablesRead = !newTable;
        if (newFile) break;
        if (!this.readTables()) break;
        continue;
      }
      if (n === 257) { if (!this.readVMCode()) break; continue; }
      if (n === 258) { if (this.lastLength) win.copy(this.lastLength, this.old[0]); continue; }
      if (n < 263) {
        const k = n - 259, dist = this.old[k];
        for (let i = k; i > 0; i--) this.old[i] = this.old[i - 1];
        this.old[0] = dist;
        const ln = this.rd.decode(bits);
        let len = LDECODE[ln] + 2;
        const b = LBITS[ln];
        if (b > 0) { len += bits.get16() >>> (16 - b); bits.add(b); }
        this.lastLength = len;
        win.copy(len, dist);
        continue;
      }
      // n < 272: a short distance
      const k = n - 263;
      let dist = SDDECODE[k] + 1;
      const b = SDBITS[k];
      if (b > 0) { dist += bits.get16() >>> (16 - b); bits.add(b); }
      this.insertOld(dist);
      this.lastLength = 2;
      win.copy(2, dist);
    }
    this.write();
    return win.done();
  }
  /* write what is unpacked, the filters run over their blocks (unrar's UnpWriteBuf30) */
  write() {
    const win = this.win, stack = this.stack;
    let border = win.wr, left = win.ptr - border;
    for (let i = 0; i < stack.length; i++) {
      const f = stack[i];
      if (!f || f.start - border < 0 || f.start - border >= left) continue;
      if (border !== f.start) { win.put(win.w, border, f.start - border); border = f.start; left = win.ptr - border; }
      if (f.len > left) { win.wr = border; return; }
      let data = run3(f, win.w.slice(f.start, f.start + f.len), win.o), size = f.r[4] & 0x3FFFF;
      stack[i] = null;
      while (i + 1 < stack.length) {
        const g = stack[i + 1];
        if (!g || g.start !== f.start || g.len !== size) break;
        const d = new Uint8Array(Math.max(size, g.len));
        d.set(data.subarray(0, size));
        data = run3(g, d, win.o);
        size = g.r[4] & 0x3FFFF;
        stack[++i] = null;
      }
      win.put(data, 0, size);
      border = f.start + f.len;
      left = win.ptr - border;
    }
    win.put(win.w, border, win.ptr - border);
    win.wr = win.ptr;
  }
}

/* ---------------- RAR 2.x ---------------- */
const NC2 = 298, DC2 = 48, RC2 = 28, MC2 = 257;
const DDECODE2 = DDECODE.slice(0, 48), DBITS2 = DBITS.slice(0, 48);

class Unpack2 {
  constructor() {
    this.win = new Window();
    this.ld = new Huff(NC2); this.dd = new Huff(DC2); this.rd = new Huff(RC2);
    this.md = [0, 1, 2, 3].map(() => new Huff(MC2));
    this.reset();
  }
  reset() {
    this.tablesRead = false; this.audio = false; this.chDelta = 0; this.ch = 0; this.channels = 1;
    this.aud = [0, 1, 2, 3].map(() => ({ k: [0, 0, 0, 0, 0], d: [0, 0, 0, 0], last: 0, lastDelta: 0, count: 0, dif: new Array(11).fill(0) }));
    this.oldTable = new Uint8Array(MC2 * 4);
    this.old = [0, 0, 0, 0]; this.oldPtr = 0; this.lastDist = 0; this.lastLength = 0;
  }
  tables() {
    const bits = this.bits, bf = bits.get16();
    this.audio = !!(bf & 0x8000);
    if (!(bf & 0x4000)) this.oldTable.fill(0);
    bits.add(2);
    let n;
    if (this.audio) {
      this.channels = ((bf >>> 12) & 3) + 1;
      if (this.ch >= this.channels) this.ch = 0;
      bits.add(2);
      n = MC2 * this.channels;
    } else n = NC2 + DC2 + RC2;
    const bl = new Uint8Array(19);
    for (let i = 0; i < 19; i++) { bl[i] = bits.get16() >>> 12; bits.add(4); }
    const bd = new Huff(19);
    bd.make(bl, 0);
    const t = new Uint8Array(n);
    for (let i = 0; i < n;) {
      if (bits.a > bits.b.length - 8) return false;
      const x = bd.decode(bits);
      if (x < 16) { t[i] = (x + this.oldTable[i]) & 15; i++; }
      else if (x === 16) {
        let k = (bits.get16() >>> 14) + 3;
        bits.add(2);
        if (i === 0) return false;
        while (k-- > 0 && i < n) { t[i] = t[i - 1]; i++; }
      } else {
        let k;
        if (x === 17) { k = (bits.get16() >>> 13) + 3; bits.add(3); } else { k = (bits.get16() >>> 9) + 11; bits.add(7); }
        while (k-- > 0 && i < n) t[i++] = 0;
      }
    }
    this.tablesRead = true;
    if (this.audio) for (let i = 0; i < this.channels; i++) this.md[i].make(t, i * MC2);
    else { this.ld.make(t, 0); this.dd.make(t, NC2); this.rd.make(t, NC2 + DC2); }
    this.oldTable.set(t);
    return true;
  }
  audioByte(delta) {
    const v = this.aud[this.ch], d = v.d, k = v.k, sb = x => (x << 24) >> 24;
    v.count++;
    d[3] = d[2]; d[2] = d[1]; d[1] = v.lastDelta - d[0]; d[0] = v.lastDelta;
    const pch = ((8 * v.last + k[0] * d[0] + k[1] * d[1] + k[2] * d[2] + k[3] * d[3] + k[4] * this.chDelta) >> 3) & 0xFF;
    const ch = (pch - delta) >>> 0;
    const D = sb(delta) * 8, dif = v.dif;
    dif[0] += Math.abs(D); dif[1] += Math.abs(D - d[0]); dif[2] += Math.abs(D + d[0]); dif[3] += Math.abs(D - d[1]); dif[4] += Math.abs(D + d[1]);
    dif[5] += Math.abs(D - d[2]); dif[6] += Math.abs(D + d[2]); dif[7] += Math.abs(D - d[3]); dif[8] += Math.abs(D + d[3]);
    dif[9] += Math.abs(D - this.chDelta); dif[10] += Math.abs(D + this.chDelta);
    this.chDelta = v.lastDelta = sb(ch - v.last);
    v.last = ch & 0xFF;
    if ((v.count & 0x1F) === 0) {
      let min = dif[0], at = 0;
      dif[0] = 0;
      for (let j = 1; j < 11; j++) { if (dif[j] < min) { min = dif[j]; at = j; } dif[j] = 0; }
      if (at) { const i = (at - 1) >> 1; if (at & 1) { if (k[i] >= -16) k[i]--; } else if (k[i] < 16) k[i]++; }
    }
    return ch & 0xFF;
  }
  copy(len, dist) {
    this.lastDist = this.old[this.oldPtr++ & 3] = dist;
    this.lastLength = len;
    this.left -= len;
    this.win.copy(len, dist);
  }
  file(packed, size, solid, dict) {
    const win = this.win, bits = this.bits = new Bits(padded(packed)), limit = packed.length + 8;
    if (!solid) this.reset();
    win.start(size, solid ? dict : 0, 0x10000);
    const w = win.w, end = win.end + 0x10000 - 300;
    if ((!solid || !this.tablesRead) && !this.tables()) { win.put(w, win.base, win.ptr - win.base); return win.done(); }
    this.left = size - 1;
    while (this.left >= 0) {
      if (bits.a > limit || win.ptr > end) break;
      if (this.audio) {
        const x = this.md[this.ch].decode(bits);
        if (x === 256) { if (!this.tables()) break; continue; }
        w[win.ptr++] = this.audioByte(x);
        if (++this.ch === this.channels) this.ch = 0;
        this.left--;
        continue;
      }
      const n = this.ld.decode(bits);
      if (n < 256) { w[win.ptr++] = n; this.left--; continue; }
      if (n > 269) {
        const k = n - 270;
        let len = LDECODE[k] + 3, b = LBITS[k];
        if (b > 0) { len += bits.get16() >>> (16 - b); bits.add(b); }
        const dn = this.dd.decode(bits);
        let dist = DDECODE2[dn] + 1;
        b = DBITS2[dn];
        if (b > 0) { dist += bits.get16() >>> (16 - b); bits.add(b); }
        if (dist >= 0x2000) { len++; if (dist >= 0x40000) len++; }
        this.copy(len, dist);
        continue;
      }
      if (n === 269) { if (!this.tables()) break; continue; }
      if (n === 256) { this.copy(this.lastLength, this.lastDist); continue; }
      if (n < 261) {
        const dist = this.old[(this.oldPtr - (n - 256)) & 3], ln = this.rd.decode(bits);
        let len = LDECODE[ln] + 2;
        const b = LBITS[ln];
        if (b > 0) { len += bits.get16() >>> (16 - b); bits.add(b); }
        if (dist >= 0x101) { len++; if (dist >= 0x2000) { len++; if (dist >= 0x40000) len++; } }
        this.copy(len, dist);
        continue;
      }
      const k = n - 261;
      let dist = SDDECODE[k] + 1;
      const b = SDBITS[k];
      if (b > 0) { dist += bits.get16() >>> (16 - b); bits.add(b); }
      this.copy(2, dist);
    }
    // the tables of the next file, when they come here (solid)
    if (bits.a + 5 <= packed.length) {
      if (this.audio) { if (this.md[this.ch].decode(bits) === 256) this.tables(); }
      else if (this.ld.decode(bits) === 269) this.tables();
    }
    win.put(w, win.base, win.ptr - win.base);
    return win.done();
  }
}

/* ---------------- RAR 5 ---------------- */
class Unpack5 {
  constructor() {
    this.win = new Window();
    this.tablesRead = false;
    this.old = [0, 0, 0, 0]; this.lastLength = 0;
  }
  blockHeader() {
    const bits = this.bits;
    bits.align();
    const flags = bits.get16() >>> 8;
    bits.add(8);
    const nb = ((flags >> 3) & 3) + 1;
    if (nb === 4) return false;
    this.blockBits = (flags & 7) + 1;
    const sum = bits.get16() >>> 8;
    bits.add(8);
    let size = 0;
    for (let i = 0; i < nb; i++) { size += (bits.get16() >>> 8) * 2 ** (8 * i); bits.add(8); }
    if (((0x5A ^ flags ^ size ^ (size >>> 8) ^ (size >>> 16)) & 0xFF) !== sum) return false;
    this.blockStart = bits.a;
    this.blockEnd = bits.a + size - 1;
    this.lastBlock = !!(flags & 0x40);
    this.tablePresent = !!(flags & 0x80);
    return true;
  }
  tables() {
    if (!this.tablePresent) return true;
    const dc = this.dc, t = readLengths(this.bits, 306 + dc + 16 + 44, null);
    if (!t) return false;
    this.ld = new Huff(306); this.dd = new Huff(dc); this.ldd = new Huff(16); this.rd = new Huff(44);
    this.ld.make(t, 0); this.dd.make(t, 306); this.ldd.make(t, 306 + dc); this.rd.make(t, 306 + dc + 16);
    this.tablesRead = true;
    return true;
  }
  slotLength(slot) {
    let len = 2, lb = 0;
    if (slot < 8) len += slot;
    else { lb = (slot >> 2) - 1; len += (4 | (slot & 3)) << lb; }
    if (lb > 0) { len += this.bits.get16() >>> (16 - lb); this.bits.add(lb); }
    return len;
  }
  filterData() {
    const bits = this.bits, n = (bits.get16() >>> 14) + 1;
    bits.add(2);
    let d = 0;
    for (let i = 0; i < n; i++) { d += (bits.get16() >>> 8) * 2 ** (8 * i); bits.add(8); }
    return d;
  }
  file(packed, size, solid, dict, v7) {
    const win = this.win, bits = this.bits = new Bits(padded(packed)), limit = packed.length + 8;
    this.dc = v7 ? 80 : 64;
    if (!solid) { this.tablesRead = false; this.old = [0, 0, 0, 0]; this.lastLength = 0; }
    this.filters = [];
    win.start(size, solid ? dict : 0, 0x10000);
    this.winSize = Math.max(dict, 0x40000);
    this.border = win.ptr + Math.min(this.winSize, 0x400000);
    const w = win.w, end = win.end + 0x10000 - 0x1100;
    if (!this.blockHeader() || !this.tables() || !this.tablesRead) { this.write(); return win.done(); }
    for (;;) {
      let done = false;
      while (bits.a > this.blockEnd || (bits.a === this.blockEnd && bits.bit >= this.blockBits)) {
        if (this.lastBlock) { done = true; break; }
        if (!this.blockHeader() || !this.tables()) { done = true; break; }
      }
      if (done || bits.a > limit || win.ptr > end) break;
      if (this.border - win.ptr < 0x1004 && this.border !== win.ptr) this.write();
      const slot = this.ld.decode(bits);
      if (slot < 256) { w[win.ptr++] = slot; continue; }
      if (slot >= 262) {
        let len = this.slotLength(slot - 262);
        const ds = this.dd.decode(bits);
        let dist = 1, db = 0;
        if (ds < 4) dist += ds;
        else { db = (ds >> 1) - 1; dist += (2 | (ds & 1)) * 2 ** db; }
        if (db > 0) {
          if (db >= 4) {
            if (db > 4) { dist += (bits.get32() >>> (36 - db)) * 16; bits.add(db - 4); }
            dist += this.ldd.decode(bits);
          } else { dist += bits.get32() >>> (32 - db); bits.add(db); }
        }
        if (dist > 0x100) { len++; if (dist > 0x2000) { len++; if (dist > 0x40000) len++; } }
        this.old[3] = this.old[2]; this.old[2] = this.old[1]; this.old[1] = this.old[0]; this.old[0] = dist;
        this.lastLength = len;
        win.copy(len, dist);
        continue;
      }
      if (slot === 256) {
        const start = this.filterData();
        let len = this.filterData();
        if (len > 0x400000) len = 0;
        const type = bits.get16() >>> 13;
        bits.add(3);
        let ch = 0;
        if (type === 0) { ch = (bits.get16() >>> 11) + 1; bits.add(5); }
        this.filters.push({ start: win.ptr + start, len, type, ch });
        continue;
      }
      if (slot === 257) { if (this.lastLength) win.copy(this.lastLength, this.old[0]); continue; }
      const k = slot - 258, dist = this.old[k];
      for (let i = k; i > 0; i--) this.old[i] = this.old[i - 1];
      this.old[0] = dist;
      const len = this.slotLength(this.rd.decode(bits));
      this.lastLength = len;
      win.copy(len, dist);
    }
    this.write();
    return win.done();
  }
  /* write what is unpacked, the filters run over their blocks (unrar's UnpWriteBuf) */
  write() {
    const win = this.win;
    let border = win.wr, left = win.ptr - border, all = true;
    for (const f of this.filters) {
      if (f.done || f.start - border < 0 || f.start - border >= left) continue;
      if (border !== f.start) { win.put(win.w, border, f.start - border); border = f.start; left = win.ptr - border; }
      if (f.len > left) { win.wr = border; all = false; break; }
      f.done = true;
      if (!f.len) continue;
      const d = win.w.slice(f.start, f.start + f.len), at = win.o;
      let res = d;
      if (f.type === 1 || f.type === 2) e8(d, f.len, at, f.type === 2, true);
      else if (f.type === 3) {
        for (let c = 0; c + 3 < f.len; c += 4) {
          if (d[c + 3] !== 0xEB) continue;
          const off = (d[c] + d[c + 1] * 256 + d[c + 2] * 65536 - Math.floor((at + c) / 4)) >>> 0;
          d[c] = off; d[c + 1] = off >>> 8; d[c + 2] = off >>> 16;
        }
      } else if (f.type === 0) {
        res = new Uint8Array(f.len);
        let s = 0;
        for (let c = 0; c < f.ch; c++) { let prev = 0; for (let p = c; p < f.len; p += f.ch) res[p] = prev = (prev - d[s++]) & 0xFF; }
      }
      win.put(res, 0, f.len);
      border = f.start + f.len;
      left = win.ptr - border;
    }
    this.filters = this.filters.filter(f => !f.done);
    if (all) { win.put(win.w, border, win.ptr - border); win.wr = win.ptr; }
    this.border = win.ptr + Math.min(this.winSize, 0x400000);
  }
}

/* ---------------- the headers ---------------- */
function dosTime(t) {
  const d = t >>> 16;
  return t ? Math.floor(new Date((d >> 9) + 1980, ((d >> 5) & 15) - 1, d & 31, (t >> 11) & 31, (t >> 5) & 63, (t & 31) * 2) / 1000) : 0;
}
/* RAR 3's names: the ASCII name, a 0, then its Unicode spelled against it */
function unicodeName(name, enc) {
  let ep = 0, out = '', flags = 0, fb = 0;
  const high = enc.length ? enc[ep++] : 0;
  const codes = [];
  while (ep < enc.length) {
    if (fb === 0) { flags = enc[ep++]; fb = 8; }
    switch (flags >> 6) {
      case 0: if (ep >= enc.length) break; codes.push(enc[ep++]); break;
      case 1: if (ep >= enc.length) break; codes.push(enc[ep++] + (high << 8)); break;
      case 2: if (ep + 1 >= enc.length) break; codes.push(enc[ep] + (enc[ep + 1] << 8)); ep += 2; break;
      case 3: {
        if (ep >= enc.length) break;
        let len = enc[ep++];
        if (len & 0x80) {
          if (ep >= enc.length) break;
          const corr = enc[ep++];
          for (len = (len & 0x7F) + 2; len > 0; len--) { const p = codes.length; codes.push(((name[p] + corr) & 0xFF) + (high << 8)); }
        } else for (len += 2; len > 0; len--) codes.push(name[codes.length]);
        break;
      }
    }
    flags = (flags << 2) & 0xFF;
    fb -= 2;
  }
  for (const c of codes) out += String.fromCharCode(c);
  return out;
}

function read4(b) {
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength), files = [];
  let p = 7;
  while (p + 7 <= b.length) {
    const type = b[p + 2], flags = dv.getUint16(p + 3, true), size = dv.getUint16(p + 5, true);
    if (size < 7) damaged();
    let add = flags & 0x8000 ? dv.getUint32(p + 7, true) : 0;
    if (type === 0x73 && (flags & 0x80)) throw new Error('encrypted: osada does not open it');
    if (type === 0x74) {
      let q = p + 7;
      let pack = dv.getUint32(q, true), unp = dv.getUint32(q + 4, true);
      const host = b[q + 8], crc = dv.getUint32(q + 9, true), ftime = dv.getUint32(q + 13, true), ver = b[q + 17], method = b[q + 18], nlen = dv.getUint16(q + 19, true), attr = dv.getUint32(q + 21, true);
      q += 25;
      if (flags & 0x100) { pack += dv.getUint32(q, true) * 2 ** 32; unp += dv.getUint32(q + 4, true) * 2 ** 32; q += 8; }
      add = pack;
      const raw = b.subarray(q, q + nlen);
      let name;
      const z = raw.indexOf(0);
      if ((flags & 0x200) && z >= 0) name = unicodeName(raw.subarray(0, z), raw.subarray(z + 1));
      else { try { name = decUtf8.decode(raw); } catch (e) { name = decOem.decode(raw); } }
      const dir = (flags & 0xE0) === 0xE0 || (host === 3 ? (attr & 0xF000) === 0x4000 : !!(attr & 0x10));
      files.push({
        name: name.replace(/\\/g, '/'), dir, size: unp, time: dosTime(ftime), crc, data: [p + size, p + size + pack],
        method: method - 0x30, ver, solid: !!(flags & 0x10), dict: 0x10000 << ((flags >> 5) & 7),
        locked: !!(flags & 0x04), spread: !!(flags & 0x03), link: host === 3 && (attr & 0xF000) === 0xA000 ? '' : null,
      });
    } else if (type === 0x7B) break;
    p += size + add;
  }
  return files;
}

function read5(b) {
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength), files = [];
  let p = 8;
  const vint = () => { let v = 0, s = 1; for (let i = 0; i < 10; i++) { if (p >= b.length) damaged(); const c = b[p++]; v += (c & 0x7F) * s; s *= 128; if (!(c & 0x80)) return v; } damaged(); };
  while (p + 4 < b.length) {
    p += 4;
    const hsize = vint(), hstart = p, type = vint(), hflags = vint();
    const extra = hflags & 1 ? vint() : 0, dataSize = hflags & 2 ? vint() : 0, hend = hstart + hsize;
    if (hend > b.length) damaged();
    if (type === 4) throw new Error('encrypted: osada does not open it');
    if (type === 5) break;
    if (type === 2) {
      const ff = vint(), unp = vint(), attr = vint();
      let time = 0, crc;
      if (ff & 2) { time = dv.getUint32(p, true); p += 4; }
      if (ff & 4) { crc = dv.getUint32(p, true); p += 4; }
      const comp = vint(), host = vint(), nlen = vint();
      let name;
      try { name = decUtf8.decode(b.subarray(p, p + nlen)); } catch (e) { name = decOem.decode(b.subarray(p, p + nlen)); }
      let locked = false, link = null, same = null;
      for (p = hend - extra; p < hend;) {
        const rsize = vint(), rstart = p, rtype = vint();
        if (rtype === 1) locked = true;
        else if (rtype === 3) {
          const tf = vint();
          if (tf & 2) time = tf & 1 ? dv.getUint32(p, true) : Math.max(0, Math.floor((dv.getUint32(p + 4, true) * 2 ** 32 + dv.getUint32(p, true)) / 1e7 - 11644473600));
        } else if (rtype === 5) {
          // a link: symbolic (1-3) shown as such; hard link or copy (4, 5): the file it names
          const kind = vint();
          vint();
          const n = vint(), to = new TextDecoder().decode(b.subarray(p, p + n)).replace(/\\/g, '/');
          if (kind >= 4) same = to; else link = to;
        }
        p = rstart + rsize;
      }
      const dictBits = (comp >> 10) & 0x1F;
      files.push({
        name: name.replace(/\\/g, '/'), dir: !!(ff & 1), size: unp, time, crc: ff & 8 ? undefined : crc, data: [hend, hend + dataSize],
        method: (comp >> 7) & 7, ver: 50, v7: (comp & 0x3F) === 1, solid: !!(comp & 0x40), dict: Math.min(2 ** 32, 0x20000 * 2 ** dictBits),
        locked, spread: !!(hflags & 0x18), link, same, unknownSize: !!(ff & 8),
      });
    }
    p = hend + dataSize;
  }
  return files;
}

async function readRar(b) {
  const five = b[6] === 1;
  const files = five ? read5(b) : read4(b);
  let dec = null, next = 0, ppmd = null;
  // file i, after the ones before it in its solid run
  const unpack = async i => {
    const f = files[i];
    if (f.locked) throw new Error('encrypted: osada does not open it');
    if (f.spread) throw new Error(SPREAD);
    if (f.data[1] > b.length) throw new Error('this RAR file is cut short');
    if (f.method === 0) return b.subarray(f.data[0], f.data[1]);
    if (!five && f.ver !== 29 && f.ver !== 36 && f.ver !== 20 && f.ver !== 26) throw new Error('this file was packed by RAR 1.5, which osada does not read');
    if (f.size > 1536 << 20) throw new Error('too big to unpack in the browser');
    let from = i;
    while (from > 0 && files[from].solid) from--;
    if (!dec || next !== i || from === i) { dec = null; next = from; }
    const two = !five && (f.ver === 20 || f.ver === 26);
    if (!five && !two && !ppmd) { ppmd = (await hold('ppmd')).PPMD; setTimeout(() => release('ppmd'), 0); }
    for (; next <= i; next++) {
      const g = files[next];
      if (g.dir || g.method === 0 || g.locked || g.spread) continue;
      if (!dec) dec = five ? new Unpack5() : two ? new Unpack2() : new Unpack3();
      dec.PPMD = ppmd;
      const out = five ? dec.file(b.subarray(g.data[0], g.data[1]), g.size, next !== from, g.dict, g.v7) : dec.file(b.subarray(g.data[0], g.data[1]), g.size, next !== from, g.dict);
      if (next === i) {
        next = i + 1;
        if (f.crc !== undefined && ZIP.crc(out) !== f.crc) { dec = null; throw new Error('this RAR file is damaged (CRC)'); }
        return out;
      }
    }
    damaged();
  };
  // one unpacking at a time: a solid run goes forward
  let queue = Promise.resolve();
  const read = i => { const r = queue.then(() => unpack(i)); queue = r.catch(() => {}); return r; };
  const entries = files.map((f, i) => {
    const to = f.same !== null && f.same !== undefined ? files.findIndex(g => g.name === f.same && !g.dir) : -1;
    return {
      path: f.dir ? f.name + '/' : f.name, dir: f.dir, size: to >= 0 ? files[to].size : f.size, time: f.time, locked: f.locked || f.spread, link: f.link,
      read: () => read(to >= 0 ? to : i),
    };
  });
  return { kind: five ? 'rar5' : 'rar', entries };
}

async function readArchive(bytes, name) {
  if (bytes[0] === 0x52 && bytes[1] === 0x61 && bytes[2] === 0x72 && bytes[3] === 0x21 && bytes[4] === 0x1A && bytes[5] === 0x07 && (bytes[6] === 0 || (bytes[6] === 1 && bytes[7] === 0))) return readRar(bytes);
  return others(bytes, name);
}

return { readArchive, RAR: { read: readRar } };
});
