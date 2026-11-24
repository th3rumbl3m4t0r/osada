"""Round 18 in headless Chromium (tests/cdp.py), run by tests/office.sh: sites at /web/<site>/ on
osada's own names (a static one with headers of its own and compressed copies; an upstream one
gets no cookies of osada's, sets none, and its redirects stay under /web/<site>/), and the
office suite: "open in office" on a document's page, LibreOffice (ZetaOffice) opens it in the
browser, and its save writes it back to osada.

    python3 -I tests/office.py <http://host:port> <admin name> <password> <work dir>
"""
import http.server
import io
import json
import os
import subprocess
import sys
import threading
import time
import zipfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW, D = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
check = cdp.Check()
ECHO = 18982


class Echo(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith("/go"):
            self.send_response(302)
            self.send_header("Location", "/there")
            self.send_header("Set-Cookie", "upstream=1; Path=/")
            self.end_headers()
            return
        body = json.dumps({"path": self.path, "cookie": self.headers.get("cookie"), "authorization": self.headers.get("authorization")}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Set-Cookie", "upstream=1; Path=/")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


threading.Thread(target=http.server.ThreadingHTTPServer(("127.0.0.1", ECHO), Echo).serve_forever, daemon=True).start()


def curl(path, *args):
    out = subprocess.run(["curl", "-s", "-D", "-", "-o", "/tmp/office-body", *args, cdp.BASE + path], capture_output=True, text=True).stdout
    head = {}
    status = 0
    for line in out.replace("\r", "").split("\n"):
        if line.startswith("HTTP/"):
            status, head = int(line.split()[1]), {}
        elif ": " in line:
            k, v = line.split(": ", 1)
            head.setdefault(k.lower(), []).append(v)
    return status, head, open("/tmp/office-body", "rb").read()


def api(b, method, path, body=None):
    js = f"fetch({json.dumps(path)}, {{method: {json.dumps(method)}, headers: {{'content-type': 'application/json', 'x-shelf': '1'}}{', body: ' + json.dumps(json.dumps(body)) if body is not None else ''}}}).then(async r => [r.status, await r.json().catch(() => null)])"
    return b.js(js)


def odt(text):
    """A small OpenDocument text."""
    z = io.BytesIO()
    with zipfile.ZipFile(z, "w") as f:
        f.writestr(zipfile.ZipInfo("mimetype"), "application/vnd.oasis.opendocument.text")
        f.writestr("META-INF/manifest.xml", '<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">'
                   '<manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>'
                   '<manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>')
        f.writestr("content.xml", '<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
                   'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.2"><office:body><office:text>'
                   f'<text:p>{text}</text:p></office:text></office:body></office:document-content>')
    return z.getvalue()


b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    check("signed in", b.wait("location.pathname === '/'", 10))
    b.errors.clear()
    ov = api(b, "GET", "/api/admin/overview")[1]
    p = ov["placement"]
    me = ov["me"]
    coop = {"Cross-Origin-Opener-Policy": "same-origin", "Cross-Origin-Embedder-Policy": "require-corp"}
    # open to anyone (Round 20: not the default), so curl reaches them without a session
    p["modules"]["zetaoffice"] = {"kind": "web", "title": "office", "node": me, "site": {"names": [], "under_web": True, "open": True, "office": True, "content": {"kind": "static"}, "headers": coop}}
    p["modules"]["echo"] = {"kind": "web", "title": "echo", "node": me, "site": {"names": [], "under_web": True, "open": True, "content": {"kind": "upstream", "url": f"http://127.0.0.1:{ECHO}"}}}
    # signed in only: the default at /web/ (Round 20)
    p["modules"]["private"] = {"kind": "web", "title": "private", "node": me, "site": {"names": [], "under_web": True, "content": {"kind": "upstream", "url": f"http://127.0.0.1:{ECHO}"}}}
    p["modules"]["plain"] = {"kind": "web", "title": "plain", "node": me, "site": {"names": ["plain.example.org"], "content": {"kind": "static"}}}
    bad = json.loads(json.dumps(p))
    bad["modules"]["zetaoffice2"] = {"kind": "web", "title": "office 2", "node": me, "site": {"names": [], "under_web": True, "office": True, "content": {"kind": "static"}}}
    r = api(b, "POST", "/api/admin/placement", bad)
    check("a second office suite is refused", r[0] == 400 and "office suite already" in json.dumps(r[1]), r)
    bad = json.loads(json.dumps(p))
    bad["modules"]["zetaoffice"]["site"]["headers"] = {"Set-Cookie": "a=b"}
    r = api(b, "POST", "/api/admin/placement", bad)
    check("a site's Set-Cookie header is refused", r[0] == 400, r)
    bad = json.loads(json.dumps(p))
    bad["modules"]["plain"]["site"]["open"] = True
    r = api(b, "POST", "/api/admin/placement", bad)
    check("open to anyone without /web/ is refused", r[0] == 400 and "open to anyone" in json.dumps(r[1]), r)
    r = api(b, "POST", "/api/admin/placement", p)
    check("the sites at /web/", r[0] == 200, r)
    # signed-in only: osada checks the session first
    st, h, _ = curl("/web/private/a?b=1")
    check("signed in only: refused without a session", st == 401, (st, h))
    st, h, _ = curl("/web/private/a?b=1", "-H", "accept: text/html,application/xhtml+xml")
    check("... a page goes to sign in and comes back", st == 302 and h.get("location") == ["/signin?next=%2Fweb%2Fprivate%2Fa%3Fb%3D1"], (st, h))
    seen = b.js("fetch('/web/private/a?b=1').then(r => r.json())")
    check("... signed in: through, without osada's cookie", seen.get("path") == "/a?b=1" and seen.get("cookie") is None, seen)
    # Round 20: every answer revalidated through osada, no service worker
    cc = b.js("fetch('/web/private/a?b=1').then(r => r.headers.get('cache-control'))")
    check("... its answers: private, no-cache (each use asks osada again)", cc == "private, no-cache", cc)
    sw = b.js("fetch('/web/private/sw.js', { headers: { 'service-worker': 'script' } }).then(r => r.status)")
    check("... no service worker", sw == 403, sw)
    reg = b.js("navigator.serviceWorker ? navigator.serviceWorker.register('/web/private/sw.js', { scope: '/web/private/' }).then(() => 'registered', e => 'refused') : 'refused'")
    check("... a browser cannot register one", reg == "refused", reg)
    st, h, _ = curl("/web/echo/a")
    check("an open site keeps its own caching", st == 200 and h.get("cache-control") != ["private, no-cache"], (st, h))
    st, h, _ = curl("/web/private/a")
    check("... a signed-in one refuses without a session, uncached", st == 401 and h.get("cache-control") == ["no-store"], (st, h))

    # /web/<site>/: the redirect to its folder, its headers, the compressed copy
    st, h, _ = curl("/web/zetaoffice?x=1")
    check("/web/zetaoffice → /web/zetaoffice/", st == 301 and h.get("location") == ["/web/zetaoffice/?x=1"], (st, h))
    st, h, body = curl("/web/zetaoffice/")
    check("its page with its headers", st == 200 and b"office.js" in body and h.get("cross-origin-embedder-policy") == ["require-corp"] and h.get("cross-origin-opener-policy") == ["same-origin"], (st, h))
    br = os.path.getsize(f"{D}/home/node/web/zetaoffice/soffice.wasm.br")
    st, h, _ = curl("/web/zetaoffice/soffice.wasm", "-I", "-H", "accept-encoding: gzip, deflate, br")
    check("soffice.wasm as brotli to a browser that takes it", st == 200 and h.get("content-encoding") == ["br"] and h.get("content-length") == [str(br)] and h.get("content-type") == ["application/wasm"], (st, h))
    st, h, _ = curl("/web/zetaoffice/soffice.wasm", "-I")
    check("... plain to one that does not", st == 200 and "content-encoding" not in h and h.get("content-length") == [str(os.path.getsize(f"{D}/home/node/web/zetaoffice/soffice.wasm"))], (st, h))
    st, _, _ = curl("/web/plain/")
    check("a site not at /web/: 404 there", st == 404, st)
    # an upstream at /web/echo/: no cookie of osada's, none set back, redirects under /web/echo/
    st, h, body = curl("/web/echo/a?b=1", "-H", "cookie: osada_session=secret-session; other=1", "-H", "authorization: Bearer x")
    seen = json.loads(body or b"{}")
    check("the upstream gets the path below /web/echo/", st == 200 and seen.get("path") == "/a?b=1", (st, seen))
    check("... no cookies (its own Authorization passes)", seen.get("cookie") is None and seen.get("authorization") == "Bearer x", seen)
    check("... and sets none", "set-cookie" not in h, h)
    st, h, _ = curl("/web/echo/go")
    check("its redirect stays under /web/echo/", st == 302 and h.get("location") == ["/web/echo/there"], (st, h))

    # admin → web: the sites at /web/ and the new settings in a site's card
    b.load("/admin/web")
    check("admin → web shows /web/zetaoffice/ (the office suite)", b.wait("document.body.textContent.includes('/web/zetaoffice/') && document.body.textContent.includes('(the office suite)')", 15), b.text()[:400])
    b.js("[...document.querySelectorAll('button')].find(x => x.textContent === 'add a site').click()")
    check("a site's card: at /web/, the office suite, headers", b.wait("!!document.getElementById('site-under-web') && !!document.getElementById('site-office') && document.querySelector('.modal').textContent.includes('headers its answers carry')", 5))
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'cancel').click()")
    # the office suite: "open in office" for a document, LibreOffice opens it, saving writes it back
    r = b.js("fetch('/api/me').then(r => r.json()).then(m => m.office)")
    check("osada 0.1 knows the office suite", r == "/web/zetaoffice/", r)
    data = odt("Hello from osada")
    st = b.js(f"fetch('/api/raw/home/test.odt', {{method: 'PUT', credentials: 'same-origin', headers: {{'x-shelf': '1'}}, body: new Uint8Array({json.dumps(list(data))})}}).then(r => r.status)")
    body = b""
    check("a document in the home", st == 200, (st, body[:200]))
    etag0 = b.js("fetch('/api/raw/home/test.odt').then(r => r.headers.get('etag'))")
    b.load("/view/home/test.odt")
    href = b.wait("(() => { const a = [...document.querySelectorAll('a')].find(a => a.textContent === 'open in office'); return a ? a.getAttribute('href') : null; })()", 15)
    check("its page offers open in office", href and href.startswith("/web/zetaoffice/?") and "loc=home" in href and "test.odt" in href, href)
    # the file list: an office button next to open, for the file picked
    b.load("/files/home/")
    b.wait("[...document.querySelectorAll('table.files tbody tr')].some(r => r.textContent.includes('test.odt'))", 15)
    off = "[...document.querySelectorAll('.toolbar button')].find(x => x.textContent === 'office')"
    check("the list has an office button next to open", b.js(f"(() => {{ const o = {off}; return !!o && o.previousElementSibling.textContent === 'open' && o.disabled; }})()"))
    b.js("[...document.querySelectorAll('table.files tbody tr')].find(r => r.textContent.includes('test.odt')).click()")
    check("... on for a document picked", b.wait(f"!{off}.disabled", 5))
    b.errors.clear()
    b.js(f"{off}.click()")
    check("... it opens the office suite with it, back to the folder", b.wait("location.pathname === '/web/zetaoffice/' && new URLSearchParams(location.search).get('back') === '/files/home/'", 15), b.js("location.href"))
    check("the office page is cross-origin isolated", b.wait("window.crossOriginIsolated === true", 10))
    check("osada's header over it (its design)", b.wait("!!document.querySelector('#topbar .brand') && document.querySelector('#topbar .brand').textContent.includes('// office') && getComputedStyle(document.querySelector('#topbar')).backgroundColor !== 'rgba(0, 0, 0, 0)'", 10))
    check("back to osada: the folder", b.wait("document.getElementById('back') && document.getElementById('back').getAttribute('href') === '/files/home/'", 10), b.js("location.href + ' back=' + (document.getElementById('back') || {getAttribute: () => 'none'}).getAttribute('href')"))
    check("share: there for the owner", b.wait("!document.getElementById('share').hidden", 10))
    b.js("document.getElementById('share').click()")
    check("... its dialog: users and links", b.wait("!!document.querySelector('.dlg') && document.querySelector('.dlg').textContent.includes('shared with') && document.querySelector('.dlg').textContent.includes('create link')", 10),
          b.js("document.querySelector('.dlg') && document.querySelector('.dlg').textContent"))
    b.js("[...document.querySelectorAll('.dlg button')].find(x => x.textContent === 'create link').click()")
    check("... a link made", b.wait("[...document.querySelectorAll('.dlg td.url a')].some(a => a.href.includes('/s/'))", 10))
    links = api(b, "GET", "/api/shares/item?loc=home&path=%2Ftest.odt")[1]["links"]
    check("... in osada", len(links) == 1, links)
    b.js("[...document.querySelectorAll('.dlg button')].find(x => x.textContent === 'close').click()")
    check("... closed", b.wait("!document.querySelector('.dlg')", 5))
    t0 = time.time()
    shown = b.wait("document.getElementById('qtcanvas').style.visibility === 'visible' || document.getElementById('loading-text').className === 'bad'", 240)
    check(f"LibreOffice shows the document ({time.time() - t0:.0f} s)", shown and b.js("document.getElementById('qtcanvas').style.visibility") == "visible",
          (b.js("document.getElementById('loading-text').textContent"), b.js("document.getElementById('msg').textContent"), b.errors[:5]))
    check("save is on", b.js("!document.getElementById('save').disabled"))
    b.js("document.getElementById('save').click()")
    saved = b.wait("/saved to osada|not saved/.test(document.getElementById('msg').textContent) ? document.getElementById('msg').textContent : null", 60)
    check("saved back to osada", saved and saved.startswith("saved to osada"), (saved, b.js("document.getElementById('msg').textContent")))
    etag1 = b.js("fetch('/api/raw/home/test.odt').then(r => r.headers.get('etag'))")
    check("the file in osada changed", etag1 and etag1 != etag0, (etag0, etag1))
    raw = b.js("fetch('/api/raw/home/test.odt').then(r => r.arrayBuffer()).then(b => [...new Uint8Array(b)])")
    try:
        txt = zipfile.ZipFile(io.BytesIO(bytes(raw))).read("content.xml").decode()
    except Exception as e:  # noqa: BLE001
        txt = f"not an odt: {e}"
    check("still the document, as LibreOffice wrote it", "Hello from osada" in txt and "office:document-content" in txt, txt[:300])
    # changed in osada meanwhile: the next save asks (a confirm); cancel keeps both
    b.js("window.confirm = () => false")
    r = b.js("fetch('/api/raw/home/test.odt', {method: 'PUT', credentials: 'same-origin', headers: {'x-shelf': '1'}, body: 'changed elsewhere'}).then(r => r.status)")
    b.js("document.getElementById('msg').textContent = ''; document.getElementById('save').click()")
    saved = b.wait("/saved as|not saved|saved to osada/.test(document.getElementById('msg').textContent) && !/saving/.test(document.getElementById('msg').textContent) ? document.getElementById('msg').textContent : null", 60)
    check("a file changed in osada is not overwritten: this version saved next to it", saved and saved.startswith("saved as test (office"), saved)
    names = b.js("fetch('/api/ls/home').then(r => r.json()).then(v => (v.entries || v.items || v.files || []).map(e => e.name))")
    check("... both there", names and "test.odt" in names and any(n.startswith("test (office ") and n.endswith(".odt") for n in names), names)
    txt = b.js("fetch('/api/raw/home/test.odt').then(r => r.text())")
    check("... the one changed elsewhere kept", txt == "changed elsewhere", txt[:80] if txt else txt)
    errs = [e for e in b.errors if "favicon" not in e]
    print("page errors (for the record):", errs[:6])
finally:
    b.close()
sys.exit(check.done())
