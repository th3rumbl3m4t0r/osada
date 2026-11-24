/* Presentations: the deck model the formats share (odp.js, pptx.js, msppt.js), the viewer
   and the editor. Part slides, on rich.

   A deck: {w, h (the slide's size, px at 96 dpi), slides, lost}. A slide: {bg (colour or
   null), shapes, notes}. A shape: {x, y, w, h, rot (degrees, clockwise), geom ('rect',
   'ellipse', 'roundRect', 'line', or null for a plain frame), fill, line {color, w},
   img (a data: URL), paras (the editor's blocks, rich.js, sizes in half-points),
   anchor ('t' 'ctr' 'b'), ph (the placeholder's kind: 'title', 'body'...), table
   {cols: [px], rows: [{cells: [{paras, span, rspan, fill}]}]}, flipH, flipV, size
   (the text's size where a run has none, pt)}. */
'use strict';

part('slides', ({ Rich }) => {

/* a deck's pictures as object URLs, made once and given back when done */
class Pics {
  constructor() { this.map = new Map(); }
  url(src) {
    if (!src || !src.startsWith('data:')) return src;
    let u = this.map.get(src);
    if (!u) {
      const d = Rich.dataBytes(src);
      u = d ? URL.createObjectURL(new Blob([d.bytes], { type: d.type })) : src;
      this.map.set(src, u);
    }
    return u;
  }
  done() { for (const u of this.map.values()) if (u.startsWith('blob:')) URL.revokeObjectURL(u); this.map.clear(); }
}

/* the text of a shape as HTML: its paragraphs at its size */
function textHtml(sh) {
  const html = Rich.html(sh.paras || [], Math.round((sh.size || 18) * 2), null);
  return html;
}

/* one shape as a positioned element (in slide pixels) */
function shapeEl(sh, pics, i) {
  const el = h('div', { class: 'shp' + (sh.ph ? ' ph-' + sh.ph : ''), 'data-i': String(i) });
  Object.assign(el.style, { left: sh.x + 'px', top: sh.y + 'px', width: Math.max(1, sh.w) + 'px', height: Math.max(1, sh.h) + 'px' });
  const tf = [];
  if (sh.rot) tf.push('rotate(' + sh.rot + 'deg)');
  if (sh.flipH) tf.push('scaleX(-1)');
  if (sh.flipV) tf.push('scaleY(-1)');
  if (tf.length) el.style.transform = tf.join(' ');
  if (sh.geom === 'line') {
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('viewBox', '0 0 ' + Math.max(1, sh.w) + ' ' + Math.max(1, sh.h));
    svg.setAttribute('preserveAspectRatio', 'none');
    const ln = document.createElementNS('http://www.w3.org/2000/svg', 'line');
    ln.setAttribute('x1', '0'); ln.setAttribute('y1', '0'); ln.setAttribute('x2', String(Math.max(1, sh.w))); ln.setAttribute('y2', String(Math.max(1, sh.h)));
    ln.setAttribute('stroke', (sh.line && sh.line.color) || '#000000');
    ln.setAttribute('stroke-width', String((sh.line && sh.line.w) || 1.33));
    svg.appendChild(ln);
    el.appendChild(svg);
    el.classList.add('shp-line');
    return el;
  }
  if (sh.fill) el.style.background = sh.fill;
  if (sh.line && sh.line.color) el.style.border = Math.max(1, Math.round(sh.line.w || 1)) + 'px solid ' + sh.line.color;
  if (sh.geom === 'ellipse') el.style.borderRadius = '50%';
  else if (sh.geom === 'roundRect') el.style.borderRadius = Math.min(sh.w, sh.h) * 0.12 + 'px';
  if (sh.img) el.appendChild(h('img', { class: 'shp-img', src: pics.url(sh.img), alt: '', draggable: false }));
  if (sh.table) {
    const t = h('table', { class: 'shp-table' });
    if (sh.table.cols && sh.table.cols.length) t.appendChild(h('colgroup', {}, sh.table.cols.map(w => h('col', { style: { width: w + 'px' } }))));
    for (const r of sh.table.rows) {
      const tr = h('tr');
      for (const c of r.cells) {
        const td = h('td', { colspan: c.span > 1 ? String(c.span) : null, rowspan: c.rspan > 1 ? String(c.rspan) : null });
        if (c.fill) td.style.background = c.fill;
        td.innerHTML = Rich.html(c.paras || [], Math.round((sh.size || 14) * 2), null) || '';
        tr.appendChild(td);
      }
      t.appendChild(tr);
    }
    el.appendChild(t);
  }
  if (sh.paras && sh.paras.length) {
    const tx = h('div', { class: 'shp-text a-' + (sh.anchor || 't') });
    tx.style.fontSize = (sh.size || 18) + 'pt';
    tx.innerHTML = textHtml(sh);
    el.appendChild(tx);
  }
  return el;
}

/* a slide at scale k: the frame (sized) and the slide inside it */
function slideEl(deck, s, pics, k) {
  const inner = h('div', { class: 'slide' });
  Object.assign(inner.style, { width: deck.w + 'px', height: deck.h + 'px', transform: 'scale(' + k + ')', background: s.bg || '#ffffff' });
  // what the master and layout draw, behind the slide's own shapes and not picked
  (s.under || []).forEach(sh => { const el = shapeEl(sh, pics, -1); el.classList.add('under'); el.removeAttribute('data-i'); inner.appendChild(el); });
  s.shapes.forEach((sh, i) => inner.appendChild(shapeEl(sh, pics, i)));
  const frame = h('div', { class: 'slide-frame' }, inner);
  Object.assign(frame.style, { width: Math.round(deck.w * k) + 'px', height: Math.round(deck.h * k) + 'px' });
  return { frame, inner };
}

/* The viewer: the slides down the side, the one picked large; "present" goes full screen.
   opts = {start} */
class SlideView {
  constructor(host, deck, opts) {
    this.deck = deck;
    this.pics = new Pics();
    this.cur = Math.min((opts && opts.start) || 0, Math.max(0, deck.slides.length - 1));
    this.rail = h('div', { class: 'slide-rail' });
    this.stage = h('div', { class: 'slide-stage-x' });
    this.count = h('span', { class: 'dim' });
    const b = (t, title, fn) => h('button', { text: t, title, onclick: fn });
    this.bar = h('div', { class: 'toolbar slides-bar' }, b('<', tx('previous slide (left arrow)'), () => this.go(this.cur - 1)), this.count,
      b('>', tx('next slide (right arrow, space)'), () => this.go(this.cur + 1)), h('span', { class: 'spacer' }),
      b(tx('present'), tx('full screen, from this slide (esc leaves)'), () => this.present()));
    this.wrap = h('div', { class: 'slides-wrap' }, this.rail, this.stage);
    clear(host).append(this.bar, this.wrap);
    host.classList.add('slides-host');
    this.onKey = e => this.key(e);
    document.addEventListener('keydown', this.onKey);
    this.ro = new ResizeObserver(() => this.paint());
    this.ro.observe(this.stage);
    this.thumbs();
    this.paint();
  }

  thumbs() {
    const k = 150 / this.deck.w;
    this.rail.replaceChildren(...this.deck.slides.map((s, i) => {
      const { frame } = slideEl(this.deck, s, this.pics, k);
      const t = h('button', { class: 'slide-thumb' + (i === this.cur ? ' on' : ''), title: tx('slide {n}', { n: i + 1 }), onclick: () => this.go(i) }, frame, h('span', { class: 'slide-no', text: String(i + 1) }));
      return t;
    }));
  }

  paint() {
    const s = this.deck.slides[this.cur];
    this.count.textContent = this.deck.slides.length ? (this.cur + 1) + ' / ' + this.deck.slides.length : tx('no slides');
    if (!s) { this.stage.replaceChildren(h('div', { class: 'viewer-note dim', text: tx('no slides') })); return; }
    const W = this.stage.clientWidth - 16, H = this.stage.clientHeight - 16;
    const k = Math.max(0.05, Math.min(W / this.deck.w, H / this.deck.h));
    this.stage.replaceChildren(slideEl(this.deck, s, this.pics, k).frame);
    this.rail.querySelectorAll('.slide-thumb').forEach((t, i) => t.classList.toggle('on', i === this.cur));
    const on = this.rail.querySelector('.slide-thumb.on');
    if (on && on.scrollIntoView) on.scrollIntoView({ block: 'nearest' });
  }

  go(i) {
    if (i < 0 || i >= this.deck.slides.length || i === this.cur) return;
    this.cur = i;
    this.paint();
    if (this.show) this.showPaint();
  }

  key(e) {
    if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey || isTyping(e)) return;
    if (e.key === 'ArrowRight' || e.key === 'PageDown' || (this.show && (e.key === ' ' || e.key === 'Enter'))) { e.preventDefault(); this.go(this.cur + 1); }
    else if (e.key === 'ArrowLeft' || e.key === 'PageUp' || (this.show && e.key === 'Backspace')) { e.preventDefault(); this.go(this.cur - 1); }
    else if (e.key === 'Home') { e.preventDefault(); this.go(0); }
    else if (e.key === 'End') { e.preventDefault(); this.go(this.deck.slides.length - 1); }
    else if (e.key === 'Escape' && this.show) { e.preventDefault(); e.stopPropagation(); this.unpresent(); }
  }

  /* full screen: one slide on black, a click goes on */
  present() {
    this.show = h('div', { class: 'slides-show', tabindex: '-1' });
    this.show.addEventListener('click', e => { if (e.clientX < innerWidth / 4) this.go(this.cur - 1); else this.go(this.cur + 1); });
    document.body.appendChild(this.show);
    this.showPaint();
    const fs = this.show.requestFullscreen ? this.show.requestFullscreen() : null;
    if (fs && fs.catch) fs.catch(() => {});
    this.onFs = () => { if (!document.fullscreenElement && this.show) this.unpresent(); };
    document.addEventListener('fullscreenchange', this.onFs);
    this.show.focus();
  }
  showPaint() {
    const s = this.deck.slides[this.cur];
    const k = Math.min(innerWidth / this.deck.w, innerHeight / this.deck.h);
    this.show.replaceChildren(slideEl(this.deck, s, this.pics, k).frame);
  }
  unpresent() {
    document.removeEventListener('fullscreenchange', this.onFs);
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
    if (this.show) this.show.remove();
    this.show = null;
  }

  destroy() {
    if (this.show) this.unpresent();
    document.removeEventListener('keydown', this.onKey);
    this.ro.disconnect();
    this.pics.done();
  }
}

/* The editor: the slides down the side (add, duplicate, delete, move), the slide large.
   A click picks a shape: drag to move it, its corner to size it; a double click edits its
   text; the toolbar formats the text being edited. opts = {onChange(dirty)}; save with
   deck() (the model as it is now). */
class SlideEditor {
  constructor(host, deck, opts) {
    this.d = deck;
    this.opts = opts || {};
    this.pics = new Pics();
    this.cur = 0;
    this.sel = -1;
    this.editing = null;
    this.undos = [];
    this.redos = [];
    this.dirty = false;
    if (!this.d.slides.length) this.d.slides.push({ bg: null, shapes: [] });
    const b = (t, title, fn, cls) => {
      const el = h('button', { text: t, title, class: cls || null });
      el.addEventListener('mousedown', e => e.preventDefault());
      el.addEventListener('click', fn);
      return el;
    };
    const cmd = (c, v) => () => { if (this.editing) { document.execCommand(c, false, v); this.touch(); } };
    const sizeSel = h('select', { title: tx('size (pt)') }, h('option', { value: '', text: tx('size') }),
      [10, 12, 14, 16, 18, 20, 24, 28, 32, 36, 40, 44, 54, 66, 80].map(s => h('option', { value: s, text: s + 'pt' })));
    sizeSel.addEventListener('change', () => { if (sizeSel.value) this.fontSize(+sizeSel.value); sizeSel.value = ''; });
    const color = h('input', { type: 'color', value: '#c0473b', title: tx('text colour') });
    color.addEventListener('input', () => cmd('foreColor', color.value)());
    const fill = h('input', { type: 'color', value: '#4472c4', title: tx('fill of the picked shape') });
    fill.addEventListener('input', () => this.setFill(fill.value));
    this.bar = h('div', { class: 'toolbar slides-bar' },
      b(tx('undo'), tx('undo (ctrl+z)'), () => this.undo()), b(tx('redo'), tx('redo (ctrl+y)'), () => this.redo()), h('span', { class: 'sep' }),
      b(tx('text box'), tx('a new text box on this slide'), () => this.addText()), b(tx('image'), tx('a picture on this slide'), () => this.addImage()),
      b(tx('shape'), tx('a rectangle on this slide'), () => this.addShape()),
      b(tx('delete'), tx('delete the picked shape (del)'), () => this.removeShape(), 'danger'), fill, h('span', { class: 'sep' }),
      b('b', tx('bold (ctrl+b)'), cmd('bold'), 'fmt-b'), b('i', tx('italic (ctrl+i)'), cmd('italic'), 'fmt-i'), b('u', tx('underline (ctrl+u)'), cmd('underline'), 'fmt-u'),
      sizeSel, color, b(tx('left'), tx('align left'), cmd('justifyLeft')), b(tx('center'), tx('center'), cmd('justifyCenter')), b(tx('right'), tx('align right'), cmd('justifyRight')),
      b(tx('• list'), tx('bullet list'), cmd('insertUnorderedList')), h('span', { class: 'sep' }),
      b(tx('new slide'), tx('a new slide after this one'), () => this.addSlide()), b(tx('duplicate'), tx('a copy of this slide after it'), () => this.dupSlide()),
      b(tx('delete slide'), null, () => this.removeSlide(), 'danger'), b('▲', tx('move the slide up'), () => this.moveSlide(-1)), b('▼', tx('move the slide down'), () => this.moveSlide(1)));
    this.rail = h('div', { class: 'slide-rail' });
    this.stage = h('div', { class: 'slide-stage-x edit' });
    this.wrap = h('div', { class: 'slides-wrap' }, this.rail, this.stage);
    clear(host).append(this.bar, this.wrap);
    host.classList.add('slides-host');
    this.onKey = e => this.key(e);
    document.addEventListener('keydown', this.onKey);
    this.ro = new ResizeObserver(() => this.paint());
    this.ro.observe(this.stage);
    this.stage.addEventListener('pointerdown', e => this.down(e));
    this.stage.addEventListener('dblclick', e => this.dbl(e));
    this.thumbs();
    this.paint();
  }

  get slide() { return this.d.slides[this.cur]; }
  /* for the edit page: the file in the format it was opened with, what it cannot keep, where focus goes */
  text() { return this.opts.format.write(this.deck()); }
  get lost() { return this.d.lost || {}; }
  get page() { return this.stage; }
  deck() { this.commitText(); return this.d; }
  touch() {
    if (!this.dirty) { this.dirty = true; if (this.opts.onChange) this.opts.onChange(true); }
  }
  markSaved() { this.dirty = false; if (this.opts.onChange) this.opts.onChange(false); }

  /* undo: whole-deck snapshots before each change (pictures are shared strings) */
  snap() {
    this.commitText();
    this.undos.push(JSON.stringify({ d: this.d, cur: this.cur }));
    if (this.undos.length > 60) this.undos.shift();
    this.redos = [];
  }
  restore(from, to) {
    if (!from.length) return;
    this.commitText();
    to.push(JSON.stringify({ d: this.d, cur: this.cur }));
    const s = JSON.parse(from.pop());
    this.d.slides = s.d.slides;
    this.cur = Math.min(s.cur, this.d.slides.length - 1);
    this.sel = -1;
    this.touch();
    this.thumbs();
    this.paint();
  }
  undo() { this.restore(this.undos, this.redos); }
  redo() { this.restore(this.redos, this.undos); }

  thumbs() {
    const k = 150 / this.d.w;
    this.rail.replaceChildren(...this.d.slides.map((s, i) => h('button', { class: 'slide-thumb' + (i === this.cur ? ' on' : ''), title: tx('slide {n}', { n: i + 1 }), onclick: () => this.pick(i) },
      slideEl(this.d, s, this.pics, k).frame, h('span', { class: 'slide-no', text: String(i + 1) }))));
  }
  thumb(i) {
    const t = this.rail.children[i];
    if (t) t.replaceChildren(slideEl(this.d, this.d.slides[i], this.pics, 150 / this.d.w).frame, h('span', { class: 'slide-no', text: String(i + 1) }));
  }
  pick(i) {
    this.commitText();
    this.cur = i;
    this.sel = -1;
    this.rail.querySelectorAll('.slide-thumb').forEach((t, j) => t.classList.toggle('on', j === i));
    this.paint();
  }

  paint() {
    const W = this.stage.clientWidth - 16, H = this.stage.clientHeight - 16;
    this.k = Math.max(0.05, Math.min(W / this.d.w, H / this.d.h));
    const { frame, inner } = slideEl(this.d, this.slide, this.pics, this.k);
    inner.style.setProperty('--k', this.k);   // handles and outlines keep their size on screen
    this.inner = inner;
    this.stage.replaceChildren(frame);
    this.mark();
  }
  /* the picked shape: outlined, with a handle at its corner */
  mark() {
    if (!this.inner) return;
    this.inner.querySelectorAll('.shp').forEach(el => el.classList.toggle('picked', +el.dataset.i === this.sel));
    this.inner.querySelectorAll('.shp-handle').forEach(x => x.remove());
    const el = this.inner.querySelector('.shp[data-i="' + this.sel + '"]');
    if (el && !this.editing) el.appendChild(h('div', { class: 'shp-handle' }));
  }

  down(e) {
    if (e.button !== 0) return;
    const el = e.target.closest('.shp:not(.under)');
    if (this.editing && el && el === this.editing.el) return;   // clicks inside the text being edited
    this.commitText();
    if (!el) { this.sel = -1; this.mark(); return; }
    this.sel = +el.dataset.i;
    this.mark();
    const sh = this.slide.shapes[this.sel], resize = e.target.classList.contains('shp-handle');
    const x0 = e.clientX, y0 = e.clientY, o = { x: sh.x, y: sh.y, w: sh.w, h: sh.h };
    let moved = false;
    const move = ev => {
      const dx = (ev.clientX - x0) / this.k, dy = (ev.clientY - y0) / this.k;
      if (!moved && Math.abs(dx) + Math.abs(dy) < 3 / this.k) return;
      if (!moved) { this.snap(); moved = true; }
      if (resize) { sh.w = Math.max(10, Math.round(o.w + dx)); sh.h = Math.max(10, Math.round(o.h + dy)); }
      else { sh.x = Math.round(o.x + dx); sh.y = Math.round(o.y + dy); }
      Object.assign(el.style, { left: sh.x + 'px', top: sh.y + 'px', width: sh.w + 'px', height: sh.h + 'px' });
    };
    const up = () => {
      document.removeEventListener('pointermove', move);
      document.removeEventListener('pointerup', up);
      if (moved) { this.touch(); this.thumb(this.cur); }
    };
    document.addEventListener('pointermove', move);
    document.addEventListener('pointerup', up);
    e.preventDefault();
  }

  /* a double click edits the shape's text (a shape without text gets some) */
  dbl(e) {
    const el = e.target.closest('.shp:not(.under)');
    if (!el) return;
    const i = +el.dataset.i, sh = this.slide.shapes[i];
    if (sh.img || sh.table || sh.geom === 'line') return;
    this.startText(i);
  }
  startText(i) {
    this.commitText();
    this.sel = i;
    const sh = this.slide.shapes[i];
    this.snap();
    let el = this.inner.querySelector('.shp[data-i="' + i + '"]');
    let t = el.querySelector('.shp-text');
    if (!t) {
      t = h('div', { class: 'shp-text a-' + (sh.anchor || 'ctr') });
      t.style.fontSize = (sh.size || 18) + 'pt';
      t.innerHTML = '<p><br></p>';
      el.appendChild(t);
    }
    t.contentEditable = 'true';
    t.addEventListener('input', () => this.touch());
    this.editing = { el, t, i };
    this.mark();
    t.focus();
    const r = document.createRange();
    r.selectNodeContents(t);
    r.collapse(false);
    getSelection().removeAllRanges();
    getSelection().addRange(r);
  }
  commitText() {
    const ed = this.editing;
    if (!ed) return;
    this.editing = null;
    ed.t.contentEditable = 'false';
    const sh = this.slide.shapes[ed.i];
    if (sh) {
      const blocks = Rich.flat(ed.t).filter(b => !b.rows);
      // sizes are kept against the shape's own size
      sh.paras = blocks.length && blocks.some(b => (b.runs || []).length) ? blocks : [];
      this.thumb(this.cur);
    }
  }

  fontSize(pt) {
    if (!this.editing) return;
    document.execCommand('fontSize', false, '7');
    for (const f of this.editing.t.querySelectorAll('font[size="7"]')) { f.removeAttribute('size'); f.style.fontSize = pt + 'pt'; }
    this.touch();
  }

  setFill(c) {
    const sh = this.slide.shapes[this.sel];
    if (!sh || sh.img) return;
    this.snap();
    sh.fill = c;
    this.touch();
    this.paint();
    this.thumb(this.cur);
  }

  center(w, hh) { return { x: Math.round((this.d.w - w) / 2), y: Math.round((this.d.h - hh) / 2), w, h: hh }; }
  addText() {
    this.snap();
    this.slide.shapes.push(Object.assign(this.center(this.d.w * 0.5, this.d.h * 0.15), { geom: null, paras: [{ runs: [{ t: tx('text'), f: {} }] }], anchor: 't', size: 24 }));
    this.after(this.slide.shapes.length - 1);
  }
  addShape() {
    this.snap();
    this.slide.shapes.push(Object.assign(this.center(this.d.w * 0.25, this.d.h * 0.25), { geom: 'rect', fill: '#4472c4', line: { color: '#2f528f', w: 1.33 }, paras: [], anchor: 'ctr', size: 18 }));
    this.after(this.slide.shapes.length - 1);
  }
  addImage() {
    const inp = h('input', { type: 'file', accept: 'image/png,image/jpeg,image/gif,image/svg+xml' });
    inp.addEventListener('change', () => {
      const f = inp.files[0];
      if (!f) return;
      const r = new FileReader();
      r.onload = () => {
        const img = new Image();
        img.onload = () => {
          const k = Math.min(1, this.d.w * 0.6 / img.naturalWidth, this.d.h * 0.6 / img.naturalHeight);
          this.snap();
          this.slide.shapes.push(Object.assign(this.center(Math.round(img.naturalWidth * k), Math.round(img.naturalHeight * k)), { img: r.result }));
          this.after(this.slide.shapes.length - 1);
        };
        img.src = r.result;
      };
      r.readAsDataURL(f);
    });
    inp.click();
  }
  after(sel) {
    this.sel = sel;
    this.touch();
    this.paint();
    this.thumb(this.cur);
  }
  removeShape() {
    if (this.sel < 0 || !this.slide.shapes[this.sel]) return;
    this.snap();
    this.slide.shapes.splice(this.sel, 1);
    this.after(-1);
  }

  addSlide() {
    this.snap();
    const title = { x: Math.round(this.d.w * 0.06), y: Math.round(this.d.h * 0.05), w: Math.round(this.d.w * 0.88), h: Math.round(this.d.h * 0.17), ph: 'title', anchor: 'ctr', size: 40, paras: [{ runs: [{ t: tx('title'), f: {} }] }] };
    const body = { x: Math.round(this.d.w * 0.06), y: Math.round(this.d.h * 0.26), w: Math.round(this.d.w * 0.88), h: Math.round(this.d.h * 0.64), ph: 'body', anchor: 't', size: 24, paras: [{ runs: [{ t: tx('text'), f: {} }], list: 'ul', level: 0 }] };
    this.d.slides.splice(this.cur + 1, 0, { bg: this.slide ? this.slide.bg : null, shapes: [title, body] });
    this.touch();
    this.thumbs();
    this.pick(this.cur + 1);
  }
  dupSlide() {
    this.snap();
    this.d.slides.splice(this.cur + 1, 0, JSON.parse(JSON.stringify(this.slide)));
    this.touch();
    this.thumbs();
    this.pick(this.cur + 1);
  }
  removeSlide() {
    if (this.d.slides.length < 2) { toast(tx('a presentation keeps at least one slide'), 'warn'); return; }
    this.snap();
    this.d.slides.splice(this.cur, 1);
    this.touch();
    this.thumbs();
    this.pick(Math.min(this.cur, this.d.slides.length - 1));
  }
  moveSlide(by) {
    const to = this.cur + by;
    if (to < 0 || to >= this.d.slides.length) return;
    this.snap();
    const [s] = this.d.slides.splice(this.cur, 1);
    this.d.slides.splice(to, 0, s);
    this.touch();
    this.thumbs();
    this.pick(to);
  }

  key(e) {
    if (e.defaultPrevented) return;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.altKey && (e.key === 'z' || e.key === 'Z')) { if (this.editing) return; e.preventDefault(); if (e.shiftKey) this.redo(); else this.undo(); return; }
    if (mod && !e.altKey && (e.key === 'y' || e.key === 'Y')) { if (this.editing) return; e.preventDefault(); this.redo(); return; }
    if (this.editing) { if (e.key === 'Escape') { e.preventDefault(); this.commitText(); this.paint(); } return; }
    if (isTyping(e) || dlgOpen) return;
    const sh = this.slide.shapes[this.sel];
    if ((e.key === 'Delete' || e.key === 'Backspace') && sh) { e.preventDefault(); this.removeShape(); }
    else if (e.key === 'Enter' && sh) { e.preventDefault(); this.startText(this.sel); }
    else if (sh && e.key.startsWith('Arrow')) {
      e.preventDefault();
      this.snap();
      const step = e.shiftKey ? 10 : 1;
      if (e.key === 'ArrowLeft') sh.x -= step; else if (e.key === 'ArrowRight') sh.x += step; else if (e.key === 'ArrowUp') sh.y -= step; else sh.y += step;
      this.touch();
      this.paint();
    } else if (!sh && (e.key === 'PageDown' || e.key === 'ArrowDown')) { e.preventDefault(); if (this.cur < this.d.slides.length - 1) this.pick(this.cur + 1); }
    else if (!sh && (e.key === 'PageUp' || e.key === 'ArrowUp')) { e.preventDefault(); if (this.cur > 0) this.pick(this.cur - 1); }
  }

  destroy() {
    document.removeEventListener('keydown', this.onKey);
    this.ro.disconnect();
    this.pics.done();
  }
}

