/* The wiki (src/wiki.rs): articles everybody reads and writes, each in the languages
   somebody wrote it in. An article opens in the language of the UI when it has it, else
   in English, else in the one it was started in; the tabs switch, and lead to a
   translation where there is none yet. Pictures belong to an article, in all its
   languages: uploaded, dropped or pasted in the editor, shown by name in the markdown.
   Articles have addresses of their own (random, not made from titles); a reference
   [text](address) is picked from a list (refPicker), or names an article yet to be
   written: it shows as missing, and a click offers to write it there. An article may
   hold a family tree (tree.js). Read only without home (util.js). */
'use strict';

part('wiki', () => {

const enc = encodeURIComponent;
const W = {
  list: q => '/api/wiki' + (q ? '?q=' + enc(q) : ''),
  one: s => '/api/wiki/' + enc(s),
  text: (s, l) => '/api/wiki/' + enc(s) + '/text/' + l,
  media: (s, n) => '/api/wiki/' + enc(s) + '/media' + (n ? '/' + enc(n) : ''),
  history: (s, l) => '/api/wiki/' + enc(s) + '/history' + (l ? '?lang=' + l : ''),
  rev: (s, id) => '/api/wiki/' + enc(s) + '/rev/' + id,
  pin: s => '/api/wiki/' + enc(s) + '/pin',
  page: (s, l) => '/wiki/' + enc(s) + (l ? '?lang=' + l : ''),
  edit: (s, l, from) => '/wiki/' + enc(s) + '/edit?lang=' + l + (from ? '&from=' + from : ''),
  past: (s, l) => '/wiki/' + enc(s) + '/history' + (l ? '?lang=' + l : ''),
  tree: (s, id) => '/api/wiki/' + enc(s) + '/tree' + (id ? '/' + id : ''),
  exp: s => '/api/wiki/' + enc(s) + '/export',
};
const langLabel = code => (LANGS.find(l => l[0] === code) || [code, code.toUpperCase()])[1];
const arg = k => new URLSearchParams(location.search).get(k);

/* which of an article's languages it opens in: the one asked for, the UI's, English, the first written */
function pickLang(texts, asked) {
  if (asked && texts[asked]) return asked;
  if (texts[LANG]) return LANG;
  if (texts.en) return 'en';
  return Object.keys(texts).sort((a, b) => (texts[a].started || 0) - (texts[b].started || 0))[0];
}

/* an address for an article yet to be written, like the server makes them (no 0/o, 1/l) */
function newId() {
  const a = 'abcdefghijkmnpqrstuvwxyz23456789';
  return Array.from(crypto.getRandomValues(new Uint8Array(8)), b => a[b % 32]).join('');
}
/* an article with fewer lines of text than this is its family tree: the tree goes only with the whole article */
const TREE_ALONE = 15;
const fold = s => s.normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLowerCase();
/* an article's title in the best language of {lang: title} */
function bestTitle(titles) { return titles ? titles[pickLang(Object.fromEntries(Object.entries(titles).map(([l, t]) => [l, { title: t }])))] : null; }

/* ---- references: find an article, or name one yet to be written -> {id, title, missing} or null ---- */
async function refPicker(text) {
  const list = await api('GET', W.list());
  const items = list.map(a => ({ id: a.slug, title: a.texts[pickLang(a.texts)].title, all: Object.values(a.texts).map(t => fold(t.title)) }))
    .sort((x, y) => nameCmp(x.title, y.title));
  const inp = h('input', { type: 'search', class: 'wide', value: text || '', placeholder: tx('find an article'), spellcheck: false });
  const tbody = h('tbody');
  let rows = [], cur = 0, done = null;
  function paint() {
    const q = fold(inp.value.trim());
    const hits = items.filter(it => !q || it.all.some(t => t.includes(q))).slice(0, 60);
    rows = hits.map(it => ({ id: it.id, title: it.title, missing: false }));
    if (inp.value.trim() && !items.some(it => it.all.includes(q))) rows.push({ id: newId(), title: inp.value.trim(), missing: true });
    cur = Math.min(cur, Math.max(0, rows.length - 1));
    tbody.replaceChildren(...rows.map((r, i) => h('tr', { class: (i === cur ? 'cur sel' : '') + (r.missing ? ' ref-new' : ''), onclick: () => done(r) },
      h('td', { class: 'nm' }, r.missing ? [h('span', { class: 'wiki-ref missing', text: r.title }), h('span', { class: 'dim', text: '  ' + tx('not written yet: a reference to write it later') })]
        : h('span', { class: 'wiki-ref', text: r.title })))));
    if (!rows.length) tbody.appendChild(h('tr', {}, h('td', { class: 'dim', text: tx('no articles yet: type a name to refer to one yet to be written') })));
    const row = tbody.children[cur];
    if (row) row.scrollIntoView({ block: 'nearest' });
  }
  inp.addEventListener('input', () => { cur = 0; paint(); });
  inp.addEventListener('keydown', e => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); cur = Math.max(0, Math.min(rows.length - 1, cur + (e.key === 'ArrowDown' ? 1 : -1))); paint(); }
    else if (e.key === 'Enter' && rows[cur]) { e.preventDefault(); done(rows[cur]); }
  });
  return dialog({
    title: tx('reference'), wide: true,
    body: [inp, h('div', { class: 'ref-list' }, h('table', { class: 'x11 files' }, tbody))],
    buttons: [{ label: tx('cancel'), value: null }],
    opened: (win, d) => { done = d; paint(); inp.select(); },
  });
}

