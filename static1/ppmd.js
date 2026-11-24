/* PPMd, variant H: the text compression of 7z (PPMd coder) and of RAR 2.9 / 3.x ("text"
   blocks). Part ppmd. The model and its memory allocator work in one byte array, exactly as
   the encoder's do: when memory runs out the model restarts at the same symbol on both
   sides, so allocation must match to the unit. Two range decoders: 7z's and RAR's. */
'use strict';

part('ppmd', () => {

const UNIT = 12, NUM_INDEXES = 38, MAX_FREQ = 124, PERIOD_BITS = 7, BIN_SCALE = 1 << 14, MAX_O = 64;
const EXP_ESCAPE = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
const INIT_BIN_ESC = [0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051];

const I2U = new Uint8Array(NUM_INDEXES), U2I = new Uint8Array(128), NS2I = new Uint8Array(256), NS2BSI = new Uint8Array(256), HB2F = new Uint8Array(256);
{
  let k = 0;
  for (let i = 0; i < NUM_INDEXES; i++) { let step = i >= 12 ? 4 : (i >> 2) + 1; do U2I[k++] = i; while (--step); I2U[i] = k; }
  NS2BSI[0] = 0; NS2BSI[1] = 2; NS2BSI.fill(4, 2, 11); NS2BSI.fill(6, 11);
  let i = 0;
  for (; i < 3; i++) NS2I[i] = i;
  for (let m = i, n = 1; i < 256; i++) { NS2I[i] = m; if (--n === 0) n = (++m) - 2; }
  HB2F.fill(8, 0x40);
}
const DUMMY = 400;   // the See entry of the order -1 context

class Model {
  /* size: memory in bytes; rar: RAR's flavour of one comparison (signed, where 7-Zip's is unsigned) */
  constructor(size, rar) {
    this.size = size;
    this.rar = !!rar;
    this.align = 4 - (size & 3);
    this.m = new Uint8Array(this.align + size + UNIT * 2);
    this.free = new Uint32Array(NUM_INDEXES);
    this.seeSumm = new Uint16Array(401); this.seeShift = new Uint8Array(401); this.seeCount = new Uint8Array(401);
    this.bin = new Uint16Array(128 * 64);
    this.mask = new Uint8Array(256);
    this.ps = new Int32Array(256);
  }
  u16(o) { return this.m[o] | (this.m[o + 1] << 8); }
  w16(o, v) { this.m[o] = v; this.m[o + 1] = v >>> 8; }
  u32(o) { const m = this.m; return (m[o] | (m[o + 1] << 8) | (m[o + 2] << 16) | (m[o + 3] << 24)) >>> 0; }
  w32(o, v) { const m = this.m; m[o] = v; m[o + 1] = v >>> 8; m[o + 2] = v >>> 16; m[o + 3] = v >>> 24; }
  // context: NumStats @0, SummFreq @2, Stats @4, Suffix @8; its one state at +2. State: Symbol, Freq, Successor (u32)
  copyState(to, from) { this.m.copyWithin(to, from, from + 6); }

  /* ---- the allocator ---- */
  insertNode(node, i) { this.w32(node, this.free[i]); this.free[i] = node; }
  removeNode(i) { const node = this.free[i]; this.free[i] = this.u32(node); return node; }
  splitBlock(ptr, oldI, newI) {
    const nu = I2U[oldI] - I2U[newI];
    ptr += I2U[newI] * UNIT;
    let i = U2I[nu - 1];
    if (I2U[i] !== nu) { const k = I2U[--i]; this.insertNode(ptr + k * UNIT, nu - k - 1); }
    this.insertNode(ptr, i);
  }
  glueFreeBlocks() {
    // nodes: Stamp @0 (0: free), NU @2, Next @4, Prev @8; the head is past the end
    const head = this.align + this.size;
    let n = head;
    this.glueCount = 255;
    for (let i = 0; i < NUM_INDEXES; i++) {
      const nu = I2U[i];
      let next = this.free[i];
      this.free[i] = 0;
      while (next !== 0) {
        const node = next;
        this.w32(node + 4, n);
        this.w32(n + 8, next);
        n = next;
        next = this.u32(node);
        this.w16(node, 0);
        this.w16(node + 2, nu);
      }
    }
    this.w16(head, 1);
    this.w32(head + 4, n);
    this.w32(n + 8, head);
    if (this.lo !== this.hi) this.w16(this.lo, 1);
    while (n !== head) {
      const node = n;
      let nu = this.u16(node + 2);
      for (;;) {
        const node2 = node + nu * UNIT;
        nu += this.u16(node2 + 2);
        if (this.u16(node2) !== 0 || nu >= 0x10000) break;
        this.w32(this.u32(node2 + 8) + 4, this.u32(node2 + 4));
        this.w32(this.u32(node2 + 4) + 8, this.u32(node2 + 8));
        this.w16(node + 2, nu);
      }
      n = this.u32(node + 4);
    }
    for (n = this.u32(head + 4); n !== head;) {
      let node = n;
      const next = this.u32(node + 4);
      let nu = this.u16(node + 2);
      for (; nu > 128; nu -= 128, node += 128 * UNIT) this.insertNode(node, NUM_INDEXES - 1);
      let i = U2I[nu - 1];
      if (I2U[i] !== nu) { const k = I2U[--i]; this.insertNode(node + k * UNIT, nu - k - 1); }
      this.insertNode(node, i);
      n = next;
    }
  }
  allocUnitsRare(indx) {
    if (this.glueCount === 0) {
      this.glueFreeBlocks();
      if (this.free[indx] !== 0) return this.removeNode(indx);
    }
    let i = indx;
    do {
      if (++i === NUM_INDEXES) {
        const bytes = I2U[indx] * UNIT;
        this.glueCount--;
        if (this.unitsStart - this.text > bytes) { this.unitsStart -= bytes; return this.unitsStart; }
        return 0;
      }
    } while (this.free[i] === 0);
    const r = this.removeNode(i);
    this.splitBlock(r, i, indx);
    return r;
  }
  allocUnits(indx) {
    if (this.free[indx] !== 0) return this.removeNode(indx);
    const bytes = I2U[indx] * UNIT;
    if (bytes <= this.hi - this.lo) { const r = this.lo; this.lo += bytes; return r; }
    return this.allocUnitsRare(indx);
  }
  shrinkUnits(old, oldNU, newNU) {
    const i0 = U2I[oldNU - 1], i1 = U2I[newNU - 1];
    if (i0 === i1) return old;
    if (this.free[i1] !== 0) {
      const p = this.removeNode(i1);
      this.m.copyWithin(p, old, old + newNU * UNIT);
      this.insertNode(old, i0);
      return p;
    }
    this.splitBlock(old, i0, i1);
    return old;
  }

  /* ---- the model ---- */
  restart() {
    const m = this.m;
    this.free.fill(0);
    this.text = this.align;
    this.hi = this.text + this.size;
    this.lo = this.unitsStart = this.hi - Math.floor(this.size / 8 / UNIT) * 7 * UNIT;
    this.glueCount = 0;
    this.orderFall = this.maxOrder;
    this.runLength = this.initRL = -(this.maxOrder < 12 ? this.maxOrder : 12) - 1;
    this.prevSuccess = 0;
    this.hi -= UNIT;
    this.minContext = this.maxContext = this.hi;
    this.w32(this.hi + 8, 0);
    this.w16(this.hi, 256);
    this.w16(this.hi + 2, 257);
    this.found = this.lo;
    this.w32(this.hi + 4, this.lo);
    for (let i = 0; i < 256; i++) { const s = this.lo + i * 6; m[s] = i; m[s + 1] = 1; this.w32(s + 2, 0); }
    this.lo += 128 * UNIT;
    for (let i = 0; i < 128; i++) for (let k = 0; k < 8; k++) {
      const v = BIN_SCALE - Math.floor(INIT_BIN_ESC[k] / (i + 2));
      for (let j = 0; j < 64; j += 8) this.bin[i * 64 + k + j] = v;
    }
    for (let i = 0; i < 25; i++) for (let k = 0; k < 16; k++) { const e = i * 16 + k; this.seeShift[e] = PERIOD_BITS - 4; this.seeSumm[e] = (5 * i + 10) << this.seeShift[e]; this.seeCount[e] = 4; }
  }
  init(maxOrder) {
    this.maxOrder = maxOrder;
    this.initEsc = 0;
    this.hiBitsFlag = 0;
    this.restart();
    this.seeShift[DUMMY] = PERIOD_BITS; this.seeSumm[DUMMY] = 0; this.seeCount[DUMMY] = 64;
  }
  createSuccessors(skip) {
    const m = this.m;
    let c = this.minContext;
    const upBranch = this.u32(this.found + 2), fsym = m[this.found];
    const ps = [];
    if (!skip) ps.push(this.found);
    while (this.u32(c + 8)) {
      c = this.u32(c + 8);
      let s;
      if (this.u16(c) !== 1) { s = this.u32(c + 4); while (m[s] !== fsym) s += 6; } else s = c + 2;
      const succ = this.u32(s + 2);
      if (succ !== upBranch) { c = succ; if (ps.length === 0) return c; break; }
      ps.push(s);
    }
    if (ps.length === 0) return c;
    const upSym = m[upBranch], upSucc = upBranch + 1;
    let upFreq;
    if (this.u16(c) === 1) upFreq = m[c + 3];
    else {
      let s = this.u32(c + 4);
      while (m[s] !== upSym) s += 6;
      const cf = m[s + 1] - 1, s0 = this.u16(c + 2) - this.u16(c) - cf;
      upFreq = (1 + (2 * cf <= s0 ? (5 * cf > s0 ? 1 : 0) : Math.floor((2 * cf + 3 * s0 - 1) / (2 * s0)))) & 0xFF;
    }
    do {
      let c1;
      if (this.hi !== this.lo) { this.hi -= UNIT; c1 = this.hi; }
      else if (this.free[0] !== 0) c1 = this.removeNode(0);
      else { c1 = this.allocUnitsRare(0); if (!c1) return 0; }
      this.w16(c1, 1);
      m[c1 + 2] = upSym; m[c1 + 3] = upFreq; this.w32(c1 + 4, upSucc);
      this.w32(c1 + 8, c);
      this.w32(ps.pop() + 2, c1);
      c = c1;
    } while (ps.length);
    return c;
  }
  swap(a, b) {
    const m = this.m;
    for (let i = 0; i < 6; i++) { const t = m[a + i]; m[a + i] = m[b + i]; m[b + i] = t; }
  }
  updateModel() {
    const m = this.m, fs = this.found, fsym = m[fs], ffreq = m[fs + 1];
    let fSucc = this.u32(fs + 2);
    const mc = this.minContext;
    if (ffreq < MAX_FREQ / 4 && this.u32(mc + 8) !== 0) {
      const c = this.u32(mc + 8);
      if (this.u16(c) === 1) { if (m[c + 3] < 32) m[c + 3]++; }
      else {
        let s = this.u32(c + 4);
        if (m[s] !== fsym) {
          do s += 6; while (m[s] !== fsym);
          if (m[s + 1] >= m[s - 5]) { this.swap(s, s - 6); s -= 6; }
        }
        if (m[s + 1] < MAX_FREQ - 9) { m[s + 1] += 2; this.w16(c + 2, this.u16(c + 2) + 2); }
      }
    }
    if (this.orderFall === 0) {
      this.minContext = this.maxContext = this.createSuccessors(true);
      if (this.minContext === 0) { this.restart(); return; }
      this.w32(this.found + 2, this.minContext);
      return;
    }
    m[this.text++] = fsym;
    let succ = this.text;
    if (this.text >= this.unitsStart) { this.restart(); return; }
    if (fSucc) {
      if (fSucc <= succ) {
        const cs = this.createSuccessors(false);
        if (cs === 0) { this.restart(); return; }
        fSucc = cs;
      }
      if (--this.orderFall === 0) { succ = fSucc; this.text -= this.maxContext !== this.minContext ? 1 : 0; }
    } else {
      this.w32(this.found + 2, succ);
      fSucc = this.minContext;
    }
    const ns = this.u16(mc), s0 = this.u16(mc + 2) - ns - (ffreq - 1);
    for (let c = this.maxContext; c !== mc; c = this.u32(c + 8)) {
      const ns1 = this.u16(c);
      if (ns1 !== 1) {
        if ((ns1 & 1) === 0) {
          const oldNU = ns1 >> 1, i = U2I[oldNU - 1];
          if (i !== U2I[oldNU]) {
            const ptr = this.allocUnits(i + 1);
            if (!ptr) { this.restart(); return; }
            const old = this.u32(c + 4);
            m.copyWithin(ptr, old, old + oldNU * UNIT);
            this.insertNode(old, i);
            this.w32(c + 4, ptr);
          }
        }
        const sf = this.u16(c + 2);
        this.w16(c + 2, sf + (2 * ns1 < ns ? 1 : 0) + 2 * ((4 * ns1 <= ns ? 1 : 0) & (sf <= 8 * ns1 ? 1 : 0)));
      } else {
        const s = this.allocUnits(0);
        if (!s) { this.restart(); return; }
        this.copyState(s, c + 2);
        this.w32(c + 4, s);
        m[s + 1] = m[s + 1] < MAX_FREQ / 4 - 1 ? m[s + 1] * 2 : MAX_FREQ - 4;
        this.w16(c + 2, m[s + 1] + this.initEsc + (ns > 3 ? 1 : 0));
      }
      let cf = 2 * ffreq * (this.u16(c + 2) + 6);
      const sf = s0 + this.u16(c + 2);
      if (cf < 6 * sf) { cf = 1 + (cf > sf ? 1 : 0) + (cf >= 4 * sf ? 1 : 0); this.w16(c + 2, this.u16(c + 2) + 3); }
      else { cf = 4 + (cf >= 9 * sf ? 1 : 0) + (cf >= 12 * sf ? 1 : 0) + (cf >= 15 * sf ? 1 : 0); this.w16(c + 2, this.u16(c + 2) + cf); }
      const s = this.u32(c + 4) + ns1 * 6;
      this.w32(s + 2, succ);
      m[s] = fsym;
      m[s + 1] = cf;
      this.w16(c, ns1 + 1);
    }
    this.maxContext = this.minContext = fSucc;
  }
  rescale() {
    const m = this.m, mc = this.minContext, stats = this.u32(mc + 4);
    let s = this.found;
    if (s !== stats) {
      const t = m.slice(s, s + 6);
      for (; s !== stats; s -= 6) this.copyState(s, s - 6);
      m.set(t, s);
    }
    let escFreq = this.u16(mc + 2) - m[s + 1];
    m[s + 1] += 4;
    const adder = this.orderFall !== 0 ? 1 : 0;
    m[s + 1] = (m[s + 1] + adder) >> 1;
    let sumFreq = m[s + 1];
    let i = this.u16(mc) - 1;
    do {
      s += 6;
      escFreq -= m[s + 1];
      m[s + 1] = (m[s + 1] + adder) >> 1;
      sumFreq += m[s + 1];
      if (m[s + 1] > m[s - 5]) {
        let s1 = s;
        const t = m.slice(s1, s1 + 6);
        do { this.copyState(s1, s1 - 6); s1 -= 6; } while (s1 !== stats && t[1] > m[s1 - 5]);
        m.set(t, s1);
      }
    } while (--i);
    if (m[s + 1] === 0) {
      const numStats = this.u16(mc);
      i = 0;
      do { i++; s -= 6; } while (m[s + 1] === 0);
      escFreq += i;
      this.w16(mc, numStats - i);
      if (this.u16(mc) === 1) {
        const t = m.slice(stats, stats + 6);
        do { t[1] -= t[1] >> 1; escFreq >>= 1; } while (escFreq > 1);
        this.insertNode(stats, U2I[((numStats + 1) >> 1) - 1]);
        this.found = mc + 2;
        m.set(t, mc + 2);
        return;
      }
      const n0 = (numStats + 1) >> 1, n1 = (this.u16(mc) + 1) >> 1;
      if (n0 !== n1) this.w32(mc + 4, this.shrinkUnits(stats, n0, n1));
    }
    this.w16(mc + 2, sumFreq + escFreq - (escFreq >> 1));
    this.found = this.u32(mc + 4);
  }
  /* the See context for an escape here, and its frequency */
  escFreq(numMasked) {
    const mc = this.minContext, ns = this.u16(mc), nonMasked = ns - numMasked;
    if (ns === 256) return [DUMMY, 1];
    const d = this.u16(this.u32(mc + 8)) - ns;
    const e = NS2I[nonMasked - 1] * 16 + ((this.rar ? nonMasked < d : (d < 0 || nonMasked < d)) ? 1 : 0) + 2 * (this.u16(mc + 2) < 11 * ns ? 1 : 0) + 4 * (numMasked > nonMasked ? 1 : 0) + this.hiBitsFlag;
    const r = this.seeSumm[e] >> this.seeShift[e];
    this.seeSumm[e] -= r;
    return [e, r + (r === 0 ? 1 : 0)];
  }
  nextContext() {
    const c = this.u32(this.found + 2);
    if (this.orderFall === 0 && c > this.text) this.minContext = this.maxContext = c;
    else this.updateModel();
  }
  update1() {
    const m = this.m;
    let s = this.found;
    m[s + 1] += 4;
    this.w16(this.minContext + 2, this.u16(this.minContext + 2) + 4);
    if (m[s + 1] > m[s - 5]) {
      this.swap(s, s - 6);
      this.found = s -= 6;
      if (m[s + 1] > MAX_FREQ) this.rescale();
    }
    this.nextContext();
  }
  update1_0() {
    const m = this.m, mc = this.minContext;
    this.prevSuccess = 2 * m[this.found + 1] > this.u16(mc + 2) ? 1 : 0;
    this.runLength += this.prevSuccess;
    this.w16(mc + 2, this.u16(mc + 2) + 4);
    if ((m[this.found + 1] += 4) > MAX_FREQ) this.rescale();
    this.nextContext();
  }
  updateBin() {
    const m = this.m;
    m[this.found + 1] += m[this.found + 1] < 128 ? 1 : 0;
    this.prevSuccess = 1;
    this.runLength++;
    this.nextContext();
  }
  update2() {
    const m = this.m;
    m[this.found + 1] += 4;
    this.w16(this.minContext + 2, this.u16(this.minContext + 2) + 4);
    if (m[this.found + 1] > MAX_FREQ) this.rescale();
    this.runLength = this.initRL;
    this.updateModel();
  }

  /* one symbol: 0..255, -1 at the end mark, -2 on damaged data */
  decode(rc) {
    const m = this.m, mask = this.mask;
    let mc = this.minContext;
    if (this.u16(mc) !== 1) {
      let s = this.u32(mc + 4);
      const summ = this.u16(mc + 2);
      const count = rc.threshold(summ);
      let hi = m[s + 1];
      if (count < hi) {
        rc.decode(0, hi);
        this.found = s;
        const sym = m[s];
        this.update1_0();
        return sym;
      }
      this.prevSuccess = 0;
      let i = this.u16(mc) - 1;
      do {
        s += 6;
        if ((hi += m[s + 1]) > count) {
          rc.decode(hi - m[s + 1], m[s + 1]);
          this.found = s;
          const sym = m[s];
          this.update1();
          return sym;
        }
      } while (--i);
      if (count >= summ) return -2;
      this.hiBitsFlag = HB2F[m[this.found]];
      rc.decode(hi, summ - hi);
      mask.fill(0xFF);
      mask[m[s]] = 0;
      i = this.u16(mc) - 1;
      do { s -= 6; mask[m[s]] = 0; } while (--i);
    } else {
      const one = mc + 2;
      this.hiBitsFlag = HB2F[m[this.found]];
      const e = (m[one + 1] - 1) * 64 + this.prevSuccess + NS2BSI[this.u16(this.u32(mc + 8)) - 1] + this.hiBitsFlag + 2 * HB2F[m[one]] + ((this.runLength >> 26) & 0x20);
      const pr = this.bin[e];
      if (rc.bit(pr, BIN_SCALE) === 0) {
        this.bin[e] = pr + (1 << 7) - ((pr + (1 << 5)) >> 7);
        this.found = one;
        const sym = m[one];
        this.updateBin();
        return sym;
      }
      this.bin[e] = pr - ((pr + (1 << 5)) >> 7);
      this.initEsc = EXP_ESCAPE[this.bin[e] >> 10];
      mask.fill(0xFF);
      mask[m[one]] = 0;
      this.prevSuccess = 0;
    }
    const ps = this.ps;
    for (;;) {
      const numMasked = this.u16(mc);
      do {
        this.orderFall++;
        if (!this.u32(mc + 8)) return -1;
        mc = this.minContext = this.u32(mc + 8);
      } while (this.u16(mc) === numMasked);
      let hi = 0, s = this.u32(mc + 4), i = 0;
      const num = this.u16(mc) - numMasked;
      do {
        if (mask[m[s]]) { hi += m[s + 1]; ps[i++] = s; }
        s += 6;
      } while (i !== num);
      const [see, esc] = this.escFreq(numMasked);
      const total = esc + hi;
      const count = rc.threshold(total);
      if (count < hi) {
        let k = 0;
        hi = 0;
        while ((hi += m[ps[k] + 1]) <= count) k++;
        s = ps[k];
        rc.decode(hi - m[s + 1], m[s + 1]);
        if (this.seeShift[see] < PERIOD_BITS && --this.seeCount[see] === 0) { this.seeSumm[see] <<= 1; this.seeCount[see] = 3 << this.seeShift[see]++; }
        this.found = s;
        const sym = m[s];
        this.update2();
        return sym;
      }
      if (count >= total) return -2;
      rc.decode(hi, total - hi);
      this.seeSumm[see] += total;
      do mask[m[ps[--i]]] = 0; while (i !== 0);
    }
  }
}

/* 7z's range decoder: a zero byte, then the code */
class Range7z {
  constructor(b, p) {
    this.b = b; this.p = p || 0;
    if (this.b[this.p++] !== 0) throw new Error('damaged PPMd data');
    this.range = 0xFFFFFFFF;
    this.code = 0;
    for (let i = 0; i < 4; i++) this.code = ((this.code * 256) + this.byte()) >>> 0;
  }
  byte() { return this.p < this.b.length ? this.b[this.p++] : (this.p++, 0); }
  norm() {
    while (this.range < 0x1000000) { this.code = ((this.code * 256) + this.byte()) >>> 0; this.range = (this.range * 256) >>> 0; }
  }
  threshold(total) { this.range = Math.floor(this.range / total); return Math.floor(this.code / this.range); }
  decode(start, size) { this.code = (this.code - start * this.range) >>> 0; this.range = (this.range * size) >>> 0; this.norm(); }
  bit(size0, total) {
    const bound = Math.floor(this.range / total) * size0;
    let r;
    if (this.code < bound) { r = 0; this.range = bound; } else { r = 1; this.code = (this.code - bound) >>> 0; this.range = (this.range - bound) >>> 0; }
    this.norm();
    return r;
  }
}

/* RAR's (Subbotin's carryless) range decoder; reads through next() */
class RangeRar {
  constructor(next) {
    this.next = next;
    this.low = 0; this.code = 0; this.range = 0xFFFFFFFF;
    for (let i = 0; i < 4; i++) this.code = ((this.code * 256) + next()) >>> 0;
  }
  norm() {
    for (;;) {
      if (((this.low ^ ((this.low + this.range) >>> 0)) >>> 0) >= 0x1000000) {
        if (this.range >= 0x8000) break;
        this.range = (-this.low) & 0x7FFF;
      }
      this.code = ((this.code * 256) + this.next()) >>> 0;
      this.range = (this.range * 256) >>> 0;
      this.low = (this.low * 256) >>> 0;
    }
  }
  threshold(total) { this.range = Math.floor(this.range / total); return Math.floor(((this.code - this.low) >>> 0) / this.range); }
  decode(start, size) { this.low = (this.low + start * this.range) >>> 0; this.range = (this.range * size) >>> 0; this.norm(); }
  bit(size0, total) { const c = this.threshold(total); if (c < size0) { this.decode(0, size0); return 0; } this.decode(size0, total - size0); return 1; }
}

const PPMD = {
  Model, RangeRar,
  /* 7z: props [order, memory (u32)], the packed bytes, the size they unpack to */
  decode7z(props, b, size) {
    const order = props[0], mem = (props[1] | (props[2] << 8) | (props[3] << 16) | (props[4] << 24)) >>> 0;
    if (order < 2 || order > MAX_O || mem < (1 << 11) || mem > 0xFFFFFFFF - 12 * 3) throw new Error('damaged PPMd data');
    const model = new Model(mem, false), rc = new Range7z(b, 0), out = new Uint8Array(size);
    model.init(order);
    for (let i = 0; i < size; i++) {
      const s = model.decode(rc);
      if (s < 0) throw new Error('damaged PPMd data');
      out[i] = s;
    }
    return out;
  },
};

return { PPMD };
});
