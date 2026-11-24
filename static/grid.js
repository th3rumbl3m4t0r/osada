// grid.js: the tables and pickers of the admin pages, after the table app
// (apps1 git: table): every table has the same toolbar (▶ run, filter, the
// count; extras and ⚙ on the right), a filter builder of OR-groups of
// AND-conditions that folds away under "filter", a quick-filter row under
// the column names (Enter or ▶ moves its entries into the builder), headers
// that sort, a column chooser and pages. Anything one picks from what users
// make (users, groups, OUs, nodes) is a picker: type for matches, or 🔍 for
// all of them in such a table.

export function el(tag, attrs, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else if (k === 'class') e.className = v;
    else if (k === 'value') e.value = v;
    else if (k === 'checked') e.checked = true;
    else if (k === 'selected') e.selected = true;
    else if (v === true) e.setAttribute(k, '');
    else e.setAttribute(k, v);
  }
  for (const kid of kids.flat(Infinity)) {
    if (kid === null || kid === undefined || kid === false) continue;
    e.append(kid instanceof Node ? kid : String(kid));
  }
  return e;
}

function LS(key, value) {
  try {
    if (value === undefined) return JSON.parse(localStorage.getItem(key));
    localStorage.setItem(key, JSON.stringify(value));
  } catch (e) { /* private mode */ }
  return null;
}

export function modal(title, body, buttons, opts) {
  opts = opts || {};
  const ovl = el('div', { id: 'ovl' });
  const close = () => ovl.remove();
  const box = el('div', { class: 'modal' + (opts.cls ? ' ' + opts.cls : '') },
    el('div', { class: 'mhead' }, el('span', { class: 'grip' }, '::'), title, el('span', { class: 'x', onclick: close }, '✕')),
    el('div', { class: 'mbody' }, body),
    buttons && buttons.length ? el('div', { class: 'mfoot' }, buttons) : null);
  ovl.append(box);
  ovl.addEventListener('mousedown', e => { if (e.target === ovl) close(); });
  document.body.append(ovl);
  return { close, node: box };
}

// ---- every table's cells (Round 19) -------------------------------------------------
// One rule for every cell of every table.grid, the grids' and any other: at most the cell's
// width (340px, app.css), one line; what does not fit ends in an accent "…". Hovering the cell
// shows all of it; clicking the "…" opens it in a small read-only editor (copy, wrap). Done by
// one observer of the page, so a table drawn anywhere gets it.

const FULL = new WeakMap();   // a cell → the whole text, when its column says (column.full)

