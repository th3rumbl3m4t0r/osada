/* Mail: .eml (RFC 5322, MIME) read into a message, and the view of a message (also for
   Outlook's .msg, msg.js). Part mail. A message: {headers: [[name, value]], raw (the
   header block), subject, from, to, cc, date, html, text, attachments: [{name, type,
   bytes, cid}]}. The view shows who and when, the body (HTML in a sandboxed frame that
   runs no script and loads nothing from outside until asked; else the plain text) and
   the attachments, to download or save next to the message. "headers" shows every header
   line, to copy or save as a .txt. Pictures from the internet stay out: the frame inherits
   the page's policy, which loads pictures only from here (and they would tell the sender). */
'use strict';

part('mail', () => {

/* bytes as a string of char codes 0-255: offsets stay byte offsets */
function binary(u8) {
  let s = '';
  for (let i = 0; i < u8.length; i += 8192) s += String.fromCharCode.apply(null, u8.subarray(i, i + 8192));
  return s;
}
const bytesOf = s => { const b = new Uint8Array(s.length); for (let i = 0; i < s.length; i++) b[i] = s.charCodeAt(i) & 0xFF; return b; };
const decoders = {};
function decode(bytes, charset) {
  let cs = (charset || 'utf-8').toLowerCase().trim().replace(/^"|"$/g, '');
  if (cs === 'us-ascii' || cs === 'ascii') cs = 'windows-1252';
  if (cs === 'gb2312') cs = 'gbk';
  try { return (decoders[cs] || (decoders[cs] = new TextDecoder(cs))).decode(bytes); } catch (e) { return (decoders.w || (decoders.w = new TextDecoder('windows-1252'))).decode(bytes); }
}

/* RFC 2047: =?charset?B|Q?text?= (spaces between two of them do not count) */
function words(s) {
  return s.replace(/(=\?[^?\s]+\?[BbQq]\?[^?\s]*\?=)\s+(?==\?[^?\s]+\?[BbQq]\?)/g, '$1').replace(/=\?([^?\s]+)\?([BbQq])\?([^?\s]*)\?=/g, (m, cs, enc, t) => {
    try {
      const raw = enc.toUpperCase() === 'B' ? atob(t.replace(/[^A-Za-z0-9+/=]/g, '')) : t.replace(/_/g, ' ').replace(/=([0-9A-Fa-f]{2})/g, (x, hh) => String.fromCharCode(parseInt(hh, 16)));
      return decode(bytesOf(raw), cs.replace(/\*.*$/, ''));
    } catch (e) { return m; }
  });
}
/* a header's value as text: raw 8-bit is taken as UTF-8 (RFC 6532), then encoded words */
const headerText = v => words(decode(bytesOf(v), 'utf-8'));

function headersOf(block) {
  const out = [];
  for (const line of block.replace(/\r?\n[ \t]+/g, ' ').split(/\r?\n/)) {
    const i = line.indexOf(':');
    if (i > 0) out.push([line.slice(0, i).trim(), line.slice(i + 1).trim()]);
  }
  return out;
}
const get = (hs, n) => { const h = hs.find(x => x[0].toLowerCase() === n); return h ? h[1] : ''; };

/* a structured value: 'type/sub; a=b; c*=utf-8''x' -> {value, params}, RFC 2231 included */
function structured(v) {
  const parts = [];
  let cur = '', q = false;
  for (const ch of v) {
    if (ch === '"') q = !q;
    if (ch === ';' && !q) { parts.push(cur); cur = ''; } else cur += ch;
  }
  parts.push(cur);
  const params = {}, cont = {};
  for (const p of parts.slice(1)) {
    const i = p.indexOf('=');
    if (i < 0) continue;
    const k = p.slice(0, i).trim().toLowerCase();
    let val = p.slice(i + 1).trim();
    if (val.startsWith('"')) val = val.slice(1, -1).replace(/\\(.)/g, '$1');
    const m = /^([^*]+)\*(\d+)?(\*)?$/.exec(k);
    if (m) { (cont[m[1]] = cont[m[1]] || []).push({ n: +(m[2] || 0), val, enc: !!m[3] || !m[2] }); continue; }
    params[k] = headerText(val);
  }
  for (const k in cont) {
    const segs = cont[k].sort((a, b) => a.n - b.n);
    let cs = 'utf-8', raw = '';
    segs.forEach((s, i) => {
      let v = s.val;
      if (i === 0 && s.enc && /^[^']*'[^']*'/.test(v)) { cs = v.split("'")[0] || 'utf-8'; v = v.split("'").slice(2).join("'"); }
      raw += s.enc ? v.replace(/%([0-9A-Fa-f]{2})/g, (x, hh) => String.fromCharCode(parseInt(hh, 16))) : v;
    });
    params[k] = decode(bytesOf(raw), cs);
  }
  return { value: parts[0].trim().toLowerCase(), params };
}

function transfer(body, enc) {
  enc = (enc || '').toLowerCase().trim();
  if (enc === 'base64') { try { return atob(body.replace(/[^A-Za-z0-9+/=]/g, '').replace(/=+(?=[^=])/g, '')); } catch (e) { return ''; } }
  if (enc === 'quoted-printable') return body.replace(/=\r?\n/g, '').replace(/=([0-9A-Fa-f]{2})/g, (m, hh) => String.fromCharCode(parseInt(hh, 16)));
  return body;
}

/* a message (bytes) -> the message model */
function parse(bytes) {
  const s = binary(bytes);
  const msg = { headers: [], raw: '', subject: '', from: '', to: '', cc: '', date: '', html: null, text: null, attachments: [], cids: {} };
  const split = /\r?\n\r?\n/.exec(s);
  const head = split ? s.slice(0, split.index) : s, body = split ? s.slice(split.index + split[0].length) : '';
  msg.raw = decode(bytesOf(head), 'utf-8');
  msg.headers = headersOf(head).map(([k, v]) => [k, headerText(v)]);
  for (const k of ['subject', 'from', 'to', 'cc', 'date']) msg[k] = get(msg.headers, k);
  walkPart(headersOf(head), body, msg, 0);
  return msg;
}

function walkPart(hs, body, msg, depth) {
  const ct = structured(get(hs, 'content-type') || 'text/plain'), cd = structured(get(hs, 'content-disposition') || '');
  const name = cd.params.filename || ct.params.name || '';
  if (ct.value.startsWith('multipart/') && ct.params.boundary && depth < 20) {
    const b = '--' + ct.params.boundary;
    const pieces = [];
    let at = body.indexOf(b);
    while (at >= 0) {
      const start = body.indexOf('\n', at);
      if (start < 0 || body.startsWith(b + '--', at)) break;
      const next = body.indexOf('\n' + b, start);
      pieces.push(body.slice(start + 1, next < 0 ? body.length : next).replace(/\r$/, ''));
      at = next < 0 ? -1 : next + 1;
    }
    const subs = pieces.map(p => { const m = /\r?\n\r?\n/.exec(p); return m ? [headersOf(p.slice(0, m.index)), p.slice(m.index + m[0].length)] : [headersOf(p), '']; });
    if (ct.value === 'multipart/alternative') {
      // the richest that is understood: HTML, else text (the last of a kind wins)
      const kinds = subs.map(([h]) => structured(get(h, 'content-type') || 'text/plain').value);
      let pick = kinds.lastIndexOf('text/html');
      if (pick < 0) pick = kinds.findIndex(k => k.startsWith('multipart/'));
      if (pick < 0) pick = kinds.lastIndexOf('text/plain');
      if (pick < 0) pick = 0;
      if (subs[pick]) walkPart(subs[pick][0], subs[pick][1], msg, depth + 1);
      // the others may still carry attachments (calendar invitations)
      subs.forEach(([h, bd], i) => { if (i !== pick && !/^text\/(plain|html)$/.test(structured(get(h, 'content-type') || 'text/plain').value)) walkPart(h, bd, msg, depth + 1); });
      return;
    }
    for (const [h, bd] of subs) walkPart(h, bd, msg, depth + 1);
    return;
  }
  const data = transfer(body, get(hs, 'content-transfer-encoding'));
  const cid = (get(hs, 'content-id') || '').replace(/^<|>$/g, '');
  if (ct.value === 'message/rfc822' || ct.value === 'message/global') {
    const sub = /\r?\n\r?\n/.exec(data);
    const subj = sub ? get(headersOf(data.slice(0, sub.index)).map(([k, v]) => [k, headerText(v)]), 'subject') : '';
    msg.attachments.push({ name: name || (subj ? subj.replace(/[\\/:*?"<>|]+/g, '_').slice(0, 80) : 'message') + '.eml', type: 'message/rfc822', bytes: bytesOf(data), cid });
    return;
  }
  const attached = cd.value === 'attachment' || (name && !(cd.value === 'inline' && /^text\//.test(ct.value)));
  if (!attached && (ct.value === 'text/html' || ct.value === 'text/plain')) {
    const t = decode(bytesOf(data), ct.params.charset);
    if (ct.value === 'text/html') msg.html = (msg.html || '') + t;
    else msg.text = msg.text === null ? t : msg.text + '\n' + t;
    return;
  }
  const bytes = bytesOf(data);
  if (cid) msg.cids[cid] = { type: ct.value, bytes };
  if (attached || !cid || !/^image\//.test(ct.value)) msg.attachments.push({ name: name || (cid || 'part') + extFor(ct.value), type: ct.value, bytes, cid });
}
const extFor = t => ({ 'image/png': '.png', 'image/jpeg': '.jpg', 'image/gif': '.gif', 'text/calendar': '.ics', 'text/plain': '.txt', 'text/html': '.html', 'application/pdf': '.pdf' }[t] || '');

/* ---------------- the view ---------------- */
function dataUrl(type, bytes) {
  let s = '';
  for (let i = 0; i < bytes.length; i += 8192) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 8192));
  return 'data:' + type + ';base64,' + btoa(s);
}

/* the HTML body as a document of its own: no script, nothing from outside */
function frameDoc(html, cids) {
  const body = html.replace(/cid:([^"'\s)>]+)/gi, (m, id) => { const c = cids[decodeURIComponent(id)] || cids[id]; return c ? dataUrl(c.type || 'application/octet-stream', c.bytes) : m; });
  const csp = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:";
  return '<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="' + csp + '"><base target="_blank">' +
    '<style>body{font-family:sans-serif;font-size:14px;margin:12px;color:#111;background:#fff;overflow-wrap:break-word}img{max-width:100%;height:auto}</style></head><body>' + body + '</body></html>';
}

/* host: the box; msg: a message; opts = {save(name, bytes): kept next to the message, or null} */
class MailView {
  constructor(host, msg, opts) {
    this.msg = msg;
    this.urls = [];
    const save = opts && opts.save;
    const row = (k, v) => (v ? h('div', { class: 'mail-h' }, h('span', { class: 'k', text: tx(k) }), h('span', { class: 'v', text: v })) : null);
    const when = msg.date ? (() => { const d = new Date(msg.date); return isNaN(d) ? msg.date : fmtTime(Math.floor(d / 1000)) + '  (' + msg.date + ')'; })() : '';
    this.hdrs = h('pre', { class: 'mail-raw', hidden: true, text: msg.raw || msg.headers.map(([k, v]) => k + ': ' + v).join('\n') });
    const bHead = h('button', { text: tx('headers'), title: tx('every header line, to copy or save') });
    bHead.addEventListener('click', () => { this.hdrs.hidden = !this.hdrs.hidden; bHead.classList.toggle('on', !this.hdrs.hidden); });
    const bCopy = h('button', { text: tx('copy headers'), onclick: () => copyText(this.hdrs.textContent).then(() => toast(tx('copied'))) });
    const bSave = h('button', { text: tx('headers as .txt'), title: tx('download the header lines as a text file'), onclick: () => this.download('headers.txt', 'text/plain;charset=utf-8', new TextEncoder().encode(this.hdrs.textContent)) });
    const atts = msg.attachments.map(a => h('span', { class: 'mail-att' },
      h('a', { href: this.url(a), download: a.name, text: a.name, title: tx('download') }), h('span', { class: 'dim', text: ' ' + fmtSize(a.bytes.length) }),
      save ? h('button', { text: tx('save here'), title: tx('save it next to the message'), onclick: () => save(a.name, a.bytes).then(() => toast(tx('saved {name}', { name: a.name })), fail) }) : null));
    this.body = h('div', { class: 'mail-body' });
    this.note = h('div', { class: 'viewer-note dim', hidden: true });
    clear(host).append(
      h('div', { class: 'mail-head' }, h('div', { class: 'mail-subject', text: msg.subject || tx('(no subject)') }),
        row('from', msg.from), row('to', msg.to), row('cc', msg.cc), row('date', when),
        h('div', { class: 'row mail-acts' }, bHead, bCopy, bSave), this.hdrs,
        atts.length ? h('div', { class: 'mail-atts' }, h('span', { class: 'k', text: tx('attachments') }), atts) : null),
      this.note, this.body);
    host.classList.add('mail-host');
    this.paint();
  }

  url(a) { const u = URL.createObjectURL(new Blob([a.bytes], { type: a.type || 'application/octet-stream' })); this.urls.push(u); return u; }
  download(name, type, bytes) {
    const u = URL.createObjectURL(new Blob([bytes], { type }));
    const a = h('a', { href: u, download: name });
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(u), 60000);
  }

  paint() {
    const m = this.msg;
    if (m.html) {
      const f = h('iframe', { class: 'mail-frame', sandbox: 'allow-popups allow-popups-to-escape-sandbox', title: tx('the message'), referrerpolicy: 'no-referrer' });
      f.srcdoc = frameDoc(m.html, m.cids || {});
      this.body.replaceChildren(f);
      if (/<img[^>]+src=["']?https?:/i.test(m.html) || /url\(\s*["']?https?:/i.test(m.html)) {
        this.note.hidden = false;
        this.note.textContent = tx('pictures from the internet are not shown (the sender would see that the message was opened).');
      }
    } else this.body.replaceChildren(h('pre', { class: 'mail-text', text: m.text || '' }));
  }

  destroy() { this.urls.forEach(u => URL.revokeObjectURL(u)); }
}

const EML = { parse, decode, words, headersOf, structured };
return { EML, MailView, readMail: async b => parse(b) };
});
