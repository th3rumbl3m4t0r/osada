#!/usr/bin/python3 -I
"""I2 in headless Chromium (tests/cdp.py): admin → nodes → certificates (node control's
certs: the grid with each one's state from the nodes, the card to add one) and a node's
card with its link address. Against a cluster with a certificate issued and copied
(e.g. the test cluster at test.example.net); changes nothing (node control is not saved).

    python3 -I tests/ui_certs.py <base URL> <admin name> <cert name> <host name> [via|direct]   (password on stdin)

via: the base URL reaches an edge, which relays (the header and admin → nodes say which node the
browser is connected to); direct: it reaches the node that answers (home on the LAN).
"""
import os
import sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, CERT, HOST = sys.argv[1:5]
HOW = sys.argv[5] if len(sys.argv) > 5 else "via"
PW = sys.stdin.read().strip()
fails = []


def ok(name, cond, extra=""):
    print(("ok   " if cond else "FAIL ") + name + ("" if cond else f"  {extra}"))
    if not cond:
        fails.append(name)


WIN = "[...document.querySelectorAll('.win')].find(w => w.querySelector('.titlebar').textContent.includes(%r))"
CW = WIN % "certificates for the public names"

b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    ok("signed in through the edge", b.wait("location.pathname === '/'", 15), b.js("location.href"))
    b.errors.clear()  # the sign-in page's own 401 for /api/account
    head = "(document.querySelector('.nodename') || {}).textContent || ''"
    b.wait(f"({head}).includes('//')", 10)
    if HOW == "via":
        ok("header: the edge connected to, then the node that answers", b.js(f"/\\/\\/ \\S+ → \\S+/.test({head})"), b.js(head))
    else:
        ok("header: the node connected to, no arrow", b.js(f"/\\/\\/ \\S+$/.test(({head}).trim()) && !({head}).includes('→')"), b.js(head))
    b.load("/admin/nodes")
    b.wait("!!document.querySelector('#you-are-here')", 15)
    here = b.js("document.querySelector('#you-are-here').textContent")
    ok("nodes: where you are connected", ("relays to" in here) == (HOW == "via"), here)
    rows = "[...document.querySelectorAll('.gridbox tbody tr')].filter(r => r.textContent.includes('you are connected here')).map(r => r.querySelector('b').textContent)"
    marked = b.js(rows)
    ok("... one row marked 'you are connected here', the right one", len(marked) == 1 and (marked[0] in here), f"{marked} / {here}")
    ok("nodes: the certificates window", b.wait(f"!!({CW})", 15))
    ok(f"... {CERT} for {HOST}", b.wait(f"({CW}).textContent.includes({HOST!r})", 10), b.js(f"({CW}) && ({CW}).textContent.slice(0, 300)"))
    ok("... its state: issued, on every node that should have it",
       b.wait(f"/issued \\d{{4}}-\\d\\d-\\d\\d, on (\\d+) of \\1 nodes/.test(({CW}).textContent)", 20), b.js(f"({CW}).textContent.slice(0, 500)"))
    # the card for a new one: checked before it is kept
    b.js(f"[...({CW}).querySelectorAll('button')].find(x => x.textContent === 'add a certificate').click()")
    ok("add a certificate: its card", b.wait("[...document.querySelectorAll('.modal .mhead')].some(t => t.textContent.includes('a new certificate'))", 5))
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'keep').click()")
    ok("... without host names it says so", b.wait("[...document.querySelectorAll('.modal .msg.bad')].some(m => m.textContent.includes('host name'))", 5), b.js("[...document.querySelectorAll('.modal .msg')].map(m => m.className + ':' + m.textContent).join(' | ')"))
    b.js("document.querySelector('.modal textarea').value = 'ui-check.example.org'")
    ok("... its name is a free one", b.js("document.querySelector('.modal input').value") == "cert-2", b.js("document.querySelector('.modal input').value"))
    b.js("document.querySelector('.modal input').value = 'ui-check'")
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'keep').click()")
    ok("... kept: a row in the grid, node control unsaved", b.wait(f"({CW}).textContent.includes('ui-check.example.org') && ({CW}).textContent.includes('not issued yet')", 5),
       b.js(f"({CW}).textContent.slice(0, 600)"))
    # a node's card: its link address (learnt by the dial list)
    b.load("/admin/nodes")
    b.wait(f"!!({CW})", 15)
    ok("reload: the unsaved certificate is gone", not b.js(f"({CW}).textContent.includes('ui-check.example.org')"))
    b.js("[...document.querySelectorAll('.gridbox tbody tr')].find(r => /^edge\\b/.test(r.textContent.trim())).click()")
    ok("a node's card: its link", b.wait("[...document.querySelectorAll('.pickmodal input')].some(i => /:7441$/.test(i.value))", 5),
       b.js("[...document.querySelectorAll('.pickmodal input')].map(i => i.value).join(' | ')"))
    ok("no page errors", not b.errors, b.errors)
finally:
    b.close()
print("FAILED: " + ", ".join(fails) if fails else "all passed")
sys.exit(1 if fails else 0)