/* a reference to an article nobody wrote yet: offer to write it there. `more`: what the new article starts from (a tree's person) */
async function offerNew(id, title, more) {
  if (offline()) { toast(tx('not written yet; that needs osada at home'), 'warn'); return; }
  if (!(await confirmBox(tx('not written yet'), tx('nobody has written "{title}" yet. write it now?', { title }), tx('write it')))) return;
  navigate('/wiki/new?id=' + enc(id) + '&title=' + enc(title) + (more || ''));
}
/* clicks on missing references in a view */
function missingClicks(el) {
  el.addEventListener('click', e => {
    const l = e.target.closest('a.wiki-ref.missing');
    if (!l || e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey) return;
    e.preventDefault();
    offerNew(decodeURIComponent(l.pathname.slice(6)), l.textContent.trim()).catch(fail);
  });
}

/* a picture for an article: its name there */
async function uploadPicture(slug, f) {
  const name = f.name && f.name !== 'image.png' ? f.name : 'picture-' + new Date().toISOString().slice(0, 19).replace(/[-:T]/g, '') + '.' + (f.type.split('/')[1] || 'png').replace('jpeg', 'jpg').replace('svg+xml', 'svg');
  const r = await fetch(W.media(slug) + '?name=' + enc(name), { method: 'POST', credentials: 'same-origin', headers: { 'x-shelf': '1' }, body: f });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(txErr(j.error || r.status + ' ' + r.statusText));
  return j;
}

function wikiCrumbs(slug, title, last) {
  const out = [h('a', { href: '/wiki', text: tx('wiki') })];
  if (title) out.push(h('span', { class: 'dim', text: ' / ' }), slug && last ? h('a', { href: W.page(slug), text: title }) : h('span', { class: 'here', text: title }));
  if (last) out.push(h('span', { class: 'dim', text: ' / ' }), h('span', { class: 'here', text: last }));
  return h('span', { class: 'crumbs' }, out);
}

/* to the top of the list for everybody, or back among the others */
async function setPinned(slug, pinned) {
  await api('PUT', W.pin(slug), { pinned });
  toast(pinned ? tx('pinned to the top of the list') : tx('no longer pinned'));
}

