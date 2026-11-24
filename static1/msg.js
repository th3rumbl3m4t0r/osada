/* Outlook messages (.msg, .oft): read into the mail model (mail.js) and shown by its view.
   Part msg, on mail, cfb and rtf. Properties from the __substg1.0_ streams (Unicode or in
   the message's code page) and __properties_version1.0 (times, numbers); recipients and
   attachments from their storages; the internet headers the message came with, for
   "headers". The body: HTML; else the compressed RTF (LZFu), as the HTML it wraps or,
   real RTF, through the RTF reader; else the plain text. */
'use strict';

part('msg', ({ CFB, EML, RTF, Rich }) => {

const u16 = (d, o) => d[o] | (d[o + 1] << 8);
const u32 = (d, o) => (d[o] | (d[o + 1] << 8) | (d[o + 2] << 16) | (d[o + 3] << 24)) >>> 0;
const CP = n => ({ 65001: 'utf-8', 20127: 'windows-1252', 28591: 'iso-8859-1', 28592: 'iso-8859-2', 28595: 'iso-8859-5', 20866: 'koi8-r', 932: 'shift_jis',
  936: 'gbk', 949: 'euc-kr', 950: 'big5', 50220: 'iso-2022-jp', 1200: 'utf-16le' }[n] || (n >= 1250 && n <= 1258 ? 'windows-' + n : null));

/* a locale's ANSI code page, for messages that do not say theirs */
function LCID_CP(id) {
  if (!id) return 0;
  const lang = id & 0x3FF, sub = id >> 10;
  if (lang === 0x04) return sub === 2 || sub === 4 ? 936 : 950;   // Chinese: simplified (PRC, Singapore), else traditional
  return { 0x11: 932, 0x12: 949, 0x19: 1251, 0x22: 1251, 0x23: 1251, 0x02: 1251, 0x2F: 1251, 0x3F: 1251, 0x44: 1251, 0x05: 1250, 0x15: 1250,
    0x0E: 1250, 0x1B: 1250, 0x1A: sub === 3 ? 1251 : 1250, 0x24: 1250, 0x18: 1250, 0x1C: 1250, 0x08: 1253, 0x1F: 1254, 0x0D: 1255, 0x01: 1256,
    0x20: 1256, 0x29: 1256, 0x25: 1257, 0x26: 1257, 0x27: 1257, 0x2A: 1258, 0x1E: 874 }[lang] || 1252;
}

/* compressed RTF (MS-OXRTFCP): LZ77 over a 4096-byte dictionary that starts with this */
const PRE = '{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx';
function lzfu(b) {
  if (b.length < 16) return null;
  const rawSize = u32(b, 4), type = u32(b, 8);
  if (type === 0x414C454D) return b.subarray(16, 16 + rawSize);   // MELA: not compressed
  if (type !== 0x75465A4C) return null;
  const dict = new Uint8Array(4096);
  for (let i = 0; i < PRE.length; i++) dict[i] = PRE.charCodeAt(i);
  let w = PRE.length, p = 16;
  const out = [];
  while (p < b.length && out.length < rawSize) {
    const ctl = b[p++];
    for (let bit = 0; bit < 8 && p < b.length && out.length < rawSize; bit++) {
      if (ctl & (1 << bit)) {
        const ref = (b[p] << 8) | b[p + 1];
        p += 2;
        const off = ref >> 4, len = (ref & 0xF) + 2;
        if (off === w) return new Uint8Array(out);
        for (let k = 0; k < len; k++) { const c = dict[(off + k) & 0xFFF]; out.push(c); dict[w] = c; w = (w + 1) & 0xFFF; }
      } else {
        const c = b[p++];
        out.push(c);
        dict[w] = c;
        w = (w + 1) & 0xFFF;
      }
    }
  }
  return new Uint8Array(out);
}

/* RTF that wraps HTML (\fromhtml1): the HTML back (MS-OXRTFEX) */
function unwrapHtml(rtf, cp) {
  let out = '', i = 0, depth = 0, skip = 0, suppress = false;
  const stack = [], bytes = [];
  const flush = () => { if (bytes.length) { out += EML.decode(new Uint8Array(bytes.splice(0)), cp); } };
  const n = rtf.length;
  while (i < n) {
    const c = rtf[i];
    if (c === '{') { stack.push({ skip, suppress }); depth++; i++; continue; }
    if (c === '}') { flush(); const s = stack.pop() || {}; skip = s.skip || 0; suppress = !!s.suppress; depth--; i++; continue; }
    if (c === '\r' || c === '\n') { i++; continue; }
    if (c !== '\\') { if (!skip && !suppress) bytes.push(c.charCodeAt(0) & 0xFF); i++; continue; }
    const nx = rtf[i + 1];
    if (nx === '\\' || nx === '{' || nx === '}') { if (!skip && !suppress) bytes.push(nx.charCodeAt(0)); i += 2; continue; }
    if (nx === "'") { if (!skip && !suppress) bytes.push(parseInt(rtf.substr(i + 2, 2), 16)); i += 4; continue; }
    if (nx === '*') {
      // an ignorable destination: of these only \htmltag is kept, its text is the HTML
      const m = /^\\\*\\([a-z]+)(-?\d+)? ?/.exec(rtf.slice(i, i + 50));
      if (m && m[1] === 'htmltag') { flush(); i += m[0].length; skip = 0; suppress = false; continue; }
      skip = 1;
      i += 2;
      continue;
    }
    const m = /^\\([a-z]+)(-?\d+)? ?/.exec(rtf.slice(i, i + 40));
    if (!m) { i += 2; continue; }
    i += m[0].length;
    const word = m[1], arg = m[2];
    if (word === 'htmlrtf') { flush(); suppress = arg !== '0'; continue; }
    if (skip || suppress) continue;
    if (/^(fonttbl|colortbl|stylesheet|info|pict|object|header|footer)$/.test(word)) { skip = 1; continue; }
    if (word === 'par' || word === 'line') bytes.push(13, 10);
    else if (word === 'tab') bytes.push(9);
    else if (word === 'u' && arg) { flush(); out += String.fromCharCode((+arg + 65536) % 65536); if (rtf[i] === '?') i++; }
  }
  flush();
  return out;
}

function parse(bytes) {
  const cf = CFB.read(bytes);
  const lost = {};
  let cp = 'windows-1252';
  const fixed = (dir, headLen) => {
    const s = cf.get((dir ? dir + '/' : '') + '__properties_version1.0');
    const out = {};
    if (!s) return out;
    for (let o = headLen; o + 16 <= s.length; o += 16) {
      const tag = u32(s, o), type = tag & 0xFFFF, id = tag >>> 16;
      if (type === 0x0003 || type === 0x000B) out[id] = u32(s, o + 8);
      else if (type === 0x0040) {
        const lo = u32(s, o + 8), hi = u32(s, o + 12), ms = (hi * 4294967296 + lo) / 10000 - 11644473600000;
        if (ms > 0) out[id] = new Date(ms);
      }
    }
    return out;
  };
  const top = fixed('', 32);
  // 8-bit strings are in the message's code page; an HTML body in the one it came in
  cp = CP(top[0x3FFD]) || CP(LCID_CP(top[0x3FF1])) || CP(top[0x3FDE]) || cp;
  const htmlCp = CP(top[0x3FDE]) || cp;
  const str = (dir, id) => {
    const tag = id.toString(16).toUpperCase().padStart(4, '0'), pre = (dir ? dir + '/' : '') + '__substg1.0_' + tag;
    const w = cf.get(pre + '001F');
    if (w) { let s = ''; for (let k = 0; k + 1 < w.length; k += 2) s += String.fromCharCode(u16(w, k)); return s.replace(/\0+$/, ''); }
    const a = cf.get(pre + '001E');
    return a ? EML.decode(a, cp).replace(/\0+$/, '') : '';
  };
  const bin = (dir, id) => cf.get((dir ? dir + '/' : '') + '__substg1.0_' + id.toString(16).toUpperCase().padStart(4, '0') + '0102');

  const msg = { headers: [], raw: '', subject: str('', 0x0037), from: '', to: str('', 0x0E04), cc: str('', 0x0E03), date: '', html: null, text: null, attachments: [], cids: {}, lost };
  // the sender: an internet address rather than Exchange's own (/O=…/CN=…)
  const name = str('', 0x0C1A) || str('', 0x0042), mail = [0x5D01, 0x5D02, 0x0C1F, 0x0065].map(id => str('', id)).find(a => a && !/^\/o=/i.test(a)) || '';
  msg.from = name && mail && name !== mail ? name + ' <' + mail + '>' : name || mail;
  const when = top[0x0039] || top[0x0E06] || top[0x3007];
  if (when) msg.date = when.toUTCString();
  // the recipients by name and address, for to and cc
  const rec = { 1: [], 2: [] };
  for (const r of cf.children('').filter(x => x.dir && /^__recip_version1\.0_/i.test(x.name))) {
    const p = fixed(r.path, 8), n = str(r.path, 0x3001), a = [str(r.path, 0x39FE), str(r.path, 0x3003)].find(x => x && !/^\/o=/i.test(x)) || '';
    const t = p[0x0C15] || 1;
    if (rec[t]) rec[t].push(n && a && n !== a ? n + ' <' + a + '>' : n || a);
  }
  if (rec[1].length) msg.to = rec[1].join(', ');
  if (rec[2].length) msg.cc = rec[2].join(', ');
  // the internet headers it came with; else the ones it has
  const th = str('', 0x007D);
  if (th) {
    msg.raw = th.trim();
    msg.headers = EML.headersOf(th).map(([k, v]) => [k, EML.words(v)]);
    const hf = msg.headers.find(x => x[0].toLowerCase() === 'from');
    if (hf) msg.from = hf[1];
  } else {
    msg.headers = [['Subject', msg.subject], ['From', msg.from], ['To', msg.to], ['Cc', msg.cc], ['Date', msg.date]].filter(x => x[1]);
    msg.raw = msg.headers.map(([k, v]) => k + ': ' + v).join('\n');
  }
  // the body
  const html = bin('', 0x1013) || null;
  if (html) msg.html = EML.decode(html, htmlCp);
  else if (str('', 0x1013)) msg.html = str('', 0x1013);
  if (!msg.html) {
    const c = bin('', 0x1009), rtfBytes = c && lzfu(c);
    if (rtfBytes) {
      const rtf = RTF.bytesToString(rtfBytes);
      if (/\\fromhtml1/.test(rtf)) msg.html = unwrapHtml(rtf, htmlCp);
      else if (!/\\fromtext/.test(rtf)) { const d = RTF.parse(rtf); msg.html = '<div style="font-family:' + Rich.esc(d.font) + ',sans-serif">' + d.html + '</div>'; }
    }
  }
  if (msg.html) msg.html = msg.html.replace(/\0+/g, '');
  else msg.text = str('', 0x1000);
  // attachments
  for (const a of cf.children('').filter(x => x.dir && /^__attach_version1\.0_/i.test(x.name))) {
    const nm = str(a.path, 0x3707) || str(a.path, 0x3704) || str(a.path, 0x3001) || 'attachment';
    const data = bin(a.path, 0x3701);
    if (!data) { lost['attached messages'] = (lost['attached messages'] || 0) + 1; continue; }
    const type = str(a.path, 0x370E) || 'application/octet-stream', cid = str(a.path, 0x3712);
    if (cid) msg.cids[cid] = { type, bytes: data };
    // pictures that are in the HTML body are not listed apart
    if (cid && msg.html && msg.html.includes('cid:' + cid)) continue;
    msg.attachments.push({ name: nm, type, bytes: data, cid });
  }
  return msg;
}

const MSG = { parse, lzfu, unwrapHtml };
return { MSG, readMail: async b => parse(b) };
});
