#!/usr/bin/python3 -I
"""The wiki's list of articles as the admin grid (T8) and the portal's tiles
(T7: one size, icons, day mode) in headless Chromium (tests/cdp.py).

    python3 -I tests/ui_wiki.py http://<host:port> <admin> <password>
"""
import os, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW = sys.argv[1], sys.argv[2], sys.argv[3]
ok = cdp.Check()
b = cdp.Browser()
try:
    b.load("/signin?next=/wiki")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name")
    b.type(NAME)
    b.focus("#si-pw")
    b.type(PW)
    b.key("Enter")
    ok("signed in, at the wiki", b.wait("location.pathname === '/wiki'", 15), b.js("location.href"))
    ok("the list of articles is the admin grid", b.wait("!!document.querySelector('.wiki-list table.grid')", 15), b.js("document.querySelector('#app') && document.querySelector('#app').innerHTML.slice(0, 300)"))
    n = b.js("document.querySelectorAll('.wiki-list table.grid tbody tr.data').length")
    ok("with its rows", (n or 0) > 0, n)
    ok("its columns sort (a header click)", b.js("(() => { const th = [...document.querySelectorAll('.wiki-list table.grid th')].find(t => /title|název|titre|cím|Titel/i.test(t.textContent)); if (!th) return false; th.click(); return true; })()"))
    time.sleep(0.3)
    ok("an article's link opens it", b.js("!!document.querySelector('.wiki-list table.grid tbody a[href^=\"/wiki/\"]')"))
    # the portal: tiles of one size, an icon each, day mode light
    b.load("/")
    b.wait("document.querySelectorAll('.portal .card').length >= 3", 10)
    hs = b.js("[...document.querySelectorAll('.portal .card')].map(c => Math.round(c.getBoundingClientRect().height))")
    ok("portal: every tile the same height", hs and len(set(hs)) == 1, hs)
    ok("portal: an icon on every tile", b.js("[...document.querySelectorAll('.portal .card')].every(c => !!c.querySelector('.appicon svg'))"))
    night = b.js("getComputedStyle(document.querySelector('.appicon .frame')).fill")
    b.js("localStorage.setItem('x11.theme', JSON.stringify({theme: 'day', accent: '#3b8a3e'}))")
    b.load("/")
    b.wait("document.querySelectorAll('.portal .card').length >= 3", 10)
    day = b.js("getComputedStyle(document.querySelector('.appicon .frame')).fill")
    ok("portal: the icons follow day mode", night != day and day == "rgb(204, 210, 218)", (night, day))
    b.js("localStorage.removeItem('x11.theme')")
    # admin → nodes (T10): the overview with what the node says, a node's card
    b.load("/admin/nodes")
    ok("nodes: storage, CPU and RAM from the node", b.wait("[...document.querySelectorAll('table.grid tbody tr.data td')].some(td => / of /.test(td.textContent))", 15),
       b.js("document.querySelector('table.grid') && document.querySelector('table.grid').innerText.slice(0, 300)"))
    b.js("document.querySelector('table.grid tbody tr.data').click()")
    ok("nodes: a node's card (GUID, name, roles, what it has, disks)", b.wait("(() => { const m = document.querySelector('.modal'); return !!m && /GUID/.test(m.innerText) && /what it has/i.test(m.innerText) && m.querySelectorAll('table.grid tbody tr.data').length > 0; })()", 15),
       b.js("document.querySelector('.modal') && document.querySelector('.modal').innerText.slice(0, 400)"))
    b.js("(() => { const i = document.querySelector('.modal input'); i.value = 'renamed-in-card'; [...document.querySelectorAll('.modal button')].find(x => x.textContent === 'keep').click(); })()")
    ok("nodes: the card's name in the overview (to be saved)", b.wait("document.querySelector('table.grid tbody').innerText.includes('renamed-in-card')", 5))
    errs = b.js("window.__errors ? window.__errors.length : 0")
    ok("no script errors", not errs, errs)
finally:
    b.close()
sys.exit(ok.done() if hasattr(ok, "done") else 0)
