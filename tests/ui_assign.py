"""Round 14 in headless Chromium, two nodes: the tabs (the setup first, a tab per module), an OU
added on the IdP tab, the edge made chat's master for it on the chat tab and saved; chat taken off
the edge on its node card (the tree says so); the IdP master of / not movable yet.
Run by tests/map-test.sh … ui_assign.py.
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
b = cdp.Browser()


def ov():
    return b.js("fetch('/api/admin/overview', {headers: {'x-shelf': '1'}}).then(r => r.json())")


def save():
    b.js("[...document.querySelectorAll('button')].find(x => x.textContent.startsWith('save')).click()")
    time.sleep(1.5)


try:
    b.load("/")
    b.wait("!!document.querySelector('#si-name')")
    b.focus("#si-name"); b.type("admin"); b.focus("#si-pw"); b.type(pw); b.key("Enter")
    check("signed in", b.wait("!!document.querySelector('#nav a[href=\"/admin\"]')", 10))
    b.load("/admin/nodes")
    tabs = b.wait("[...document.querySelectorAll('.tabs a')].map(a => a.textContent).join(' ')", 10)
    check("the setup first, a tab per module", tabs.startswith("nodes IdP files mail web chat wiki links"), tabs)
    check("no voter on the page", "voter" not in b.text("#app").lower(), b.text("#app")[:300])
    o = ov()
    edge = next(i for i, n in o["placement"]["nodes"].items() if n.get("name") == "edge" or i != o["me"])
    # an OU on the IdP tab
    b.load("/admin/idp")
    b.wait("!!document.querySelector('input[name=name]')", 10)
    b.js("document.querySelector('input[name=name]').value = 'EMEA'")
    b.js("document.querySelector('input[name=name]').closest('form').requestSubmit()")
    check("EMEA in the OU tree", b.wait("[...document.querySelectorAll('.tree a.tree-item')].some(t => t.textContent.startsWith('EMEA'))", 5))
    save()
    o = ov()
    emea = next((int(i) for i, x in o["placement"]["ous"].items() if x["name"] == "EMEA"), None)
    check("EMEA saved", emea is not None, list(o["placement"]["ous"].values()))
    # the edge chat's master for EMEA
    b.load("/admin/chat")
    # + new, then EMEA found by searching its path (Round 16)
    b.wait("!!document.querySelector('button.master-new')", 10)
    check("only / listed at first", b.js("document.querySelectorAll('table.master-tree tbody tr').length") == 1)
    b.js("document.querySelector('button.master-new').click()")
    b.focus("table.master-tree ~ .row .refpick input")
    b.type("EME")
    b.wait("[...document.querySelectorAll('.refpick .dd .opt')].some(o => o.textContent === '/EMEA')", 10)
    b.js("[...document.querySelectorAll('.refpick .dd .opt')].find(o => o.textContent === '/EMEA').dispatchEvent(new MouseEvent('mousedown', {bubbles: true}))")
    b.wait(f"!!document.querySelector('table.master-tree select[data-ou=\"{emea}\"]')", 10)
    b.js(f"(() => {{ const s = document.querySelector('table.master-tree select[data-ou=\"{emea}\"]'); s.value = {json.dumps(edge)}; s.dispatchEvent(new Event('change')); }})()")
    check("in effect for EMEA: the edge", b.wait(f"[...document.querySelectorAll('table.master-tree tr')].some(r => r.textContent.startsWith('/EMEA') && r.cells[2].textContent.startsWith('edge'))", 5),
          b.js("[...document.querySelectorAll('table.master-tree tr')].map(r => r.innerText).join(' | ')"))
    save()
    o = ov()
    check("saved: EMEA's chat on the edge", (o["placement"]["ous"][str(emea)].get("links") or {}).get("chat", {}).get("node") == edge, o["placement"]["ous"][str(emea)])
    # chat off the edge: its card; the tree says the edge has no chat
    b.load("/admin/nodes")
    b.wait("[...document.querySelectorAll('table.grid tr.data')].length >= 2", 10)
    b.js(f"[...document.querySelectorAll('table.grid tr.data')].find(r => r.textContent.includes('edge')).click()")
    b.wait("!!document.querySelector('#node-modules input[data-module=chat]')", 5)
    check("the card: the edge has chat", b.js("document.querySelector('#node-modules input[data-module=chat]').checked"))
    b.js("document.querySelector('#node-modules input[data-module=chat]').click()")
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'keep').click()")
    # within the page (the draft is not saved yet)
    b.js("document.querySelector('.tabs a[data-tab=chat]').click()")
    check("the tree: no chat on the edge any more", b.wait("[...document.querySelectorAll('table.master-tree option')].some(o => o.textContent.includes('(no chat here)'))", 10))
    # the IdP master of / is not moved yet
    r = b.js(f"""(async () => {{ const o = await (await fetch('/api/admin/overview', {{headers: {{'x-shelf': '1'}}}})).json(); const p = o.placement; p.defaults.directory = {json.dumps(edge)};
        const r = await fetch('/api/admin/placement', {{method: 'POST', headers: {{'content-type': 'application/json', 'x-shelf': '1'}}, body: JSON.stringify(p)}}); return [r.status, (await r.json()).error || ''] }})()""")
    check("the IdP master of / stays (not movable yet)", r[0] != 200 and "not built yet" in r[1], r)
    check("no page errors", not [e for e in b.errors if "401" not in e and "400" not in e], b.errors)
finally:
    b.close()
sys.exit(check.done())
