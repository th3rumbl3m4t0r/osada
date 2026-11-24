"""The node map (D-U34) in headless Chromium: admin → nodes draws both nodes and their link with
its round trip and rates (after the nodes have sampled their links), the script loaded only for
it, and no page errors. Run by tests/map-test.sh.

    python3 -I tests/ui_map.py <data dir of the first node> <http://127.0.0.1:port>
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

D, cdp.BASE = sys.argv[1], sys.argv[2]
check = cdp.Check()
pw = dict(l.split(": ", 1) for l in open(D + "/first-admin").read().splitlines())["password"]
b = cdp.Browser()
try:
    b.load("/")
    b.wait("!!document.querySelector('#si-name')")
    b.focus("#si-name"); b.type("admin"); b.focus("#si-pw"); b.type(pw); b.key("Enter")
    check("signed in", b.wait("!!document.querySelector('#nav a[href=\"/admin\"]')", 10))
    check("no map script on other pages", not any("cytoscape.js" in u for u in b.requests))
    # the nodes sample their links every 10 s; a rate needs two samples
    time.sleep(22)
    b.load("/admin/nodes")
    check("the map is drawn", b.wait("!!document.querySelector('#node-map canvas')", 15), b.text('#node-map'))
    check("its script came", any("cytoscape.js" in u for u in b.requests))
    n = b.js("(() => { const c = document.querySelector('#node-map .node-map-area'); return c && c._cyreg && c._cyreg.cy ? [c._cyreg.cy.nodes().length, c._cyreg.cy.edges().length, c._cyreg.cy.edges()[0] && c._cyreg.cy.edges()[0].data('label')] : null; })()")
    check("two nodes, one link", n and n[0] == 2 and n[1] == 1, n)
    check("the link's round trip and rate", n and " ms" in (n[2] or "") and "bit/s" in (n[2] or ""), n)
    b.js("(() => { const cy = document.querySelector('#node-map .node-map-area')._cyreg.cy; cy.edges()[0].emit('tap'); })()")
    check("a link's card: speed and rate both ways, 1 and 10 minutes", b.wait("(() => { const t = (document.querySelector('.node-map-tip') || {}).textContent || ''; return t.includes('10 min') && t.includes('speed') && t.split('→').length === 5; })()", 5), b.text('.node-map-tip'))
    # what a node exposes (D-U35): the column, the card's section, a check now through the peer
    check("the exposed column", b.wait("[...document.querySelectorAll('table.grid th')].some(t => t.textContent.includes('exposed'))", 5))
    b.js("[...document.querySelectorAll('table.grid tr.data')].find(r => r.textContent.includes('home')).click()")
    check("the card's exposure", b.wait("[...document.querySelectorAll('.modal .titlebar')].some(t => t.textContent === 'exposed to the internet')", 5))
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'check now').click()")
    check("checked now from the edge", b.wait("[...document.querySelectorAll('.modal p.dim')].some(p => /Checked .* from edge/.test(p.textContent))", 25), b.text('.modal'))
    check("no page errors", not [e for e in b.errors if "401" not in e], b.errors)
finally:
    b.close()
sys.exit(check.done())
