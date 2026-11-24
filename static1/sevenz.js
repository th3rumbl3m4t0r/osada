/* 7z archives: read into the archive view's entries. Part sevenz, on archive and lzma; the
   bzip2 and ppmd parts are held only while a block packed with them is unpacked. The
   header (often packed itself) gives the folders (blocks: a chain of coders over packed
   streams) and the files in them; a solid block is unpacked whole, and the last one is kept
   for the next file from it. Coders: copy, LZMA, LZMA2, PPMd, BZip2, Deflate, and the
   filters BCJ, BCJ2, ARM, ARM Thumb, ARM64, PowerPC, SPARC and delta. Encrypted (AES) blocks
   and headers are refused. */
'use strict';

part('sevenz', ({ readArchive: others, ZIP, LZMA, FILTERS }) => {

const SIG = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];
const damaged = () => { throw new Error('this 7z file is damaged'); };
const BLOCK_MAX = 1536 << 20;
const METHODS = { '040109': 'Deflate64', '03030401': 'IA64', '0b': 'RISC-V', '04f71101': 'Zstandard', '04f71102': 'Brotli', '04f71104': 'LZ4', '04f71105': 'LZ5', '04f71106': 'Lizard' };

class R {
  constructor(b) { this.b = b; this.p = 0; }
  byte() { if (this.p >= this.b.length) damaged(); return this.b[this.p++]; }
  num() {
    const first = this.byte();
    let mask = 0x80, v = 0;
    for (let i = 0; i < 8; i++) {
      if ((first & mask) === 0) return v + (first & (mask - 1)) * 2 ** (8 * i);
      v += this.byte() * 2 ** (8 * i);
      mask >>= 1;
    }
    return v;
  }
  u32() { const b = this.b, p = this.p; if (p + 4 > b.length) damaged(); this.p += 4; return (b[p] | (b[p + 1] << 8) | (b[p + 2] << 16) | (b[p + 3] << 24)) >>> 0; }
  bytes(n) { if (this.p + n > this.b.length) damaged(); const s = this.b.subarray(this.p, this.p + n); this.p += n; return s; }
  bits(n) { const out = []; let v = 0; for (let i = 0; i < n; i++) { if ((i & 7) === 0) v = this.byte(); out.push(!!(v & (0x80 >> (i & 7)))); } return out; }
  defined(n) { return this.byte() ? new Array(n).fill(true) : this.bits(n); }
  digests(n) { const d = this.defined(n); return d.map(x => (x ? this.u32() : undefined)); }
}

function folder(r) {
  const nc = r.num(), coders = [];
  let ins = 0, outs = 0;
  for (let i = 0; i < nc; i++) {
    const flags = r.byte();
    if (flags & 0x80) throw new Error('this 7z file uses coder alternatives, which osada does not read');
    const id = [...r.bytes(flags & 0xF)].map(x => x.toString(16).padStart(2, '0')).join('');
    const nIn = flags & 0x10 ? r.num() : 1, nOut = flags & 0x10 ? r.num() : 1;
    const props = flags & 0x20 ? r.bytes(r.num()) : new Uint8Array(0);
    coders.push({ id, nIn, nOut, props, firstIn: ins, firstOut: outs });
    ins += nIn;
    outs += nOut;
  }
  const bind = [];
  for (let i = 0; i < outs - 1; i++) bind.push({ inI: r.num(), outI: r.num() });
  const packed = [];
  if (ins - bind.length === 1) { for (let i = 0; i < ins; i++) if (!bind.some(x => x.inI === i)) { packed.push(i); break; } }
  else for (let i = 0; i < ins - bind.length; i++) packed.push(r.num());
  return { coders, bind, packed, outs, locked: coders.some(c => c.id === '06f10701') };
}

function streamsInfo(r) {
  const si = { packPos: 0, packSizes: [], folders: [] };
  for (;;) {
    const id = r.num();
    if (id === 0) break;
    if (id === 0x06) {
      si.packPos = r.num();
      const n = r.num();
      for (let t = r.num(); t !== 0; t = r.num()) {
        if (t === 0x09) for (let i = 0; i < n; i++) si.packSizes.push(r.num());
        else if (t === 0x0A) r.digests(n);
        else r.bytes(r.num());
      }
    } else if (id === 0x07) {
      if (r.num() !== 0x0B) damaged();
      const n = r.num();
      if (r.byte() !== 0) throw new Error('this 7z file keeps its folders elsewhere, which osada does not read');
      for (let i = 0; i < n; i++) si.folders.push(folder(r));
      if (r.num() !== 0x0C) damaged();
      for (const f of si.folders) {
        f.sizes = [];
        for (let i = 0; i < f.outs; i++) f.sizes.push(r.num());
        f.main = [...f.sizes.keys()].find(o => !f.bind.some(x => x.outI === o));
        f.size = f.sizes[f.main];
      }
      for (let t = r.num(); t !== 0; t = r.num()) {
        if (t === 0x0A) r.digests(n).forEach((d, i) => { si.folders[i].crc = d; });
        else r.bytes(r.num());
      }
    } else if (id === 0x08) {
      const F = si.folders, nums = F.map(() => 1), sizes = [], crcs = [];
      let t = r.num();
      if (t === 0x0D) { for (let i = 0; i < F.length; i++) nums[i] = r.num(); t = r.num(); }
      const explicit = t === 0x09;
      for (let i = 0; i < F.length; i++) {
        if (nums[i] === 0) continue;
        let sum = 0;
        if (explicit) for (let j = 0; j < nums[i] - 1; j++) { const s = r.num(); sizes.push(s); sum += s; }
        sizes.push(F[i].size - sum);
      }
      if (explicit) t = r.num();
      let unknown = 0;
      for (let i = 0; i < F.length; i++) if (nums[i] !== 1 || F[i].crc === undefined) unknown += nums[i];
      let got = null;
      for (; t !== 0; t = r.num()) {
        if (t === 0x0A) got = r.digests(unknown);
        else r.bytes(r.num());
      }
      let k = 0;
      for (let i = 0; i < F.length; i++) {
        if (nums[i] === 1 && F[i].crc !== undefined) crcs.push(F[i].crc);
        else for (let j = 0; j < nums[i]; j++) crcs.push(got ? got[k++] : undefined);
      }
      Object.assign(si, { nums, sizes, crcs });
    } else damaged();
  }
  if (!si.nums) Object.assign(si, { nums: si.folders.map(() => 1), sizes: si.folders.map(f => f.size), crcs: si.folders.map(f => f.crc) });
  return si;
}

function filesInfo(r) {
  const n = r.num(), files = [];
  for (let i = 0; i < n; i++) files.push({ name: '', empty: false, emptyFile: false, anti: false, time: 0, attr: 0 });
  for (;;) {
    const t = r.num();
    if (t === 0) break;
    const size = r.num(), end = r.p + size;
    if (t === 0x0E) r.bits(n).forEach((x, i) => { files[i].empty = x; });
    else if (t === 0x0F || t === 0x10) {
      const ef = files.filter(f => f.empty), bits = r.bits(ef.length);
      ef.forEach((f, i) => { if (t === 0x0F) f.emptyFile = bits[i]; else f.anti = bits[i]; });
    } else if (t === 0x11) {
      if (r.byte() !== 0) throw new Error('this 7z file keeps its names elsewhere, which osada does not read');
      const names = new TextDecoder('utf-16le').decode(r.bytes(end - r.p)).split('\0');
      files.forEach((f, i) => { f.name = names[i] || ''; });
    } else if (t === 0x14 || t === 0x15) {
      const d = r.defined(n);
      if (r.byte() !== 0) damaged();
      d.forEach((x, i) => {
        if (!x) return;
        if (t === 0x14) { const lo = r.u32(), hi = r.u32(); files[i].time = Math.max(0, Math.floor((hi * 2 ** 32 + lo) / 1e7 - 11644473600)); }
        else files[i].attr = r.u32();
      });
    }
    r.p = end;
  }
  return files;
}

async function inflateRaw(b) {
  return new Uint8Array(await new Response(new Blob([b]).stream().pipeThrough(new DecompressionStream('deflate-raw'))).arrayBuffer());
}

function bcj2(main, call, jump, rcs, size) {
  const out = new Uint8Array(size), p = new Uint16Array(258).fill(1024);
  let ip = 0, o = 0, cp = 0, jp = 0, rp = 0, range = 0xFFFFFFFF, code = 0, prev = 0;
  const rb = () => (rp < rcs.length ? rcs[rp++] : 0);
  for (let i = 0; i < 5; i++) code = ((code * 256) + rb()) >>> 0;
  while (o < size) {
    let b = 0, found = false;
    while (o < size && ip < main.length) {
      b = main[ip++];
      out[o++] = b;
      if ((b & 0xFE) === 0xE8 || (prev === 0x0F && (b & 0xF0) === 0x80)) { found = true; break; }
      prev = b;
    }
    if (!found || o === size) break;
    const at = b === 0xE8 ? prev : b === 0xE9 ? 256 : 257, pr = p[at], bound = (range >>> 11) * pr;
    let bit;
    if (code < bound) { range = bound; p[at] = pr + ((2048 - pr) >>> 5); bit = 0; }
    else { range -= bound; code -= bound; p[at] = pr - (pr >>> 5); bit = 1; }
    if (range < 0x1000000) { range = (range * 256) >>> 0; code = ((code * 256) + rb()) >>> 0; }
    if (!bit) { prev = b; continue; }
    let src, q;
    if (b === 0xE8) { src = call; q = cp; cp += 4; } else { src = jump; q = jp; jp += 4; }
    if (q + 4 > src.length) damaged();
    const dest = (((src[q] << 24) | (src[q + 1] << 16) | (src[q + 2] << 8) | src[q + 3]) - (o + 4)) >>> 0;
    for (let k = 0; k < 4 && o < size; k++) out[o++] = dest >>> (8 * k);
    prev = dest >>> 24;
  }
  if (o !== size) damaged();
  return out;
}

/* one coder over its inputs, to `size` bytes */
async function run(c, ins, size) {
  const [x] = ins;
  switch (c.id) {
    case '00': return x.subarray(0, size);
    case '030101': return LZMA.decode(c.props, x, size);
    case '21': return LZMA.decode2(x, size);
    case '030401': {
      const { PPMD } = await hold('ppmd');
      try { return PPMD.decode7z(c.props, x, size); } finally { release('ppmd'); }
    }
    case '040202': {
      const { BZIP2 } = await hold('bzip2');
      try { return BZIP2.decode(x).subarray(0, size); } finally { release('bzip2'); }
    }
    case '040108': return (await inflateRaw(x)).subarray(0, size);
    case '0303011b': return bcj2(ins[0], ins[1], ins[2], ins[3], size);
    case '03030103': return FILTERS.x86(x.slice(0, size));
    case '03030205': return FILTERS.ppc(x.slice(0, size));
    case '03030501': return FILTERS.arm(x.slice(0, size));
    case '03030701': return FILTERS.armt(x.slice(0, size));
    case '03030805': return FILTERS.sparc(x.slice(0, size));
    case '0a': return FILTERS.arm64(x.slice(0, size));
    case '03': return FILTERS.delta(x.slice(0, size), (c.props[0] || 0) + 1);
    case '06f10701': throw new Error('encrypted: osada does not open it');
    default: throw new Error(tx('this 7z file is packed with {method}, which osada does not read', { method: METHODS[c.id] || c.id }));
  }
}

async function unpackFolder(b, si, fi) {
  const f = si.folders[fi];
  if (f.size > BLOCK_MAX) throw new Error('this 7z block is too big to unpack in the browser');
  let pi = 0;
  for (let k = 0; k < fi; k++) pi += si.folders[k].packed.length;
  let pos = 32 + si.packPos;
  for (let k = 0; k < pi; k++) pos += si.packSizes[k];
  const packs = f.packed.map((_, j) => { const n = si.packSizes[pi + j], s = b.subarray(pos, pos + n); if (pos + n > b.length) throw new Error('this 7z file is cut short'); pos += n; return s; });
  const outOf = async o => {
    const c = f.coders.find(k => o >= k.firstOut && o < k.firstOut + k.nOut);
    const ins = [];
    for (let j = 0; j < c.nIn; j++) {
      const i = c.firstIn + j, k = f.packed.indexOf(i);
      if (k >= 0) ins.push(packs[k]);
      else { const bp = f.bind.find(x => x.inI === i); if (!bp) damaged(); ins.push(await outOf(bp.outI)); }
    }
    const out = await run(c, ins, f.sizes[o]);
    if (out.length !== f.sizes[o]) damaged();
    return out;
  };
  const out = await outOf(f.main);
  if (f.crc !== undefined && ZIP.crc(out) !== f.crc) throw new Error('this 7z file is damaged (CRC)');
  return out;
}

async function read7z(b) {
  if (b.length < 32) damaged();
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const off = Number(dv.getBigUint64(12, true)), size = Number(dv.getBigUint64(20, true));
  if (size === 0) return { kind: '7z', entries: [] };
  if (32 + off + size > b.length) throw new Error('this 7z file is cut short (or one part of several)');
  let r = new R(b.subarray(32 + off, 32 + off + size)), id = r.num();
  while (id === 0x17) {
    const si = streamsInfo(r);
    if (si.folders.some(f => f.locked)) throw new Error('encrypted: osada does not open it');
    r = new R(await unpackFolder(b, si, 0));
    id = r.num();
  }
  if (id !== 0x01) damaged();
  let si = { folders: [], nums: [], sizes: [], crcs: [] }, files = [];
  for (id = r.num(); id !== 0; id = r.num()) {
    if (id === 0x02) { for (let t = r.num(); t !== 0; t = r.num()) r.bytes(r.num()); }
    else if (id === 0x03) streamsInfo(r);
    else if (id === 0x04) si = streamsInfo(r);
    else if (id === 0x05) files = filesInfo(r);
    else damaged();
  }
  // the last block unpacked, for the next file from it
  let last = { i: -1, data: null };
  const block = async i => {
    if (last.i !== i) { last = { i: -1, data: null }; last = { i, data: await unpackFolder(b, si, i) }; }
    return last.data;
  };
  const entries = [];
  let fi = 0, k = 0, s = 0, offset = 0;
  for (const f of files) {
    const path = f.name.replace(/\\/g, '/'), time = f.time;
    if (f.anti) continue;
    if (f.empty) {
      const dir = !f.emptyFile || !!(f.attr & 0x10);
      entries.push({ path: dir ? path + '/' : path, dir, size: 0, time, read: async () => new Uint8Array(0) });
      continue;
    }
    while (fi < si.folders.length && si.nums[fi] === 0) fi++;
    if (fi >= si.folders.length) damaged();
    const at = fi, from = offset, n = si.sizes[s], crc = si.crcs[s], locked = si.folders[fi].locked;
    // a symbolic link keeps its target as its data, its kind in the Unix mode (high half of the attributes)
    const link = f.attr & 0x8000 && ((f.attr >>> 16) & 0xF000) === 0xA000 ? '' : null;
    entries.push({
      path, dir: false, size: n, time, locked, link,
      read: async () => {
        if (locked) throw new Error('encrypted: osada does not open it');
        const data = (await block(at)).subarray(from, from + n);
        // a block's own CRC was checked as it was unpacked
        if (crc !== undefined && !(si.nums[at] === 1 && si.folders[at].crc !== undefined) && ZIP.crc(data) !== crc) throw new Error('this 7z file is damaged (CRC)');
        return data;
      },
    });
    s++;
    offset += n;
    if (++k >= si.nums[fi]) { fi++; k = 0; offset = 0; }
  }
  return { kind: '7z', entries };
}

async function readArchive(bytes, name) {
  if (SIG.every((x, i) => bytes[i] === x)) return read7z(bytes);
  return others(bytes, name);
}

return { readArchive, SEVENZ: { read: read7z } };
});
