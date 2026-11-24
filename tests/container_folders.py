"""Round 18, run by tests/container_folders.sh: a site's container (nginx) with a osada folder.
A public link's folder is given to it read only, its files are what the site serves (at
/web/cf-nginx/), the container cannot write there; the link removed: the container stops, and
says why; a user's folder instead: it runs again. Through the admin API, in headless Chromium.

    python3 -I tests/container_folders.py <http://host:port> <admin name> <password> <work dir>
"""
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW, D = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
check = cdp.Check()
SITE = "cf-nginx"


def api(b, method, path, body=None):
    js = f"fetch({json.dumps(path)}, {{method: {json.dumps(method)}, headers: {{'content-type': 'application/json', 'x-shelf': '1'}}{', body: ' + json.dumps(json.dumps(body)) if body is not None else ''}}}).then(async r => [r.status, await r.json().catch(() => null)])"
    return b.js(js)


def until(f, secs=60):
    end = time.time() + secs
    while time.time() < end:
        try:
            v = f()
            if v:
                return v
        except Exception:
            pass
        time.sleep(1)
    return False


def get(path):
    r = subprocess.run(["curl", "-s", "-w", "\n%{http_code}", cdp.BASE + path], capture_output=True, text=True).stdout
    body, _, code = r.rpartition("\n")
    return int(code or 0), body


def status():
    try:
        return json.load(open(f"{D}/home/node/web/containers-status.json"))["sites"].get(SITE, {})
    except Exception:
        return {}


b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    check("signed in", b.wait("location.pathname === '/'", 10))
    put = lambda path, text: b.js(f"fetch({json.dumps('/api/raw/home' + path + '?mkdirs=1')}, {{method: 'PUT', credentials: 'same-origin', headers: {{'x-shelf': '1'}}, body: {json.dumps(text)}}}).then(r => r.status)")
    check("a folder with a page", put("/films & more/index.html", "hello from a osada folder") == 200)
    r = api(b, "POST", "/api/links", {"loc": "home", "path": "/films & more", "days": None})
    check("a public link to it", r[0] == 200 and "/s/" in r[1].get("url", ""), r)
    token = r[1]["url"].rsplit("/s/", 1)[1].strip("/")
    link_id = next(l["id"] for l in api(b, "GET", "/api/shares/item?loc=home&path=" + "/films%20%26%20more")[1]["links"])

    ov = api(b, "GET", "/api/admin/overview")[1]
    p, me = ov["placement"], ov["me"]
    if "worker" not in p["nodes"][me]["roles"]:
        p["nodes"][me]["roles"].append("worker")
    folders = [{"share": token, "path": "/", "to": "/usr/share/nginx/html"}]
    p["modules"][SITE] = {"kind": "web", "title": "a osada folder in nginx", "node": me,
                          "site": {"names": [], "under_web": True, "content": {"kind": "container", "image": "docker.io/library/nginx:alpine", "port": 80, "folders": folders}}}
    bad = json.loads(json.dumps(p))
    bad["modules"][SITE]["site"]["content"]["folders"] = [{"share": token, "user": "admin", "path": "/", "to": "/x"}]
    r = api(b, "POST", "/api/admin/placement", bad)
    check("a folder with two sources is refused", r[0] == 400 and "one of them" in json.dumps(r[1]), r)
    r = api(b, "POST", "/api/admin/placement", p)
    check("the site with its container and the link's folder", r[0] == 200, r)
    w = until(lambda: json.load(open(f"{D}/home/node/web/containers.json")).get(SITE), 20)
    check("the node found the folder for the agent", w and w.get("binds") and w["binds"][0]["host"].endswith("/users/admin/films & more") and not w["binds"][0].get("write"), w)
    check("the container runs (podman)", until(lambda: status().get("state") == "running", 240), (status(), open(f"{D}/agent.log").read()[-600:]))
    code, body = until(lambda: (lambda x: x if x[0] == 200 else None)(get(f"/web/{SITE}/")), 30) or get(f"/web/{SITE}/")
    check("the site serves the osada folder's file", code == 200 and body.strip() == "hello from a osada folder", (code, body[:200]))
    put("/films & more/new.txt", "added in osada")
    check("a file added in osada is there at once", get(f"/web/{SITE}/new.txt")[1].strip() == "added in osada", get(f"/web/{SITE}/new.txt"))
    t = subprocess.run(["podman", "exec", f"osada-web-{SITE}", "sh", "-c", "touch /usr/share/nginx/html/x"], capture_output=True, text=True)
    check("the container cannot write there (read only)", t.returncode != 0 and "Read-only" in (t.stderr + t.stdout), t.stderr)
    avc = subprocess.run(["ausearch", "-m", "avc", "-ts", "recent", "-c", "nginx"], capture_output=True, text=True).stdout
    check("no SELinux denials for it", "denied" not in avc, avc[-400:])

    # the link removed: the container stops, and says why
    r = api(b, "DELETE", f"/api/links/{link_id}")
    check("the link removed", r[0] in (200, 204), r)
    check("the container is gone", until(lambda: status().get("state") == "missing", 30), status())
    last = (status().get("last") or {}).get("what", "")
    check("... and says why", "not run" in last and "no such link" in last, last)
    # a user's folder instead
    p = api(b, "GET", "/api/admin/overview")[1]["placement"]
    p["modules"][SITE]["site"]["content"]["folders"] = [{"user": "admin", "path": "/films & more", "to": "/usr/share/nginx/html"}]
    r = api(b, "POST", "/api/admin/placement", p)
    check("a user's folder instead", r[0] == 200, r)
    check("it runs again", until(lambda: status().get("state") == "running", 60), status())
    check("serving that folder", until(lambda: get(f"/web/{SITE}/new.txt")[1].strip() == "added in osada", 30))

    # the web tab: the folders on the site's card
    b.load("/admin/web")
    check("admin → web shows the container's folder", b.wait(f"document.body.textContent.includes('nginx:alpine + user:admin/films & more → /usr/share/nginx/html')", 15), b.text()[:500])
    b.js(f"[...document.querySelectorAll('table.grid tr.data')].find(r => r.textContent.includes('{SITE}')).click()")
    check("its card has the folders, one a line", b.wait("!!document.getElementById('site-folders') && [...document.querySelectorAll('.modal textarea')].some(t => t.value === 'user:admin/films & more /usr/share/nginx/html')", 10),
          b.js("[...document.querySelectorAll('.modal textarea')].map(t => t.value).join(' | ')"))
    b.js("(() => { const t = [...document.querySelectorAll('.modal textarea')].find(t => t.value.startsWith('user:admin')); t.value = 'http://osada.example.org/s/AbC_d-9/sub dir /media rw'; })()")
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'keep').click()")
    b.js(f"[...document.querySelectorAll('table.grid tr.data')].find(r => r.textContent.includes('{SITE}')).click()")
    f = b.wait("(() => { const t = [...document.querySelectorAll('.modal textarea')].find(t => t.value.includes('/media')); return t ? t.value : null; })()", 10)
    check("a link's address read from the card (kept as share:<token>/<sub>)", f == "share:AbC_d-9/sub dir /media rw", f)
finally:
    b.close()
sys.exit(check.done())