/* ---- every article: the pinned ones first ---- */
async function wikiList() {
  const q = arg('q') || '';
  const list = await api('GET', W.list(q));
  const items = list.map(a => {
    const lang = pickLang(a.texts), t = a.texts[lang];
    return { slug: a.slug, lang, title: t.title, texts: a.texts, updated: Math.max(...Object.values(a.texts).map(x => x.updated)), author: t.author, pinned: !!a.pinned };
  });
  const order = () => items.sort((a, b) => (b.pinned - a.pinned) || nameCmp(a.title, b.title));
  order();
  // the list as the admin pages' grid (T8): sort, filter and choose columns; pinned first at the start
  const filter = h('input', { type: 'search', class: 'filter', placeholder: tx('search the texts (enter)'), spellcheck: false, value: q });
  filter.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.preventDefault(); navigate('/wiki' + (filter.value.trim() ? '?q=' + enc(filter.value.trim()) : '')); }
    if (e.key === 'Escape' && q) navigate('/wiki');
  });
  const add = btn(tx('new article'), tx('write a new article'), () => navigate('/wiki/new'), 'primary');
  add.hidden = offline();
  const bar = h('div', { class: 'toolbar' }, add, q ? h('a', { class: 'btn-link', href: '/wiki', text: tx('all articles') }) : null, h('span', { class: 'spacer' }), filter);
  const host = h('div', { class: 'wiki-grid wiki-list' });
  let g = null;
  try {
    const mod = await import('/static/grid.js');
    g = mod.grid({
      id: 'wiki-list',
      rows: items,
      empty: q ? tx('no article has {q} in it', { q }) : tx('no articles yet: write the first one'),
      columns: [
        { key: 'title', label: tx('title'), cell: a => h('span', {}, h('a', { href: W.page(a.slug), text: a.title }), a.pinned ? h('span', { class: 'tag', text: tx('pinned') }) : null) },
        { key: 'langs', label: tx('languages'), value: a => LANGS.filter(([c]) => a.texts[c]).map(([, l]) => l).join(' '),
          cell: a => h('span', { class: 'langs' }, LANGS.filter(([c]) => a.texts[c]).map(([c, label]) =>
            h('a', { class: 'tag' + (c === a.lang ? ' on' : ''), href: W.page(a.slug, c), text: label, title: a.texts[c].title }))) },
        { key: 'updated', label: tx('changed'), type: 'date' },
        { key: 'author', label: tx('by') },
        { key: 'pinned', label: '', value: a => a.pinned ? 1 : 0, cell: a => offline() ? '' : btn(a.pinned ? tx('unpin') : tx('pin'),
          a.pinned ? tx('take it off the top of the list') : tx('pin it to the top of the list, for everybody'),
          () => setPinned(a.slug, !a.pinned).then(() => { a.pinned = !a.pinned; order(); g.refresh(items); }, fail)) },
      ],
    });
    host.append(g.node);
  } catch (e) {
    host.append(h('div', { class: 'empty bad', text: String(e) }));
  }
  setStatus(txn(items.length, '{n} article', '{n} articles'));
  const el = h('section', { class: 'win filewin wiki-listwin' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), wikiCrumbs(null, q ? tx('found: {q}', { q }) : null)),
    bar, host);
  return {
    el, cls: 'fill', title: tx('wiki'),
    shown: () => { if (q) filter.focus(); },
    key: e => {
      if (isTyping(e)) return false;
      if (e.key === '/') { filter.focus(); filter.select(); return true; }
      if (e.key === 'n' && !offline()) { navigate('/wiki/new'); return true; }
      return false;
    },
  };
}

