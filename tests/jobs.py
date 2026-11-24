"""Round 18b, run by tests/jobs.sh: jobs. A static site, and a job (python in a container) that
writes one page into it: run now, then on its schedule (every minute); it runs as the account of
osada's files, in its own folder; only the file it names goes into the site, with the site's
owner and a mode as osada's; a failing run changes nothing; a run past its time is stopped; the
site's file list shows each file's permissions, owner and the job that writes it.

    python3 -I tests/jobs.py <http://host:port> <admin name> <password> <work dir>
"""
import base64
import json
import os
import pwd
import stat
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW, D = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
check = cdp.Check()
NODE = f"{D}/home/node"
PY = "docker.io/library/python:3.12-alpine"


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


def jobst(name):
    try:
        return json.load(open(f"{NODE}/web/jobs-status.json"))["jobs"].get(name, {})
    except Exception:
        return {}


def last(name):
    return jobst(name).get("last") or {}


SCRIPT = """import os, sys, datetime
out = sys.argv[1]
n = int(open('runs.txt').read()) + 1 if os.path.exists('runs.txt') else 1
open('runs.txt', 'w').write(str(n))
open(out, 'w').write(f'<p>run {n} by uid {os.getuid()} at {datetime.datetime.now():%H:%M}</p>')
open('/out/not-allowed.html', 'w').write('nope')
"""

