"""Round 14: the web nodes in the web tab: what each does (a multi-pick), the authoritative node; saved in node control. Run by tests/map-test.sh … ui_web_nodes.py."""
import os, sys, time, json
sys.path.insert(0, '/root/osada-0.2/tests')
import cdp
D, cdp.BASE = sys.argv[1], sys.argv[2]
check = cdp.Check()
pw = dict(l.split(": ", 1) for l in open(D + "/first-admin").read().splitlines())["password"]
b = cdp.Browser()
b.load("/"); b.wait("!!document.querySelector('#si-name')")
b.focus("#si-name"); b.type("admin"); b.focus("#si-pw"); b.type(pw); b.key("Enter")
b.wait("!!document.querySelector('#nav a[href=\"/admin\"]')", 10)
b.load("/admin/web")
check("web nodes drawn", b.wait("document.querySelectorAll('[data-web-node]').length === 2", 10), b.text('#app')[:400])
ov = b.js("fetch('/api/admin/overview', {headers: {'x-shelf': '1'}}).then(r => r.json())")
edge = next(i for i, n in ov["placement"]["nodes"].items() if i != ov["me"])
b.js(f"document.querySelector('[data-web-node=\"{edge}\"] .mpick-btn').click()")
b.js(f"document.querySelector('[data-web-node=\"{edge}\"] input[data-value=containers]').click()")
b.js(f"(() => {{ const s = document.getElementById('web-authority'); s.value = {json.dumps(ov['me'])}; s.dispatchEvent(new Event('change')); }})()")
b.js("[...document.querySelectorAll('button')].find(x => x.textContent.startsWith('save')).click()")
time.sleep(2)
ov = b.js("fetch('/api/admin/overview', {headers: {'x-shelf': '1'}}).then(r => r.json())")
w = ov["placement"].get("web", {})
check("saved: the edge without containers, the authority", w.get("authority") == ov["me"] and w.get("nodes", {}).get(edge) == {"edge": True, "static_files": True, "containers": False}, w)
check("the edge's cell", b.wait(f"(document.querySelector('[data-web-node=\"{edge}\"] .mpick-text') || {{}}).textContent === 'edge, static files'", 10))
check("no page errors", not [e for e in b.errors if "401" not in e], b.errors)
b.close()
sys.exit(check.done())