/* ---- an article ---- */
async function wikiPage(slug, parts) {
  let a;
  try { a = await api('GET', W.one(slug)); } catch (e) { if (e.status === 404) return missingPage(slug); throw e; }
  const lang = pickLang(a.texts, arg('lang')), t = a.texts[lang];
  const tabs = h('span', { class: 'wiki-langs' }, LANGS.map(([c, label]) => (a.texts[c]
    ? h('a', { href: W.page(slug, c), class: c === lang ? 'on' : null, text: label, title: a.texts[c].title })
    : offline() ? null : h('a', { href: W.edit(slug, c, lang), class: 'add', text: '+' + label, title: tx('translate into {lang}', { lang: label }) }))));
  const body = h('div', { class: 'md-body' });
  body.innerHTML = t.html;
  gridTables(body, slug + '-' + lang);
  // other sites, and a picture of the article shown whole, in a tab of their own; articles and files stay in the app
  for (const l of body.querySelectorAll('a[href^="http"], a[href^="/api/wiki/"]')) { l.target = '_blank'; l.rel = 'noopener'; }
  const edit = () => navigate(W.edit(slug, lang));
  const hasTree = !!(a.tree && a.tree.data);
  const pinBtn = btn('', '', () => setPinned(slug, !a.pinned).then(() => { a.pinned = !a.pinned; paintPin(); }, fail));
  const paintPin = () => {
    pinBtn.textContent = a.pinned ? tx('unpin') : tx('pin');
    pinBtn.title = a.pinned ? tx('take it off the top of the list') : tx('pin it to the top of the list, for everybody');
  };
  paintPin();
  const tools = offline() ? [] : [
    btn(tx('edit'), tx('edit (e)'), edit), pinBtn,
    hasTree ? null : btn(tx('add a family tree'), tx('a family tree in this article: people, their partners, parents and children'), () => addTree().catch(fail)),
    a.mine || t.mine ? btn(tx('remove'), tx('remove this language or the whole article'), () => removeIt().catch(fail)) : null,
  ];
  async function addTree() {
    await api('PUT', W.tree(slug), { data: { people: {}, unions: [] }, base: a.tree ? a.tree.rev : 0, note: '' });
    route();
  }
  async function removeIt() {
    const others = Object.keys(a.texts).length > 1;
    const buttons = [{ label: tx('cancel'), value: null }];
    if (t.mine && others) buttons.push({ label: tx('remove {lang}', { lang: langLabel(lang) }), value: 'lang', danger: true });
    if (a.mine) buttons.push({ label: tx('remove the article'), value: 'all', danger: true, primary: true });
    else if (!others) buttons.push({ label: tx('remove the article'), value: 'lang', danger: true, primary: true });
    const v = await dialog({ title: tx('remove'), body: h('div', { class: 'dlg-text', text: tx('{title}: remove it with its history? this cannot be undone.', { title: t.title }) }), buttons });
    if (v === 'all') await api('DELETE', W.one(slug));
    else if (v === 'lang') {
      const r = await api('DELETE', W.text(slug, lang));
      if (!r.gone) { toast(tx('removed')); navigate(W.page(slug), true); return; }
    } else return;
    toast(tx('removed'));
    navigate('/wiki', true);
  }

  // the family tree: shared by all the article's languages
  let tree = null;
  const treeHost = hasTree ? h('div', { class: 'wiki-tree' }) : null;
  if (hasTree) {
    const x = await parts.hold('tree');
    let state = { rev: a.tree.rev, data: a.tree.data, refs: a.refs, layout: a.tree.layout };
    tree = new x.FamilyTree(treeHost, {
      data: state.data, refs: state.refs, layout: state.layout, editable: !offline(), editing: !Object.keys(state.data.people).length && !offline(),
      droppable: Math.max(...Object.values(a.texts).map(t => t.body.split('\n').filter(l => l.trim()).length)) >= TREE_ALONE,
      picture: n => W.media(slug, n), pictures: () => a.media.map(m => m.name),
      upload: async f => { const j = await uploadPicture(slug, f); a.media.push({ name: j.name, size: j.size, author: S.me.username, mine: true }); return j.name; },
      pick: refPicker,
      title: id => bestTitle(state.refs[id]),
      // a person's name: their article, or the offer to write it, starting from what the tree says
      write: (pid, p) => offerNew(p.ref, p.name, '&tree=' + enc(slug) + '&person=' + enc(pid)),
      // a change: made on the newest tree, laid out here once and saved with where its cards go; somebody else's
      // change meanwhile is fetched and it is made again on that
      async save(change, note) {
        for (let i = 0; i < 4; i++) {
          const next = change(state.data ? JSON.parse(JSON.stringify(state.data)) : { people: {}, unions: [] });
          // the people in the order the server keeps them (by id): the layout sent is then the one any page works out from it
          if (next) next.people = Object.fromEntries(Object.keys(next.people).sort().map(k => [k, next.people[k]]));
          let saved = false;
          try {
            await api('PUT', W.tree(slug), { data: next, base: state.rev, note: note || '', layout: next ? x.layout(next) : null });
            saved = true;
          } catch (e) {
            if (e.status !== 409) throw e;
          }
          const now = await api('GET', W.one(slug));
          state = { rev: now.tree ? now.tree.rev : 0, data: now.tree ? now.tree.data : null, refs: now.refs, layout: now.tree ? now.tree.layout : null };
          if (saved) return state;
        }
        throw new Error(tx('the family tree keeps changing; try again'));
      },
      removed: () => route(),
      // the tree with its people's articles, named after the article
      exported: offline() ? null : async () => {
        const u = URL.createObjectURL(new Blob([JSON.stringify(await api('GET', W.exp(slug)), null, 2)], { type: 'application/json' }));
        const l = h('a', { href: u, download: t.title.replace(/[\\/:*?"<>|]+/g, '-') + '.json' });
        document.body.appendChild(l);
        l.click();
        l.remove();
        setTimeout(() => URL.revokeObjectURL(u), 60000);
      },
    });
  }
  const el = h('section', { class: 'win viewwin wikiwin' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), wikiCrumbs(slug, t.title),
      h('span', { class: 'spacer' }), tabs, ...tools,
      h('a', { class: 'btn-link', href: W.past(slug, lang), text: tx('history') })),
    h('div', { class: 'wiki-scroll' }, h('article', { class: 'wiki-view' + (hasTree ? ' with-tree' : ''), lang },
      h('h1', { class: 'wiki-title', text: t.title }), treeHost, body,
      h('div', { class: 'wiki-meta', text: tx('changed {time} by {name}', { time: fmtTime(t.updated), name: t.author }) }))));
  missingClicks(el);
  return {
    el, cls: 'fill', title: t.title,
    status: tx('changed {time} by {name}', { time: fmtTime(t.updated), name: t.author }),
    shown: () => { if (tree) tree.shown(); },
    destroy: () => { if (tree) tree.destroy(); },
    key: e => {
      if (isTyping(e) || e.ctrlKey || e.metaKey || e.altKey) return false;
      if (tree && tree.key(e)) return true;
      if (e.key === 'e' && !offline()) { edit(); return true; }
      if (e.key === 'Escape' || e.key === 'Backspace') { navigate('/wiki'); return true; }
      return false;
    },
  };
}

/* an address nobody has written an article for (yet) */
function missingPage(slug) {
  const title = arg('title') || slug;
  const can = !offline() && /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(slug) && slug !== 'new';
  const el = win(wikiCrumbs(null, title), [
    h('p', { text: can ? tx('nobody has written "{title}" yet.', { title }) : offline() ? tx('not kept on this device') : tx('there is no such article.') }),
    can ? h('div', { class: 'row-end' }, btn(tx('write it'), tx('write the article this reference leads to'), () => navigate('/wiki/new?id=' + enc(slug) + '&title=' + enc(title)), 'primary')) : null,
  ], 'narrow');
  return { el, title };
}

/* ---- writing: a new article (slug null), a language of one, or a translation ---- */
async function wikiEdit(slug, parts) {
  if (offline()) return { redirect: slug ? W.page(slug) : '/wiki' };
  const x = await parts.hold('edit');
  const a = slug ? await api('GET', W.one(slug)) : null;
  let lang = arg('lang');
  if (!LANGS.some(l => l[0] === lang)) lang = a ? pickLang(a.texts) : LANG;
  let base = 0, title = '', text = '';
  // a new one where a reference leads: its address and name; from a family tree, what the tree says of the person
  const at = !slug && arg('id') ? arg('id') : null;
  if (a && a.texts[lang]) ({ rev: base, title, body: text } = a.texts[lang]);
  else if (a) {
    // a translation starts as the text it is translated from
    const from = a.texts[pickLang(a.texts, arg('from'))];
    ({ title, body: text } = from);
  } else {
    title = arg('title') || '';
    if (arg('tree') && arg('person')) text = await personText(arg('tree'), arg('person')).catch(() => '');
  }
  let fresh = !!a && !a.texts[lang], startTitle = fresh ? '' : title;
  const titleIn = h('input', { type: 'text', class: 'wiki-title-in', value: title, placeholder: tx('title'), spellcheck: true });
  const noteIn = h('input', { type: 'text', class: 'wiki-note-in', placeholder: tx('what changed (optional)'), spellcheck: true });
  const langSel = h('select', { title: tx('the language this text is in') }, LANGS.map(([c, label]) => h('option', { value: c, text: label })));
  langSel.value = lang;
  const mark = h('span', { class: 'dirty' });
  const saveBtn = btn(tx('save'), tx('save and show the article (ctrl+s: save, stay here)'), () => save(false).catch(fail), 'primary');
  const host = h('div', { class: 'edit-host edit-md' });
  let saving = false, done = false;
  const dirty = () => !done && (ed.dirty || titleIn.value.trim() !== startTitle || fresh);
  const onChange = () => { mark.textContent = dirty() ? '* ' + tx('modified') : ''; };
  const ed = new x.TextEditor(host, text, { md: true, onChange, onStatus: setStatus, imgBase: slug ? W.media(slug) + '/' : '', linkBase: '/wiki/', refBase: '' });
  titleIn.addEventListener('input', onChange);

  /* ---- pictures ---- */
  const pics = h('div', { class: 'wiki-pics' });
  function insert(md) {
    const ta = ed.ta, s = ta.selectionStart, e = ta.selectionEnd;
    const pre = s > 0 && ta.value[s - 1] !== '\n' ? '\n' : '';
    ta.setRangeText(pre + md + '\n', s, e, 'end');
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    ta.focus();
  }
  function paintPics() {
    if (!slug) { pics.replaceChildren(h('span', { class: 'dim', text: tx('pictures can be added once the article is saved') })); return; }
    const up = btn(tx('add pictures'), tx('upload pictures (or drop or paste them into the text)'), () => {
      const inp = h('input', { type: 'file', multiple: true, accept: 'image/*' });
      inp.addEventListener('change', () => upload(Array.from(inp.files)));
      inp.click();
    });
    pics.replaceChildren(h('span', { class: 'lbl', text: tx('pictures') }), up, ...a.media.map(m => h('span', { class: 'wiki-pic', title: m.name + ' · ' + fmtSize(m.size) + ' · ' + m.author },
      h('button', { class: 'wiki-pic-in', title: tx('put {name} into the text', { name: m.name }), onclick: () => insert('![' + stem(m.name) + '](' + m.name + ')') },
        h('img', { src: W.media(slug, m.name), alt: m.name, loading: 'lazy' })),
      m.mine ? h('button', { class: 'wiki-pic-x', text: 'x', title: tx('remove {name}', { name: m.name }), onclick: () => dropPic(m).catch(fail) }) : null)));
  }
  async function upload(files) {
    for (const f of files.filter(f => /^image\//.test(f.type) || /\.(png|jpe?g|gif|webp|avif|svg)$/i.test(f.name))) {
      busy(true);
      try {
        const j = await uploadPicture(slug, f);
        a.media.push({ name: j.name, size: j.size, author: S.me.username, mine: true });
        insert('![' + stem(j.name) + '](' + j.name + ')');
      } catch (e) { fail(e); } finally { busy(false); }
    }
    paintPics();
  }
  async function dropPic(m) {
    if (!(await confirmBox(tx('remove'), tx('remove {name}? the texts that show it lose it.', { name: m.name }), tx('remove'), true))) return;
    await api('DELETE', W.media(slug, m.name));
    a.media = a.media.filter(y => y !== m);
    paintPics();
  }
  if (slug) {
    const pick = e => Array.from((e.clipboardData || e.dataTransfer || {}).files || []).filter(f => /^image\//.test(f.type));
    ed.ta.addEventListener('paste', e => { const f = pick(e); if (f.length) { e.preventDefault(); upload(f); } });
    ed.ta.addEventListener('dragover', e => { if (hasFiles(e)) e.preventDefault(); });
    ed.ta.addEventListener('drop', e => { const f = pick(e); if (f.length) { e.preventDefault(); e.stopPropagation(); upload(f); } });
  }
  paintPics();

  /* ---- saving ---- */
  async function save(stay, force) {
    if (saving) return;
    const t = titleIn.value.trim();
    if (!t) { toast(tx('a title is needed'), 'err'); titleIn.focus(); return; }
    saving = true;
    busy(true);
    try {
      if (!slug) {
        let r;
        try { r = await api('POST', '/api/wiki', { title: t, lang: langSel.value, body: ed.ta.value, id: at }); } catch (e) {
          // somebody wrote it meanwhile: theirs is where the reference leads now
          if (e.status !== 409 || !at) throw e;
          toast(tx('somebody wrote it meanwhile: here it is'), 'warn');
          done = true;
          navigate(W.page(at), true);
          return;
        }
        done = true;
        toast(tx('saved'));
        navigate(stay ? W.edit(r.slug, langSel.value) : W.page(r.slug, langSel.value), true);
        return;
      }
      let r;
      try {
        r = await api('PUT', W.text(slug, lang), { title: t, body: ed.ta.value, base: force !== undefined ? force : base, note: noteIn.value });
      } catch (e) {
        if (e.status !== 409) throw e;
        const how = await dialog({
          title: tx('conflict'), body: h('div', { class: 'dlg-text', text: tx('{title} was changed by someone else since you opened it.', { title: t }) }),
          buttons: [{ label: tx('cancel'), value: null }, { label: tx('overwrite theirs'), value: 'over', danger: true, primary: true }],
        });
        if (how !== 'over') return;
        const now = await api('GET', W.one(slug));
        saving = false;
        return save(stay, now.texts[lang] ? now.texts[lang].rev : 0);
      }
      base = r.rev;
      title = startTitle = t;
      fresh = false;
      ed.markSaved(ed.ta.value);
      noteIn.value = '';
      onChange();
      toast(tx('saved'));
      if (!stay) { done = true; navigate(W.page(slug, lang)); }
    } finally { saving = false; busy(false); }
  }

  const el = h('section', { class: 'win editwin wiki-edit' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }),
      wikiCrumbs(slug, slug ? title : tx('new article'), slug ? (fresh ? tx('translate into {lang}', { lang: langLabel(lang) }) : tx('edit')) : null), mark,
      h('span', { class: 'spacer' }), slug ? h('span', { class: 'tag', text: langLabel(lang) }) : langSel, saveBtn,
      btn(tx('cancel'), tx('back without saving'), () => navigate(slug ? W.page(slug, a.texts[lang] ? lang : null) : '/wiki'))),
    h('div', { class: 'wiki-fields' }, titleIn, slug ? noteIn : null),
    fresh ? h('div', { class: 'note dim', text: tx('this is the {from} text: write it in {lang} over it', { from: langLabel(pickLang(a.texts, arg('from'))), lang: langLabel(lang) }) }) : null,
    host, pics);
  onChange();
  return {
    el, cls: 'fill', title: slug ? title : tx('new article'),
    dirty, save: () => save(true).catch(fail),
    destroy: () => ed.destroy(),
    shown: () => { if (!titleIn.value) titleIn.focus(); else ed.focus(); },
  };
}

/* what a new article on a tree's person starts with: their dates and line, and the way back to the tree */
async function personText(treeSlug, pid) {
  const a = await api('GET', W.one(treeSlug));
  const p = a.tree && a.tree.data && a.tree.data.people[pid];
  if (!p) return '';
  const lines = [];
  if (p.born) lines.push(tx('born {date}', { date: p.born }));
  if (p.died) lines.push(tx('died {date}', { date: p.died }));
  let out = lines.join(', ') + (lines.length ? '\n\n' : '');
  if (p.desc) out += p.desc + '\n\n';
  out += tx('in the family tree: {link}', { link: '[' + a.texts[pickLang(a.texts)].title + '](' + treeSlug + ')' }) + '\n';
  return out;
}

/* ---- earlier texts ---- */
async function wikiHistory(slug, parts) {
  const lang = arg('lang') || '';
  const [a, revs] = await Promise.all([api('GET', W.one(slug)), api('GET', W.history(slug, lang))]);
  const shown = a.texts[pickLang(a.texts, lang)];
  const sel = h('select', { title: tx('language') }, h('option', { value: '', text: tx('all languages') }),
    LANGS.filter(([c]) => revs.some(r => r.lang === c) || c === lang).map(([c, label]) => h('option', { value: c, text: label })),
    revs.some(r => r.lang === 'tree') || lang === 'tree' ? h('option', { value: 'tree', text: tx('family tree') }) : null);
  sel.value = lang;
  sel.addEventListener('change', () => navigate(W.past(slug, sel.value), true));
  /* an earlier family tree: who was in it; put back as the newest */
  async function openTree(r) {
    const old = await api('GET', W.tree(slug, r.rev));
    const people = old.data ? Object.values(old.data.people).map(p => p.name).sort(nameCmp) : [];
    const latest = a.tree && a.tree.rev === r.rev;
    const buttons = [{ label: tx('close'), value: null }];
    if (!offline() && !latest) buttons.push({ label: tx('put this tree back'), value: 'back', primary: true });
    const body = h('div', { class: 'wiki-old' }, old.data ? [h('p', { text: txn(people.length, '{n} person', '{n} people') }), h('p', { class: 'dim', text: people.join(', ') })]
      : h('p', { class: 'dim', text: tx('the family tree was removed') }));
    const v = await dialog({ title: tx('family tree') + ' · ' + fmtTime(old.created) + ' · ' + old.author, body, buttons, wide: true });
    if (v !== 'back') return;
    const layout = old.data ? (await parts.hold('tree')).layout(old.data) : null;
    await api('PUT', W.tree(slug), { data: old.data, base: a.tree ? a.tree.rev : 0, note: tx('put back the tree of {time}', { time: fmtTime(old.created) }), layout });
    toast(tx('put back'));
    navigate(W.page(slug));
  }
  async function open(r) {
    if (r.lang === 'tree') return openTree(r);
    const full = await api('GET', W.rev(slug, r.rev));
    const view = h('div', { class: 'md-body wiki-old' });
    view.innerHTML = full.html;
    const cur = a.texts[full.lang];
    const latest = cur && cur.rev === full.rev;
    const buttons = [{ label: tx('close'), value: null }];
    if (!offline() && !latest) buttons.push({ label: tx('put this text back'), value: 'back', primary: true });
    const v = await dialog({ title: full.title + ' · ' + langLabel(full.lang) + ' · ' + fmtTime(full.created) + ' · ' + full.author, body: view, buttons, wide: true });
    if (v !== 'back') return;
    await api('PUT', W.text(slug, full.lang), { title: full.title, body: full.body, base: cur ? cur.rev : 0, note: tx('put back the text of {time}', { time: fmtTime(full.created) }) });
    toast(tx('put back'));
    navigate(W.page(slug, full.lang));
  }
  const rows = revs.map(r => h('tr', {},
    h('td', { class: 'tm', text: fmtTime(r.created) }),
    h('td', {}, h('span', { class: 'tag', text: r.lang === 'tree' ? tx('tree') : langLabel(r.lang) })),
    h('td', { class: 'nm' }, h('a', { href: '#', text: r.lang === 'tree' ? tx('family tree') + (r.removed ? ' (' + tx('removed') + ')' : '') : r.title, onclick: e => { e.preventDefault(); open(r).catch(fail); } }),
      (r.lang === 'tree' ? a.tree && a.tree.rev === r.rev : a.texts[r.lang] && a.texts[r.lang].rev === r.rev) ? h('span', { class: 'tag', text: tx('now') }) : null),
    h('td', { text: r.author }),
    h('td', { class: 'dim', text: r.note }),
    h('td', { class: 'sz', text: fmtSize(r.size) })));
  const el = h('section', { class: 'win filewin wiki-histwin' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), wikiCrumbs(slug, shown ? shown.title : slug, tx('history')), h('span', { class: 'spacer' }), sel),
    h('div', { class: 'listwrap' }, h('table', { class: 'x11 files' },
      h('thead', {}, h('tr', {}, ['when', 'language', 'title', 'by', 'what changed', 'size'].map(t => h('th', { text: tx(t) })))), h('tbody', {}, rows))));
  return { el, cls: 'fill', title: tx('history') + ' · ' + (shown ? shown.title : slug), status: txn(revs.length, '{n} version', '{n} versions') };
}

return { wikiList, wikiPage, wikiEdit, wikiHistory, refPicker };
});