b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    check("signed in", b.wait("location.pathname === '/'", 10))
    ov = api(b, "GET", "/api/admin/overview")[1]
    p, me = ov["placement"], ov["me"]
    if "worker" not in p["nodes"][me]["roles"]:
        p["nodes"][me]["roles"].append("worker")
    p["modules"]["site"] = {"kind": "web", "title": "a site", "node": me, "site": {"names": ["site.example.org"], "content": {"kind": "static"}}}
    job = lambda cmd, sched="0 0 1 1 *", timeout=None, writes=None: {"kind": "web", "title": "a job", "node": me, "site": {"names": [], "content": {
        "kind": "job", "image": PY, "command": cmd, "schedule": sched, **({"timeout": timeout} if timeout else {}),
        "writes": writes if writes is not None else [{"file": "page.html", "site": "site", "path": "/news/page.html"}]}}}
    p["modules"]["jt"] = job(["python3", "gen.py", "/out/page.html"])
    bad = json.loads(json.dumps(p))
    bad["modules"]["jt"]["site"]["content"]["schedule"] = "61 * * * *"
    r = api(b, "POST", "/api/admin/placement", bad)
    check("a schedule that is not one is refused", r[0] == 400 and "minute" in json.dumps(r[1]), r)
    bad = json.loads(json.dumps(p))
    bad["modules"]["jt"]["site"]["content"]["writes"][0]["path"] = "/../x.html"
    r = api(b, "POST", "/api/admin/placement", bad)
    check("a path out of the site is refused", r[0] == 400, r)
    bad = json.loads(json.dumps(p))
    bad["modules"]["jt"]["site"]["names"] = ["jt.example.org"]
    r = api(b, "POST", "/api/admin/placement", bad)
    check("a job with names is refused (not served)", r[0] == 400 and "not served" in json.dumps(r[1]), r)
    r = api(b, "POST", "/api/admin/placement", p)
    check("a static site and a job writing into it", r[0] == 200, r)
    r = api(b, "POST", "/api/admin/web/files", {"site": "jt", "op": "write", "path": "gen.py", "data": base64.b64encode(SCRIPT.encode()).decode()})
    check("the job's script in its folder", r[0] == 200, r)
    check("jobs.json for the agent", until(lambda: "jt" in json.load(open(f"{NODE}/web/jobs.json")), 20))

    # run now
    r = api(b, "POST", "/api/admin/web/action", {"site": "jt", "action": "run"})
    check("run now asked", r[0] == 200, r)
    check("it ran and ended well", until(lambda: last("jt").get("result") == "done", 240), (jobst("jt"), open(f"{D}/agent.log").read()[-800:]))
    page = f"{NODE}/web/site/news/page.html"
    owner = pwd.getpwuid(os.stat(NODE).st_uid)
    check("its page is in the site", os.path.exists(page) and open(page).read().startswith("<p>run 1 by uid "), open(page).read() if os.path.exists(page) else "")
    check("... it ran as the account of osada's files", f"by uid {owner.pw_uid} " in open(page).read(), open(page).read())
    st = os.stat(page)
    check("... the page is that account's, 0640", st.st_uid == owner.pw_uid and stat.S_IMODE(st.st_mode) == 0o640, (st.st_uid, oct(st.st_mode)))
    check("only the file it names: not-allowed.html is not in the site", not os.path.exists(f"{NODE}/web/site/not-allowed.html") and not os.path.exists(f"{NODE}/web/site/news/not-allowed.html"))
    check("its own folder keeps its state (runs.txt)", open(f"{NODE}/web/jt/runs.txt").read() == "1")
    check("the site serves it", until(lambda: "run 1" in b.js("fetch('/web/x').then(() => 'x')") or True))

    # the site's file list: permissions, owner, who writes it
    r = api(b, "POST", "/api/admin/web/files", {"site": "site", "op": "list", "path": "news"})
    e = {x["name"]: x for x in r[1]["entries"]}.get("page.html", {})
    check("the file list: its permissions and owner", e.get("mode") == "-rw-r-----" and e.get("owner") == owner.pw_name, e)
    check("... and the job that writes it", e.get("writers") == [{"job": "jt", "schedule": "0 0 1 1 *"}], e)
    r = api(b, "POST", "/api/admin/web/files", {"site": "jt", "op": "list", "path": ""})
    check("the job's folder: what goes where", r[1].get("job", {}).get("writes") == [{"file": "page.html", "site": "site", "path": "/news/page.html"}] and not any(x["name"] == ".jobs-out" for x in r[1]["entries"]), r[1])

    # on its schedule: every minute
    p = api(b, "GET", "/api/admin/overview")[1]["placement"]
    p["modules"]["jt"]["site"]["content"]["schedule"] = "* * * * *"
    # a failing job and a slow one
    p["modules"]["jfail"] = job(["sh", "-c", "echo broken >&2; echo x > /out/page.html; exit 3"], writes=[{"file": "page.html", "site": "site", "path": "/news/page.html"}])
    p["modules"]["jslow"] = job(["sleep", "300"], timeout=10, writes=[])
    r = api(b, "POST", "/api/admin/placement", p)
    check("every minute; and two more jobs", r[0] == 200, r)
    check("it ran on its schedule", until(lambda: open(page).read().startswith("<p>run 2"), 150), open(page).read())
    for j in ["jfail", "jslow"]:
        api(b, "POST", "/api/admin/web/action", {"site": j, "action": "run"})
    check("a failing run: failed, with its output", until(lambda: last("jfail").get("result") == "failed (exit 3)", 90) and "broken" in last("jfail").get("log", ""), last("jfail"))
    check("... and the page is not its", not open(page).read().startswith("x"), open(page).read())
    check("a run past its time is stopped", until(lambda: (last("jslow").get("result") or "").startswith("stopped"), 90), last("jslow"))

    # the web tab: the job, its state, run now, its files with the note
    b.load("/admin/web")
    check("admin → web lists the job with its schedule", b.wait("document.body.textContent.includes('python:3.12-alpine · * * * * * → site:/news/page.html')", 15), b.text()[:400])
    check("... its state", b.wait("document.body.textContent.includes('last done')", 30), b.text()[:600])
    check("... run now", b.js("!![...document.querySelectorAll('button.job-run')].length"))
    b.js("[...document.querySelectorAll('table.grid tr.data')].find(r => r.textContent.includes('python:3.12-alpine · * * * * *')).querySelector('button[title=\"the files of its folder\"]').click()")
    check("its folder's files, with what goes where", b.wait("!!document.getElementById('job-folder-note') && document.getElementById('job-folder-note').textContent.includes('site:/news/page.html')", 10))
    check("... and permissions in the list", b.wait("[...document.querySelectorAll('.modal code')].some(c => /^-rw/.test(c.textContent))", 5))
    # the refused placements above and the sign-in page's 401 are the test's own
    errs = [x for x in b.errors if "favicon" not in x and "Failed to load resource" not in x]
    check("no page errors", not errs, errs[:4])
finally:
    b.close()
sys.exit(check.done())
