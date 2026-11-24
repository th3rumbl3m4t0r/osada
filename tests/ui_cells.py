"""Round 19 in headless Chromium: every table's cells, the map's labels, the nodes table's link
column from the node the browser is connected to. Run by tests/map-test.sh:

    tests/map-test.sh target/release/osada ui_cells.py

- cells: on every admin tab, no cell's text runs out of it unmarked: a cell whose content does
  not fit ends in an accent "…"; hovering shows all of it, clicking the "…" opens it in the small
  editor (read-only, copy); a certificate with many host names is the long one;
- the map: no node's label is wider than its box, and the boxes do not overlap;
- the link column: the page pretends the browser came through the edge (the overview's `via`,
  patched in fetch): the column is "link (from edge)", with the round trip from the edge (its
  link_rates) to home, which answers.
"""
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

D, cdp.BASE = sys.argv[1], sys.argv[2]
check = cdp.Check()
pw = dict(l.split(": ", 1) for l in open(D + "/first-admin").read().splitlines())["password"]

# every cell: overflowing → has its "…"; none outside a .cell box overflows (but pickers' cells)
CELLS = """(() => {
  const bad = [];
  for (const c of document.querySelectorAll('table.grid td > .cell')) {
    if (!c.offsetParent) continue;
    const over = c.scrollWidth > c.clientWidth + 1;
    if (over !== !!c.querySelector(':scope > .more')) bad.push('cell ' + (over ? 'cut, no …' : '… but fits') + ': ' + c.textContent.slice(0, 60));
  }
  for (const td of document.querySelectorAll('table.grid td')) {
    if (!td.offsetParent || td.querySelector(':scope > .cell') || td.classList.contains('pick') || td.classList.contains('free') || td.hasAttribute('colspan') || td.closest('tr.qrow')) continue;
    if (td.scrollWidth > td.clientWidth + 1 || td.getBoundingClientRect().width > 360) bad.push('unwrapped: ' + td.textContent.slice(0, 60));
  }
  return { n: document.querySelectorAll('table.grid td > .cell').length, bad };
})()"""