// An article's tables as the admin pages' grids (T8): sort by a column, filter,
// choose columns; a column of numbers sorts as numbers; cells keep their links.
async function gridTables(root, key) {
  const tables = [...root.querySelectorAll('table')].filter(t => !t.closest('pre'));
  if (!tables.length) return;
  let mod;
  try { mod = await import('/static/grid.js'); } catch (e) { return; }
  tables.forEach((t, i) => {
    const headRow = t.tHead && t.tHead.rows[0] ? t.tHead.rows[0] : t.rows[0];
    if (!headRow) return;
    const head = [...headRow.cells];
    const rowsEl = [...t.rows].filter(r => r !== headRow && !(t.tHead && r.parentElement === t.tHead));
    const rows = rowsEl.map(r => {
      const o = {};
      [...r.cells].forEach((c, j) => { o['c' + j] = c.textContent.trim(); o['h' + j] = c.innerHTML; });
      return o;
    });
    const cols = head.map((c, j) => {
      const col = { key: 'c' + j, label: c.textContent.trim() || '#' + (j + 1) };
      const vals = rows.map(r => r['c' + j]).filter(v => v !== '' && v !== undefined);
      if (vals.length && vals.every(v => /^-?\d+([.,]\d+)?$/.test(v))) {
        col.type = 'number';
        col.value = r => (r['c' + j] === '' || r['c' + j] === undefined) ? null : +r['c' + j].replace(',', '.');
      }
      // the article's own markup (rendered by the server, as the rest of the article)
      col.cell = r => { const s = document.createElement('span'); s.innerHTML = r['h' + j] || ''; return s; };
      return col;
    });
    const g = mod.grid({ id: 'wiki-' + key + '-' + i, columns: cols, rows, empty: '—' });
    const box = document.createElement('div');
    box.className = 'wiki-grid';
    box.append(g.node);
    t.replaceWith(box);
  });
}
