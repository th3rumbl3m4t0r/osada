#!/usr/bin/python3 -I
"""T2 in headless Chromium (tests/cdp.py): admin → files has the nodes with
their roles for files and what they keep, other storage (added from the
page, its secret never shown back) and a new nightly backup's storage picker (Round 17).
Run after tests/other_storage.py (storage mock-s3 is there).

    python3 -I tests/ui_files_where.py <http://host:port> <admin name> <password>
"""
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW = sys.argv[1], sys.argv[2], sys.argv[3]
fails = []
UI = f"ui-{int(time.time()) % 100000}"  # a new name every run


def ok(name, cond, extra=""):
    print(("ok   " if cond else "FAIL ") + name + ("" if cond else f"  {extra}"))
    if not cond:
        fails.append(name)


WIN = "[...document.querySelectorAll('.win')].find(w => w.querySelector('.titlebar').textContent.includes(%r))"

b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    b.wait("location.pathname === '/'", 10)
    b.errors.clear()  # the sign-in page's own 401 for /api/account
    b.load("/admin/files")
    ok("files: the where-they-are window", b.wait(f"!!({WIN % 'where they are'})", 10))
    ok("... a row for the node with its file roles", b.wait(f"({WIN % 'where they are'}).textContent.includes('xt') && ({WIN % 'where they are'}).textContent.includes('external cache')", 10),
       b.js(f"({WIN % 'where they are'}).textContent.slice(0, 400)"))
    ok("... what the node keeps (its files, by part)", b.wait(f"/files [0-9.]+ [KMG]?B \\(/.test(({WIN % 'where they are'}).textContent)", 10),
       b.js(f"({WIN % 'where they are'}).textContent.slice(0, 600)"))
    ok("... other storage there too", b.js(f"({WIN % 'where they are'}).textContent.includes('mock-s3')"))
    ok("... no own target of the backup's any more (Round 17: it is other storage)", not b.js(f"({WIN % 'where they are'}).textContent.includes(\"the backup's own target\")"))
    # ticking a role marks node control changed (save bar); the roles are in a multi-pick (Round 14)
    b.js(f"({WIN % 'where they are'}).querySelector('[data-for^=\"node:\"] .mpick-btn').click()")
    b.js(f"[...({WIN % 'where they are'}).querySelectorAll('.mpick-row')].find(l => l.textContent === 'cache').querySelector('input').click()")
    ok("a role ticked: node control asks to be saved", b.wait("!!document.querySelector('.bar-save')", 5))
    b.js("[...document.querySelectorAll('.bar-save button')].find(x => x.textContent === 'discard').click()")
    ok("discarded", b.wait("!document.querySelector('.bar-save')", 5))
    # other storage: the list and the form
    ok("other storage: mock-s3 listed with its secret set", b.wait(f"({WIN % 'other storage (SFTP'}).textContent.includes('mock-s3') && ({WIN % 'other storage (SFTP'}).textContent.includes('set (secret_key)')", 10))
    b.js(f"[...({WIN % 'other storage (SFTP'}).querySelectorAll('button')].find(x => x.textContent === 'add other storage').click()")
    ok("add: a form", b.wait("!!document.querySelector('.modal')", 5))
    b.js("""(() => { const m = document.querySelector('.modal'); const ins = [...m.querySelectorAll('input')];
        const set = (el, v) => { el.value = v; el.dispatchEvent(new Event('input')); };
        set(ins[0], 'UINAME');
        const vis = ins.filter(i => i.offsetParent !== null);
        set(vis.find(i => i.placeholder === 'name or address'), 'nas.example.org');
        set(m.querySelectorAll('input[type=number]')[0], '2222');
        const user = [...m.querySelectorAll('.lbl')].find(l => l.textContent === 'user').nextElementSibling; set(user, 'backup');
        const pw = [...m.querySelectorAll('input[type=password]')].find(i => i.offsetParent !== null); set(pw, 'ui-secret-pw-123');
        [...m.querySelectorAll('button')].find(x => x.textContent === 'save').click(); })()""".replace('UINAME', UI))
    ok("saved: listed", b.wait(f"document.querySelector('#status').textContent.startsWith('storage {UI} saved') && ({WIN % 'other storage (SFTP'}).textContent.includes('{UI}') && ({WIN % 'other storage (SFTP'}).textContent.includes('sftp://backup@nas.example.org:2222')", 10),
       b.js("document.querySelector('#status') && document.querySelector('#status').textContent"))
    ok("... not pinned yet: test it", b.js(f"({WIN % 'other storage (SFTP'}).textContent.includes('not pinned')"))
    ok("the secret is nowhere on the page", not b.js("document.documentElement.outerHTML.includes('ui-secret-pw-123')"))
    # change: the password field is empty, says it is set
    b.js(f"[...({WIN % 'other storage (SFTP'}).querySelectorAll('tr')].find(r => r.textContent.includes('{UI}')).querySelectorAll('button')[1].click()")
    ok("change: the form says the password is set, without it", b.wait("!!document.querySelector('.modal') && [...document.querySelectorAll('.modal input[type=password]')].some(i => i.placeholder.startsWith('(set') && i.value === '')", 5))
    b.js("document.querySelector('.modal .x').click()")
    # a new nightly backup's storage picker: S3 cannot be picked, the unpinned one says to test it
    b.js("document.querySelector('[data-add-backup]').click()")
    ok("backup: add a backup, a storage picker", b.wait("!!document.querySelector('select[data-to]') && document.querySelector('select[data-to]').options.length > 1", 10))
    opts = b.js("[...document.querySelector('select[data-to]').options].map(o => o.textContent + (o.disabled ? ' [off]' : ''))")
    ok("... the storages, S3 off, the unpinned one to be tested", opts and any('mock-s3' in o and '[off]' in o for o in opts) and any(UI in o and 'test it first' in o for o in opts), opts)
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'cancel').click()")
    ok("no errors in the page", not b.errors, b.errors)
finally:
    b.close()
print("\n" + ("ALL PASS" if not fails else f"{len(fails)} FAILED: " + "; ".join(fails)))
sys.exit(1 if fails else 0)