/// The text of a cell as a person reads it (JSON pretty), without its "…".
function cellText(cell) {
  let t;
  if (FULL.has(cell)) t = FULL.get(cell);
  else {
    const c = cell.cloneNode(true);
    c.className = 'cellcopy';
    for (const m of c.querySelectorAll('.more')) m.remove();
    // fields show their value (a clone has not got it), boxes nothing
    const mine = cell.querySelectorAll('input, select, textarea'), copies = c.querySelectorAll('input, select, textarea');
    copies.forEach((x, i) => x.replaceWith(document.createTextNode(['checkbox', 'radio', 'button'].includes(mine[i].type) ? '' : mine[i].value || '')));
    // innerText (a line where a block ends) needs it laid out: off-screen, a moment
    c.style.cssText = 'position:absolute;left:-99999px;top:0;white-space:pre-wrap;max-width:none;overflow:visible';
    document.body.append(c);
    t = c.innerText;
    c.remove();
  }
  t = String(t ?? '').replace(/\n{3,}/g, '\n\n').trim();
  if (/^[[{]/.test(t)) { try { t = JSON.stringify(JSON.parse(t), null, 2); } catch (e) { /* not JSON */ } }
  return t;
}

/// The column's name over a cell (its th), for the editor's title.
function cellTitle(cell) {
  const td = cell.closest('td'), table = cell.closest('table');
  if (!td || !table) return 'the whole text';
  const row = td.parentElement;
  const th = row && row.querySelector('th');
  if (th && th !== td && row.contains(th)) return th.textContent.trim() || 'the whole text';
  const head = table.querySelector('thead tr');
  const hc = head && head.children[td.cellIndex];
  return (hc && hc.textContent.replace(/[▲▼]/g, '').trim()) || 'the whole text';
}

/// The small editor: all of a cell's text, read-only, with copy and wrap.
export function textModal(title, text) {
  const lines = text.split('\n');
  const ta = el('textarea', { class: 'celltext', readonly: true, spellcheck: 'false', wrap: 'soft', rows: Math.min(24, Math.max(4, lines.length + 1)) });
  ta.value = text;
  const info = el('span', { class: 'dim' }, `${lines.length} line${lines.length === 1 ? '' : 's'} · ${text.length.toLocaleString()} characters`);
  const wrap = el('input', { type: 'checkbox', checked: true, onchange: e => { ta.wrap = e.target.checked ? 'soft' : 'off'; ta.classList.toggle('nowrap', !e.target.checked); } });
  const copy = el('button', { type: 'button', onclick: async () => {
    try { await navigator.clipboard.writeText(text); } catch (e) { ta.select(); document.execCommand('copy'); }
    copy.textContent = 'copied';
    setTimeout(() => { copy.textContent = 'copy'; }, 1200);
  } }, 'copy');
  const m = modal(title, el('div', { class: 'stack' }, ta, el('div', { class: 'row' }, info, el('label', { class: 'row' }, wrap, 'wrap lines'))),
    [copy, el('button', { type: 'button', class: 'primary', onclick: () => m.close() }, 'close')], { cls: 'textmodal' });
  setTimeout(() => { ta.focus(); ta.setSelectionRange(0, 0); ta.scrollTop = 0; }, 0);
  return m;
}

// cells that hold a picker keep their pop-ups (nothing cut off), as does a row's message over all columns
const FREE = '.refpick, .mpick, .dd, textarea';

/// Puts a cell's content into a .cell box (once); its box, if it has one.
function wrapCell(td) {
  if (td.classList.contains('pick') || td.hasAttribute('colspan') || (td.parentElement && td.parentElement.classList.contains('qrow'))) return null;
  if (td.childNodes.length === 1 && td.firstChild.nodeType === 1 && td.firstChild.classList.contains('cell')) return td.firstChild;
  if (td.querySelector(FREE)) { td.classList.add('free'); return null; }
  const box = document.createElement('div');
  box.className = 'cell';
  box.append(...td.childNodes);
  td.append(box);
  return box;
}

function wrapCells(table) {
  for (const td of table.querySelectorAll('td')) if (td.closest('table') === table) wrapCell(td);
}

/// Marks the cells that do not fit (reads first, then writes: one layout).
function measure(table) {
  if (!table.isConnected) return;
  const cells = [...table.querySelectorAll('td > .cell')].filter(c => c.closest('table') === table);
  const over = cells.map(c => c.clientWidth > 0 && c.scrollWidth > c.clientWidth + 1);
  cells.forEach((c, i) => {
    const more = c.querySelector(':scope > .more');
    if (over[i] && !more) {
      c.classList.add('over');
      // a title that says the same as the hover would, twice
      const all = cellText(c);
      for (const t of c.querySelectorAll('[title]')) if (t.getAttribute('title').trim() === all) t.removeAttribute('title');
      c.append(el('span', { class: 'more', title: 'all of it', onclick: e => { e.stopPropagation(); e.preventDefault(); hideTip(); textModal(cellTitle(c), cellText(c)); } }, '…'));
    } else if (!over[i] && more) {
      c.classList.remove('over');
      more.remove();
    }
  });
}

const seen = new WeakSet();
const pending = new Set();
let frame = 0;
const resized = typeof ResizeObserver === 'function' ? new ResizeObserver(entries => { for (const e of entries) later(e.target); }) : null;
function later(table) {
  pending.add(table);
  if (!frame) frame = requestAnimationFrame(() => {
    frame = 0;
    const list = [...pending];
    pending.clear();
    for (const t of list) { if (t.isConnected) { wrapCells(t); measure(t); } }
  });
}

/// A table's cells by the rule above (the observer does it for every table.grid; this, at once).
export function fitTable(table) {
  wrapCells(table);
  if (resized && !seen.has(table)) { seen.add(table); resized.observe(table); }
  later(table);
}

// the hover: all of a cut cell, at once, under it
let tip = null, tipFor = null, tipTimer = 0;
function hideTip() { clearTimeout(tipTimer); tipFor = null; if (tip) tip.hidden = true; }
function showTip(cell) {
  if (!tip) { tip = el('div', { class: 'celltip', hidden: true }); document.body.append(tip); }
  tip.textContent = cellText(cell);
  const r = cell.getBoundingClientRect();
  tip.hidden = false;
  const w = tip.offsetWidth, h = tip.offsetHeight;
  const left = Math.max(4, Math.min(r.left, window.innerWidth - w - 8));
  const top = r.bottom + h + 6 < window.innerHeight ? r.bottom + 2 : Math.max(4, r.top - h - 2);
  tip.style.left = `${left + window.scrollX}px`;
  tip.style.top = `${top + window.scrollY}px`;
}

if (typeof document !== 'undefined' && typeof MutationObserver === 'function') {
  const watch = () => {
    new MutationObserver(list => {
      for (const m of list) {
        for (const n of m.addedNodes) {
          if (n.nodeType !== 1) continue;
          if (n.classList.contains('more') || n.classList.contains('celltip') || n.classList.contains('cellcopy')) continue;
          const t = n.closest('table.grid');
          if (t) { if (!seen.has(t) && resized) { seen.add(t); resized.observe(t); } later(t); }
          if (n.querySelectorAll) for (const x of n.querySelectorAll('table.grid')) fitTable(x);
        }
      }
    }).observe(document.body, { childList: true, subtree: true });
    document.addEventListener('mouseover', e => {
      const c = e.target.closest && e.target.closest('td > .cell.over');
      if (c === tipFor) return;
      hideTip();
      if (!c || e.target.closest('.more')) return;
      tipFor = c;
      tipTimer = setTimeout(() => { if (tipFor === c && c.isConnected) showTip(c); }, 250);
    });
    document.addEventListener('scroll', hideTip, true);
    for (const t of document.querySelectorAll('table.grid')) fitTable(t);
  };
  if (document.body) watch(); else document.addEventListener('DOMContentLoaded', watch);
}

export function opsFor(type) {
  switch (type) {
    case 'number': return ['=', '!=', '<', '<=', '>', '>=', 'between'];
    case 'date': return ['after', 'before', 'between'];
    case 'enum': return ['=', '!=', 'contains'];
    case 'bool': return ['=', '!='];
    default: return ['contains', '=', '!=', 'startswith', 'endswith', 'regexp'];
  }
}

function pad(n) { return String(n).padStart(2, '0'); }
function inputFromMs(ms) { const d = new Date(ms); return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`; }
function msFromInput(s) { const t = Date.parse(s); return isNaN(t) ? null : t; }
function dayRange(s) {
  const m = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(s.trim());
  if (!m) return null;
  const a = new Date(+m[1], +m[2] - 1, +m[3]).getTime();
  return [a, a + 86399999];
}

/// Whether one condition holds for a cell's value (an array: for any of
/// it; "!=": for none of it).
function holds(line, raw, type) {
  const list = Array.isArray(raw) ? raw : [raw];
  const v0 = line.vals[0], v1 = line.vals[1];
  if (type === 'number' || type === 'date') {
    const nums = list.map(Number).filter(n => !isNaN(n));
    const a = Number(v0), b = Number(v1);
    switch (line.op) {
      case '=': return nums.some(n => n === a);
      case '!=': return !nums.some(n => n === a);
      case '<': case 'before': return nums.some(n => n < a);
      case '<=': return nums.some(n => n <= a);
      case '>': case 'after': return nums.some(n => n > a);
      case '>=': return nums.some(n => n >= a);
      case 'between': return nums.some(n => n >= a && n <= b);
      default: return true;
    }
  }
  if (type === 'bool') { const want = v0 === true || v0 === 'true'; return line.op === '!=' ? !list.some(x => !!x === want) : list.some(x => !!x === want); }
  const texts = list.map(x => String(x ?? '').toLowerCase());
  const w = String(v0 ?? '').toLowerCase();
  switch (line.op) {
    case 'contains': return texts.some(t => t.includes(w));
    case '=': return texts.some(t => t === w);
    case '!=': return !texts.some(t => t === w);
    case 'startswith': return texts.some(t => t.startsWith(w));
    case 'endswith': return texts.some(t => t.endsWith(w));
    case 'regexp': try { const re = new RegExp(String(v0), 'i'); return list.some(x => re.test(String(x ?? ''))); } catch (e) { return false; }
    default: return true;
  }
}

export function matches(row, filter, cols) {
  if (!filter) return true;
  return filter.groups.some(g => g.every(line => {
    const c = cols[line.col];
    return !c || holds(line, c.value(row), c.type);
  }));
}

/// A table. opts: id (remembers sort, columns, page size), columns:
/// [{ key, label, type: text|number|date|enum|bool, options, value(row),
/// cell(row) → node or text, hidden }], onRow(row, event), extras: [nodes
/// on the right], filter: initial {groups}, empty: text when no rows; and
/// where the rows come from, one of:
/// - rows: all of them, here (small tables: nodes, a few domains);
/// - fetch({filter, sort, desc, offset, limit}) → {rows, total}: the node
///   filters, sorts and pages (a million rows);
/// - cursor({filter, before, limit}) → {rows, next}: the node pages on from
///   a cursor, newest first (logs).
/// In the last two, the page shows what the node sends and filters nothing
/// itself: it only ever has one page.
/// With select and key(row), rows can be picked (a first column of boxes,
/// the header's for the page); picks stay over pages and filters.
export function grid(opts) {
  const cols = {};
  for (const c of opts.columns) {
    cols[c.key] = { type: 'text', value: r => r[c.key], ...c };
  }
  const saved = LS('grid:' + opts.id) || {};
  const st = {
    groups: opts.filter ? structuredClone(opts.filter.groups) : [],
    showBuilder: !!(opts.filter && opts.filter.groups.length),
    sort: saved.sort || opts.sort || null,
    dir: saved.dir || opts.dir || 'asc',
    page: 0,
    shown: (saved.shown || opts.columns.filter(c => !c.hidden).map(c => c.key)).filter(k => cols[k]),
    pageSize: saved.pageSize || opts.pageSize || 50,
  };
  const remember = () => LS('grid:' + opts.id, { sort: st.sort, dir: st.dir, shown: st.shown, pageSize: st.pageSize });
  let rows = opts.rows || [];
  let shownRows = [];
  let total = 0;
  const cursors = [null];
  let next = null;
  const remote = !!(opts.fetch || opts.cursor);
  const quick = {};
  const picked = new Set();

  const count = el('span', { class: 'dim' });
  const filterBtn = el('button', { type: 'button', onclick: () => {
    st.showBuilder = !st.showBuilder;
    if (st.showBuilder && !st.groups.length) st.groups = [[]];
    drawBuilder();
    filterBtn.classList.toggle('on', st.showBuilder);
  } }, 'filter');
  const toolbar = el('div', { class: 'toolbar' },
    el('button', { class: 'primary', type: 'button', onclick: () => run() }, '▶ run'),
    filterBtn, count,
    el('div', { class: 'right' }, opts.extras || [], el('button', { type: 'button', title: 'columns and page size', onclick: gear }, '⚙')));
  const builderHost = el('div', {});
  const tableHost = el('div', { class: 'gridscroll' });
  const pager = el('div', { class: 'pager' });
  const node = el('div', { class: 'gridbox' }, toolbar, builderHost, tableHost, pager);

  function quickInput(c) {
    if (c.type === 'enum' || c.type === 'bool') {
      const options = c.type === 'bool' ? ['true', 'false'] : (c.options || []);
      const s = el('select', { onchange: () => { quick[c.key] = s.value; run(); } }, el('option', { value: '' }, ''), options.map(o => el('option', { value: o }, o)));
      s.value = quick[c.key] || '';
      return s;
    }
    const i = el('input', {
      type: 'text', value: quick[c.key] || '', placeholder: c.type === 'date' ? 'YYYY-MM-DD' : '',
      oninput: () => { quick[c.key] = i.value; },
      onkeydown: e => { if (e.key === 'Enter') run(); },
    });
    return i;
  }

  function quickToLines() {
    const lines = [];
    for (const [key, raw] of Object.entries(quick)) {
      const v = (raw || '').trim();
      const c = cols[key];
      if (!v || !c) continue;
      if (c.type === 'number') { const n = parseFloat(v); if (!isNaN(n)) lines.push({ col: key, op: '=', vals: [n] }); }
      else if (c.type === 'date') { const r = dayRange(v); if (r) lines.push({ col: key, op: 'between', vals: r }); }
      else if (c.type === 'enum' || c.type === 'bool') lines.push({ col: key, op: '=', vals: [v] });
      else lines.push({ col: key, op: 'contains', vals: [v] });
    }
    return lines;
  }

  function currentFilter() {
    const groups = st.groups.map(g => g.filter(l => l.col && l.vals.length && l.vals.every(x => x !== null && x !== ''))).filter(g => g.length);
    return groups.length ? { groups } : null;
  }

  function drawBuilder() {
    builderHost.replaceChildren();
    if (!st.showBuilder) return;
    const box = el('div', { class: 'fbuilder' });
    st.groups.forEach((g, gi) => {
      if (gi > 0) box.append(el('div', { class: 'orlbl' }, '— or —'));
      const gbox = el('div', { class: 'fgroup' }, el('span', { class: 'gtag' }, 'all of (and)'));
      g.forEach((line, li) => gbox.append(builderLine(g, line, li)));
      gbox.append(el('div', { class: 'fline' },
        el('button', { type: 'button', onclick: () => { g.push({ col: '', op: '', vals: [] }); drawBuilder(); } }, '+ and'),
        g.length === 0 && st.groups.length > 1 ? el('button', { type: 'button', class: 'danger', onclick: () => { st.groups.splice(gi, 1); drawBuilder(); } }, 'remove group') : null));
      box.append(gbox);
    });
    box.append(el('div', { class: 'fline' },
      el('button', { type: 'button', onclick: () => { st.groups.push([{ col: '', op: '', vals: [] }]); drawBuilder(); } }, '+ or group'),
      el('button', { type: 'button', onclick: () => { st.groups = []; st.showBuilder = false; filterBtn.classList.remove('on'); run(); } }, 'clear filter'),
      el('button', { type: 'button', class: 'primary', onclick: () => run() }, '▶ run')));
    builderHost.append(box);
  }

  function builderLine(g, line, li) {
    const colSel = el('select', {}, el('option', { value: '' }, '· column ·'),
      opts.columns.map(c => el('option', { value: c.key, selected: line.col === c.key }, c.label)));
    const opHost = el('span', {});
    const valHost = el('span', {});
    function valInput(c, idx) {
      const set = v => { line.vals[idx] = v; };
      if (c.type === 'date') {
        const i = el('input', { type: 'datetime-local', value: line.vals[idx] != null ? inputFromMs(line.vals[idx]) : '' });
        i.onchange = () => set(msFromInput(i.value));
        return i;
      }
      if ((c.type === 'enum' && line.op !== 'contains') || c.type === 'bool') {
        const options = c.type === 'bool' ? ['true', 'false'] : (c.options || []);
        const s = el('select', {}, options.map(o => el('option', { value: o, selected: String(line.vals[idx]) === o }, o)));
        set(s.value);
        s.onchange = () => set(s.value);
        return s;
      }
      const i = el('input', { type: 'text', class: 'val', value: line.vals[idx] != null ? line.vals[idx] : '' });
      i.oninput = () => {
        if (c.type === 'number') { const n = parseFloat(i.value); set(isNaN(n) ? null : n); } else set(i.value);
      };
      i.onkeydown = e => { if (e.key === 'Enter') run(); };
      return i;
    }
    function drawVals(c) {
      valHost.replaceChildren();
      if (line.op === 'between') valHost.append(valInput(c, 0), ' and ', valInput(c, 1));
      else valHost.append(valInput(c, 0));
    }
    function drawOps() {
      opHost.replaceChildren();
      valHost.replaceChildren();
      const c = cols[colSel.value];
      if (!c) return;
      const ops = opsFor(c.type);
      if (!ops.includes(line.op)) { line.op = ops[0]; line.vals = []; }
      const opSel = el('select', {}, ops.map(o => el('option', { value: o, selected: line.op === o }, o)));
      opSel.onchange = () => { line.op = opSel.value; line.vals = []; drawVals(c); };
      opHost.append(opSel);
      drawVals(c);
    }
    colSel.onchange = () => { line.col = colSel.value; line.vals = []; drawOps(); };
    if (line.col) drawOps();
    return el('div', { class: 'fline' }, colSel, opHost, valHost,
      el('button', { type: 'button', class: 'danger', title: 'remove condition', onclick: () => { g.splice(li, 1); drawBuilder(); } }, '✕'));
  }

  async function run() {
    const ql = quickToLines();
    if (ql.length) {
      if (!st.groups.length) st.groups = [[]];
      for (const g of st.groups) g.push(...ql.map(l => structuredClone(l)));
      for (const k of Object.keys(quick)) delete quick[k];
      st.showBuilder = true;
      filterBtn.classList.add('on');
    }
    st.page = 0;
    cursors.length = 1;
    drawBuilder();
    await load();
  }

  async function load() {
    const f = currentFilter();
    if (remote) {
      count.textContent = 'loading…';
      try {
        if (opts.fetch) {
          const r = await opts.fetch({ filter: f, sort: st.sort, desc: st.dir === 'desc', offset: st.page * st.pageSize, limit: st.pageSize });
          shownRows = r.rows;
          total = r.total;
        } else {
          const r = await opts.cursor({ filter: f, before: cursors[st.page], limit: st.pageSize });
          shownRows = r.rows;
          next = r.next || null;
        }
      } catch (err) { count.textContent = ''; tableHost.replaceChildren(el('p', { class: 'bad' }, err.message)); return; }
      draw();
      return;
    }
    shownRows = rows.filter(r => matches(r, f, cols));
    if (st.sort && cols[st.sort]) {
      const c = cols[st.sort];
      const key = r => { const v = c.value(r); return Array.isArray(v) ? v.join(', ') : v; };
      const num = c.type === 'number' || c.type === 'date';
      shownRows.sort((a, b) => {
        const x = key(a), y = key(b);
        const r = num ? (Number(x) || 0) - (Number(y) || 0) : String(x ?? '').localeCompare(String(y ?? ''), undefined, { numeric: true });
        return st.dir === 'asc' ? r : -r;
      });
    }
    draw();
  }

  function sortClick(c) {
    if (opts.cursor) return;
    if (st.sort !== c.key) { st.sort = c.key; st.dir = 'asc'; } else if (st.dir === 'asc') st.dir = 'desc'; else st.sort = null;
    st.page = 0;
    remember();
    load();
  }

  function draw() {
    const vis = st.shown.map(k => cols[k]).filter(Boolean);
    let slice, pages, prev, nextPage;
    if (opts.fetch) {
      pages = Math.max(1, Math.ceil(total / st.pageSize));
      slice = shownRows;
      const from = total ? st.page * st.pageSize + 1 : 0;
      count.textContent = `${from}–${st.page * st.pageSize + slice.length} of ${total.toLocaleString()} rows` + (picked.size ? ` · ${picked.size.toLocaleString()} picked` : '');
      prev = st.page > 0;
      nextPage = st.page < pages - 1;
    } else if (opts.cursor) {
      slice = shownRows;
      pages = null;
      count.textContent = `page ${st.page + 1}: ${slice.length} entries${next ? ', more after' : ''}`;
      prev = st.page > 0;
      nextPage = !!next;
    } else {
      pages = Math.max(1, Math.ceil(shownRows.length / st.pageSize));
      st.page = Math.min(st.page, pages - 1);
      slice = shownRows.slice(st.page * st.pageSize, (st.page + 1) * st.pageSize);
      count.textContent = `${shownRows.length} of ${rows.length} rows`;
      prev = st.page > 0;
      nextPage = st.page < pages - 1;
    }
    const pickCell = row => {
      const k = String(opts.key(row));
      return el('td', { class: 'pick' }, el('input', { type: 'checkbox', checked: picked.has(k), onchange: e => { if (e.target.checked) picked.add(k); else picked.delete(k); draw(); } }));
    };
    const allHere = !!opts.select && slice.length > 0 && slice.every(r => picked.has(String(opts.key(r))));
    const thead = el('thead', {},
      el('tr', {}, opts.select ? el('th', { class: 'pick', title: 'pick all on this page' }, el('input', { type: 'checkbox', checked: allHere, onchange: e => {
        for (const r of slice) { const k = String(opts.key(r)); if (e.target.checked) picked.add(k); else picked.delete(k); }
        draw();
      } })) : null, vis.map(c => el('th', { onclick: () => sortClick(c), title: opts.cursor ? 'newest first' : 'sort' }, c.label,
        st.sort === c.key ? el('span', { class: 'arrow' }, st.dir === 'asc' ? ' ▲' : ' ▼') : ''))),
      el('tr', { class: 'qrow' }, opts.select ? el('td', {}) : null, vis.map(c => el('td', {}, quickInput(c)))));
    const tbody = el('tbody', {}, slice.length ? slice.map(row => el('tr', {
      class: 'data' + (opts.onRow ? ' click' : '') + (opts.rowClass ? ' ' + (opts.rowClass(row) || '') : ''),
      onclick: e => { if (opts.onRow && !e.target.closest('a, button, input, select, label')) opts.onRow(row, e); },
    }, opts.select ? pickCell(row) : null, vis.map(c => {
      const td = el('td', {}, c.cell ? c.cell(row) : fmt(c, c.value(row)));
      if (c.full) { const box = wrapCell(td); if (box) FULL.set(box, c.full(row)); }
      return td;
    }))) : el('tr', {}, el('td', { colspan: vis.length + (opts.select ? 1 : 0), class: 'dim' }, opts.empty || 'nothing')));
    const table = el('table', { class: 'grid' }, thead, tbody);
    tableHost.replaceChildren(table);
    fitTable(table);
    const turn = d => {
      if (opts.cursor && d > 0) cursors[st.page + 1] = next;
      st.page += d;
      if (remote) load(); else draw();
    };
    pager.replaceChildren(
      el('button', { type: 'button', disabled: !prev, onclick: () => turn(-1) }, '◀ prev'),
      el('span', { class: 'dim' }, pages ? `page ${st.page + 1} of ${pages.toLocaleString()}` : `page ${st.page + 1}`),
      el('button', { type: 'button', disabled: !nextPage, onclick: () => turn(1) }, 'next ▶'));
    pager.hidden = !prev && !nextPage;
  }

  function gear() {
    const order = [...st.shown, ...opts.columns.map(c => c.key).filter(k => !st.shown.includes(k))];
    const on = new Set(st.shown);
    const list = el('table', { class: 'grid' }, order.map(k => el('tr', {},
      el('td', {}, el('input', { type: 'checkbox', checked: on.has(k), onchange: e => { if (e.target.checked) on.add(k); else on.delete(k); } })),
      el('td', {}, cols[k].label))));
    const size = el('input', { type: 'number', min: 5, max: 1000, value: st.pageSize });
    const m = modal('columns and page size', el('div', {}, list, el('div', { class: 'fline' }, el('span', { class: 'lbl' }, 'rows per page'), size)), [
      el('button', { type: 'button', onclick: () => { st.shown = opts.columns.filter(c => !c.hidden).map(c => c.key); st.pageSize = 50; remember(); m.close(); draw(); } }, 'defaults'),
      el('button', { type: 'button', class: 'primary', onclick: () => { st.shown = order.filter(k => on.has(k)); st.pageSize = Math.max(5, Number(size.value) || 50); remember(); m.close(); draw(); } }, 'apply'),
    ]);
  }

  drawBuilder();
  filterBtn.classList.toggle('on', st.showBuilder);
  load();
  return {
    node,
    run,
    refresh(newRows) { if (newRows) rows = newRows; st.page = 0; cursors.length = 1; load(); },
    setFilter(groups) { st.groups = structuredClone(groups); st.showBuilder = groups.length > 0; filterBtn.classList.toggle('on', st.showBuilder); run(); },
    picked: () => [...picked],
    unpick() { picked.clear(); draw(); },
    /// The query the table shows now (filter and sort), for work on all of it.
    query: () => ({ filter: currentFilter(), sort: st.sort, desc: st.dir === 'desc' }),
    total: () => total,
  };
}

function fmt(c, v) {
  if (v === null || v === undefined) return '';
  if (c.type === 'date') return new Date(v).toLocaleString();
  if (Array.isArray(v)) return v.join(', ');
  return String(v);
}

/// A picker over things users make. opts: title, key(row), label(row),
/// columns (for the 🔍 table), placeholder, onpick(row or null), empty:
/// label of "none" (allows none); and where the things come from, either
/// items() → all rows (a few: nodes) with value (the key picked first), or,
/// for a million, lookup(text) → up to 15 matches (asked once typing
/// pauses) and fetch (the 🔍 table's, as in grid), with initial: a row.
export function picker(opts) {
  let chosen = opts.initial || null;
  const all = () => (opts.items ? opts.items() : []);
  if (opts.items && opts.value !== undefined && opts.value !== null && opts.value !== '') chosen = all().find(r => String(opts.key(r)) === String(opts.value)) || null;
  const input = el('input', { type: 'text', placeholder: opts.placeholder || `type to search ${opts.title}…` });
  const dd = el('div', { class: 'dd', hidden: true });
  let sel = -1;
  let shown = [];
  const show = () => {
    input.value = chosen ? opts.label(chosen) : '';
    input.classList.toggle('haspick', !!chosen);
  };
  const pick = row => {
    chosen = row;
    dd.hidden = true;
    show();
    if (opts.onpick) opts.onpick(row);
  };
  const text = r => [opts.label(r), ...(opts.columns || []).map(c => { const v = c.value ? c.value(r) : r[c.key]; return Array.isArray(v) ? v.join(' ') : v; })].join(' ').toLowerCase();
  let asked = 0;
  let timer = null;
  const suggest = async () => {
    const q = input.value.trim().toLowerCase();
    if (opts.lookup) {
      const mine = ++asked;
      let found = [];
      try { found = await opts.lookup(q); } catch (e) { found = []; }
      if (mine !== asked) return;
      shown = found.slice(0, 15);
    } else {
      shown = all().filter(r => !q || text(r).includes(q)).slice(0, 15);
    }
    sel = -1;
    // replaceChildren takes nodes, not arrays or nulls
    const list = shown.length
      ? shown.map((r, i) => el('div', { class: 'opt', 'data-i': i, onmousedown: e => { e.preventDefault(); pick(r); } }, opts.label(r)))
      : [el('div', { class: 'opt dim' }, 'no match: pick an existing one')];
    if (opts.empty !== undefined) list.unshift(el('div', { class: 'opt dim', onmousedown: e => { e.preventDefault(); pick(null); } }, opts.empty));
    dd.replaceChildren(...list);
    dd.hidden = false;
  };
  input.addEventListener('input', () => { chosen = null; clearTimeout(timer); timer = setTimeout(suggest, opts.lookup ? 200 : 0); });
  input.addEventListener('focus', () => { input.select(); suggest(); });
  input.addEventListener('blur', () => setTimeout(() => { dd.hidden = true; show(); }, 150));
  input.addEventListener('keydown', e => {
    const opts_ = dd.querySelectorAll('.opt[data-i]');
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      sel = Math.max(0, Math.min(opts_.length - 1, sel + (e.key === 'ArrowDown' ? 1 : -1)));
      opts_.forEach((o, i) => o.classList.toggle('sel', i === sel));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (sel >= 0 && shown[sel]) pick(shown[sel]); else if (shown.length === 1) pick(shown[0]);
    } else if (e.key === 'Escape') { dd.hidden = true; show(); }
  });
  const lookup = () => {
    let m = null;
    const g = grid({
      id: 'pick:' + opts.title,
      columns: opts.columns || [{ key: 'label', label: opts.title, value: r => opts.label(r) }],
      ...(opts.fetch ? { fetch: opts.fetch } : { rows: all() }),
      onRow: r => { m.close(); pick(r); },
    });
    m = modal(`choose ${opts.title}`, el('div', { class: 'pickhost' }, g.node), [], { cls: 'pickmodal' });
  };
  const node = el('span', { class: 'refpick' }, input,
    el('button', { type: 'button', title: `all ${opts.title}`, onclick: e => { e.preventDefault(); lookup(); } }, '🔍'), dd);
  show();
  return { node, get: () => (chosen ? opts.key(chosen) : null), row: () => chosen, set: row => { chosen = row; show(); } };
}
