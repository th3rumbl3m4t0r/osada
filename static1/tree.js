/* A family tree in a wiki article (wiki.js; the data is checked by src/wiki.rs):
     people {id: {name, desc, born, died, pic, ref}}   ref: the address of their own article
     unions [{a, b, kids}]                              a couple (b null: one parent) and their children
   Shown as cards (the picture on the left, the name leading to their article, the dates, a
   line about them) in rows by generation, partners side by side, lines down to the children.
   Dragged to move, wheel or pinch to zoom. While editing, a click on a card offers to change
   the person, add a parent, a partner or a child (with any of their partners, somebody new or
   anyone else in the tree), set their parents, or take them out; every change is saved at
   once (opts.save), made again on somebody else's newer tree if there is one. */
'use strict';

part('tree', () => {

const CW = 232, CH = 74, PGAP = 36, BGAP = 52, ROWH = CH + 104, PAD = 40;
// how many times the rows are sorted by where the families are, one row after another down and then up
const SWEEPS = 6;
const SVG = 'http://www.w3.org/2000/svg';

/* ---- changes to the data, on the copy save() hands out ---- */
function newPid(d) { let id; do { id = 'p' + Math.random().toString(36).slice(2, 9); } while (d.people[id]); return id; }
function newRef() {
  const a = 'abcdefghijkmnpqrstuvwxyz23456789';
  return Array.from(crypto.getRandomValues(new Uint8Array(8)), b => a[b % 32]).join('');
}
function clean(p) {
  const o = {};
  for (const k of ['name', 'desc', 'born', 'died', 'pic', 'ref']) { const v = String(p[k] || '').trim(); if (v) o[k] = v; }
  return o;
}
const unionOf = (d, x, y) => d.unions.find(u => (u.a === x && (u.b || null) === (y || null)) || (y && u.a === y && u.b === x));
const parentsOf = (d, x) => d.unions.find(u => u.kids.includes(x));
const partnersOf = (d, x) => d.unions.filter(u => u.b && (u.a === x || u.b === x)).map(u => (u.a === x ? u.b : u.a));
const nameOf = (d, x) => (d.people[x] || {}).name || '?';
function ancestors(d, x, seen = new Set()) {
  const u = parentsOf(d, x);
  if (u) for (const p of [u.a, u.b]) if (p && !seen.has(p)) { seen.add(p); ancestors(d, p, seen); }
  return seen;
}
function descendants(d, x, seen = new Set()) {
  for (const u of d.unions) if (u.a === x || u.b === x) for (const k of u.kids) if (!seen.has(k)) { seen.add(k); descendants(d, k, seen); }
  return seen;
}
const own = () => new Error(tx('that would make someone their own ancestor'));

const T = {
  add(d, p) { const id = newPid(d); d.people[id] = Object.assign(clean(p), { ref: clean(p).ref || newRef() }); return id; },
  edit(d, x, p) { const ref = d.people[x].ref; d.people[x] = Object.assign(clean(p), { ref: clean(p).ref || ref || newRef() }); },
  /* y a partner of x; `adopt`: and the other parent of x's children with nobody else known */
  partner(d, x, y, adopt) {
    if (x === y) throw own();
    let u = unionOf(d, x, y);
    if (!u) d.unions.push(u = { a: x, b: y, kids: [] });
    const lone = adopt && unionOf(d, x, null);
    if (lone) {
      for (const k of lone.kids) if (!u.kids.includes(k) && k !== y && !ancestors(d, y).has(k)) u.kids.push(k);
      d.unions.splice(d.unions.indexOf(lone), 1);
    }
  },
  /* c a child of x (and of y, x's partner, or nobody else) */
  child(d, x, y, c) {
    if (parentsOf(d, c)) throw new Error(tx('{name} has parents already', { name: nameOf(d, c) }));
    if (c === x || c === y || ancestors(d, x).has(c) || (y && ancestors(d, y).has(c))) throw own();
    let u = unionOf(d, x, y || null);
    if (!u) d.unions.push(u = { a: x, b: y || null, kids: [] });
    u.kids.push(c);
  },
  /* p a parent of x */
  parent(d, x, p) {
    if (p === x || descendants(d, x).has(p)) throw own();
    const u = parentsOf(d, x);
    if (!u) {
      // p's other children with nobody known are x's brothers and sisters
      const lone = unionOf(d, p, null);
      if (lone) lone.kids.push(x); else d.unions.push({ a: p, b: null, kids: [x] });
      return;
    }
    if (u.a === p || u.b === p) return;
    if (u.b) throw new Error(tx('{name} has two parents already: change them with parents…', { name: nameOf(d, x) }));
    // the two are a couple now; if they were one already, the children go together
    const v = unionOf(d, u.a, p);
    if (v) { for (const k of u.kids) if (!v.kids.includes(k)) v.kids.push(k); d.unions.splice(d.unions.indexOf(u), 1); } else u.b = p;
  },
  /* c the child of p1 and p2 (either may be null), out of the couple c was a child of before */
  setParents(d, c, p1, p2) {
    if (!p1) [p1, p2] = [p2, null];
    if (p1 && p1 === p2) p2 = null;
    for (const p of [p1, p2]) if (p && (p === c || descendants(d, c).has(p))) throw own();
    const old = parentsOf(d, c);
    if (old) {
      old.kids = old.kids.filter(k => k !== c);
      // one parent and no children left: nothing to keep
      if (!old.b && !old.kids.length) d.unions.splice(d.unions.indexOf(old), 1);
    }
    if (!p1) return;
    let u = unionOf(d, p1, p2);
    if (!u) d.unions.push(u = { a: p1, b: p2 || null, kids: [] });
    u.kids.push(c);
  },
  remove(d, x) {
    delete d.people[x];
    for (const u of d.unions) {
      u.kids = u.kids.filter(k => k !== x);
      if (u.b === x) u.b = null;
      if (u.a === x) { u.a = u.b; u.b = null; }
    }
    d.unions = d.unions.filter(u => u.a && (u.b || u.kids.length));
  },
};

/* ---- where the cards go ----
   Partners make a block, side by side: the one longest in the tree on the left, whoever joins
   it later goes on the far side of the one they joined (a second partner on the other side);
   turned around, at the end, only where that uncrosses lines. A block's row is its
   generation, counted from anyone (parents a row up, children a row down); where relatives
   are related twice over: children below their parents, brothers and sisters in one row (the
   lowest any of them has to be in), and a block with no parents in the tree just above its children.
   Rows are sorted by where the parents sit, brothers and sisters by birth (else in the order
   they were added), the order made as good as moves can make it (no crossing lines before
   brothers and sisters together, both before a tight tree: improve), and the blocks placed as
   near as they fit to where their parents and children pull them, a few times over, top down
   and bottom up. */
const born = p => { const m = /\d{3,4}/.exec(p.born || ''); return m ? +m[0] : Infinity; };
function layout(d) {
  const P = d.people, ids = Object.keys(P), order = new Map(ids.map((k, i) => [k, i]));
  const byBirth = (a, b) => born(P[a]) - born(P[b]) || order.get(a) - order.get(b);
  const U = d.unions.filter(u => P[u.a] && (!u.b || P[u.b])).map(u => ({ a: u.a, b: u.b || null, kids: u.kids.filter(k => P[k]).sort(byBirth) }));
  const parentU = {}, partners = {};
  ids.forEach(i => { partners[i] = []; });
  U.forEach(u => { u.kids.forEach(k => { if (!parentU[k]) parentU[k] = u; }); if (u.b) { partners[u.a].push(u.b); partners[u.b].push(u.a); } });
  const blocks = [], blockOf = {};
  for (const i of ids) {
    if (blockOf[i] !== undefined) continue;
    const comp = [], stack = [i], seen = new Set([i]);
    while (stack.length) { const x = stack.pop(); comp.push(x); for (const y of partners[x]) if (!seen.has(y)) { seen.add(y); stack.push(y); } }
    // a chain of partners from one end: someone married twice sits between the two
    const chain = from => { const out = [], vis = new Set(); const walk = x => { vis.add(x); out.push(x); for (const y of partners[x]) if (!vis.has(y)) walk(y); }; walk(from); return out; };
    const ends = comp.filter(x => partners[x].length <= 1);
    let line = chain(ends[0] || comp[0]);
    if (ends.length > 1) {
      // of the two ways round: the first in the tree as far left as it goes, the next ones to the right of it
      const back = line.slice().reverse(), rank = l => comp.slice().sort((a, b) => order.get(a) - order.get(b)).map((m, i) => (i ? -1 : 1) * l.indexOf(m));
      const ra = rank(line), rb = rank(back), j = ra.findIndex((v, i) => v !== rb[i]);
      if (j >= 0 && rb[j] < ra[j]) line = back;
    }
    line.forEach(x => { blockOf[x] = blocks.length; });
    blocks.push({ members: line, gen: 0, x: 0 });
  }
  const kidsOf = bi => U.filter(u => blockOf[u.a] === bi).flatMap(u => u.kids);
  // generations, counted from anyone: their parents a row up, their children a row down, partners in their
  // row. In a family where nobody is related twice over, that fits everyone exactly, however many generations
  // each line goes back (counted from the top, a short line met a long one far below its own parents)
  const nexts = bi => [...U.filter(u => u.kids.some(k => blockOf[k] === bi)).map(u => [blockOf[u.a], -1]), ...kidsOf(bi).map(k => [blockOf[k], 1])];
  const reached = new Set();
  for (let start = 0; start < blocks.length; start++) {
    if (reached.has(start)) continue;
    const comp = [start];
    reached.add(start);
    blocks[start].gen = 0;
    for (let i = 0; i < comp.length; i++) {
      for (const [nb, step] of nexts(comp[i])) {
        if (reached.has(nb)) continue;
        reached.add(nb);
        blocks[nb].gen = blocks[comp[i]].gen + step;
        comp.push(nb);
      }
    }
    const top = Math.min(...comp.map(bi => blocks[bi].gen));
    comp.forEach(bi => { blocks[bi].gen -= top; });
  }
  // where somebody is related twice over and that does not fit: children below their parents (only ever down,
  // so it ends; the cap is for data with someone their own ancestor), then once, brothers and sisters in one
  // row and a block with no parents in the tree just above its children, and the rows nobody is in closed up
  const settle = () => {
    for (let n = 0, moved = true; moved && n < 4 * blocks.length + 4; n++) {
      moved = false;
      for (const u of U) {
        const row = blocks[blockOf[u.a]].gen + 1;
        for (const k of u.kids) { const kb = blocks[blockOf[k]]; if (kb.gen < row) { kb.gen = row; moved = true; } }
      }
    }
  };
  settle();
  for (const u of U) {
    const row = Math.max(...u.kids.map(k => blocks[blockOf[k]].gen));
    for (const k of u.kids) blocks[blockOf[k]].gen = row;
  }
  settle();
  blocks.forEach((b, bi) => {
    if (b.members.some(m => parentU[m])) return;
    const kg = kidsOf(bi).map(k => blocks[blockOf[k]].gen);
    if (kg.length) b.gen = Math.max(b.gen, Math.min(...kg) - 1);
  });
  const used = [...new Set(blocks.map(b => b.gen))].sort((a, b) => a - b);
  blocks.forEach(b => { b.gen = used.indexOf(b.gen); });
  const rows = [];
  blocks.forEach((b, bi) => { (rows[b.gen] = rows[b.gen] || []).push(bi); });
  for (let g = 0; g < rows.length; g++) rows[g] = rows[g] || [];

  const W = b => b.members.length * CW + (b.members.length - 1) * PGAP;
  const cx = m => { const b = blocks[blockOf[m]]; return b.x + b.members.indexOf(m) * (CW + PGAP) + CW / 2; };
  // where a couple's children hang from: between the two, or for two not side by side (someone with more
  // partners between them) in the gap next to the one with fewer partners, on the other one's side; p: a
  // person's place, step: from one member of a block to the next
  const hang = (u, p, step) => {
    if (!u.b) return p(u.a);
    const a = p(u.a), b = p(u.b);
    if (Math.abs(a - b) < 1.5 * step) return (a + b) / 2;
    const [o, m] = partners[u.b].length <= partners[u.a].length ? [b, a] : [a, b];
    return o - Math.sign(o - m) * step / 2;
  };
  const ux = u => hang(u, cx, CW + PGAP);
  const downs = blocks.map(() => []);
  U.forEach(u => { if (u.kids.length) downs[blockOf[u.a]].push(u); });
  const down = bi => downs[bi];
  const mean = a => a.reduce((s, x) => s + x, 0) / a.length;
  function want(bi, dir) {
    const b = blocks[bi], t = [];
    if (dir !== 'up') for (const m of b.members) { const u = parentU[m]; if (u) t.push(ux(u) - (cx(m) - b.x)); }
    if (dir !== 'down') for (const u of down(bi)) t.push(mean(u.kids.map(cx)) - (ux(u) - b.x));
    return t.length ? mean(t) : null;
  }
  // as near to where each wants as they fit side by side, in this order (pool adjacent violators)
  function place(row, desired) {
    let off = 0;
    const t = [], offs = [];
    for (const bi of row) { offs.push(off); t.push(desired(bi) - off); off += W(blocks[bi]) + BGAP; }
    const pools = [];
    t.forEach((v, j) => {
      pools.push({ sum: v, n: 1, from: j });
      while (pools.length > 1 && pools[pools.length - 2].sum / pools[pools.length - 2].n > pools[pools.length - 1].sum / pools[pools.length - 1].n) {
        const q = pools.pop();
        pools[pools.length - 1].sum += q.sum;
        pools[pools.length - 1].n += q.n;
      }
    });
    pools.forEach((p, k) => {
      const v = p.sum / p.n, to = k + 1 < pools.length ? pools[k + 1].from : t.length;
      for (let j = p.from; j < to; j++) blocks[row[j]].x = v + offs[j];
    });
  }
  // brothers and sisters in birth order: the first of a block's members with parents here, where among them
  const rank = bi => Math.min(...blocks[bi].members.map(m => (parentU[m] ? parentU[m].kids.indexOf(m) : Infinity)));
  const centre = bi => blocks[bi].x + W(blocks[bi]) / 2;
  // places in the rows, the members of each block one after another: the lines from one row to the next cross
  // where two of them go in different orders
  const places = () => { const at = {}; rows.forEach(row => { let i = 0; for (const bi of row) for (const m of blocks[bi].members) at[m] = i++; }); return at; };
  const midAt = (at, u) => hang(u, m => at[m], 1);
  function crossings(at) {
    let n = 0;
    for (const row of rows) {
      const e = row.flatMap(bi => down(bi).flatMap(u => u.kids.map(k => [midAt(at, u), at[k]])));
      for (let i = 0; i < e.length; i++) for (let j = i + 1; j < e.length; j++) if ((e[i][0] - e[j][0]) * (e[i][1] - e[j][1]) < 0) n++;
    }
    return n;
  }
  // after each sorting, each block moved to wherever in its row its lines cross the fewest others: the sorting
  // goes by the middle of each block's parents or children, so a child whose partner's parents are on one side
  // stays among the brothers and sisters, and a family married into the middle of another one splits it. Moving
  // one block changes only whether its own lines cross those of the blocks it passes
  function uncross() {
    let at = places();
    const lines = bi => ({
      up: blocks[bi].members.filter(m => parentU[m]).map(m => [midAt(at, parentU[m]), m]),
      down: down(bi).flatMap(u => u.kids.map(k => [u, at[k]])),
    });
    // the crossings of the lines of p and q with p left of q, and with p right of q
    const pair = (p, q) => {
      let l = 0, r = 0;
      for (const [t] of p.up) for (const [v] of q.up) { if (t > v) l++; else if (t < v) r++; }
      for (const [, k] of p.down) for (const [, j] of q.down) { if (k > j) l++; else if (k < j) r++; }
      return [l, r];
    };
    for (let pass = 0, better = true; better && pass < 20; pass++) {
      better = false;
      for (const row of rows) {
        const ls = new Map(row.map(bi => [bi, lines(bi)]));
        for (const bi of row.slice()) {
          const others = row.filter(o => o !== bi), from = row.indexOf(bi);
          const c = others.map(o => pair(ls.get(bi), ls.get(o)));
          // at j: right of others[0 .. j-1], left of the rest; the nearest of the best, if better than where it is
          const costs = [c.reduce((sum, x) => sum + x[0], 0)];
          c.forEach((x, j) => costs.push(costs[j] + x[1] - x[0]));
          let to = from;
          costs.forEach((v, j) => { if (v < costs[to] || (v === costs[to] && to !== from && Math.abs(j - from) < Math.abs(to - from))) to = j; });
          if (to !== from) { others.splice(to, 0, bi); row.splice(0, row.length, ...others); better = true; }
        }
        at = places();
      }
    }
    // placed again in the new order, where they were as near as that goes
    rows.forEach(row => place(row, bi => blocks[bi].x));
    return crossings(at);
  }
  // the rows one after another (down or up) sorted by where the parents (or, going up, the children) are,
  // brothers and sisters by birth, and placed
  function sweep(dir, sort) {
    const gs = rows.map((_, g) => (dir === 'down' ? g : rows.length - 1 - g));
    for (const g of gs) {
      const row = rows[g];
      const key = new Map(row.map(bi => [bi, want(bi, dir)]));
      const at = bi => (key.get(bi) !== null ? key.get(bi) : blocks[bi].x);
      if (sort) {
        const by = new Map(row.map(bi => {
          const ups = blocks[bi].members.map(m => parentU[m]).filter(Boolean), kids = down(bi).flatMap(u => u.kids);
          const k = dir === 'down' && ups.length ? mean(ups.map(ux)) : dir === 'up' && kids.length ? mean(kids.map(cx)) : centre(bi);
          return [bi, Math.round(k)];
        }));
        row.sort((p, q) => by.get(p) - by.get(q) || rank(p) - rank(q) || p - q);
      }
      place(row, at);
    }
  }
  // the orders to start from: the one everyone came in, and going round the families one after another (from
  // each block to its parents, then its children), so that families married into each other start side by side
  const came = rows.map(r => r.slice());
  const round = new Array(blocks.length).fill(-1);
  let visited = 0;
  const walk = bi => {
    round[bi] = visited++;
    const next = [...blocks[bi].members.filter(m => parentU[m]).map(m => blockOf[parentU[m].a]), ...down(bi).flatMap(u => u.kids.map(k => blockOf[k]))];
    for (const o of next) if (round[o] < 0) walk(o);
  };
  blocks.forEach((_, bi) => { if (round[bi] < 0) walk(bi); });
  const went = rows.map(r => r.slice().sort((p, q) => round[p] - round[q]));
  // from one of them, sorted over and over: of all the sortings the last of those with the fewest crossings
  function arrange(start, turned) {
    start.forEach((r, g) => { rows[g] = turned ? r.slice().reverse() : r.slice(); });
    rows.forEach(row => { let x = 0; for (const bi of row) { blocks[bi].x = x; x += W(blocks[bi]) + BGAP; } });
    let best = null;
    for (let it = 0; it < SWEEPS; it++) {
      sweep(it % 2 ? 'up' : 'down', true);
      const n = uncross();
      if (!best || n <= best.n) best = { n, rows: rows.map(r => r.slice()), xs: blocks.map(b => b.x) };
    }
    return best;
  }
  // the last word on the order: lines crossing first, then brothers and sisters apart (blocks of others between
  // them); kept only what makes it better. Each block to the best place in its row, partners turned round (with
  // those they have lines to put right after) where that uncrosses lines; then pieces that hang on by one line (a
  // family married in with its forebears, someone with all their descendants) moved as one: the sortings leave
  // some crossings only such moves undo, a family married into another one going over to its side. Crossings
  // and the rest are counted only in the rows a move changes
  const families = U.filter(u => u.kids.length > 1);
  function improve() {
    const G = rows.length, at = {};
    const E = rows.map((_, g) => U.filter(u => blocks[blockOf[u.a]].gen === g).flatMap(u => u.kids.map(k => [u, k])));
    const fam = rows.map(() => []);
    families.forEach(u => fam[blocks[blockOf[u.kids[0]]].gen].push(u));
    const index = g => { let i = 0; for (const bi of rows[g]) for (const m of blocks[bi].members) at[m] = i++; };
    const gapCross = g => {
      if (g < 0 || g >= G) return 0;
      const e = E[g].map(([u, k]) => [midAt(at, u), at[k]]);
      let n = 0;
      for (let i = 0; i < e.length; i++) for (let j = i + 1; j < e.length; j++) if ((e[i][0] - e[j][0]) * (e[i][1] - e[j][1]) < 0) n++;
      return n;
    };
    const scatter = g => {
      const pos = new Map(rows[g].map((bi, i) => [bi, i]));
      let n = 0;
      for (const u of fam[g]) { const idx = new Set(u.kids.map(k => pos.get(blockOf[k]))); n += Math.max(...idx) - Math.min(...idx) + 1 - idx.size; }
      return n;
    };
    rows.forEach((_, g) => index(g));
    const cr = rows.map((_, g) => gapCross(g)), sc = rows.map((_, g) => scatter(g));
    const touch = g => { index(g); if (g > 0) cr[g - 1] = gapCross(g - 1); cr[g] = gapCross(g); sc[g] = scatter(g); };
    const sum = a => a.reduce((x, y) => x + y, 0);
    const total = () => sum(cr) * 1000 + sum(sc);
    const rowOf = bi => rows[blocks[bi].gen];
    // bi to place j of its row; gives back where it was
    const to = (bi, j) => { const row = rowOf(bi), i = row.indexOf(bi); row.splice(i, 1); row.splice(j, 0, bi); touch(blocks[bi].gen); return i; };
    const linked = bi => [...new Set([...blocks[bi].members.filter(m => parentU[m]).map(m => blockOf[parentU[m].a]),
      ...down(bi).flatMap(u => u.kids.map(k => blockOf[k]))])];
    let cur = total();
    const snapshot = () => rows.map(r => r.slice());
    const restore = snap => { snap.forEach((r, g) => { rows[g] = r.slice(); }); rows.forEach((_, g) => index(g)); rows.forEach((_, g) => { cr[g] = gapCross(g); sc[g] = scatter(g); }); cur = total(); };
    // each block to the best place in its row, over and over
    const singles = () => {
      for (let pass = 0, better = true; better && pass < 6; pass++) {
        better = false;
        for (let bi = 0; bi < blocks.length; bi++) {
          const n = rowOf(bi).length, from = rowOf(bi).indexOf(bi);
          let best = { s: cur, j: from };
          for (let j = 0; j < n; j++) {
            if (j === from) continue;
            to(bi, j);
            const v = total();
            if (v < best.s) best = { s: v, j };
            to(bi, from);
          }
          if (best.j !== from) { to(bi, best.j); cur = best.s; better = true; }
          // partners the other way round, those they have lines to each put where it is best then: kept only where
          // that uncrosses lines
          if (blocks[bi].members.length > 1) {
            const snap = snapshot(), before = cur;
            blocks[bi].members.reverse();
            touch(blocks[bi].gen);
            for (const ci of linked(bi)) {
              const row = rowOf(ci), from = row.indexOf(ci);
              let bv = total(), bj = from;
              for (let j = 0; j < row.length; j++) { if (j === from) continue; to(ci, j); const v = total(); if (v < bv) { bv = v; bj = j; } to(ci, from); }
              to(ci, bj);
            }
            const v = total();
            if (v <= before - 1000) { cur = v; better = true; } else { blocks[bi].members.reverse(); restore(snap); }
          }
        }
      }
    };
    singles();
    // pieces that hang on by one line: cut a line from a block to its parents' or a child's, and whatever is
    // still reached from the far end without it, if less than half of everyone (a family married in with its
    // forebears; someone with all their descendants), is kept together as one piece
    const side = (from, cut) => {
      const seen = new Set([from]), st = [from];
      while (st.length) { const x = st.pop(); for (const y of linked(x)) if (!seen.has(y) && !(x === from && y === cut)) { seen.add(y); st.push(y); } }
      return seen;
    };
    const clusters = [], seenC = new Set();
    blocks.forEach((_, bi) => {
      for (const ci of linked(bi)) {
        const c = side(ci, bi);
        if (c.has(bi) || c.size < 2 || c.size >= blocks.length / 2) continue;
        const key = [...c].sort((x, y) => x - y).join(' ');
        if (!seenC.has(key)) { seenC.add(key); clusters.push({ c, at: ci }); }
      }
    });
    // the family as one piece in each row: first where it hangs on (the row of the parents of the one who married
    // out), at every place there; from that row up and down, each row's piece where it crosses the fewest
    const put = (g, inC, j) => { const rest = rows[g].filter(bi => !inC.includes(bi)); rows[g] = [...rest.slice(0, j), ...inC, ...rest.slice(j)]; touch(g); };
    const bestPut = (g, inC) => {
      const n = rows[g].length - inC.length;
      let bv = Infinity, bj = 0;
      for (let j = 0; j <= n; j++) { put(g, inC, j); const v = total(); if (v < bv) { bv = v; bj = j; } }
      put(g, inC, bj);
    };
    const sift = ({ c: C, at: A }) => {
      const snap = snapshot(), before = cur, gA = blocks[A].gen;
      const parts = rows.map(row => row.filter(bi => C.has(bi)));
      let best = { s: cur, snap };
      for (let j = 0; j <= rows[gA].length - parts[gA].length; j++) {
        put(gA, parts[gA], j);
        for (let g = gA - 1; g >= 0; g--) if (parts[g].length) bestPut(g, parts[g]);
        for (let g = gA + 1; g < G; g++) if (parts[g].length) bestPut(g, parts[g]);
        const v = total();
        if (v < best.s) best = { s: v, snap: snapshot() };
        restore(snap);
      }
      if (best.s >= before) return false;
      restore(best.snap);
      singles();
      return true;
    };
    for (let round = 0, better = true; better && round < 3 && cur >= 1000; round++) {
      better = false;
      for (const C of clusters) if (sift(C)) better = true;
    }
  }
  // the first of them, and where that leaves lines crossing the others, each turned around too: one of those
  // only if it crosses fewer
  let best = arrange(came, false);
  for (const [start, turned] of [[came, true], [went, false], [went, true]]) {
    if (!best.n) break;
    const other = arrange(start, turned);
    if (other.n < best.n) best = other;
  }
  best.rows.forEach((r, g) => { rows[g] = r; });
  best.xs.forEach((x, bi) => { blocks[bi].x = x; });
  improve();
  sweep('down', false);
  sweep('up', false);
  for (let it = 0; it < 4; it++) for (const row of rows) place(row, bi => { const w = want(bi); return w !== null ? w : blocks[bi].x; });
  // last, from the bottom: who has no parents here sits over their children
  for (let g = rows.length - 1; g >= 0; g--) {
    place(rows[g], bi => { const w = blocks[bi].members.some(m => parentU[m]) ? null : want(bi, 'up'); return w !== null ? w : blocks[bi].x; });
  }
  // and a couple turned around only where that uncrosses lines, to their parents or to their children: done
  // at the end, not along the way, so a tree being built up does not turn its couples back and forth
  const gapEdges = g => U.filter(u => blocks[blockOf[u.a]].gen === g).flatMap(u => u.kids.map(k => [ux(u), cx(k)]));
  const crossed = g => { const e = gapEdges(g); let n = 0; for (let i = 0; i < e.length; i++) for (let j = i + 1; j < e.length; j++) if ((e[i][0] - e[j][0]) * (e[i][1] - e[j][1]) < 0) n++; return n; };
  for (let it = 0; it < 2; it++) {
    for (const b of blocks) {
      if (b.members.length < 2) continue;
      const near = () => crossed(b.gen - 1) + crossed(b.gen);
      const before = near();
      if (!before) continue;
      b.members.reverse();
      if (near() >= before) b.members.reverse();
    }
  }

  const minX = Math.min(0, ...blocks.map(b => b.x));
  const pos = {};
  blocks.forEach(b => b.members.forEach((m, j) => { pos[m] = { x: b.x - minX + PAD + j * (CW + PGAP), y: PAD + b.gen * ROWH }; }));
  const w = Math.max(CW, ...blocks.map(b => b.x - minX + W(b))) + 2 * PAD, h = Math.max(CH, rows.length * ROWH - (ROWH - CH)) + 2 * PAD;
  // lines, each a few runs of points at right angles: partners joined at mid height, two not side by side
  // round under the ones between them; from the couple down to a rail, from the rail down to each child
  const lines = [];
  const at = m => pos[m].x + CW / 2, apart = u => u.b && Math.abs(pos[u.a].x - pos[u.b].x) > 1.5 * (CW + PGAP);
  // of those going round from one person to one side, the shorter ones nearer the cards
  const around = U.filter(apart).map(u => {
    const [o, m] = partners[u.b].length <= partners[u.a].length ? [u.b, u.a] : [u.a, u.b];
    return { u, o, m, dir: Math.sign(pos[o].x - pos[m].x), span: Math.abs(pos[o].x - pos[m].x) };
  });
  around.forEach(r => { r.k = Math.min(2, around.filter(q => q.m === r.m && q.dir === r.dir && q.span < r.span).length); });
  const roundOf = new Map(around.map(r => [r.u, r]));
  const from = new Map();
  for (const u of U) {
    const pa = pos[u.a], mid = pa.y + CH / 2;
    if (!u.b) { from.set(u, [at(u.a), pa.y + CH]); continue; }
    const r = roundOf.get(u);
    if (!r) {
      const l = Math.min(pa.x, pos[u.b].x) + CW, rr = Math.max(pa.x, pos[u.b].x);
      lines.push({ cls: 'couple', runs: [[[l, mid], [rr, mid]]] });
      from.set(u, [(at(u.a) + at(u.b)) / 2, mid]);
      continue;
    }
    // down from the one with more partners, under the cards between, up next to the other and in from the side
    const sx = r.dir > 0 ? pos[r.m].x + CW - 24 - 12 * r.k : pos[r.m].x + 24 + 12 * r.k;
    const gx = hang(u, at, CW + PGAP), by = pa.y + CH + 10 + 6 * r.k;
    lines.push({ cls: 'couple', runs: [[[sx, pa.y + CH], [sx, by], [gx, by], [gx, mid], [r.dir > 0 ? pos[r.o].x : pos[r.o].x + CW, mid]]] });
    from.set(u, [gx, by]);
  }
  // each couple's rail on the first height below the row where it overlaps no other rail (from the left)
  const rails = U.filter(u => u.kids.length).map(u => {
    const x = from.get(u)[0], kx = u.kids.map(at);
    return { u, x, l: Math.min(x, ...kx), r: Math.max(x, ...kx), level: 0 };
  }).sort((p, q) => pos[p.u.a].y - pos[q.u.a].y || p.l - q.l);
  const ends = {};
  for (const rl of rails) {
    const e = ends[pos[rl.u.a].y] = ends[pos[rl.u.a].y] || [];
    let lv = e.findIndex(r => r < rl.l - 8);
    if (lv < 0 && e.length < 6) lv = e.length;
    if (lv < 0) lv = e.indexOf(Math.min(...e));
    e[lv] = rl.r;
    rl.level = lv;
  }
  for (const rl of rails) {
    const [x, y0] = from.get(rl.u), ry = pos[rl.u.a].y + CH + 26 + rl.level * 12;
    lines.push({ cls: 'kids', runs: [[[x, y0], [x, ry]], [[rl.l, ry], [rl.r, ry]], ...rl.u.kids.map(k => [[at(k), ry], [at(k), pos[k].y]])] });
  }
  // where a line goes straight across another one's upright, a small bridge over it: not a joint
  const HOP = 5;
  const ups = lines.flatMap(ln => ln.runs.flatMap(run => run.slice(1).map((q, i) => [run[i], q])
    .filter(([p, q]) => p[0] === q[0]).map(([p, q]) => ({ ln, x: p[0], y1: Math.min(p[1], q[1]), y2: Math.max(p[1], q[1]) }))));
  const num = v => Math.round(v * 10) / 10;
  const paths = lines.map(ln => {
    let d = '';
    for (const run of ln.runs) {
      d += `M${num(run[0][0])} ${num(run[0][1])}`;
      for (let i = 1; i < run.length; i++) {
        const [x0, y] = run[i - 1], [x1, y1] = run[i];
        if (x0 === x1) { d += `V${num(y1)}`; continue; }
        const dir = Math.sign(x1 - x0), lo = Math.min(x0, x1) + HOP, hi = Math.max(x0, x1) - HOP;
        const xs = ups.filter(v => v.ln !== ln && v.x > lo && v.x < hi && v.y1 < y - 0.5 && v.y2 > y + 0.5).map(v => v.x).sort((p, q) => dir * (p - q));
        let last = null;
        for (const c of xs) {
          if (last !== null && Math.abs(c - last) < 2 * HOP + 2) continue;
          d += `H${num(c - dir * HOP)}A${HOP} ${HOP} 0 0 ${dir > 0 ? 1 : 0} ${num(c + dir * HOP)} ${num(y)}`;
          last = c;
        }
        d += `H${num(x1)}`;
      }
    }
    return { cls: ln.cls, d };
  });
  return { pos, w, h, paths };
}

function dates(p) {
  if (p.born && p.died) return p.born + ' – ' + p.died;
  if (p.born) return '* ' + p.born;
  if (p.died) return '† ' + p.died;
  return '';
}
const initials = n => n.split(/\s+/).filter(Boolean).slice(0, 2).map(w => w[0].toUpperCase()).join('');
const personOption = (d, x) => h('option', { value: x, text: nameOf(d, x) + (dates(d.people[x]) ? '  (' + dates(d.people[x]) + ')' : '') });
const byName = d => (a, b) => nameCmp(nameOf(d, a), nameOf(d, b));

class FamilyTree {
  /* opts: {data, refs, editable, editing, picture(name) -> url, pictures() -> [names], upload(file) -> name,
     pick(text) -> {id, title, missing}, title(ref) -> its article's title or null, write(pid, person),
     save(change(data) -> data, note) -> {data, refs, layout}, removed(), droppable: the tree may be removed on its own,
     exported(): the tree with its people's articles as a file, layout: where the cards of data go (layout(),
     as kept with the tree by whoever saved it), if known} */
  constructor(host, opts) {
    this.o = opts;
    this.data = opts.data;
    this.placed = opts.layout ? { data: opts.data, L: opts.layout } : null;
    this.refs = opts.refs || {};
    this.editing = !!opts.editing;
    this.view = { x: 0, y: 0, k: 1 };
    this.fitted = false;
    this.stage = h('div', { class: 'ft-stage' });
    this.svg = document.createElementNS(SVG, 'svg');
    this.svg.setAttribute('class', 'ft-lines');
    this.wrap = h('div', { class: 'ft-wrap' }, this.stage);
    this.count = h('span', { class: 'dim' });
    this.editBtn = opts.editable ? btn(tx('edit the tree'), tx('click a card to change the person or add a parent, a partner or a child'), () => this.setEditing(!this.editing)) : null;
    this.dropBtn = opts.editable && opts.droppable ? btn(tx('remove the tree'), tx('remove the family tree from this article (the history keeps it)'), () => this.drop().catch(fail), 'danger') : null;
    this.fullBtn = btn(tx('full screen'), tx('the tree on the whole screen (esc: back)'), () => this.full(!this.el.classList.contains('ft-full')));
    this.exportBtn = opts.exported ? btn(tx('export JSON'), tx("the tree and its people's articles as a JSON file: the texts, and of the pictures where they are and what the texts say they show"), () => opts.exported().catch(fail)) : null;
    this.el = h('div', { class: 'ft' },
      h('div', { class: 'toolbar ft-bar' }, btn(tx('fit'), tx('the whole tree in view'), () => this.fit()), btn('−', tx('zoom out'), () => this.zoomBy(1 / 1.25)), btn('+', tx('zoom in'), () => this.zoomBy(1.25)),
        this.fullBtn, this.exportBtn, h('span', { class: 'spacer' }), this.count, this.dropBtn, this.editBtn),
      this.wrap);
    host.appendChild(this.el);
    this.pointers();
    this.stage.addEventListener('click', e => this.click(e));
    this.render();
  }

  setEditing(on) {
    this.editing = on;
    this.render();
  }

  render() {
    // where the cards go: as kept with the tree when it was saved, else worked out here, once
    const d = this.data, ids = Object.keys(d.people);
    const fits = L => L && L.pos && L.paths && ids.length === Object.keys(L.pos).length && ids.every(id => L.pos[id]);
    if (!this.placed || this.placed.data !== d || !fits(this.placed.L)) this.placed = { data: d, L: layout(d) };
    const L = this.L = this.placed.L;
    this.stage.style.width = L.w + 'px';
    this.stage.style.height = L.h + 'px';
    this.svg.setAttribute('width', L.w);
    this.svg.setAttribute('height', L.h);
    this.svg.replaceChildren(...L.paths.map(p => { const e = document.createElementNS(SVG, 'path'); e.setAttribute('d', p.d); e.setAttribute('class', p.cls); return e; }));
    const cards = ids.map(id => this.card(id, d.people[id], L.pos[id]));
    this.stage.replaceChildren(this.svg, ...cards);
    this.wrap.querySelectorAll('.ft-empty').forEach(e => e.remove());
    if (!ids.length) {
      this.wrap.appendChild(h('div', { class: 'ft-empty' }, this.o.editable
        ? btn(tx('add the first person'), tx('the tree starts with one person; parents, partners and children are added from them'), () => this.first().catch(fail), 'primary')
        : h('span', { class: 'dim', text: tx('nobody in the tree yet') })));
    }
    this.count.textContent = txn(ids.length, '{n} person', '{n} people');
    this.el.classList.toggle('editing', this.editing);
    if (this.editBtn) { this.editBtn.textContent = this.editing ? tx('done') : tx('edit the tree'); this.editBtn.classList.toggle('primary', this.editing); }
    if (this.dropBtn) this.dropBtn.hidden = !this.editing;
    if (!this.fitted) this.fit();
  }

  card(id, p, at) {
    const there = !!(p.ref && this.refs[p.ref]);
    const title = there ? this.o.title(p.ref) : null;
    const pic = p.pic ? h('img', { src: this.o.picture(p.pic), alt: '', draggable: 'false', loading: 'lazy' }) : h('span', { class: 'ft-initials', text: initials(p.name) });
    return h('div', { class: 'ft-card', 'data-p': id, style: { left: at.x + 'px', top: at.y + 'px' } },
      h('div', { class: 'ft-pic' }, pic),
      h('div', { class: 'ft-txt' },
        h('a', { class: 'ft-name wiki-ref' + (there ? '' : ' missing'), href: '/wiki/' + p.ref, text: p.name, draggable: 'false',
          title: there ? (title && title !== p.name ? title : tx('their article')) : tx('not written yet: a click offers to write it') }),
        dates(p) ? h('div', { class: 'ft-dates', text: dates(p) }) : null,
        p.desc ? h('div', { class: 'ft-desc', text: p.desc, title: p.desc }) : null));
  }

  click(e) {
    if (this.dragged) { e.preventDefault(); e.stopPropagation(); return; }
    const card = e.target.closest('.ft-card');
    if (!card) return;
    const id = card.dataset.p, p = this.data.people[id], name = card.querySelector('a.ft-name');
    const onName = e.target.closest('a.ft-name');
    if (this.editing && !onName) { e.preventDefault(); this.menu(id, e.clientX, e.clientY); return; }
    // the name, or the card when not editing: their article, or the offer to write it
    e.preventDefault();
    e.stopPropagation();
    if (name.classList.contains('missing')) this.o.write(id, p);
    else navigate('/wiki/' + encodeURIComponent(p.ref));
  }

  /* ---- moving around ---- */
  apply() { this.stage.style.transform = `translate(${this.view.x}px, ${this.view.y}px) scale(${this.view.k})`; }
  fit() {
    const w = this.wrap.clientWidth, hh = this.wrap.clientHeight;
    if (!w || !hh) return;
    const k = Math.max(0.2, Math.min(1, (w - 20) / this.L.w, (hh - 20) / this.L.h));
    this.view = { k, x: (w - this.L.w * k) / 2, y: Math.max(0, (hh - this.L.h * k) / 2) };
    this.fitted = Object.keys(this.data.people).length > 0;
    // fitted again after changes, until the view is moved by hand
    this.auto = true;
    this.apply();
  }
  /* a person in view, if they are not */
  show(id) {
    const at = this.L.pos[id];
    if (!at) return;
    const v = this.view, w = this.wrap.clientWidth, hh = this.wrap.clientHeight;
    const x0 = v.x + at.x * v.k, y0 = v.y + at.y * v.k, x1 = x0 + CW * v.k, y1 = y0 + CH * v.k;
    if (x0 >= 0 && y0 >= 0 && x1 <= w && y1 <= hh) return;
    this.view = { k: v.k, x: w / 2 - (at.x + CW / 2) * v.k, y: hh / 2 - (at.y + CH / 2) * v.k };
    this.apply();
  }
  zoomAt(k, px, py) {
    k = Math.max(0.2, Math.min(2.5, k));
    const v = this.view, f = k / v.k;
    this.view = { k, x: px - (px - v.x) * f, y: py - (py - v.y) * f };
    this.apply();
  }
  zoomBy(f) { this.auto = false; this.zoomAt(this.view.k * f, this.wrap.clientWidth / 2, this.wrap.clientHeight / 2); }
  full(on) {
    this.el.classList.toggle('ft-full', on);
    this.fullBtn.classList.toggle('on', on);
    requestAnimationFrame(() => this.fit());
  }
  pointers() {
    const w = this.wrap, pts = new Map();
    let start = null;
    const at = e => { const r = w.getBoundingClientRect(); return { x: e.clientX - r.left, y: e.clientY - r.top }; };
    const pair = () => { const [a, b] = [...pts.values()]; return { d: Math.hypot(a.x - b.x, a.y - b.y), x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }; };
    w.addEventListener('wheel', e => { e.preventDefault(); this.auto = false; const p = at(e); this.zoomAt(this.view.k * Math.exp(-e.deltaY * 0.0015), p.x, p.y); }, { passive: false });
    w.addEventListener('pointerdown', e => {
      if (e.pointerType === 'mouse' && e.button !== 0) return;
      pts.set(e.pointerId, at(e));
      start = { p: at(e), v: Object.assign({}, this.view), pair: pts.size === 2 ? pair() : null };
      this.dragged = false;
    });
    w.addEventListener('pointermove', e => {
      if (!pts.has(e.pointerId) || !start) return;
      pts.set(e.pointerId, at(e));
      if (pts.size === 2 && start.pair) {
        const q = pair();
        this.dragged = true;
        this.auto = false;
        this.view = Object.assign({}, start.v);
        this.zoomAt(start.v.k * q.d / start.pair.d, start.pair.x, start.pair.y);
        this.view.x += q.x - start.pair.x;
        this.view.y += q.y - start.pair.y;
        this.apply();
        return;
      }
      const p = at(e), dx = p.x - start.p.x, dy = p.y - start.p.y;
      if (!this.dragged && Math.hypot(dx, dy) < 6) return;
      if (!this.dragged) { this.dragged = true; this.auto = false; w.setPointerCapture(e.pointerId); }
      this.view.x = start.v.x + dx;
      this.view.y = start.v.y + dy;
      this.apply();
    });
    const up = e => {
      pts.delete(e.pointerId);
      if (pts.size === 1) { const [p] = [...pts.values()]; start = { p, v: Object.assign({}, this.view), pair: null }; }
      // the click that ends a drag is no click
      if (!pts.size) setTimeout(() => { this.dragged = false; }, 0);
    };
    w.addEventListener('pointerup', up);
    w.addEventListener('pointercancel', up);
  }

  /* ---- editing ---- */
  /* `fn` makes the change on the newest tree and returns it; `focus()`, after: whom to show */
  async change(fn, note, focus) {
    busy(true);
    try {
      const st = await this.o.save(fn, note);
      if (!st.data) { this.o.removed(); return; }
      this.data = st.data;
      this.placed = st.layout ? { data: st.data, L: st.layout } : null;
      this.refs = st.refs || {};
      this.render();
      const id = focus && focus();
      if (this.auto) this.fit(); else if (id) this.show(id);
    } finally { busy(false); }
  }

  menu(id, x, y) {
    const p = this.data.people[id];
    let close = null;
    const item = (label, fn, cls) => h('button', { class: 'ft-item' + (cls ? ' ' + cls : ''), text: label, onclick: () => { close(); fn().catch(fail); } });
    const w = h('div', { class: 'win ft-menu' }, h('div', { class: 'titlebar' }, h('span', { class: 'grip', text: '::' }), p.name),
      h('div', { class: 'body' },
        item(tx('change…'), () => this.editPerson(id)),
        // two parents already: who they are is set with parents…
        parentsOf(this.data, id) && parentsOf(this.data, id).b ? null : item(tx('add a parent…'), () => this.relative(id, 'parent')),
        item(tx('add a partner…'), () => this.relative(id, 'partner')),
        item(tx('add a child…'), () => this.relative(id, 'child')),
        item(tx('parents…'), () => this.parents(id)),
        item(tx('take out of the tree'), () => this.takeOut(id), 'danger')));
    close = popup(x, y, w);
  }

  /* the fields of a person; `who`: offer people already in the tree too; `with`: the other parent of a child */
  form(p, opts) {
    const d = this.data;
    const f = k => h('input', { type: 'text', value: p[k] || '', spellcheck: k === 'desc' || k === 'name' });
    const name = f('name'), desc = f('desc'), born = f('born'), died = f('died');
    born.placeholder = died.placeholder = tx('e.g. 1890-03-02, 1890, about 1850');
    let ref = p.ref || null, refTitle = ref && this.refs[ref] ? this.o.title(ref) : null;
    const refShow = h('span', { class: 'ft-ref' });
    const paintRef = () => {
      refShow.replaceChildren(refTitle ? h('span', { class: 'wiki-ref', text: refTitle }) : h('span', { class: 'wiki-ref missing', text: tx('not written yet') }));
    };
    paintRef();
    const pickRef = btn(tx('another article…'), tx('the article the name leads to: find one, or name one yet to be written'), async () => {
      const r = await this.o.pick(name.value.trim());
      if (r) { ref = r.id; refTitle = r.missing ? null : r.title; paintRef(); }
    });
    const pics = this.o.pictures();
    const pic = h('select', {}, h('option', { value: '', text: tx('(none)') }), pics.map(n => h('option', { value: n, text: n })));
    pic.value = p.pic && pics.includes(p.pic) ? p.pic : '';
    const preview = h('img', { class: 'ft-preview', alt: '' });
    const paintPic = () => { preview.hidden = !pic.value; if (pic.value) preview.src = this.o.picture(pic.value); };
    pic.addEventListener('change', paintPic);
    paintPic();
    const up = btn(tx('upload…'), tx('a new picture, kept with this article'), () => {
      const inp = h('input', { type: 'file', accept: 'image/*' });
      inp.addEventListener('change', async () => {
        if (!inp.files[0]) return;
        busy(true);
        try {
          const n = await this.o.upload(inp.files[0]);
          pic.appendChild(h('option', { value: n, text: n }));
          pic.value = n;
          paintPic();
        } catch (e) { fail(e); } finally { busy(false); }
      });
      inp.click();
    });
    const fields = h('div', { class: 'ft-form' },
      field(tx('name'), name), field(tx('about them'), desc), h('div', { class: 'ft-two' }, field(tx('born'), born), field(tx('died'), died)),
      field(tx('picture'), h('div', { class: 'ft-picrow' }, pic, up, preview)),
      opts.withRef !== false ? field(tx('article'), h('div', { class: 'ft-picrow' }, refShow, pickRef)) : null);
    let who = null, other = null;
    if (opts.who) {
      who = h('select', {}, h('option', { value: '', text: tx('somebody new') }),
        opts.who.map(x => h('option', { value: x, text: nameOf(d, x) + (dates(d.people[x]) ? '  (' + dates(d.people[x]) + ')' : '') })));
      who.addEventListener('change', () => { fields.hidden = !!who.value; });
    }
    let otherName = null;
    if (opts.other) {
      // a child's other parent: a partner (one is the guess when there is only one), somebody new, anyone else here, nobody yet
      const x = opts.other, mates = partnersOf(d, x).sort(byName(d));
      const far = new Set([x, ...ancestors(d, x), ...descendants(d, x)]);
      const rest = Object.keys(d.people).filter(o => !far.has(o) && !mates.includes(o)).sort(byName(d));
      other = h('select', {},
        mates.length > 1 ? h('option', { value: '?', text: tx('choose…') }) : null,
        mates.length ? h('optgroup', { label: tx('partners') }, mates.map(o => personOption(d, o))) : null,
        h('option', { value: '+', text: tx('somebody new…') }),
        rest.length ? h('optgroup', { label: tx('others in the tree') }, rest.map(o => personOption(d, o))) : null,
        h('option', { value: '', text: tx('nobody else (yet)') }));
      other.value = mates.length === 1 ? mates[0] : mates.length > 1 ? '?' : '';
      otherName = h('input', { type: 'text', class: 'ft-other-name', placeholder: tx('their name'), hidden: true, spellcheck: true });
      other.addEventListener('change', () => { otherName.hidden = other.value !== '+'; if (!otherName.hidden) otherName.focus(); });
    }
    let adopt = null;
    if (opts.adopt) adopt = h('input', { type: 'checkbox', checked: true });
    const body = [other ? field(tx('other parent'), h('div', { class: 'ft-picrow' }, other, otherName)) : null, who ? field(tx('who'), who) : null,
      adopt ? h('label', { class: 'chk ft-adopt' }, adopt, ' ' + tx('also the other parent of {names}', { names: opts.adopt.map(k => nameOf(d, k)).join(', ') })) : null, fields];
    const value = () => ({
      who: who ? who.value : '', with: other ? other.value || null : null, withName: otherName ? otherName.value.trim() : '', adopt: !!(adopt && adopt.checked),
      p: { name: name.value, desc: desc.value, born: born.value, died: died.value, pic: pic.value, ref },
    });
    return { body, value, focus: () => (who && who.value ? who : name).focus() };
  }

  async ask(title, f, ok) {
    return dialog({
      title, body: f.body, wide: true,
      buttons: [{ label: tx('cancel'), value: null }, { label: ok, primary: true, value: () => f.value() }],
      check: v => (v.with === '?' ? tx('choose who the other parent is') : v.with === '+' && !v.withName ? tx('the other parent needs a name')
        : !v.who && !v.p.name.trim() ? tx('a name is needed') : null),
      opened: () => f.focus(),
    });
  }

  async first() {
    const f = this.form({}, {});
    const v = await this.ask(tx('the first person'), f, tx('add'));
    if (v) await this.change(d => { T.add(d, v.p); return d; }, tx('{name} added', { name: v.p.name.trim() }));
    this.fit();
  }

  async editPerson(id) {
    const f = this.form(this.data.people[id], {});
    const v = await this.ask(tx('change {name}', { name: nameOf(this.data, id) }), f, tx('save'));
    if (v) await this.change(d => { if (d.people[id]) T.edit(d, id, v.p); return d; }, tx('{name} changed', { name: v.p.name.trim() }), () => id);
  }

  async relative(id, kind) {
    const d = this.data, x = nameOf(d, id);
    const others = Object.keys(d.people).filter(o => o !== id);
    // who of those already there may be it
    const fits = kind === 'parent' ? others.filter(o => !descendants(d, id).has(o))
      : kind === 'child' ? others.filter(o => !parentsOf(d, o) && !ancestors(d, id).has(o))
        : others.filter(o => !partnersOf(d, id).includes(o));
    fits.sort((a, b) => nameCmp(nameOf(d, a), nameOf(d, b)));
    const lone = kind === 'partner' && unionOf(d, id, null);
    const f = this.form({}, { who: fits, other: kind === 'child' ? id : null, adopt: lone && lone.kids.length ? lone.kids : null });
    const title = { parent: tx('a parent of {name}', { name: x }), partner: tx('a partner of {name}', { name: x }), child: tx('a child of {name}', { name: x }) }[kind];
    const v = await this.ask(title, f, tx('add'));
    if (!v) return;
    const who = v.who ? nameOf(d, v.who) : v.p.name.trim();
    const note = { parent: tx('{who} added as a parent of {name}', { who, name: x }), partner: tx('{who} added as a partner of {name}', { who, name: x }), child: tx('{who} added as a child of {name}', { who, name: x }) }[kind];
    let added = null;
    await this.change(dd => {
      if (!dd.people[id]) throw new Error(tx('{name} is not in the tree any more', { name: x }));
      const y = added = v.who && dd.people[v.who] ? v.who : T.add(dd, v.p);
      if (kind === 'parent') T.parent(dd, id, y);
      else if (kind === 'partner') T.partner(dd, id, y, v.adopt);
      else {
        // the other parent: somebody new becomes a partner, as does anyone else from the tree
        const w = v.with === '+' ? T.add(dd, { name: v.withName }) : v.with && dd.people[v.with] ? v.with : null;
        T.child(dd, id, w, y);
      }
      return dd;
    }, note, () => added);
  }

  /* who someone's parents are: to set them late, or to put a child with another couple */
  async parents(id) {
    const d = this.data, x = nameOf(d, id), cur = parentsOf(d, id);
    const far = new Set([id, ...descendants(d, id)]);
    const people = Object.keys(d.people).filter(o => !far.has(o)).sort(byName(d));
    const pick = () => h('select', {}, h('option', { value: '', text: tx('(nobody)') }), people.map(o => personOption(d, o)));
    const p1 = pick(), p2 = pick();
    p1.value = cur ? cur.a : '';
    p2.value = cur && cur.b ? cur.b : '';
    const v = await dialog({
      title: tx('the parents of {name}', { name: x }), wide: true,
      body: [field(tx('parent'), p1), field(tx('other parent'), p2),
        h('p', { class: 'dim', text: tx('the two become a couple, if they are not one yet; their other children stay as they are') })],
      buttons: [{ label: tx('cancel'), value: null }, { label: tx('save'), primary: true, value: () => [p1.value || null, p2.value || null] }],
      check: v => (v[0] && v[0] === v[1] ? tx('two different people') : null),
    });
    if (!v) return;
    await this.change(dd => {
      if (!dd.people[id]) throw new Error(tx('{name} is not in the tree any more', { name: x }));
      T.setParents(dd, id, v[0] && dd.people[v[0]] ? v[0] : null, v[1] && dd.people[v[1]] ? v[1] : null);
      return dd;
    }, tx('the parents of {name} changed', { name: x }), () => id);
  }

  async takeOut(id) {
    const x = nameOf(this.data, id);
    if (!(await confirmBox(tx('take out of the tree'), tx('take {name} out of the tree? their article stays.', { name: x }), tx('take out'), true))) return;
    await this.change(d => { T.remove(d, id); return d; }, tx('{name} taken out', { name: x }));
  }

  async drop() {
    if (!(await confirmBox(tx('remove the tree'), tx('remove the family tree from this article? the history keeps it and can put it back.'), tx('remove'), true))) return;
    await this.change(() => null, tx('the family tree removed'));
  }

  shown() { requestAnimationFrame(() => this.fit()); }
  key(e) {
    if (e.key === 'Escape' && this.el.classList.contains('ft-full')) { this.full(false); return true; }
    return false;
  }
  destroy() { this.el.remove(); }
}

return { FamilyTree, layout, T };
});