b = cdp.Browser()
try:
    b.load("/")
    b.wait("!!document.querySelector('#si-name')")
    b.focus("#si-name"); b.type("admin"); b.focus("#si-pw"); b.type(pw); b.key("Enter")
    check("signed in", b.wait("!!document.querySelector('#nav a[href=\"/admin\"]')", 10))
    b.errors.clear()
    # every admin tab: the cells keep to the rule
    for tab in ["nodes", "idp", "files", "mail", "web", "chat", "wiki", "links", "dns", "users", "logs", "modules", "collections"]:
        b.load(f"/admin/{tab}")
        b.wait("!!document.querySelector('table.grid')", 10)
        time.sleep(1.5)
        r = b.js(CELLS)
        check(f"{tab}: every cell fits or ends in …", r and not r["bad"], r)
    # a long one: a certificate with many host names (the draft: not saved)
    b.load("/admin/nodes")
    CW = "[...document.querySelectorAll('.win')].find(w => w.querySelector('.titlebar').textContent.includes('certificates for the public names'))"
    b.wait(f"!!({CW})", 15)
    b.js(f"[...({CW}).querySelectorAll('button')].find(x => x.textContent === 'add a certificate').click()")
    b.wait("!!document.querySelector('.modal textarea')", 5)
    names = [f"name{i:02d}.a-rather-long-domain.example.org" for i in range(12)]
    b.js(f"(() => {{ const t = document.querySelector('.modal textarea'); t.value = {json.dumps(chr(10).join(names))}; }})()")
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'keep').click()")
    b.wait(f"!!({CW}) && ({CW}).textContent.includes('name00')", 10)
    cell = f"[...({CW}).querySelectorAll('td > .cell')].find(c => c.textContent.includes('name00'))"
    check("the long cell ends in an accent …", b.wait(f"!!({cell}) && !!({cell}).querySelector('.more')", 5), b.js(f"({cell}) && ({cell}).outerHTML.slice(0, 300)"))
    col = b.js(f"getComputedStyle(({cell}).querySelector('.more')).color")
    acc = b.js("(() => { const x = document.createElement('span'); x.style.color = 'var(--accent)'; document.body.append(x); const c = getComputedStyle(x).color; x.remove(); return c; })()")
    check("... in the accent colour", col == acc, f"{col} vs {acc}")
    check("... one line, at most the column's width", b.js(f"({cell}).getBoundingClientRect().width <= 341 && ({cell}).getBoundingClientRect().height < 30"), b.js(f"JSON.stringify(({cell}).getBoundingClientRect())"))
    # hover: all of it
    # the table is drawn again when the nodes' state comes (the certificate's state): after that
    b.wait(f"({CW}).textContent.includes('not issued yet')", 15)
    time.sleep(0.5)
    b.js(f"({cell}).scrollIntoView({{ block: 'center' }})")
    time.sleep(0.5)
    rect = b.js(f"(() => {{ const r = ({cell}).getBoundingClientRect(); return [r.left + 20, r.top + r.height / 2]; }})()")
    b.cmd("Input.dispatchMouseEvent", True, type="mouseMoved", x=rect[0], y=rect[1])
    check("hovering shows all of it", b.wait("(() => { const t = document.querySelector('.celltip'); return !!t && !t.hidden && t.textContent.includes('name11'); })()", 5), b.js("(document.querySelector('.celltip') || {}).textContent"))
    b.cmd("Input.dispatchMouseEvent", True, type="mouseMoved", x=5, y=5)
    # the … opens the editor; the row's card does not open
    b.js(f"({cell}).querySelector('.more').click()")
    check("the … opens the editor with all of it", b.wait("!!document.querySelector('.modal.textmodal textarea')", 5))
    v = b.js("document.querySelector('.modal.textmodal textarea').value")
    check("... every name in it, read-only", v and all(n in v for n in names) and b.js("document.querySelector('.modal.textmodal textarea').readOnly"), v)
    check("... only the editor (not the certificate's card)", b.js("document.querySelectorAll('.modal').length") == 1, b.js("[...document.querySelectorAll('.modal .mhead')].map(x => x.textContent)"))
    check("... titled by its column", "host names" in b.js("document.querySelector('.modal.textmodal .mhead').textContent").lower(), b.js("document.querySelector('.modal.textmodal .mhead').textContent"))
    b.js("[...document.querySelectorAll('.modal.textmodal button')].find(x => x.textContent === 'close').click()")
    check("... closes", b.wait("!document.querySelector('.modal')", 5))
    # any table.grid drawn anywhere: the same
    b.js("document.querySelector('#app').append(Object.assign(document.createElement('table'), { className: 'grid', id: 'x-t', innerHTML: '<tr><th>k</th><td>' + 'x'.repeat(400) + '</td><td>short</td></tr>' }))")
    check("a table drawn by hand gets it too", b.wait("(() => { const c = document.querySelectorAll('#x-t td > .cell'); return c.length === 2 && !!c[0].querySelector('.more') && !c[1].querySelector('.more'); })()", 5), b.js("(document.querySelector('#x-t') || {}).outerHTML"))
    b.js("document.querySelector('#x-t').remove()")
    check("no page errors so far", not [e for e in b.errors if "401" not in e], b.errors)

    # the link column from the node connected to: the overview says the edge relayed (patched)
    b.cmd("Page.addScriptToEvaluateOnNewDocument", True, source="""(() => {
      const f = window.fetch;
      window.fetch = async (u, o) => {
        const r = await f(u, o);
        if (!String(u).includes('/api/admin/overview')) return r;
        const j = await r.clone().json();
        const edge = Object.entries(j.placement.nodes).find(([id, n]) => (n.name || id) === 'edge');
        if (edge) j.via = edge[0];
        return new Response(JSON.stringify(j), { status: r.status, headers: r.headers });
      };
    })()""")
    time.sleep(12)  # the edge has sampled its links (every 10 s)
    b.load("/admin/nodes")
    b.wait("!!document.querySelector('#you-are-here')", 15)
    here = b.js("document.querySelector('#you-are-here').textContent")
    check("connected to the edge, home answers", "connected to edge" in here and "edge's" in here, here)
    head = b.js("[...document.querySelectorAll('table.grid th')].map(t => t.textContent).find(t => t.startsWith('link'))")
    check("the column: link (from edge)", head == "link (from edge)", head)
    row = "(n => [...document.querySelectorAll('table.grid tr.data')].find(r => r.querySelector('b') && r.querySelector('b').textContent === n))"
    ok = b.wait(f"/\\d+\\.\\d\\d ms · answers these pages/.test(({row})('home').textContent)", 20)
    check("home: the round trip from the edge, and it answers", ok, b.js(f"({row})('home').textContent"))
    check("edge: you are connected here", "you are connected here" in b.js(f"({row})('edge').textContent"), b.js(f"({row})('edge').textContent"))

    # the map: labels inside their boxes, boxes apart
    b.wait("!!document.querySelector('#node-map canvas')", 15)
    time.sleep(1)
    m = b.js("""(() => {
      const cy = document.querySelector('#node-map .node-map-area')._cyreg.cy;
      const ns = cy.nodes().map(n => ({ id: n.id(), lines: n.data('label').split('\\n'), box: n.boundingBox({ includeLabels: false }), all: n.boundingBox({ includeLabels: true }) }));
      const inside = ns.every(n => n.all.w <= n.box.w + 1 && n.all.h <= n.box.h + 1);
      const apart = ns.every((a, i) => ns.every((b, j) => j <= i || a.box.x2 < b.box.x1 || b.box.x2 < a.box.x1 || a.box.y2 < b.box.y1 || b.box.y2 < a.box.y1));
      return { inside, apart, longest: Math.max(...ns.flatMap(n => n.lines.map(l => l.length))), ns: ns.map(n => [n.lines, Math.round(n.box.w), Math.round(n.all.w)]) };
    })()""")
    check("map: every label inside its box", m and m["inside"], m)
    check("map: no box over another", m and m["apart"], m)
    check("map: no line over 24 characters", m and m["longest"] <= 24, m)
    check("no page errors", not [e for e in b.errors if "401" not in e], b.errors)
finally:
    b.close()
sys.exit(check.done())
