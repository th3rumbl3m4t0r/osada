#!/usr/bin/python3 -I
"""D-U38 in headless Chromium (tests/cdp.py): admin → mail (the mail instance's Stalwart: domains
with their DNS records, on/off, add, remove; mailboxes with their osada account and size) and the
user card's mailbox with its size. Against a cluster whose mail instance has the domain <domain>
and a mailbox <user>@<domain> for the osada account <user>; leaves that mailbox's size at 1 GB
and removes the domain it adds.

    python3 -I tests/ui_mail.py <base URL> <admin name> <domain> <user>   (password on stdin)
"""
import json
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, DOMAIN, USER = sys.argv[1:5]
PW = sys.stdin.read().strip()
BOX = f"{USER}@{DOMAIN}"
NEW = f"ui-{int(time.time()) % 100000}.{DOMAIN}"
fails = []


def ok(name, cond, extra=""):
    print(("ok   " if cond else "FAIL ") + name + ("" if cond else f"  {extra}"))
    if not cond:
        fails.append(name)


WIN = "[...document.querySelectorAll('.win')].find(w => w.querySelector('.titlebar').textContent.includes(%r))"
DOMS, BOXES = WIN % "domains (", WIN % "mailboxes ("
ROW = "[...(%s).querySelectorAll('tbody tr')].find(r => r.textContent.includes(%r))"
MBTN = "[...document.querySelectorAll('.modal button')].find(x => x.textContent === %r)"


def state():
    """The mail tab's data, as the page gets it."""
    return json.loads(b.js("fetch('/api/admin/mail', {headers: {'x-shelf': '1'}}).then(r => r.text())", ) or "{}")


b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    ok("signed in", b.wait("location.pathname === '/'", 15))
    b.errors.clear()
    b.load("/admin/mail")
    ok("a mail tab", b.wait("[...document.querySelectorAll('.tabs a')].some(a => a.textContent === 'mail' && a.classList.contains('on'))", 10))
    ok("... the instance and its node", b.wait(f"!!({WIN % 'mail instance'}) && ({WIN % 'mail instance'}).textContent.includes('node')", 15),
       b.js("document.querySelector('#app').textContent.slice(0, 400)"))
    ok(f"... domain {DOMAIN}", b.wait(f"!!({DOMS}) && !!({ROW % (DOMS, DOMAIN)})", 10))
    ok(f"... mailbox {BOX}, the osada account {USER}", b.wait(f"!!({ROW % (BOXES, BOX)}) && ({ROW % (BOXES, BOX)}).textContent.includes({USER!r})", 10),
       b.js(f"({BOXES}) && ({BOXES}).textContent.slice(0, 400)"))
    # its size: 2 GB from the tab
    b.js(f"({ROW % (BOXES, BOX)}).click()")
    ok("a mailbox's size card", b.wait(f"!!({MBTN % 'set'})", 5))
    b.js("document.querySelector('.modal input').value = '2'")
    b.js(f"({MBTN % 'set'}).click()")
    ok("... 2 GB: in the grid", b.wait(f"!!({ROW % (BOXES, BOX)}) && ({ROW % (BOXES, BOX)}).textContent.includes('2 GB')", 15), b.js(f"({BOXES}) && ({ROW % (BOXES, BOX)}) && ({ROW % (BOXES, BOX)}).textContent"))
    a = [x for x in state().get("result", {}).get("accounts", []) if x["email"] == BOX]
    ok("... and in Stalwart (maxDiskQuota)", a and a[0].get("quota") == 2 * 2**30, a)
    # a domain: added, its DNS records, off, removed
    b.js(f"[...({DOMS}).querySelectorAll('button')].find(x => x.textContent === 'add a domain').click()")
    b.wait("!!document.querySelector('.modal input')", 5)
    b.js(f"document.querySelector('.modal input').value = {NEW!r}")
    b.js(f"({MBTN % 'add'}).click()")
    ok(f"add a domain: {NEW}", b.wait(f"!!({DOMS}) && !!({ROW % (DOMS, NEW)})", 15))
    b.js(f"({ROW % (DOMS, NEW)}).click()")
    ok("... its card: the DNS records Stalwart wants (DKIM)", b.wait("!!document.querySelector('.modal pre.zone') && document.querySelector('.modal pre.zone').textContent.includes('_domainkey')", 5),
       b.js("document.querySelector('.modal pre.zone') && document.querySelector('.modal pre.zone').textContent.slice(0, 200)"))
    b.js(f"({MBTN % 'turn off'}).click()")
    ok("... turned off", b.wait(f"!!({DOMS}) && !!({ROW % (DOMS, NEW)}) && ({ROW % (DOMS, NEW)}).textContent.includes('off')", 15))
    b.js(f"({ROW % (DOMS, NEW)}).click()")
    b.wait(f"!!({MBTN % 'remove'})", 5)
    b.js(f"({MBTN % 'remove'}).click()")
    b.wait(f"!!({MBTN % 'yes'})", 5)
    b.js(f"({MBTN % 'yes'}).click()")
    ok("... removed", b.wait(f"!!({DOMS}) && !({ROW % (DOMS, NEW)})", 15))
    ok("... and gone from Stalwart", not [d for d in state().get("result", {}).get("domains", []) if d["name"] == NEW])
    # the user card: the mailbox and its size
    uid = [x for x in state()["result"]["accounts"] if x["email"] == BOX][0]["user"]["id"]
    b.load(f"/admin/users/{uid}")
    ok("user card: the mailbox, its use and size", b.wait("!!document.querySelector('#mailbox-of') && document.querySelector('#mailbox-of').textContent.includes('of 2 GB')", 15),
       b.js("document.querySelector('#app').textContent.slice(0, 600)"))
    b.js("document.querySelector('#mailbox-size').click()")
    b.wait(f"!!({MBTN % 'set'})", 5)
    b.js("document.querySelector('.modal input').value = '1'")
    b.js(f"({MBTN % 'set'}).click()")
    ok("... its size from the card: 1 GB", b.wait("!!document.querySelector('#mailbox-of') && document.querySelector('#mailbox-of').textContent.includes('of 1 GB')", 15),
       b.js("document.querySelector('#mailbox-of') && document.querySelector('#mailbox-of').textContent"))
    ok("no page errors", not b.errors, b.errors)
finally:
    b.close()
print("FAILED: " + ", ".join(fails) if fails else "all passed")
sys.exit(1 if fails else 0)
