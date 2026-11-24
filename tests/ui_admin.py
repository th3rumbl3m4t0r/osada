"""The admin pages in headless Chromium (tests/cdp.py): every tab, the tables
(toolbar, filter, quick filters, sorting), the pickers (typing, 🔍), a user
card, the OU listing, the log table; and no script errors anywhere.

    python3 -I tests/ui_admin.py <data dir> <http://host:port>
"""
import base64
import hmac
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

D, cdp.BASE = sys.argv[1], sys.argv[2]
pw = dict(l.split(": ", 1) for l in open(D + "/first-admin").read().splitlines())["password"]
fails = []


def ok(name, cond, extra=""):
    print(("ok   " if cond else "FAIL ") + name + ("" if cond else f"  {extra}"))
    if not cond:
        fails.append(name)


b = cdp.Browser()
try:
    b.load("/")
    ok("sign-in form", b.wait("!!document.querySelector('#si-name')"))
    b.focus("#si-name")
    b.type("admin")
    b.focus("#si-pw")
    b.type(pw)
    b.key("Enter")
    ok("signed in", b.wait("!!document.querySelector('#nav a[href=\"/admin\"]')"))

    for tab in ["nodes", "ous", "collections", "users", "logs"]:
        b.load("/admin/" + tab)
        ok(f"{tab}: tables drawn", b.wait("document.querySelectorAll('.gridbox table.grid').length > 0", 8))
        same = b.js("[...document.querySelectorAll('.gridbox > .toolbar')].every(t => t.children[0].textContent === '▶ run' && t.children[1].textContent === 'filter')")
        ok(f"{tab}: every toolbar alike (▶ run, filter)", same)

    b.load("/admin/users")
    b.wait("document.querySelectorAll('.gridbox table.grid tbody tr.data').length > 0")
    b.js("document.querySelector('form .refpick input').focus()")
    b.type("/")
    ok("picker: matches while typing", b.wait("document.querySelectorAll('form .refpick .dd .opt[data-i]').length > 0"))
    ok("picker: the list is only its options", b.js("!document.querySelector('form .refpick .dd').textContent.includes('null') && !document.querySelector('form .refpick .dd').textContent.includes('[object')"))
    ok("no stray text on the page", b.js("![...document.querySelector('#app').childNodes].some(n => n.nodeType === 3 && n.textContent.trim())"))
    b.key("ArrowDown")
    b.key("Enter")
    ok("picker: arrow and Enter pick", b.js("document.querySelector('form .refpick input').classList.contains('haspick')"))
    b.js("document.querySelector('form .refpick button').click()")
    ok("picker: 🔍 opens a table of all", b.wait("!!document.querySelector('#ovl .pickmodal table.grid tbody tr.data')"))
    ok("picker: that table has quick filters", b.js("!!document.querySelector('#ovl table.grid tr.qrow input')"))
    b.js("document.querySelector('#ovl table.grid tbody tr.data').click()")
    ok("picker: a row picks and closes", b.wait("!document.querySelector('#ovl')") and b.js("document.querySelector('form .refpick input').value === '/'"))
    b.js("const f = document.querySelector('form'); f.querySelector('[name=name]').value = 'zoe'; f.querySelector('[name=password]').value = 'zoe password'; f.requestSubmit()")
    ok("a user made through the form", b.wait("[...document.querySelectorAll('table.grid td')].some(td => td.textContent === 'zoe')", 8))

    b.js("document.querySelector('.gridbox table.grid tr.qrow input').focus()")
    b.type("zoe")
    b.key("Enter")
    ok("quick filter: Enter moves it into the builder, the node filters", b.wait("!!document.querySelector('.gridbox .fbuilder') && document.querySelector('.gridbox .toolbar .dim').textContent.endsWith('of 1 rows')"))
    b.js("document.querySelector('.gridbox table.grid th').click()")
    ok("sorting: a header click", b.wait("!!document.querySelector('.gridbox table.grid th .arrow')"))
    b.js("[...document.querySelectorAll('.gridbox table.grid tbody tr.data')].find(tr => tr.textContent.includes('zoe')).click()")
    ok("user card: opens, its tables drawn", b.wait("location.pathname.startsWith('/admin/users/') && document.querySelectorAll('.gridbox').length >= 3", 8))
    ok("user card: a group picker", b.js("!!document.querySelector('.refpick input[placeholder^=\"type a group\"]')"))

    # A module instance, added in the modules tab and saved with node control.
    b.load("/admin/modules")
    ok("modules: the tab", b.wait("[...document.querySelectorAll('.titlebar')].some(t => t.textContent.startsWith('add a module'))"))
    b.js("const f = [...document.querySelectorAll('form')].pop(); f.querySelector('[name=name]').value = 'wiki-lab'; f.querySelector('[name=title]').value = 'Lab wiki'")
    b.js("[...document.querySelectorAll('form')].pop().querySelector('.refpick input').focus()")
    b.type(b.js("document.title.split(' · ').pop()"))  # this node's name
    b.wait("document.querySelectorAll('form .refpick .dd .opt[data-i]').length === 1")
    b.key("Enter")
    b.js("[...document.querySelectorAll('form')].pop().requestSubmit()")
    ok("modules: added, to be saved", b.wait("!!document.querySelector('.bar-save')"))
    b.js("document.querySelector('.bar-save button.primary').click()")
    ok("modules: saved in node control", b.wait("!document.querySelector('.bar-save') && [...document.querySelectorAll('table.grid td')].some(td => td.textContent === 'wiki-lab')", 8))

    # A group, then a mass update: two picked users into it.
    b.load("/admin/users")
    b.wait("document.querySelectorAll('.gridbox table.grid tbody tr.data').length > 1")
    b.js("[...document.querySelectorAll('form')][1].querySelector('[name=name]').value = 'lab'")
    b.js("[...document.querySelectorAll('form')][1].querySelector('.refpick input').focus()")
    b.type("/")
    b.wait("document.querySelectorAll('form .refpick .dd .opt[data-i]').length > 0")
    b.key("ArrowDown")
    b.key("Enter")
    b.js("[...document.querySelectorAll('form')][1].requestSubmit()")
    ok("a group made", b.wait("[...document.querySelectorAll('table.grid td')].some(td => td.textContent === 'lab')", 8))
    for i in range(2):  # a pick redraws the table: look the box up again each time
        b.js(f"document.querySelector('.gridbox table.grid tbody').querySelectorAll('td.pick input')[{i}].click()")
    ok("users: two picked", b.wait("document.querySelector('.gridbox .toolbar .dim').textContent.includes('2 picked')"))
    b.js("[...document.querySelectorAll('.gridbox .toolbar button')].find(x => x.textContent.startsWith('mass update')).click()")
    ok("mass update: the dialog", b.wait("!!document.querySelector('#ovl .modal')"))
    b.js("document.querySelector('#ovl .refpick input').focus()")
    b.type("lab")
    # the matches for "lab" (not a list from before the typing paused)
    b.wait("(o => o.length > 0 && o.every(x => x.textContent.includes('lab')))([...document.querySelectorAll('#ovl .refpick .dd .opt[data-i]')])")
    b.key("ArrowDown")
    b.key("Enter")
    b.js("[...document.querySelectorAll('#ovl button')].find(x => x.textContent === 'run').click()")
    ok("mass update: the job runs to its end", b.wait("document.querySelector('#ovl') && document.querySelector('#ovl').textContent.includes('2 done')", 10))
    b.js("document.querySelector('#ovl .mhead .x').click()")
    ok("users: member of shows it", b.wait("[...document.querySelector('.gridbox table.grid tbody').querySelectorAll('td')].filter(td => td.textContent === 'lab').length === 2", 8),
       b.js("[...document.querySelector('.gridbox table.grid tbody').querySelectorAll('tr')].map(tr => tr.textContent).join(' | ')"))

    # Access to the module, given on a user card.
    b.js("[...document.querySelectorAll('.gridbox table.grid tbody tr.data')].find(tr => tr.textContent.includes('zoe')).click()")
    b.wait("!!document.querySelector('.refpick input[placeholder^=\"type a module\"]')", 8)
    b.js("document.querySelector('.refpick input[placeholder^=\"type a module\"]').focus()")
    b.type("wiki")
    b.wait("document.querySelectorAll('.refpick .dd .opt[data-i]').length === 1")
    b.key("Enter")
    b.js("[...document.querySelectorAll('button')].find(x => x.textContent === 'give').click()")
    ok("user card: access given", b.wait("[...document.querySelectorAll('table.grid td')].some(td => td.textContent === 'wiki-lab')", 8))

    b.load("/admin/ous")
    ok("OUs: the tree", b.wait("!!document.querySelector('.tree a.tree-item.on')"))
    ok("OUs: what is in the OU", b.wait("[...document.querySelectorAll('.win .titlebar')].some(t => t.textContent.startsWith('in /')) && [...document.querySelectorAll('table.grid td')].some(td => td.textContent === 'zoe')"))

    b.load("/admin/logs")
    ok("logs: entries", b.wait("document.querySelectorAll('.gridbox table.grid tbody tr.data').length > 0", 8))
    b.js("document.querySelector('.gridbox .toolbar').children[1].click()")
    ok("logs: filter opens the builder", b.wait("!!document.querySelector('.gridbox .fbuilder')"))
    b.js("[...document.querySelectorAll('.gridbox table.grid tbody tr.data')].find(tr => tr.textContent.includes('sign_in')).click()")
    ok("logs: an entry in full", b.wait("!!document.querySelector('#ovl .modal')"))
    b.js("[...document.querySelectorAll('#ovl button')].find(x => x.textContent.startsWith('everything of this session')).click()")
    ok("logs: narrowed to its session", b.wait("[...document.querySelectorAll('.gridbox .fbuilder select')].some(s => s.value === 'session')", 8))
    ok("logs: the session's entries only", b.wait("document.querySelectorAll('.gridbox table.grid tbody tr.data').length > 0"))

    # TOTP: set up on the account page, then asked for at sign-in.
    def totp_code(key, ahead=0):
        secret = base64.b32decode(key + "=" * (-len(key) % 8))
        mac = hmac.new(secret, (int(time.time()) // 30 + ahead).to_bytes(8, "big"), "sha1").digest()
        at = mac[-1] & 15
        return "%06d" % ((int.from_bytes(mac[at:at + 4], "big") & 0x7fffffff) % 1000000)
    b.load("/account")
    b.wait("[...document.querySelectorAll('button')].some(x => x.textContent === 'set up TOTP')")
    b.js("[...document.querySelectorAll('button')].find(x => x.textContent === 'set up TOTP').click()")
    ok("TOTP: a QR code and a key", b.wait("!!document.querySelector('.totp img.qr') && !!document.querySelector('.totp-key')"))
    key = b.js("document.querySelector('.totp-key').textContent").replace(" ", "")
    b.js(f"document.querySelector('input[placeholder=\"the code it shows\"]').value = '{totp_code(key)}'")
    b.js("[...document.querySelectorAll('button')].find(x => x.textContent === 'keep it').click()")
    ok("TOTP: on", b.wait("document.querySelector('#app').textContent.includes('TOTP is on.')", 8))
    b.js("[...document.querySelectorAll('#who button')].find(x => x.textContent === 'sign out').click()")
    b.wait("!!document.querySelector('#si-name')")
    b.focus("#si-name")
    b.type("admin")
    b.focus("#si-pw")
    b.type(pw)
    b.key("Enter")
    ok("TOTP: asked for at sign-in", b.wait("!!document.querySelector('#si-code')"))
    b.focus("#si-code")
    b.type(totp_code(key, 1))
    b.key("Enter")
    ok("TOTP: signed in with it", b.wait("!!document.querySelector('#nav a[href=\"/admin\"]')", 8))

    errors = [e for e in b.errors if "Failed to load resource" not in str(e)]
    ok("no script errors", not errors, errors[:3])
finally:
    b.close()
print("FAILED: " + ", ".join(fails) if fails else "all passed")
sys.exit(1 if fails else 0)