/* ---- for the readers ---- */
/* colours: '#rrggbb' from 'rrggbb', with lumMod/lumOff style changes as fractions */
function shade(hex, lumMod, lumOff, tint, shadeBy) {
  let r = parseInt(hex.slice(1, 3), 16) / 255, g = parseInt(hex.slice(3, 5), 16) / 255, b = parseInt(hex.slice(5, 7), 16) / 255;
  if (tint !== undefined) { r = 1 - (1 - r) * tint; g = 1 - (1 - g) * tint; b = 1 - (1 - b) * tint; }
  if (shadeBy !== undefined) { r *= shadeBy; g *= shadeBy; b *= shadeBy; }
  if (lumMod !== undefined || lumOff !== undefined) {
    const mx = Math.max(r, g, b), mn = Math.min(r, g, b);
    let H = 0, S = 0, L = (mx + mn) / 2;
    if (mx !== mn) {
      const d = mx - mn;
      S = L > 0.5 ? d / (2 - mx - mn) : d / (mx + mn);
      H = mx === r ? (g - b) / d + (g < b ? 6 : 0) : mx === g ? (b - r) / d + 2 : (r - g) / d + 4;
      H /= 6;
    }
    L = Math.min(1, Math.max(0, L * (lumMod === undefined ? 1 : lumMod) + (lumOff || 0)));
    const f = (p, q, t) => { if (t < 0) t += 1; if (t > 1) t -= 1; return t < 1 / 6 ? p + (q - p) * 6 * t : t < 1 / 2 ? q : t < 2 / 3 ? p + (q - p) * (2 / 3 - t) * 6 : p; };
    if (S === 0) r = g = b = L;
    else { const q = L < 0.5 ? L * (1 + S) : L + S - L * S, p = 2 * L - q; r = f(p, q, H + 1 / 3); g = f(p, q, H); b = f(p, q, H - 1 / 3); }
  }
  return '#' + [r, g, b].map(x => Math.round(Math.min(1, Math.max(0, x)) * 255).toString(16).padStart(2, '0')).join('');
}

/* the text of a deck, for a slide's title in lists */
function slideTitle(s) {
  const t = s.shapes.find(x => x.ph === 'title' || x.ph === 'ctrTitle') || s.shapes.find(x => x.paras && x.paras.length);
  return t ? t.paras.map(p => (p.runs || []).map(r => r.t || '').join('')).join(' ').trim() : '';
}

return { SlideView, SlideEditor, Pics, shade, slideTitle };
});
