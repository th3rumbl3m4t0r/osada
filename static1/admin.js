/* The admin page: users, the nightly backup, sign-in for other programs. Part admin (util.js): loaded when the page opens, it
   uses the app's helpers (app.js, util.js) as globals. */
'use strict';

part('admin', ({ sftpFields }) => {

/* ---- admin: users, backup ---- */
async function adminPage() {
  if (!S.me.admin) throw new Error(tx('admins only'));
  const [users, bk, oidc] = await Promise.all([api('GET', '/api/users'), api('GET', '/api/backup'), api('GET', '/api/oidc/clients')]);
  const again = () => navigate(location.pathname, true);
  // a note: how the AS numbers in an outside list were looked up
  const patch = (u, body, msg) => api('PATCH', '/api/users/' + u.id, body).then(r => {
    if (r && r.note) toast(r.note, r.note.includes('not fetched') ? 'err' : null);
    else if (msg) toast(msg);
    again();
  }, fail);
  const rows = users.map(u => {
    const me = u.id === S.me.id;
    return h('tr', { class: u.disabled ? 'off' : null },
      h('td', { text: u.username }),
      h('td', { class: u.admin ? 'warn' : 'dim', text: u.admin ? tx('admin') : tx('user') }),
      h('td', { class: u.disabled ? 'bad' : 'ok', text: u.disabled ? tx('disabled') : tx('active') }),
      h('td', { class: u.remote ? 'warn' : 'dim', text: u.remote || tx('no') }),
      h('td', { class: u.quota == null ? 'dim' : u.used >= u.quota * 0.8 ? 'warn' : null,
        text: u.quota == null ? (u.used == null ? tx('no') : fmtShort(u.used, 999.5)) : u.used == null ? fmtLimit(u.quota) : tx('{used} of {limit}', { used: fmtShort(u.used, 999.5), limit: fmtShort(u.quota, 999.5) }) }),
      h('td', { class: 'dim', text: fmtDate(u.created) }),
      h('td', { class: 'act' },
        me ? h('span', { class: 'dim', text: tx('(you)') }) : [
          btn(u.admin ? tx('make user') : tx('make admin'), null, () => patch(u, { admin: !u.admin })),
          btn(u.disabled ? tx('enable') : tx('disable'), u.disabled ? null : tx('logs them out; their shares and links stop working'), () => patch(u, { disabled: !u.disabled })),
        ],
        btn(tx('outside'), tx('networks this account may be used from through the public address'), async () => {
          const v = await ask(tx('outside access for {name}', { name: u.username }), tx('networks: CIDR or AS number (AS5610 = every network of that provider), comma separated; empty: from anywhere'), u.remote, tx('set'));
          if (v !== null) patch(u, { remote: v }, v.trim() ? null : tx('{name}: from anywhere', { name: u.username }));
        }),
        u.apptab == null ? null : btn(u.apptab ? tx('apptab: on') : tx('apptab: off'), tx("apps1's apps in a tab of osada, also from outside"),
          () => patch(u, { apptab: !u.apptab }, tx(u.apptab ? '{name}: apptab off' : '{name}: apptab on', { name: u.username }))),
        btn(tx('limit'), tx('room this account may use in its home'), async () => {
          const v = await ask(tx('storage limit for {name}', { name: u.username }), tx('size, like 50G, 500M or 1.5T; empty: no limit'),
            u.quota == null ? '' : fmtLimit(u.quota), tx('set'));
          if (v !== null) patch(u, { quota: v }, v.trim() ? tx('{name}: storage limit {size}', { name: u.username, size: v.trim() }) : tx('{name}: no storage limit', { name: u.username }));
        }),
        btn(tx('set password'), null, async () => {
          const p = await ask(tx('password for {name}', { name: u.username }), tx('new password'), genPassword(), tx('set'));
          if (!p) return;
          try {
            await api('PATCH', '/api/users/' + u.id, { password: p });
            await dialog({ title: tx('password set'), body: [h('div', { class: 'dlg-text', text: tx('the new password for {name}:', { name: u.username }) }), h('pre', { class: 'secret', text: p })] });
          } catch (e) { fail(e); }
        }),
        me ? null : btn(tx('delete'), null, async () => {
          if (!(await confirmBox(tx('delete user'), tx('delete {name}? their files are moved aside on the server, not destroyed.', { name: u.username }), tx('delete'), true))) return;
          try {
            const r = await api('DELETE', '/api/users/' + u.id);
            if (r.files_kept_at) await dialog({ title: tx('deleted'), body: h('div', { class: 'dlg-text', text: tx('the files of {name} are kept at {path} on the server.', { name: u.username, path: r.files_kept_at }) }) });
            again();
          } catch (e) { fail(e); }
        }, 'danger')));
  });
  const nu = h('input', { type: 'text', spellcheck: false, autocomplete: 'off', placeholder: tx('username') });
  const np = h('input', { type: 'text', spellcheck: false, autocomplete: 'off', value: genPassword() });
  const na = h('input', { type: 'checkbox' });
  const addUser = btn(tx('add user'), null, async () => {
    try {
      await api('POST', '/api/users', { username: nu.value.trim(), password: np.value, admin: na.checked });
      await dialog({ title: tx('user added'), body: [h('div', { class: 'dlg-text', text: tx('hand these to {name} (they can change the password in settings):', { name: nu.value.trim() }) }),
        h('pre', { class: 'secret', text: 'user:     ' + nu.value.trim() + '\npassword: ' + np.value + '\naddress:  ' + location.origin + '/' + (S.me.public_url && S.me.public_url !== location.origin ? '\noutside:  ' + S.me.public_url + '/ (once allowed under "outside")' : '') })] });
      again();
    } catch (e) { fail(e); }
  }, 'primary');

  const bkBox = h('div');
  const page = { el: null, title: tx('admin'), destroy: () => clearInterval(page.poll) };
  backupBox(bkBox, bk, page);
  page.el = h('div', { class: 'stack' },
    win(tx('users'), [
      h('table', { class: 'x11' }, h('thead', {}, h('tr', {}, ['user', 'role', 'state', 'from outside', 'limit', 'since', ''].map(t => h('th', { text: tx(t) })))), h('tbody', {}, rows)),
      h('div', { class: 'row' }, nu, np, btn(tx('new'), tx('another password'), () => { np.value = genPassword(); }), h('label', { class: 'chk' }, na, ' ' + tx('admin')), addUser)]),
    oidcWin(oidc, again),
    win(tx('nightly backup'), bkBox));
  return page;
}

function backupBox(box, bk, page) {
  // osada 0.2 (Round 17): the nightly backups are in the cluster's settings, set in its admin page
  if (bk.migrated) {
    box.append(h('p', {}, 'The nightly backups are set in osada 0.2\'s admin page: admin → files → backups (where they go: other storage).'));
    return;
  }
  const c = bk.config || {};
  const on = h('input', { type: 'checkbox', checked: c.enabled !== false && !!bk.config });
  const time = h('input', { type: 'time', value: c.time || '02:30' });
  const f = sftpFields(c, !!bk.config);
  const reset = h('input', { type: 'checkbox' });
  const state = h('div', { class: 'bk-state' });
  const snaps = h('div', { class: 'dim' });
  const runs = h('div');

  function paint(d) {
    const running = d.running;
    state.replaceChildren(
      running ? h('span', { class: 'warn', text: tx('running:') + ' ' })
        : h('span', { class: 'dim', text: (d.config ? (d.config.enabled ? tx('next run {time} daily.', d.config) : tx('nightly runs are off.')) : tx('not set up yet.')) + ' ' }),
      h('span', { text: running ? (d.status || tx('starting')) : (d.status ? tx('last:') + ' ' + d.status : '') }));
    snaps.textContent = d.snapshots.length ? tx('on the target:') + ' ' + d.snapshots.join(', ') : '';
    runBtn.disabled = running || !d.config;
    testBtn.disabled = !d.config;
    const rs = d.runs.map(r => {
      const took = r.finished ? Math.max(0, r.finished - r.started) : null;
      const res = r.ok === true ? ['ok', 'ok'] : r.finished || !running ? ['failed', 'bad'] : ['running', 'warn'];
      return h('tr', {},
        h('td', { text: fmtTime(r.started) }),
        h('td', { class: 'dim', text: took === null ? '' : took < 90 ? took + ' s' : Math.round(took / 60) + ' min' }),
        h('td', { class: res[1], text: tx(res[0]) }),
        h('td', { class: 'num', text: r.uploaded }), h('td', { class: 'num', text: fmtSize(r.bytes) }), h('td', { class: 'num', text: r.linked }),
        h('td', { class: 'dim msg', text: r.message || '' }));
    });
    runs.replaceChildren(rs.length
      ? h('table', { class: 'x11' }, h('thead', {}, h('tr', {}, ['started', 'took', 'result', 'files sent', 'size', 'linked', 'message'].map(t => h('th', { text: tx(t) })))), h('tbody', {}, rs))
      : h('div', { class: 'dim', text: tx('no runs yet') }));
    clearInterval(page.poll);
    if (running) page.poll = setInterval(() => api('GET', '/api/backup').then(paint, () => {}), 2000);
  }

  const saveBtn = btn(tx('save'), tx('connects to the target first'), async () => {
    busy(true);
    try {
      const r = await api('PUT', '/api/backup', Object.assign({ enabled: on.checked, time: time.value, reset_host_key: reset.checked }, f.value()));
      toast(tx('saved; target reachable ({entries} entries), host key {host_key}', r));
      reset.checked = false;
      paint(await api('GET', '/api/backup'));
    } catch (e) { fail(e); } finally { busy(false); }
  }, 'primary');
  const testBtn = btn(tx('test'), tx('connect to the target'), () => { busy(true); api('POST', '/api/backup/test').then(r => toast(txn(r.entries, 'ok: {n} entry at the target', 'ok: {n} entries at the target')), fail).finally(() => busy(false)); });
  const runBtn = btn(tx('run now'), null, () => api('POST', '/api/backup/run').then(() => api('GET', '/api/backup')).then(paint, fail));

  box.replaceChildren(
    h('p', { class: 'dim', text: tx('copies everything needed to rebuild this server to an sftp target: a consistent snapshot of the database (the server keeps running), ' +
      'the encryption key, the service settings, the program and all user files. each run is a complete folder named by its time; three are kept: ' +
      'the newest, one up to a week older and one one to four weeks older. files already on the target are hard-linked, not sent again, so each is stored once. ' +
      'RESTORE.md on the target says how to rebuild. files on users\' sftp storage are not included.') }),
    h('div', { class: 'row' }, h('label', { class: 'chk' }, on, ' ' + tx('every night at')), time),
    f.el,
    bk.config ? h('label', { class: 'chk' }, reset, ' ' + tx('forget the pinned host key') + (c.host_key ? ' (' + c.host_key + ')' : '')) : '',
    h('div', { class: 'row' }, saveBtn, testBtn, runBtn),
    state, snaps, runs);
  paint(bk);
}

/* ---- admin: sign-in for other programs (OpenID Connect, oidc.rs) ---- */
function oidcWin(o, again) {
  const handOver = (title, id, secret) => dialog({ title, body: [
    h('div', { class: 'dlg-text', text: secret ? tx('the program needs these; the secret is shown only now:') : tx('the program needs these:') }),
    h('pre', { class: 'secret', text: 'issuer:        ' + (o.issuer || tx('(not set)')) + '\nclient_id:     ' + id + (secret ? '\nclient_secret: ' + secret : '') })] });
  const rows = o.clients.map(c => h('tr', {},
    h('td', { text: c.name }),
    h('td', { class: 'dim', text: c.public ? tx('public (PKCE)') : tx('with secret') }),
    h('td', { class: 'small', text: c.redirect_uris.join(' ') }),
    h('td', { class: c.users ? 'warn' : 'dim', text: c.users || tx('everyone') }),
    h('td', { class: 'num', text: c.signed_in }),
    h('td', { class: 'dim', text: c.last_used ? fmtDate(c.last_used) : tx('never') }),
    h('td', { class: 'act' },
      btn(tx('edit'), null, () => clientDialog(c).then(v => { if (v) again(); })),
      c.public ? null : btn(tx('new secret'), tx('the old one stops working'), async () => {
        if (!(await confirmBox(tx('new secret'), tx('make a new secret for {name}? it stops working until it has the new one.', { name: c.name }), tx('new secret'), true))) return;
        try { const r = await api('POST', '/api/oidc/clients/' + c.id + '/secret'); await handOver(tx('new secret'), c.name, r.client_secret); } catch (e) { fail(e); }
      }),
      btn(tx('remove'), null, async () => {
        if (await confirmBox(tx('remove program'), tx('remove {name}? nobody can log in to it with osada any more.', { name: c.name }), tx('remove'), true)) api('DELETE', '/api/oidc/clients/' + c.id).then(again, fail);
      }, 'danger'))));
  return win(tx('sign-in for other programs'), [
    h('p', { class: 'dim', text: o.issuer
      ? tx('programs on the network can let people log in with their osada account (OpenID Connect). issuer: {issuer}', o)
      : tx('off: set OSADA_OIDC_ISSUER in /etc/osada/osada.env and restart osada. programs can be added already.') }),
    rows.length ? h('table', { class: 'x11' }, h('thead', {}, h('tr', {}, ['program', 'kind', 'redirect URIs', 'who', 'logged in', 'last used', ''].map(t => h('th', { text: tx(t) })))), h('tbody', {}, rows))
      : h('p', { class: 'dim', text: tx('no programs yet.') }),
    h('div', { class: 'row' }, btn(tx('add program'), null, () => clientDialog(null).then(r => { if (r) handOver(tx('program added'), r.client_id, r.client_secret).then(again); }), 'primary'))]);
}

/* add (c null) or edit a program; resolves to the server's answer, or null */
function clientDialog(c) {
  const name = h('input', { type: 'text', value: c ? c.name : '', disabled: !!c, spellcheck: false, autocomplete: 'off', placeholder: tx('e.g. tracker; it is the client_id') });
  const uris = h('textarea', { class: 'key', rows: 3, spellcheck: false, value: c ? c.redirect_uris.join('\n') : '', placeholder: 'http://192.168.1.11/oauth2/callback' });
  const who = h('input', { type: 'text', value: c ? c.users : '', spellcheck: false, autocomplete: 'off', placeholder: tx('everyone') });
  const pub = h('input', { type: 'checkbox' });
  let res = null;
  return dialog({
    title: c ? tx('edit {name}', { name: c.name }) : tx('add program'), wide: true,
    body: [field(tx('name'), name), field(tx('redirect URIs, one per line'), uris), field(tx('who may log in: usernames, comma separated; empty: everyone'), who),
      c ? null : h('label', { class: 'chk' }, pub, ' ' + tx('public: an app on a phone or in a browser, which cannot keep a secret (it must use PKCE)'))],
    buttons: [{ label: tx('cancel'), value: null }, { label: c ? tx('save') : tx('add'), primary: true,
      value: () => c ? { redirect_uris: uris.value, users: who.value } : { name: name.value.trim(), redirect_uris: uris.value, users: who.value, public: pub.checked } }],
    check: async v => { res = await api(c ? 'PATCH' : 'POST', c ? '/api/oidc/clients/' + c.id : '/api/oidc/clients', v); },
  }).then(v => v && res);
}

return { adminPage };
});
