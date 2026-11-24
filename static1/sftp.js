/* Sftp connection fields, for storage (settings) and the backup target (admin). Part sftp (util.js): loaded when the page opens, it
   uses the app's helpers (app.js, util.js) as globals. */
'use strict';

part('sftp', () => {

/* ---- sftp connection fields, for storage and for the backup target ---- */
function sftpFields(v, editing) {
  v = v || {};
  const host = h('input', { type: 'text', value: v.host || '', placeholder: tx('name or address'), spellcheck: false });
  const port = h('input', { type: 'number', value: v.port || 22, min: 1, max: 65535 });
  const user = h('input', { type: 'text', value: v.username || '', spellcheck: false, autocomplete: 'off' });
  const auth = h('select', { value: v.auth || 'password' }, h('option', { value: 'password', text: tx('password') }), h('option', { value: 'key', text: tx('private key') }));
  const pass = h('input', { type: 'password', placeholder: editing ? tx('(unchanged)') : '', autocomplete: 'new-password' });
  const key = h('textarea', { rows: 4, class: 'key', spellcheck: false, placeholder: editing ? tx('(unchanged)') : '-----BEGIN OPENSSH PRIVATE KEY-----' });
  const phrase = h('input', { type: 'password', placeholder: editing ? tx('(unchanged)') : tx('(if the key has one)'), autocomplete: 'new-password' });
  const root = h('input', { type: 'text', value: v.root || '', placeholder: tx('(the login folder)'), spellcheck: false });
  const pw = field(tx('password'), pass, 'span2');
  const kb = h('div', { class: 'span2 form2' }, field(tx('private key (openssh or pem)'), key, 'span2'), field(tx('key passphrase'), phrase));
  const sync = () => { pw.hidden = auth.value !== 'password'; kb.hidden = auth.value !== 'key'; };
  auth.addEventListener('change', sync);
  sync();
  return {
    el: h('div', { class: 'form2' }, field(tx('host'), host), field(tx('port'), port), field(tx('user'), user), field(tx('log in with'), auth), pw, kb, field(tx('folder'), root, 'span2')),
    value: () => ({ host: host.value.trim(), port: +port.value || 22, username: user.value.trim(), auth: auth.value, password: pass.value, key: key.value, passphrase: phrase.value, root: root.value.trim() }),
  };
}

function mountDialog(m) {
  const name = h('input', { type: 'text', value: m ? m.name : '', placeholder: tx('shown in places, e.g. nas') });
  const f = sftpFields(m, !!m);
  const reset = h('input', { type: 'checkbox' });
  return dialog({
    title: m ? tx('edit {name}', { name: m.name }) : tx('add sftp storage'), wide: true,
    body: [field(tx('name'), name), f.el,
      m ? h('label', { class: 'chk' }, reset, ' ' + tx('forget the pinned host key (after the server was reinstalled)')) : null,
      h('p', { class: 'dim', text: tx('saving connects first; the server\'s host key is pinned from then on. passwords and keys are stored encrypted.') })],
    buttons: [{ label: tx('cancel'), value: null }, { label: m ? tx('save') : tx('add'), primary: true, value: () => Object.assign({ name: name.value.trim(), reset_host_key: reset.checked }, f.value()) }],
    check: async v => {
      const r = await api(m ? 'PUT' : 'POST', m ? '/api/mounts/' + m.id : '/api/mounts', v);
      toast(txn(r.entries, 'connected: {n} entry at the folder', 'connected: {n} entries at the folder'));
    },
  });
}

return { sftpFields, mountDialog };
});
