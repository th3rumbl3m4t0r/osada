/* LZMA and LZMA2, the branch filters (x86 BCJ, ARM, ARM Thumb, ARM64, PowerPC, SPARC) and
   delta: what 7z and xz files are packed with. Part lzma. Everything is decoded into one
   array of the known size, which is also the dictionary. XZ.decode reads an .xz file (its
   blocks: LZMA2 with the filters before it; the index is not needed to read it). */
'use strict';

part('lzma', () => {

const bad = what => { throw new Error('damaged ' + what + ' data'); };

/* ---- the range decoder ---- */
class Range {
  constructor(b, p) {
    this.b = b;
    if (b[p] !== 0) bad('LZMA');
    this.p = p + 5;
    this.range = 0xFFFFFFFF;
    this.code = ((b[p + 1] << 24) | (b[p + 2] << 16) | (b[p + 3] << 8) | b[p + 4]) >>> 0;
  }
  bit(probs, i) {
    const pr = probs[i], bound = (this.range >>> 11) * pr;
    let r;
    if (this.code < bound) { this.range = bound; probs[i] = pr + ((2048 - pr) >>> 5); r = 0; }
    else { this.range -= bound; this.code -= bound; probs[i] = pr - (pr >>> 5); r = 1; }
    if (this.range < 0x1000000) { this.range = (this.range * 256) >>> 0; this.code = ((this.code * 256) + this.b[this.p++]) >>> 0; }
    return r;
  }
  direct(n) {
    let res = 0;
    while (n--) {
      this.range >>>= 1;
      let t = 0;
      if (this.code >= this.range) { this.code -= this.range; t = 1; }
      res = ((res * 2) + t) >>> 0;
      if (this.range < 0x1000000) { this.range = (this.range * 256) >>> 0; this.code = ((this.code * 256) + this.b[this.p++]) >>> 0; }
    }
    return res;
  }
  tree(probs, at, bits) { let m = 1; for (let i = 0; i < bits; i++) m = (m << 1) + this.bit(probs, at + m); return m - (1 << bits); }
  reverse(probs, at, bits) { let m = 1, s = 0; for (let i = 0; i < bits; i++) { const b = this.bit(probs, at + m); m = (m << 1) + b; s |= b << i; } return s; }
}

const probs = n => new Uint16Array(n).fill(1024);

class LenDec {
  constructor() { this.p = probs(2 + 16 * 8 * 2 + 256); }   // choice, choice2, low[16][8], mid[16][8], high[256]
  reset() { this.p.fill(1024); }
  decode(rc, ps) {
    if (rc.bit(this.p, 0) === 0) return rc.tree(this.p, 2 + ps * 8, 3);
    if (rc.bit(this.p, 1) === 0) return 8 + rc.tree(this.p, 2 + 128 + ps * 8, 3);
    return 16 + rc.tree(this.p, 2 + 256, 8);
  }
}

/* the LZMA decoder's state, kept between LZMA2 chunks */
class Lzma {
  constructor(out) {
    this.out = out;
    this.pos = 0;     // where the next byte goes
    this.base = 0;    // where the dictionary starts (an LZMA2 dictionary reset)
    this.len = new LenDec();
    this.rep = new LenDec();
    this.isMatch = probs(192); this.isRep = probs(12); this.isRepG0 = probs(12); this.isRepG1 = probs(12); this.isRepG2 = probs(12); this.isRep0Long = probs(192);
    this.slot = probs(4 * 64); this.spec = probs(115); this.align = probs(16);
    this.lit = null;
  }
  props(d) {
    if (d >= 225) bad('LZMA');
    this.lc = d % 9; d = (d / 9) | 0; this.lp = d % 5; this.pb = (d / 5) | 0;
    const n = 0x300 << (this.lc + this.lp);
    if (!this.lit || this.lit.length !== n) this.lit = new Uint16Array(n);
  }
  reset() {
    for (const a of [this.isMatch, this.isRep, this.isRepG0, this.isRepG1, this.isRepG2, this.isRep0Long, this.slot, this.spec, this.align, this.lit]) a.fill(1024);
    this.len.reset(); this.rep.reset();
    this.state = 0; this.r0 = 0; this.r1 = 0; this.r2 = 0; this.r3 = 0;
  }
  /* decodes until `end` (or the end marker) */
  run(rc, end) {
    const out = this.out, pbMask = (1 << this.pb) - 1, lpMask = (1 << this.lp) - 1, lc = this.lc, lit = this.lit;
    let pos = this.pos, state = this.state, r0 = this.r0, r1 = this.r1, r2 = this.r2, r3 = this.r3;
    const base = this.base;
    while (pos < end) {
      const rel = pos - base, ps = rel & pbMask;
      if (rc.bit(this.isMatch, (state << 4) + ps) === 0) {
        const prev = rel > 0 ? out[pos - 1] : 0;
        const at = 0x300 * (((rel & lpMask) << lc) + (prev >> (8 - lc)));
        let sym = 1;
        if (state < 7) { while (sym < 0x100) sym = (sym << 1) | rc.bit(lit, at + sym); }
        else {
          let mb = out[pos - r0 - 1];
          while (sym < 0x100) {
            const m = (mb >> 7) & 1;
            mb <<= 1;
            const b = rc.bit(lit, at + ((1 + m) << 8) + sym);
            sym = (sym << 1) | b;
            if (m !== b) { while (sym < 0x100) sym = (sym << 1) | rc.bit(lit, at + sym); break; }
          }
        }
        out[pos++] = sym & 0xFF;
        state = state < 4 ? 0 : state < 10 ? state - 3 : state - 6;
        continue;
      }
      let len;
      if (rc.bit(this.isRep, state)) {
        if (rel === 0) bad('LZMA');
        if (rc.bit(this.isRepG0, state) === 0) {
          if (rc.bit(this.isRep0Long, (state << 4) + ps) === 0) {
            state = state < 7 ? 9 : 11;
            out[pos] = out[pos - r0 - 1];
            pos++;
            continue;
          }
        } else {
          let d;
          if (rc.bit(this.isRepG1, state) === 0) d = r1;
          else { if (rc.bit(this.isRepG2, state) === 0) d = r2; else { d = r3; r3 = r2; } r2 = r1; }
          r1 = r0; r0 = d;
        }
        len = this.rep.decode(rc, ps);
        state = state < 7 ? 8 : 11;
      } else {
        r3 = r2; r2 = r1; r1 = r0;
        len = this.len.decode(rc, ps);
        state = state < 7 ? 7 : 10;
        const slot = rc.tree(this.slot, (len < 3 ? len : 3) * 64, 6);
        if (slot < 4) r0 = slot;
        else {
          const nd = (slot >> 1) - 1;
          let d = ((2 | (slot & 1)) << nd) >>> 0;
          if (slot < 14) d += rc.reverse(this.spec, d - slot, nd);
          else d = (d + rc.direct(nd - 4) * 16 + rc.reverse(this.align, 0, 4)) >>> 0;
          r0 = d;
        }
        if (r0 === 0xFFFFFFFF) break;   // the end marker
      }
      len += 2;
      if (r0 >= pos - base) bad('LZMA');
      for (let s = pos - r0 - 1; len > 0 && pos < end; len--) out[pos++] = out[s++];
    }
    Object.assign(this, { pos, state, r0, r1, r2, r3 });
    return pos;
  }
}

const LZMA = {
  /* 7z's LZMA: props (5 bytes: lc/lp/pb, dictionary size), the stream, the size it unpacks to */
  decode(props, b, size, out, at) {
    out = out || new Uint8Array(size);
    const d = new Lzma(out);
    d.pos = d.base = at || 0;
    d.props(props[0]);
    d.reset();
    d.run(new Range(b, 0), d.pos + size);
    return out;
  },
  /* LZMA2: chunks, each LZMA (with or without resets) or stored */
  decode2(b, size, out, at) {
    out = out || new Uint8Array(size);
    const d = new Lzma(out);
    d.pos = d.base = at || 0;
    const end = d.pos + size;
    let p = 0, ready = false;
    while (p < b.length) {
      const c = b[p++];
      if (c === 0) break;
      if (c === 1 || c === 2) {
        if (c === 1) d.base = d.pos;
        const n = ((b[p] << 8) | b[p + 1]) + 1;
        p += 2;
        if (d.pos + n > end || p + n > b.length) bad('LZMA2');
        out.set(b.subarray(p, p + n), d.pos);
        d.pos += n;
        p += n;
        continue;
      }
      if (c < 0x80) bad('LZMA2');
      const un = ((c & 0x1F) << 16) + (b[p] << 8) + b[p + 1] + 1, pk = ((b[p + 2] << 8) | b[p + 3]) + 1;
      p += 4;
      const mode = (c >> 5) & 3;
      if (mode === 3) d.base = d.pos;
      if (mode >= 2) d.props(b[p++]);
      if (mode >= 1) { if (!d.lit) bad('LZMA2'); d.reset(); ready = true; }
      if (!ready || d.pos + un > end) bad('LZMA2');
      d.run(new Range(b.subarray(p, p + pk), 0), d.pos + un);
      p += pk;
    }
    return out;
  },
};

/* ---- branch filters, decoding, start address 0 ---- */
const u32le = (b, i) => (b[i] | (b[i + 1] << 8) | (b[i + 2] << 16) | (b[i + 3] << 24)) >>> 0;
const put32le = (b, i, v) => { b[i] = v; b[i + 1] = v >>> 8; b[i + 2] = v >>> 16; b[i + 3] = v >>> 24; };
const FILTERS = {
  x86(b) {
    const allowed = [1, 1, 1, 0, 1, 0, 0, 0], bitNo = [0, 1, 2, 2, 3, 3, 3, 3];
    const ms = v => v === 0 || v === 0xFF;
    const size = b.length;
    if (size < 5) return b;
    let pos = 0, prevPos = -1, mask = 0;
    const ip = 5;
    for (;;) {
      let p = pos;
      const limit = size - 4;
      while (p < limit && (b[p] & 0xFE) !== 0xE8) p++;
      pos = p;
      if (p >= limit) break;
      const d = pos - prevPos;
      if (d > 3) mask = 0;
      else {
        mask = (mask << (d - 1)) & 7;
        if (mask !== 0) {
          const v = b[p + 4 - bitNo[mask]];
          if (!allowed[mask] || ms(v)) { prevPos = pos; mask = ((mask << 1) & 7) | 1; pos++; continue; }
        }
      }
      prevPos = pos;
      if (ms(b[p + 4])) {
        let src = ((b[p + 4] << 24) | (b[p + 3] << 16) | (b[p + 2] << 8) | b[p + 1]) >>> 0, dest;
        for (;;) {
          dest = (src - (ip + pos)) >>> 0;
          if (mask === 0) break;
          const idx = bitNo[mask] * 8, v = (dest >>> (24 - idx)) & 0xFF;
          if (!ms(v)) break;
          src = (dest ^ ((2 ** (32 - idx)) - 1)) >>> 0;
        }
        b[p + 4] = ~(((dest >>> 24) & 1) - 1);
        b[p + 3] = dest >>> 16; b[p + 2] = dest >>> 8; b[p + 1] = dest;
        pos += 5;
      } else { mask = ((mask << 1) & 7) | 1; pos++; }
    }
    return b;
  },
  arm(b) {
    for (let i = 0; i + 4 <= b.length; i += 4) {
      if (b[i + 3] !== 0xEB) continue;
      const src = ((b[i + 2] << 16) | (b[i + 1] << 8) | b[i]) * 4;
      const dest = ((src - (i + 8)) >>> 2) & 0xFFFFFF;
      b[i + 2] = dest >>> 16; b[i + 1] = dest >>> 8; b[i] = dest;
    }
    return b;
  },
  armt(b) {
    for (let i = 0; i + 4 <= b.length; i += 2) {
      if ((b[i + 1] & 0xF8) !== 0xF0 || (b[i + 3] & 0xF8) !== 0xF8) continue;
      const src = (((b[i + 1] & 7) << 19) | (b[i] << 11) | ((b[i + 3] & 7) << 8) | b[i + 2]) * 2;
      const dest = (src - (i + 4)) >>> 1;
      b[i + 1] = 0xF0 | ((dest >>> 19) & 7); b[i] = dest >>> 11; b[i + 3] = 0xF8 | ((dest >>> 8) & 7); b[i + 2] = dest;
      i += 2;
    }
    return b;
  },
  arm64(b) {
    for (let i = 0; i + 4 <= b.length; i += 4) {
      let v = u32le(b, i);
      if ((v >>> 26) === 0x25) {
        put32le(b, i, (0x94000000 | ((v - (i >>> 2)) & 0x03FFFFFF)) >>> 0);
      } else if (((v & 0x9F000000) >>> 0) === 0x90000000) {
        const src = ((v >>> 29) & 3) | ((v >>> 3) & 0x001FFFFC);
        if ((src + 0x00020000) & 0x001C0000) continue;
        const dest = (src - (i >>> 12)) >>> 0;
        v = (v & 0x9000001F) >>> 0;
        v |= (dest & 3) << 29;
        v |= (dest & 0x0003FFFC) << 3;
        v |= (0 - (dest & 0x00020000)) & 0x00E00000;
        put32le(b, i, v >>> 0);
      }
    }
    return b;
  },
  ppc(b) {
    for (let i = 0; i + 4 <= b.length; i += 4) {
      if ((b[i] >> 2) !== 0x12 || (b[i + 3] & 3) !== 1) continue;
      const src = (((b[i] & 3) << 24) | (b[i + 1] << 16) | (b[i + 2] << 8) | (b[i + 3] & ~3)) >>> 0;
      const dest = (src - i) >>> 0;
      b[i] = 0x48 | ((dest >>> 24) & 3); b[i + 1] = dest >>> 16; b[i + 2] = dest >>> 8; b[i + 3] = (b[i + 3] & 3) | (dest & 0xFC);
    }
    return b;
  },
  sparc(b) {
    for (let i = 0; i + 4 <= b.length; i += 4) {
      if (!((b[i] === 0x40 && (b[i + 1] & 0xC0) === 0) || (b[i] === 0x7F && (b[i + 1] & 0xC0) === 0xC0))) continue;
      let src = (((b[i] << 24) | (b[i + 1] << 16) | (b[i + 2] << 8) | b[i + 3]) >>> 0) * 4;
      let dest = ((src - i) >>> 0) >>> 2;
      dest = (((0 - ((dest >>> 22) & 1)) << 22) & 0x3FFFFFFF) | (dest & 0x3FFFFF) | 0x40000000;
      b[i] = dest >>> 24; b[i + 1] = dest >>> 16; b[i + 2] = dest >>> 8; b[i + 3] = dest;
    }
    return b;
  },
  delta(b, dist) {
    for (let i = dist; i < b.length; i++) b[i] = (b[i] + b[i - dist]) & 0xFF;
    return b;
  },
};

/* an LZMA2 stream at p: [bytes it takes, bytes it unpacks to], from its chunk headers */
function lzma2Size(b, p) {
  const start = p;
  let size = 0;
  while (p < b.length) {
    const c = b[p];
    if (c === 0) { p++; break; }
    if (c === 1 || c === 2) { const n = ((b[p + 1] << 8) | b[p + 2]) + 1; size += n; p += 3 + n; continue; }
    if (c < 0x80) bad('LZMA2');
    size += ((c & 0x1F) << 16) + (b[p + 1] << 8) + b[p + 2] + 1;
    p += 5 + (((c >> 5) & 3) >= 2 ? 1 : 0) + ((b[p + 3] << 8) | b[p + 4]) + 1;
  }
  return [p - start, size];
}

/* ---- .xz: blocks of LZMA2 behind up to three filters ---- */
const XZ = {
  decode(b) {
    if (!(b[0] === 0xFD && b[1] === 0x37 && b[2] === 0x7A && b[3] === 0x58 && b[4] === 0x5A && b[5] === 0)) bad('xz');
    const parts = [];
    let p = 12, total = 0;
    const num = () => { let v = 0, s = 1; for (let i = 0; i < 9; i++) { const c = b[p++]; v += (c & 0x7F) * s; s *= 128; if (!(c & 0x80)) return v; } bad('xz'); };
    const checkSize = [0, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64][b[7] & 15];
    while (p < b.length) {
      if (b[p] === 0) {   // the index: then the footer, and maybe another stream
        p++;
        const n = num();
        for (let i = 0; i < n; i++) { num(); num(); }
        while (p & 3) p++;
        p += 4 + 12;
        while (p < b.length && b[p] === 0) p++;   // stream padding
        if (p + 12 <= b.length && b[p] === 0xFD) { p += 12; continue; }
        break;
      }
      const start = p, hsize = (b[p] + 1) * 4, flags = b[p + 1];
      p += 2;
      const nf = (flags & 3) + 1;
      let packed = -1, unpacked = -1;
      if (flags & 0x40) packed = num();
      if (flags & 0x80) unpacked = num();
      const filters = [];
      for (let i = 0; i < nf; i++) {
        const id = num(), sz = num();
        filters.push({ id, props: b.subarray(p, p + sz) });
        p += sz;
      }
      p = start + hsize;
      const last = filters[filters.length - 1];
      if (last.id !== 0x21) throw new Error('an xz filter osada does not read');
      // the sizes, when the header does not say them: the chunk headers do
      const [used, size] = lzma2Size(b, p);
      const out = LZMA.decode2(b.subarray(p, p + used), unpacked >= 0 ? unpacked : size);
      for (let i = filters.length - 2; i >= 0; i--) {
        const f = filters[i];
        if (f.id === 3) FILTERS.delta(out, f.props[0] + 1);
        else if (f.id === 4) FILTERS.x86(out);
        else if (f.id === 5) FILTERS.ppc(out);
        else if (f.id === 7) FILTERS.arm(out);
        else if (f.id === 8) FILTERS.armt(out);
        else if (f.id === 9) FILTERS.sparc(out);
        else if (f.id === 0x0A) FILTERS.arm64(out);
        else throw new Error('an xz filter osada does not read');
      }
      parts.push(out);
      total += out.length;
      p += packed >= 0 ? packed : used;
      while (p & 3) p++;   // block padding
      p += checkSize;
    }
    const all = new Uint8Array(total);
    let o = 0;
    for (const x of parts) { all.set(x, o); o += x.length; }
    return all;
  },
};

return { LZMA, FILTERS, XZ };
});
