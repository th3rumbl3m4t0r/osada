/* Calendars (.ics) and contacts (.vcf): what is in them, readable. Part cal. Both are lines
   of NAME;PARAM=…:VALUE (RFC 5545, RFC 6350, and vCard 2.1 with its quoted-printable and
   charsets). A calendar shows its events and to-dos by date; a contact file its cards with
   names, numbers, addresses and the photo when it is in the file. */
'use strict';

part('cal', () => {

/* lines -> [{name, params, value}] (unfolded, values still escaped) */
function lines(text) {
  const out = [];
  const raw = text.replace(/\r\n?/g, '\n').replace(/\n[ \t]/g, '').split('\n');
  for (let i = 0; i < raw.length; i++) {
    let l = raw[i];
    // vCard 2.1: quoted-printable soft breaks end in '='
    while (/QUOTED-PRINTABLE/i.test(l.split(':')[0]) && l.endsWith('=') && i + 1 < raw.length) l = l.slice(0, -1) + raw[++i];
    const m = /^([^:;]+)((?:;[^:;]*(?:"[^"]*"[^:;]*)*)*):(.*)$/.exec(l);
    if (!m) continue;
    // ;NAME=a,"b, c";TYPE (a bare word: a vCard 2.1 type)
    const params = {};
    for (const p of m[2].match(/;[^;"]*(?:"[^"]*"[^;"]*)*/g) || []) {
      const e = p.indexOf('=');
      if (e < 0) { (params.TYPE = params.TYPE || []).push(p.slice(1).toUpperCase()); continue; }
      params[p.slice(1, e).toUpperCase()] = (p.slice(e + 1).match(/"[^"]*"|[^,]+/g) || []).map(v => v.replace(/^"|"$/g, ''));
    }
    out.push({ name: m[1].replace(/^item\d+\./i, '').toUpperCase(), params, value: m[3] });
  }
  return out;
}
const unesc = v => v.replace(/\\([nN,;\\])/g, (_, c) => (c === 'n' || c === 'N' ? '\n' : c));
function decode(p) {
  let v = p.value;
  const enc = (p.params.ENCODING || [''])[0].toUpperCase();
  if (enc === 'QUOTED-PRINTABLE') {
    const b = [];
    for (let i = 0; i < v.length; i++) { if (v[i] === '=' && /^[0-9A-F]{2}$/i.test(v.substr(i + 1, 2))) { b.push(parseInt(v.substr(i + 1, 2), 16)); i += 2; } else b.push(v.charCodeAt(i) & 0xFF); }
    try { v = new TextDecoder((p.params.CHARSET || ['utf-8'])[0]).decode(new Uint8Array(b)); } catch (e) { v = new TextDecoder().decode(new Uint8Array(b)); }
  }
  return unesc(v);
}

/* components: {type, props: [{name, params, value}], kids} */
function tree(text) {
  const root = { type: '', props: [], kids: [] }, stack = [root];
  for (const p of lines(text)) {
    const top = stack[stack.length - 1];
    if (p.name === 'BEGIN') { const c = { type: p.value.trim().toUpperCase(), props: [], kids: [] }; top.kids.push(c); stack.push(c); } else if (p.name === 'END') { if (stack.length > 1) stack.pop(); } else top.props.push(p);
  }
  return root;
}
const prop = (c, n) => c.props.find(p => p.name === n);
const val = (c, n) => { const p = prop(c, n); return p ? decode(p) : ''; };
const all = (c, n) => c.props.filter(p => p.name === n);

/* 20261025, 20261025T100000Z, with TZID -> {date: Date, allDay, tz} */
function when(p) {
  if (!p) return null;
  const m = /^(\d{4})(\d{2})(\d{2})(?:T(\d{2})(\d{2})(\d{2})?(Z)?)?/.exec(p.value.trim());
  if (!m) return null;
  const [, y, mo, d, hh, mi, ss, z] = m;
  if (!hh) return { date: new Date(+y, mo - 1, +d), allDay: true };
  if (z) return { date: new Date(Date.UTC(+y, mo - 1, +d, +hh, +mi, +(ss || 0))) };
  return { date: new Date(+y, mo - 1, +d, +hh, +mi, +(ss || 0)), tz: (p.params.TZID || [''])[0] };
}
const fmtDay = d => d.toLocaleDateString(uiLocale(), { weekday: 'short', year: 'numeric', month: 'short', day: 'numeric' });
const fmtHm = d => d.toLocaleTimeString(uiLocale(), { hour: '2-digit', minute: '2-digit' });

function readCal(text) {
  const root = tree(text);
  const cals = root.kids.filter(k => k.type === 'VCALENDAR');
  if (!cals.length) throw new Error('not a calendar (no VCALENDAR)');
  const items = [];
  for (const cal of cals) for (const c of cal.kids.filter(k => k.type === 'VEVENT' || k.type === 'VTODO' || k.type === 'VJOURNAL')) {
    const start = when(prop(c, 'DTSTART')) || when(prop(c, 'DUE')), end = when(prop(c, 'DTEND'));
    items.push({
      type: c.type, start, end, summary: val(c, 'SUMMARY'), where: val(c, 'LOCATION'), text: val(c, 'DESCRIPTION'), url: val(c, 'URL'),
      repeat: val(c, 'RRULE'), status: val(c, 'STATUS'),
      who: all(c, 'ORGANIZER').concat(all(c, 'ATTENDEE')).map(p => ((p.params.CN || [''])[0] || p.value.replace(/^mailto:/i, '')) + (p.name === 'ORGANIZER' ? ' *' : '')),
      alarm: c.kids.some(k => k.type === 'VALARM'),
    });
  }
  items.sort((a, b) => (a.start ? +a.start.date : Infinity) - (b.start ? +b.start.date : Infinity));
  return { name: val(cals[0], 'X-WR-CALNAME'), items };
}

/* RRULE, said shortly: FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10 */
function sayRule(r) {
  const o = Object.fromEntries(r.split(';').map(x => x.split('=')));
  const every = { DAILY: tx('every day'), WEEKLY: tx('every week'), MONTHLY: tx('every month'), YEARLY: tx('every year') }[o.FREQ] || o.FREQ;
  const bits = [(o.INTERVAL && o.INTERVAL !== '1' ? o.INTERVAL + '× ' : '') + every];
  if (o.BYDAY) bits.push(o.BYDAY);
  if (o.COUNT) bits.push(tx('{n} times', { n: o.COUNT }));
  if (o.UNTIL) { const u = when({ value: o.UNTIL, params: {} }); if (u) bits.push(tx('until {date}', { date: fmtDay(u.date) })); }
  return bits.join(', ');
}

function readCards(text) {
  const cards = tree(text).kids.filter(k => k.type === 'VCARD');
  if (!cards.length) throw new Error('no contacts in this file (no VCARD)');
  return cards.map(c => {
    const n = val(c, 'N').split(';');
    const photo = prop(c, 'PHOTO') || prop(c, 'LOGO');
    let pic = null;
    if (photo) {
      const v = photo.value.trim(), enc = (photo.params.ENCODING || [''])[0].toUpperCase();
      if (/^data:image\//.test(v)) pic = v;
      else if (enc === 'B' || enc === 'BASE64') pic = 'data:image/' + ((photo.params.TYPE || ['jpeg'])[0].toLowerCase().replace('jpg', 'jpeg')) + ';base64,' + v.replace(/\s/g, '');
    }
    const typed = name => all(c, name).map(p => ({ type: [].concat(p.params.TYPE || []).map(t => t.toLowerCase()).filter(t => t !== 'internet' && t !== 'pref' && t !== 'voice').join(', '), value: decode(p) }));
    return {
      name: val(c, 'FN') || [n[3], n[1], n[2], n[0], n[4]].filter(Boolean).join(' ').trim() || val(c, 'ORG').replace(/;/g, ' ').trim(),
      org: val(c, 'ORG').split(';').filter(Boolean).join(', '), title: val(c, 'TITLE'), pic,
      tel: typed('TEL'), email: typed('EMAIL'), url: typed('URL'),
      adr: typed('ADR').map(a => ({ type: a.type, value: a.value.split(';').filter(s => s.trim()).join(', ') })),
      bday: val(c, 'BDAY'), note: val(c, 'NOTE'),
    };
  }).sort((a, b) => collator.compare(a.name, b.name));
}

class CalView {
  constructor(host, cal) {
    const list = h('div', { class: 'cal-list' });
    const q = h('input', { type: 'search', placeholder: tx('filter'), 'aria-label': tx('filter') });
    clear(host).append(h('div', { class: 'toolbar' }, cal.name ? h('b', { text: cal.name }) : null, h('span', { class: 'dim', text: txn(cal.items.length, '{n} entry', '{n} entries') }), h('span', { class: 'spacer' }), q), list);
    const draw = () => {
      const f = q.value.trim().toLowerCase();
      clear(list);
      let day = null;
      for (const it of cal.items) {
        if (f && ![it.summary, it.where, it.text].join(' ').toLowerCase().includes(f)) continue;
        const d = it.start ? fmtDay(it.start.date) : tx('no date');
        if (d !== day) { list.append(h('h3', { class: 'cal-day', text: d })); day = d; }
        let time = it.start && !it.start.allDay ? fmtHm(it.start.date) : tx('all day');
        if (it.end && !it.end.allDay && it.start && !it.start.allDay) time += ' – ' + (fmtDay(it.end.date) === d ? '' : fmtDay(it.end.date) + ' ') + fmtHm(it.end.date);
        if (it.start && it.start.tz) time += ' (' + it.start.tz + ')';
        list.append(h('div', { class: 'cal-item' + (it.status === 'CANCELLED' ? ' cal-off' : '') },
          h('div', { class: 'cal-time dim', text: it.type === 'VTODO' ? tx('to do') + (it.start ? ', ' + time : '') : time }),
          h('div', { class: 'cal-body' },
            h('b', { text: it.summary || tx('(no title)') }),
            it.where ? h('div', { text: it.where }) : null,
            it.repeat ? h('div', { class: 'dim', text: sayRule(it.repeat) }) : null,
            it.who.length ? h('div', { class: 'dim', text: it.who.join(', ') }) : null,
            it.text ? h('div', { class: 'cal-text', text: it.text }) : null,
            /^https?:/.test(it.url) ? h('a', { href: it.url, target: '_blank', rel: 'noopener noreferrer', text: it.url }) : null)));
      }
      if (!list.firstChild) list.append(h('p', { class: 'dim', text: tx('nothing here') }));
    };
    q.addEventListener('input', draw);
    draw();
  }
  destroy() {}
}

class CardView {
  constructor(host, cards) {
    const list = h('div', { class: 'card-list' });
    const q = h('input', { type: 'search', placeholder: tx('filter'), 'aria-label': tx('filter') });
    clear(host).append(h('div', { class: 'toolbar' }, h('span', { class: 'dim', text: txn(cards.length, '{n} contact', '{n} contacts') }), h('span', { class: 'spacer' }), q), list);
    const row = (label, items, link) => items.map(x => h('div', { class: 'card-row' }, h('span', { class: 'dim', text: x.type || label }),
      link ? h('a', { href: link(x.value), target: '_blank', rel: 'noopener noreferrer', text: x.value }) : h('span', { text: x.value })));
    const draw = () => {
      const f = q.value.trim().toLowerCase();
      clear(list);
      for (const c of cards) {
        if (f && ![c.name, c.org, ...c.tel.map(x => x.value), ...c.email.map(x => x.value)].join(' ').toLowerCase().includes(f)) continue;
        list.append(h('div', { class: 'card' },
          c.pic ? h('img', { class: 'card-pic', src: c.pic, alt: '' }) : h('div', { class: 'card-pic card-initial', text: (c.name || '?').slice(0, 1).toUpperCase() }),
          h('div', { class: 'card-body' },
            h('b', { text: c.name || tx('(no name)') }),
            c.org || c.title ? h('div', { class: 'dim', text: [c.title, c.org].filter(Boolean).join(', ') }) : null,
            row(tx('phone'), c.tel, v => 'tel:' + v.replace(/[^+\d]/g, '')), row(tx('email'), c.email, v => 'mailto:' + v),
            row(tx('address'), c.adr), row('web', c.url.filter(u => /^https?:/.test(u.value)), v => v),
            c.bday ? h('div', { class: 'card-row' }, h('span', { class: 'dim', text: tx('birthday') }), h('span', { text: c.bday })) : null,
            c.note ? h('div', { class: 'cal-text', text: c.note }) : null)));
      }
    };
    q.addEventListener('input', draw);
    draw();
  }
  destroy() {}
}

return { readCal, readCards, CalView, CardView, CAL: { lines, tree, when } };
});
