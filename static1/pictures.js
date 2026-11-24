/* The pictures metaplace: every picture, by month, and the slideshow. Part pictures (util.js): loaded when the page opens, it
   uses the app's helpers (app.js, util.js) as globals. */
'use strict';

part('pictures', ({ shareDialog }) => {

/* ---- metaplace: every picture, newest first, and a slideshow through them ---- */
const PIC_SRC = { e: 'exif', n: 'from the name', f: 'from the folder', m: 'file time' };
function thumbUrl(p, screen) { return '/api/thumb/' + p.loc + encP(p.path) + '?v=' + p.mtime + (screen ? '&s=screen' : ''); }
/* `taken` is the wall-clock time written as if it were UTC */
function picMonth(p) { const d = new Date(p.taken * 1000); return d.getUTCFullYear() + '-' + pad2(d.getUTCMonth() + 1); }
function picDate(p) {
  const d = new Date(p.taken * 1000), day = picMonth(p) + '-' + pad2(d.getUTCDate());
  // a folder or a name without a time gives only the day
  return p.src === 'f' || (p.src === 'n' && p.taken % 86400 === 0) ? day : day + ' ' + pad2(d.getUTCHours()) + ':' + pad2(d.getUTCMinutes());
}

async function picturesPage() {
  const [r, places] = await Promise.all([api('GET', '/api/pictures'), loadPlaces().catch(() => null)]);
  const pics = r.items.map(([d, name, taken, mtime, src, links]) => {
    const [loc, dir, write] = r.dirs[d];
    // dir is null for a single shared picture: the share is the file; links: other names keep a deleted one
    return { loc, dir, write, name, taken, mtime, src, links: links || 1, path: dir === null ? '/' : joinPath(dir, name) };
  });
  const shareName = loc => { const s = places && places.shares.find(x => x.loc === loc); return s ? s.name : tx('shared'); };

  const wrap = h('div', { class: 'picwrap', tabindex: '0' });
  const tiles = [];
  const years = h('select', { title: tx('jump to a year'), onchange: () => {
    const hd = wrap.querySelector('[data-y="' + years.value + '"]');
    if (hd) wrap.scrollTop = hd.nextSibling.offsetTop - hd.offsetHeight;
    years.value = '';
    wrap.focus();
  } }, h('option', { value: '', text: tx('year') }));
  for (let i = 0; i < pics.length;) {
    const m = picMonth(pics[i]);
    let j = i;
    while (j < pics.length && picMonth(pics[j]) === m) j++;
    const y = m.slice(0, 4), first = !years.querySelector('option[value="' + y + '"]');
    if (first) years.appendChild(h('option', { value: y, text: y }));
    const grid = h('div', { class: 'pic-grid' });
    for (let k = i; k < j; k++) {
      const p = pics[k];
      const t = h('a', { class: 'pic', href: U.view(p.loc, p.path), title: p.name + '\n' + picDate(p), 'data-i': String(k) });
      tiles.push(t);
      grid.appendChild(t);
    }
    wrap.append(h('div', { class: 'pic-month', 'data-y': first ? y : null }, m, h('span', { class: 'dim', text: ' ' + (j - i) })), grid);
    i = j;
  }
  if (!pics.length) wrap.appendChild(h('div', { class: 'empty dim', text: r.scanning ? tx('looking for pictures; reload in a while') : tx('no pictures here yet') }));

  // thumbnails once the scrolling stops, so a fast scroll does not fetch all it passes
  const want = new Set();
  let lazy = 0;
  const io = new IntersectionObserver(es => {
    for (const e of es) { if (e.isIntersecting) want.add(e.target); else want.delete(e.target); }
    clearTimeout(lazy);
    lazy = setTimeout(() => { want.forEach(fill); want.clear(); }, 120);
  }, { root: wrap, rootMargin: '400px 0px' });
  tiles.forEach(t => io.observe(t));
  function fill(t) {
    io.unobserve(t);
    const p = pics[+t.dataset.i];
    const img = h('img', { alt: '', decoding: 'async' });
    img.onerror = () => {
      // one the server cannot shrink (avif …) may still show as itself
      if (!img.dataset.again && !/^jpe?g$/.test(extOf(p.name))) { img.dataset.again = '1'; img.src = thumbUrl(p, true); return; }
      img.remove();
      t.classList.add('broken');
      t.textContent = p.name;
    };
    img.src = thumbUrl(p);
    t.appendChild(img);
  }

  // the slideshow takes one history step, so "back" closes it; #<n> is the picture
  let pushed = false;
  const slides = slideshow(pics, shareName, {
    moved: n => history.replaceState(null, '', '/pictures#' + n),
    close: () => { if (pushed) history.back(); else { history.replaceState(null, '', '/pictures'); hide(); } },
    renamed: p => {
      const t = tiles[pics.indexOf(p)];
      t.href = U.view(p.loc, p.path);
      t.title = p.name + '\n' + picDate(p);
      if (t.classList.contains('broken')) t.textContent = p.name;
    },
    removed: p => {
      const i = pics.indexOf(p), t = tiles[i], grid = t.parentNode, hd = grid.previousSibling;
      pics.splice(i, 1);
      tiles.splice(i, 1);
      io.unobserve(t);
      want.delete(t);
      t.remove();
      for (let k = i; k < tiles.length; k++) tiles[k].dataset.i = String(k);
      lbl.textContent = txn(pics.length, '{n} picture', '{n} pictures');
      if (grid.children.length) { hd.lastChild.textContent = ' ' + grid.children.length; return; }
      // the month is gone; the year's jump moves to its next month, if it has one
      const y = hd.dataset.y, nx = grid.nextSibling;
      if (y && nx && nx.firstChild.textContent.startsWith(y)) nx.dataset.y = y;
      else if (y) years.querySelector('option[value="' + y + '"]').remove();
      hd.remove();
      grid.remove();
      if (!pics.length) wrap.appendChild(h('div', { class: 'empty dim', text: tx('no pictures here yet') }));
    },
  });
  const inHash = () => { const m = /^#(\d+)$/.exec(location.hash); return m && pics.length ? Math.min(+m[1], pics.length - 1) : null; };
  function open(n) {
    history.pushState(null, '', '/pictures#' + n);
    pushed = true;
    slides.show(n);
  }
  function hide() {
    const n = slides.hide();
    pushed = false;
    if (tiles[n]) tiles[n].scrollIntoView({ block: 'center' });
    wrap.focus();
  }
  wrap.addEventListener('click', e => {
    const t = e.target.closest('a.pic');
    if (!t || e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    open(+t.dataset.i);
  });

  const scan = r.scanning ? h('div', { class: 'note dim', text: r.scanning.total
    ? tx('still looking for pictures ({done} of {total}); reload in a while for the rest', r.scanning)
    : tx('still looking for pictures; reload in a while for the rest') }) : null;
  const lbl = h('span', { class: 'lbl', text: txn(pics.length, '{n} picture', '{n} pictures') });
  const bar = h('div', { class: 'toolbar' }, lbl, pics.length ? years : null,
    h('span', { class: 'spacer' }), pics.length ? btn(tx('slideshow'), tx('from the newest, playing'), () => { open(0); slides.play(true); }) : null);
  const pw = h('section', { class: 'win picwin' },
    h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), h('span', { class: 'crumbs' }, h('span', { class: 'here', text: tx('pictures') }))),
    bar, scan, wrap, slides.el);
  const range = pics.length ? ', ' + tx('{from} to {to}', { from: picMonth(pics[pics.length - 1]), to: picMonth(pics[0]) }) : '';
  return {
    el: h('div', { class: 'files-layout' }, placesBox(places, 'pictures'), pw), cls: 'fill', title: tx('pictures'),
    status: txn(pics.length, '{n} picture', '{n} pictures') + range + '; ' + tx('dates from exif, else the file name, the folder or the file time'),
    shown: () => { const n = inHash(); if (n !== null) slides.show(n); else wrap.focus(); },
    pop: () => {
      if (location.pathname !== '/pictures') return false;
      const n = inHash();
      if (n === null) hide(); else slides.show(n);
      return true;
    },
    key: e => slides.open() ? slides.key(e) : false,
    destroy: () => { io.disconnect(); clearTimeout(lazy); slides.hide(); },
  };
}

/* full-window pictures, one at a time: arrows, swipes, space plays; on wide screens
   the picture can be renamed, moved, shared and deleted */
function slideshow(pics, shareName, on) {
  let cur = -1, playing = false, timer = 0;
  let img = h('img', { class: 'slide-img', alt: '' });
  const msg = h('div', { class: 'slide-msg dim', hidden: true });
  const count = h('span', { class: 'slide-count' });
  const date = h('span', { class: 'slide-date' });
  const name = h('a', { class: 'slide-name', title: tx('open in the viewer') });
  const folder = h('a', { class: 'slide-dir dim', title: tx('the folder') });
  const dl = h('a', { class: 'btn-link', download: '', text: tx('download') });
  const playB = btn(tx('play'), tx('play / pause (space)'), () => play(!playing));
  const bRen = btn(tx('rename'), tx('rename (f2)'), () => act(rename), 'slide-act');
  const bMove = btn(tx('move'), tx('move to another folder'), () => act(move), 'slide-act');
  const bShare = btn(tx('share'), tx('share with users or by link'), () => act(p => shareDialog(p.loc, p.path, p.name)), 'slide-act');
  const bDel = btn(tx('delete'), tx('delete (del)'), () => act(remove), 'slide-act danger');
  const stage = h('div', { class: 'slide-stage' }, img, msg,
    h('button', { class: 'slide-go prev', text: '<', title: tx('previous (left arrow)'), onclick: () => go(cur - 1) }),
    h('button', { class: 'slide-go next', text: '>', title: tx('next (right arrow)'), onclick: () => go(cur + 1) }));
  const el = h('div', { class: 'slides', hidden: true },
    h('div', { class: 'slide-bar' }, count, date, name, folder, h('span', { class: 'spacer' }), playB,
      bRen, bMove, bShare, bDel, dl, btn(tx('close'), tx('close (esc)'), () => on.close())),
    stage);

  let sx = null, sy = 0;
  stage.addEventListener('pointerdown', e => { sx = e.clientX; sy = e.clientY; });
  stage.addEventListener('pointerup', e => {
    if (sx === null) return;
    const dx = e.clientX - sx, dy = e.clientY - sy;
    sx = null;
    if (Math.abs(dx) > 40 && Math.abs(dx) > Math.abs(dy)) go(cur + (dx < 0 ? 1 : -1));
  });
  stage.addEventListener('pointercancel', () => { sx = null; });
  stage.addEventListener('dragstart', e => e.preventDefault());

  // while playing: the next picture `ms` after this one is up
  function later(ms) {
    clearTimeout(timer);
    if (playing) timer = setTimeout(() => { if (!go(cur + 1)) play(false); }, ms);
  }

  function show(n) {
    if (n < 0 || n >= pics.length) return false;
    el.hidden = false;
    cur = n;
    const p = pics[n];
    count.textContent = (n + 1) + ' / ' + pics.length;
    date.textContent = picDate(p) + ' (' + tx(PIC_SRC[p.src]) + ')';
    name.textContent = p.name;
    name.href = U.view(p.loc, p.path);
    folder.textContent = p.dir === null ? shareName(p.loc) : (p.loc === 'home' ? '' : shareName(p.loc)) + p.dir;
    folder.href = p.dir === null ? '/shared' : U.files(p.loc, p.dir);
    dl.href = U.raw(p.loc, p.path) + '?dl=1';
    // a shared file is the root of its share; only the owner shares
    bRen.disabled = bMove.disabled = bDel.disabled = !p.write || p.dir === null;
    bShare.disabled = p.loc !== 'home';
    clearTimeout(timer);
    // swapped in once loaded: the last picture stays up meanwhile
    const next = h('img', { class: 'slide-img', alt: '' });
    next.onload = () => {
      if (cur !== n) return;
      img.replaceWith(next);
      img = next;
      msg.hidden = true;
      later(4000);
    };
    next.onerror = () => {
      if (cur !== n) return;
      img.removeAttribute('src');
      msg.textContent = tx('cannot show {name}', { name: p.name });
      msg.hidden = false;
      later(1000);
    };
    next.src = thumbUrl(p, true);
    for (const k of [n + 1, n + 2, n - 1]) if (pics[k]) new Image().src = thumbUrl(pics[k], true);
    return true;
  }
  function go(n) {
    if (!show(n)) return false;
    on.moved(n);
    return true;
  }
  function play(yes) {
    playing = yes;
    playB.textContent = yes ? tx('pause') : tx('play');
    playB.classList.toggle('on', yes);
    later(4000);
  }

  /* the show pauses for the dialog */
  function act(f) {
    if (!pics[cur]) return;
    play(false);
    f(pics[cur]).catch(fail);
  }
  async function rename(p) {
    const nn = await dialogAsk(tx('rename'), tx('new name'), p.name, tx('rename'), async v => {
      const bad = checkName(v);
      if (bad) return bad;
      if (v === p.name) return null;
      await api('POST', '/api/mv', { loc: p.loc, moves: [{ from: p.path, to: joinPath(p.dir, v) }] });
    });
    if (nn && nn !== p.name) changed(p, p.dir, nn);
  }
  async function move(p) {
    const dest = await pickFolder(p.loc, p.dir, tx('move {what}', { what: p.name }), []);
    if (dest === null || dest === p.dir) return;
    await api('POST', '/api/mv', { loc: p.loc, moves: [{ from: p.path, to: joinPath(dest, p.name) }] });
    toast(tx('moved to {dest}', { dest }));
    changed(p, dest, p.name);
  }
  function changed(p, dir, nn) {
    p.dir = dir;
    p.name = nn;
    p.path = joinPath(dir, nn);
    on.renamed(p);
    if (pics[cur] === p) show(cur);
  }
  async function remove(p) {
    const also = p.links > 1 ? ' ' + txn(p.links - 1, 'the same file is also at another place (hard links), which keeps it.',
      'the same file is also at {n} other places (hard links), which keep it.') : '';
    if (!(await confirmBox(tx('delete'), tx('delete {what}? this cannot be undone.', { what: p.name }) + also, tx('delete'), true))) return;
    await api('POST', '/api/rm', { loc: p.loc, paths: [p.path] });
    const n = pics.indexOf(p);
    on.removed(p);
    if (!pics.length) on.close();
    else go(Math.min(n, pics.length - 1));
  }
  return {
    el, show, play,
    open: () => !el.hidden,
    /* closes; the index of the last picture shown */
    hide: () => {
      play(false);
      el.hidden = true;
      const n = cur;
      cur = -1;
      return n;
    },
    key: e => {
      if (e.ctrlKey || e.metaKey || e.altKey) return false;
      if (e.target instanceof HTMLButtonElement && (e.key === ' ' || e.key === 'Enter')) return false;
      if (e.key === 'ArrowLeft' || e.key === 'PageUp') go(cur - 1);
      else if (e.key === 'ArrowRight' || e.key === 'PageDown') go(cur + 1);
      else if (e.key === 'Home') go(0);
      else if (e.key === 'End') go(pics.length - 1);
      else if (e.key === ' ') play(!playing);
      else if (e.key === 'Escape') on.close();
      else if (e.key === 'F2' && !bRen.disabled) act(rename);
      else if (e.key === 'Delete' && !bDel.disabled) act(remove);
      else return false;
      return true;
    },
  };
}

return { picturesPage };
});
