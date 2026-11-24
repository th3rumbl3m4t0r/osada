/* The settings page: password, storage, phones, API keys. Part settings (util.js): loaded when the page opens, it
   uses the app's helpers (app.js, util.js) as globals. */
'use strict';

part('settings', ({ keyRows, mountDialog }) => {

/* ---- settings: phones ----
   Phones paired for folder sync, which only puts files into the home. Inside the
   android app, window.OsadaApp also runs this phone's side: pairing, the folders
   and where they go, and the last run's outcome; the app calls osadaAppEvent(). */
function phonesWin(devices, again) {
  const app = window.OsadaApp;
  let info = app ? JSON.parse(app.info()) : null;
  const rows = devices.map(d => h('tr', {},
    h('td', {}, d.name, info && info.paired && d.id === info.device_id ? h('span', { class: 'dim', text: ' (' + tx('this phone') + ')' }) : null),
    h('td', { class: 'dim', text: tx('paired {date}', { date: fmtDate(d.created) }) }),
    h('td', { class: 'dim', text: tx('seen {time}', { time: fmtTime(d.last_seen) }) }),
    h('td', { text: txn(d.files, '{n} file put', '{n} files put') }),
    h('td', { class: 'act' }, btn(tx('remove'), tx('the phone stops syncing; the files it put stay'), async () => {
      if (!(await confirmBox(tx('remove phone'), tx('remove {name}? it stops syncing; the files it put stay.', { name: d.name }), tx('remove'), true))) return;
      api('DELETE', '/api/devices/' + d.id).then(() => { if (info && d.id === info.device_id) app.unpair(); again(); }, fail);
    }, 'danger'))));
  const body = [rows.length ? h('table', { class: 'x11' }, h('tbody', {}, rows)) : null];
  if (!app) {
    const url = 'http://' + location.hostname + ':88/';
    body.push(h('p', { class: 'dim' }, tx('the osada app for android puts phone folders (camera, screenshots, ...) here, one way. install it') + ' ',
      S.me.remote ? tx('from port 88 of this server, at home') : h('a', { href: url, text: url }), ' ' + tx('(Obtainium can keep it up to date from there), then pair the phone here, in the app.')));
    return { el: win(tx('phones'), body) };
  }
  if (!info.paired) {
    body.push(h('p', { class: 'dim', text: tx('pairing gives this phone a key of its own; it can only put files into your home, never change or remove what somebody else put there.') }),
      h('div', { class: 'row' }, btn(tx('pair this phone'), null, () => api('POST', '/api/devices', { name: info.model })
        .then(r => { app.pair(r.token, r.username, r.name, r.id); again(); }, fail), 'primary')));
    return { el: win(tx('phones'), body) };
  }

  const folders = h('div'), status = h('p', { class: 'dim' });
  const wifi = h('input', { type: 'checkbox', checked: info.wifi_only, onchange: () => app.setWifiOnly(wifi.checked) });
  const refused = f => dialog({
    title: tx('refused in {name}', { name: f.name }),
    body: [h('p', { class: 'dim', text: tx('osada keeps these as they are there; they are tried again when they change on the phone, or now.') }),
      h('table', { class: 'x11' }, h('tbody', {}, f.problems.map(p => h('tr', {}, h('td', { text: p.path }), h('td', { class: 'dim', text: p.why })))))],
    buttons: [{ label: tx('close'), value: null }, { label: tx('try again'), primary: true, value: true }],
  }).then(v => { if (v) { app.retry(f.id); toast(tx('tried again at the next sync')); show(); } });
  const dest = (f, label) => ask(tx('sync {name}', { name: f.name }), tx('into this folder of your home'), f.dest, label);
  let shown = '';
  function show() {
    info = JSON.parse(app.info());
    const st = info.status;
    status.textContent = st.running || info.running ? tx('syncing:') + ' ' + (st.text || '')
      : st.finished ? tx('last sync {time}', { time: fmtTime(st.finished / 1000) }) + ', ' + (st.sent ? txn(st.sent, '{n} file put', '{n} files put') : tx('nothing new')) + (st.error ? '. ' + st.error : '')
      : tx('not synced yet');
    const key = JSON.stringify(info.folders);
    if (key === shown) return;
    shown = key;
    add(clear(folders), info.folders.length ? h('table', { class: 'x11' }, h('tbody', {}, info.folders.map(f => h('tr', {},
      h('td', { text: f.name }),
      h('td', { class: 'dim', text: '→ ' + f.dest }),
      h('td', { text: f.local >= 0 ? tx('{sent} of {local} put', f) : tx('{sent} put', f) }),
      h('td', { class: 'act' },
        f.refused ? btn(tx('{refused} refused', f), tx('files osada did not take'), () => refused(f)) : null,
        btn(tx('change'), tx('where the files go'), () => dest(f, tx('save')).then(v => { if (v !== null) { app.setDest(f.id, v); show(); } })),
        btn(tx('remove'), tx('stop syncing this folder; nothing is removed'), () => { app.removeFolder(f.id); show(); }, 'danger'))))))
      : h('p', { class: 'dim', text: tx('no folders yet: add one, like DCIM/Camera or Pictures/Screenshots.') }));
  }
  window.osadaAppEvent = (what, id) => {
    show();
    const f = what === 'picked' && info.folders.find(x => x.id === id);
    if (f) dest(f, tx('add')).then(v => {
      if (v === null) app.removeFolder(f.id);
      else { app.setDest(f.id, v); toast(tx('{name} syncs within the hour, or now with sync now', { name: f.name })); }
      show();
    });
  };
  show();
  const poll = setInterval(show, 2000);
  body.push(folders, status,
    h('div', { class: 'row' },
      btn(tx('add folder'), null, () => app.addFolder(), 'primary'),
      btn(tx('sync now'), tx('uses any network'), () => { app.syncNow(); setTimeout(show, 300); }),
      h('label', { class: 'chk' }, wifi, ' ' + tx('sync by itself only on Wi-Fi')),
      btn(tx('unpair'), tx('this phone stops syncing; the files it put stay'), async () => {
        if (!(await confirmBox(tx('unpair'), tx('unpair this phone? it stops syncing; the files it put stay.'), tx('unpair'), true))) return;
        app.unpair();
        setTimeout(again, 500);
      }, 'danger')),
    h('p', { class: 'dim', text: tx('one way: new and changed files go to osada, by themselves every hour and soon after a new picture or video. nothing is ever removed, and what was changed on osada is not overwritten.') }));
  return { el: win(tx('phones'), body), destroy: () => { clearInterval(poll); window.osadaAppEvent = null; } };
}

/* ---- settings: password, storage, phones, API keys ---- */
async function settingsPage() {
  const [mounts, devices, keys] = await Promise.all([api('GET', '/api/mounts'), api('GET', '/api/devices'), api('GET', '/api/keys')]);
  const old = h('input', { type: 'password', autocomplete: 'current-password' });
  const n1 = h('input', { type: 'password', autocomplete: 'new-password' });
  const n2 = h('input', { type: 'password', autocomplete: 'new-password' });
  const change = btn(tx('change password'), null, async () => {
    if (n1.value !== n2.value) { toast(tx('the new passwords differ'), 'err'); return; }
    try {
      await api('POST', '/api/password', { old: old.value, new: n1.value });
      old.value = n1.value = n2.value = '';
      toast(tx('password changed'));
    } catch (e) { fail(e); }
  }, 'primary');
  const again = () => navigate(location.pathname, true);
  const phones = phonesWin(devices, again);
  const rows = mounts.map(m => h('tr', {},
    h('td', {}, h('a', { href: U.files(m.loc, '/'), text: m.name })),
    h('td', { text: m.username + '@' + m.host + (m.port !== 22 ? ':' + m.port : '') }),
    h('td', { class: 'dim', text: m.root || tx('(login folder)') }),
    h('td', { class: 'dim small', text: m.host_key || '' }),
    h('td', { class: 'act' },
      btn(tx('test'), tx('connect now'), () => api('POST', '/api/mounts/' + m.id + '/test').then(r => toast(txn(r.entries, 'ok: {n} entry', 'ok: {n} entries')), fail)),
      btn(tx('edit'), null, () => mountDialog(m).then(v => { if (v) again(); })),
      btn(tx('remove'), tx('only the connection; no files are touched'), async () => {
        if (await confirmBox(tx('remove storage'), tx('remove {name}? files on the server stay; shares and links into it stop working.', { name: m.name }), tx('remove'), true)) api('DELETE', '/api/mounts/' + m.id).then(again, fail);
      }, 'danger'))));
  const el = h('div', { class: 'stack' },
    win(tx('account'), [h('p', {}, tx('logged in as') + ' ', h('b', { text: S.me.username }), S.me.admin ? ' (' + tx('admin') + ')' : ''),
      h('div', { class: 'form3' }, field(tx('current password'), old), field(tx('new password'), n1), field(tx('new password again'), n2)), h('div', { class: 'row' }, change)]),
    win(tx('external storage (sftp)'), [
      rows.length ? h('table', { class: 'x11' }, h('thead', {}, h('tr', {}, ['name', 'login', 'folder', 'host key', ''].map(t => h('th', { text: tx(t) })))), h('tbody', {}, rows))
        : h('p', { class: 'dim', text: tx('none yet. storage shows up under places in the file list, only for you.') }),
      h('div', { class: 'row' }, btn(tx('add sftp storage'), null, () => mountDialog(null).then(v => { if (v) again(); }), 'primary'))]),
    phones.el,
    win(tx('API keys'), [
      keys.length ? h('table', { class: 'x11' }, h('thead', {}, h('tr', {}, ['name', 'folder', 'access', 'expires', 'last used', ''].map(t => h('th', { text: tx(t) })))), h('tbody', {}, keyRows(keys, again, true)))
        : h('p', { class: 'dim', text: tx('none yet. a key lets a program list, download and (if it may edit) upload, move and delete files in one folder.') }),
      h('p', { class: 'dim', text: tx('make one with share on a folder in the file list.') })]),
    win(tx('about'), h('p', { class: 'dim', text: tx('osada {version} on {host}', S.me) + (S.me.remote ? ', ' + tx('reached from outside ({ip})', { ip: S.me.remote }) : '') })));
  return { el, title: tx('settings'), destroy: phones.destroy };
}

return { settingsPage };
});
