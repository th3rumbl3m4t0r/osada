/* Sharing one item: users, public links, API keys. Part share (util.js): loaded when the page opens, it
   uses the app's helpers (app.js, util.js) as globals. */
'use strict';

part('share', () => {

/* ---- API keys: a program gets one folder (docs/API.md) ---- */
function keyRows(keys, again, withFolder) {
  return keys.map(k => {
    const expired = k.expires && k.expires * 1000 < Date.now();
    return h('tr', {},
      h('td', { text: k.name }),
      withFolder ? h('td', {}, h('a', { href: U.files(k.loc, k.path), text: locName(k) + ':' + k.path })) : null,
      h('td', { class: k.write ? 'ok' : 'dim', text: k.write ? tx('can edit') : tx('read only') }),
      h('td', { class: expired ? 'bad' : 'dim', text: k.expires ? (expired ? tx('expired {date}', { date: fmtDate(k.expires) }) : tx('until {date}', { date: fmtDate(k.expires) })) : tx('no expiry') }),
      h('td', { class: 'dim', text: k.last_used ? tx('used {time}', { time: fmtTime(k.last_used) }) : tx('never used') }),
      h('td', { class: 'act' }, btn(tx('remove'), tx('the key stops working'), async () => {
        if (await confirmBox(tx('remove API key'), tx('remove {name}? whatever uses it loses access.', { name: k.name }), tx('remove'), true)) api('DELETE', '/api/keys/' + k.id).then(again, fail);
      }, 'danger')));
  });
}

/* the key is shown once, here */
function keyShown(r) {
  const key = h('input', { type: 'text', value: r.key, readonly: true, spellcheck: false, class: 'wide mono' });
  const base = absUrl('/api/v1');
  return dialog({
    title: tx('API key {name}', { name: r.name }), wide: true,
    body: [h('p', { text: tx('copy the key now: osada keeps only a hash of it and cannot show it again.') }),
      h('div', { class: 'row' }, key, btn(tx('copy'), null, () => copyText(r.key).then(ok => toast(ok ? tx('copied') : tx('copy failed'), ok ? '' : 'err')))),
      h('p', { class: 'dim', text: r.write ? tx('the program sends it as "Authorization: Bearer <key>" to {url} (read and write). for example:', { url: base })
        : tx('the program sends it as "Authorization: Bearer <key>" to {url} (read only). for example:', { url: base }) }),
      h('pre', { class: 'mono', text: 'curl -H "Authorization: Bearer ' + r.key + '" ' + base + '/list' }),
      h('p', { class: 'dim', text: tx('the API is described in docs/API.md in the osada repository.') })],
    buttons: [{ label: tx('done'), primary: true, value: true }],
    opened: () => key.select(),
  });
}

/* ---- share one item (Round 21): one search for anyone, then who has it ----
   The search box on top takes a name or anyone's address (a UPN, user@domain) and looks, in this
   order: 1) people of this instance, 2) federated instances, 3) public e-mail: an address nobody
   here has gets a view-only link mailed to it from the person's own mailbox ("public link sent to
   …"). Below: who it is shared with, the links, and for folders the API keys. */
function shareDialog(loc, path, name) {
  const box = h('div', { class: 'share' });
  const q = '?loc=' + encodeURIComponent(loc) + '&path=' + encodeURIComponent(path);
  const copied = ok => toast(ok ? tx('copied') : tx('copy failed'), ok ? '' : 'err');
  let item = null;
  // the search, always on top; its results under it
  const find = h('input', { type: 'search', class: 'wide share-find', placeholder: tx('a name, or anyone\'s address: user@domain'), autocomplete: 'off', spellcheck: false });
  const results = h('div', { class: 'share-results', hidden: true });
  let asked = 0, timer = null;
  const head = t => h('div', { class: 'lbl', text: t });
  async function search() {
    const text = find.value.trim();
    if (text.length < 2) { results.hidden = true; return; }
    const mine = ++asked;
    let r;
    try { r = await api('GET', '/api/people?q=' + encodeURIComponent(text)); } catch (e) { fail(e); return; }
    if (mine !== asked) return;
    const taken = new Set(((item && item.shares) || []).map(s => s.user));
    const local = r.local.map(p => {
      const edit = h('input', { type: 'checkbox', title: tx('can change, add and delete') });
      const act = p.me ? h('span', { class: 'dim', text: tx('(you)') })
        : taken.has(p.user) ? h('span', { class: 'ok', text: tx('shared') })
        : btn(tx('share'), tx('share with {user}', { user: p.upn }), () => api('POST', '/api/shares', { loc, path, user: p.user, write: edit.checked }).then(() => { toast(tx('shared with {user}', { user: p.upn })); load(); search(); }, fail), 'primary');
      return h('tr', { class: 'person' }, h('td', {}, h('b', { text: p.display })), h('td', { class: 'mono', text: p.upn }),
        h('td', {}, p.me || taken.has(p.user) ? null : h('label', { class: 'chk' }, edit, ' ' + tx('can edit'))), h('td', { class: 'act' }, act));
    });
    // 2) someone of a federated instance (Round 22): view only, through their instance
    const fedTaken = new Set(((item && item.federated) || []).map(f => f.upn));
    const fed = r.federated.map(p => h('tr', { class: 'person fed' }, h('td', {}, h('b', { text: p.display || p.upn })), h('td', { class: 'mono', text: p.upn }),
      h('td', { class: 'dim', text: tx('on {instance}, view only', { instance: p.instance || '' }) }),
      h('td', { class: 'act' }, fedTaken.has(p.upn) ? h('span', { class: 'ok', text: tx('shared') })
        : btn(tx('share'), tx('share with {user}: view only, through their instance', { user: p.upn }), () => api('POST', '/api/shares', { loc, path, user: p.upn, write: false }).then(() => { toast(tx('shared with {user}', { user: p.upn })); load(); search(); }, fail), 'primary'))));
    const parts = [head(tx('1. on this server')),
      local.length ? h('table', { class: 'x11' }, h('tbody', {}, local)) : h('div', { class: 'dim', text: tx('nobody here matches') }),
      head(tx('2. federated instances')),
      fed.length ? h('table', { class: 'x11' }, h('tbody', {}, fed)) : h('div', { class: 'dim', text: r.federation === 'none' ? tx('none set up') : r.federation === 'unreachable' ? tx('their instance does not answer') : tx('nobody there matches') })];
    if (r.email) {
      const e = r.email;
      parts.push(head(tx('3. public e-mail')));
      if (!e.can_send) parts.push(h('div', { class: 'bad', text: tx('{address}: cannot mail a link to it: {why}', { address: e.address, why: tx(e.why_not || '') }) }));
      else {
        const days = h('input', { type: 'number', min: 1, max: 3650, placeholder: tx('never'), class: 'num' });
        const send = btn(tx('send link'), tx('a view-only link, mailed from your mailbox'), async () => {
          send.disabled = true;
          try {
            await api('POST', '/api/links', { loc, path, days: days.value ? +days.value : null, send_to: e.address });
            toast(tx('public link sent to {address}', { address: e.address }));
            find.value = ''; results.hidden = true; load();
          } catch (err) { fail(err); send.disabled = false; }
        }, 'primary');
        parts.push(h('div', { class: 'dim', text: tx('{address} is nobody here: they get a link to open it, view only, mailed from your mailbox.', { address: e.address }) }),
          h('div', { class: 'row' }, h('span', { class: 'dim', text: tx('expires after') }), days, h('span', { class: 'dim', text: tx('days') }), send));
      }
    } else if (text.includes('@') && !local.length) parts.push(h('div', { class: 'dim', text: tx('not an e-mail address') }));
    results.replaceChildren(...parts);
    results.hidden = false;
  }
  find.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(search, 250); });
  find.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); clearTimeout(timer); search(); } });
  const searchBtn = btn(tx('search'), tx('this server, then federated instances, then public e-mail'), () => { clearTimeout(timer); search(); }, 'primary');
  const top = h('div', { class: 'row share-top' }, find, searchBtn);
  const below = h('div', {});
  box.replaceChildren(top, results, below);

  async function load() {
    const [it, allKeys] = await Promise.all([api('GET', '/api/shares/item' + q), api('GET', '/api/keys')]);
    item = it;
    const urows = it.shares.map(s => {
      const w = h('input', { type: 'checkbox', checked: s.write, title: tx('can change, add and delete') });
      w.addEventListener('change', () => api('POST', '/api/shares', { loc, path, user: s.user, write: w.checked }).then(load, fail));
      return h('tr', {}, h('td', {}, h('b', { text: s.display || s.user })), h('td', { class: 'mono', text: s.upn || s.user }),
        h('td', {}, h('label', { class: 'chk' }, w, ' ' + tx('can edit'))), h('td', { class: 'dim', text: fmtDate(s.created) }),
        h('td', { class: 'act' }, btn(tx('remove'), tx('stop sharing with {user}', { user: s.upn || s.user }), () => api('DELETE', '/api/shares/' + s.id).then(load, fail))));
    });
    const linkRow = l => {
      const url = absUrl(l.url);
      const expired = l.expires && l.expires * 1000 < Date.now();
      return h('tr', {}, l.sent_to ? h('td', {}, tx('public link sent to'), ' ', h('b', { class: 'mono', text: l.sent_to })) : null,
        h('td', { class: 'url' }, h('a', { href: url, target: '_blank', rel: 'noopener', text: url })),
        h('td', { class: expired ? 'bad' : 'dim', text: l.expires ? (expired ? tx('expired {date}', { date: fmtDate(l.expires) }) : tx('until {date}', { date: fmtDate(l.expires) })) : tx('no expiry') }),
        h('td', { class: 'dim', text: (l.sent_to ? fmtDate(l.created) + ' · ' : '') + txn(l.hits, 'opened {n} time', 'opened {n} times') }),
        h('td', { class: 'act' }, btn(tx('copy'), null, () => copyText(url).then(copied)),
          btn(tx('remove'), tx('the link stops working'), () => api('DELETE', '/api/links/' + l.id).then(load, fail))));
    };
    const frows = (it.federated || []).map(f => h('tr', { class: 'fed' }, h('td', {}, h('b', { text: f.upn })), h('td', { class: 'dim', text: tx('on {instance}', { instance: f.instance }) }),
      h('td', { class: 'dim', text: tx('view only') }), h('td', { class: 'dim', text: fmtDate(f.created) }),
      h('td', { class: 'act' }, btn(tx('remove'), tx('stop sharing with {user}', { user: f.upn }), () => api('DELETE', '/api/fedshares/' + f.id).then(load, fail)))));
    const sent = it.links.filter(l => l.sent_to).map(linkRow);
    const lrows = it.links.filter(l => !l.sent_to).map(linkRow);
    const days = h('input', { type: 'number', min: 1, max: 3650, placeholder: tx('never'), class: 'num' });
    const mk = btn(tx('create link'), null, async () => {
      try {
        const r = await api('POST', '/api/links', { loc, path, days: days.value ? +days.value : null });
        copied(await copyText(absUrl(r.url)));
        load();
      } catch (e) { fail(e); }
    }, 'primary');
    const parts = [
      h('div', { class: 'lbl gap', text: tx('shared with') }),
      urows.length || frows.length || sent.length ? h('table', { class: 'x11 share-with' }, h('tbody', {}, urows, frows, sent)) : h('div', { class: 'dim', text: tx('not shared with anyone') }),
      h('div', { class: 'lbl gap', text: tx('links: anyone with the link can open it, no login') }),
      lrows.length ? h('table', { class: 'x11' }, h('tbody', {}, lrows)) : h('div', { class: 'dim', text: tx('no links') }),
      h('div', { class: 'row' }, h('span', { class: 'dim', text: tx('expires after') }), days, h('span', { class: 'dim', text: tx('days') }), mk)];
    if (it.dir) {
      const krows = keyRows(allKeys.filter(k => k.loc === loc && k.path === it.path), load, false);
      const kname = h('input', { type: 'text', placeholder: tx('what uses it, e.g. backup script'), maxlength: 60 });
      const kwrite = h('input', { type: 'checkbox' });
      const kdays = h('input', { type: 'number', min: 1, max: 3650, placeholder: tx('never'), class: 'num' });
      const mkKey = btn(tx('create key'), null, async () => {
        try {
          const r = await api('POST', '/api/keys', { loc, path, name: kname.value, write: kwrite.checked, days: kdays.value ? +kdays.value : null });
          kname.value = kdays.value = '';
          await keyShown(r);
          load();
        } catch (e) { fail(e); }
      }, 'primary');
      parts.push(
        h('div', { class: 'lbl gap', text: tx('API keys: a program with the key can use this folder and what is in it, nothing else') }),
        krows.length ? h('table', { class: 'x11' }, h('tbody', {}, krows)) : h('div', { class: 'dim', text: tx('no keys') }),
        h('div', { class: 'row' }, kname, h('label', { class: 'chk' }, kwrite, ' ' + tx('can edit')),
          h('span', { class: 'dim', text: tx('expires after') }), kdays, h('span', { class: 'dim', text: tx('days') }), mkKey));
    }
    below.replaceChildren(...parts);
  }
  return dialog({
    title: tx('share {name}', { name }), wide: true, body: box,
    buttons: [{ label: tx('close'), value: true, primary: true }],
    opened: () => { find.focus(); return load().catch(e => below.replaceChildren(h('div', { class: 'bad', text: e.message }))); },
  });
}

return { shareDialog, keyRows, keyShown };
});
