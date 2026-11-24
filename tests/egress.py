"""Round 19, run by tests/egress.sh: egress ("outgoing connections exit by"). On a drop-in home with
its agent, a job and a container site that exit by the node "exit" (another network namespace),
and a job that does not:

- node control refuses an exit node that is the node itself, or no node;
- the job: HTTP(S)_PROXY set; a plain request by a name only the exit node knows, and by its
  address, come from the exit node's address; a tunnel (CONNECT, what https takes) too; the exit
  node's own loopback is refused (502); the echo service sees no Proxy-* headers;
- the job without an exit node: no proxy, its connection does not come from the exit node;
- the container: its proxy works (busybox wget), and again after the container restarts (a new
  network: the agent makes the proxy anew);
- the web overview counts the connections.

    python3 -I tests/egress.py <http://host:port> <admin name> <password> <work dir>
"""
import base64
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW, D = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
check = cdp.Check()
NODE = f"{D}/home/node"
PY = "docker.io/library/python:3.12-alpine"
NGINX = "docker.io/library/nginx:alpine"
EXIT_IP = "10.78.0.2"


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


def podman(*a):
    return subprocess.run(["podman", *a], capture_output=True, text=True, timeout=120)


SCRIPT = r'''
import json, os, socket, urllib.request, urllib.error
out = {"env": {k: os.environ.get(k) for k in ("HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy")}}
def get(url):
    try:
        with urllib.request.urlopen(url, timeout=30) as r:
            return {"status": r.status, "body": json.loads(r.read())}
    except urllib.error.HTTPError as e:
        return {"status": e.code, "body": e.read().decode(errors="replace")[:300]}
    except Exception as e:
        return {"error": repr(e)}
out["by_name"] = get("http://echo.test:18995/plain?x=1")
out["by_address"] = get("http://10.78.0.2:18995/addr")
out["loopback"] = get("http://127.0.0.2:47402/")
if os.environ.get("HTTPS_PROXY"):
    try:
        s = socket.create_connection(("127.0.0.1", 17128), timeout=30)
        s.sendall(b"CONNECT echo.test:18995 HTTP/1.1\r\nHost: echo.test:18995\r\n\r\n")
        head = b""
        while b"\r\n\r\n" not in head:
            c = s.recv(1)
            if not c:
                break
            head += c
        s.sendall(b"GET /tunnel HTTP/1.1\r\nHost: echo.test:18995\r\nConnection: close\r\n\r\n")
        data = b""
        while True:
            c = s.recv(65536)
            if not c:
                break
            data += c
        out["connect"] = {"head": head.decode(errors="replace").split("\r\n")[0], "body": json.loads(data.split(b"\r\n\r\n", 1)[1])}
    except Exception as e:
        out["connect"] = {"error": repr(e)}
json.dump(out, open("result.json", "w"))
'''

b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    check("signed in", b.wait("location.pathname === '/'", 10))
    ov = until(lambda: (lambda o: o if any((n.get("name") or i) == "exit" for i, n in o["placement"]["nodes"].items()) else None)(api(b, "GET", "/api/admin/overview")[1]), 60)
    check("the exit node joined", bool(ov))
    p, me = ov["placement"], ov["me"]
    xid = next(i for i, n in p["nodes"].items() if (n.get("name") or i) == "exit")
    check("... and is linked", until(lambda: api(b, "GET", "/api/admin/overview")[1]["links"].get(xid) is not None, 60))
    if "worker" not in p["nodes"][me]["roles"]:
        p["nodes"][me]["roles"].append("worker")
    job = lambda egress: {"kind": "web", "title": "egress", "node": me, "site": {"names": [], "content": {
        "kind": "job", "image": PY, "command": ["python3", "t.py"], "schedule": "0 0 1 1 *", "writes": [], **({"egress": egress} if egress else {})}}}
    p["modules"]["eg"] = job(xid)
    p["modules"]["egdirect"] = job(None)
    for bad_exit, says in [(me, "anyway"), ("no-such-node", "no node")]:
        bad = json.loads(json.dumps(p))
        bad["modules"]["eg"]["site"]["content"]["egress"] = bad_exit
        r = api(b, "POST", "/api/admin/placement", bad)
        check(f"node control refuses the exit node {bad_exit[:8]}…", r[0] >= 400 and says in json.dumps(r[1]), r)
    r = api(b, "POST", "/api/admin/placement", p)
    check("node control takes a job with an exit node", r[0] == 200, r)
    for j in ["eg", "egdirect"]:
        until(lambda: os.path.exists(f"{NODE}/web/jobs.json") and j in json.load(open(f"{NODE}/web/jobs.json")), 30)
        r = api(b, "POST", "/api/admin/web/files", {"site": j, "op": "write", "path": "t.py", "data": base64.b64encode(SCRIPT.encode()).decode()})
        check(f"{j}: its script", r[0] == 200, r)
        r = api(b, "POST", "/api/admin/web/action", {"site": j, "action": "run"})
        check(f"{j}: run now", r[0] == 200, r)
    res = {}
    for j in ["eg", "egdirect"]:
        f = f"{NODE}/web/{j}/result.json"
        res[j] = until(lambda: json.load(open(f)), 120) or {}
        check(f"{j}: ran", bool(res[j]), open(f"{D}/agent.log").read()[-1500:])
    e = res["eg"]
    check("the job has the proxy in HTTP(S)_PROXY", (e.get("env") or {}).get("HTTPS_PROXY") == "http://127.0.0.1:17128" and (e.get("env") or {}).get("http_proxy") == "http://127.0.0.1:17128", e.get("env"))
    check("a name only the exit node knows: it comes from the exit node", (e.get("by_name") or {}).get("body", {}).get("client") == EXIT_IP, e.get("by_name"))
    check("... its path and query kept, no Proxy-* headers", (e.get("by_name") or {}).get("body", {}).get("path") == "/plain?x=1" and (e.get("by_name") or {}).get("body", {}).get("proxy_headers") == [], e.get("by_name"))
    check("by address: from the exit node", (e.get("by_address") or {}).get("body", {}).get("client") == EXIT_IP, e.get("by_address"))
    check("a tunnel (CONNECT): established, from the exit node", (e.get("connect") or {}).get("head", "").endswith("200 Connection established") and (e.get("connect") or {}).get("body", {}).get("client") == EXIT_IP, e.get("connect"))
    check("the exit node's own loopback: refused (502)", (e.get("loopback") or {}).get("status") == 502, e.get("loopback"))
    d = res["egdirect"]
    check("without an exit node: no proxy", not any((d.get("env") or {}).values()), d.get("env"))
    check("... and its connection does not come from the exit node", (d.get("by_address") or {}).get("body", {}).get("client") != EXIT_IP and "error" in (d.get("by_name") or {}), d)

    # a container site with an exit node
    p["modules"]["egweb"] = {"kind": "web", "title": "egress web", "node": me, "site": {"names": ["egweb.example.org"], "content": {"kind": "container", "image": NGINX, "port": 80, "egress": xid}}}
    r = api(b, "POST", "/api/admin/placement", p)
    check("node control takes a container with an exit node", r[0] == 200, r)
    running = lambda: podman("inspect", "--format", "{{.State.Status}}", "osada-web-egweb").stdout.strip() == "running"
    check("the container runs", until(running, 120), open(f"{D}/agent.log").read()[-1500:])
    env = podman("exec", "osada-web-egweb", "env").stdout
    check("... with the proxy in its environment", "http_proxy=http://127.0.0.1:17128" in env, env)
    wget = lambda: json.loads(podman("exec", "osada-web-egweb", "wget", "-q", "-O", "-", "-T", "20", "http://echo.test:18995/c").stdout or "{}")
    w = until(lambda: (lambda x: x if x.get("client") else None)(wget()), 30)
    check("its request goes out by the exit node", (w or {}).get("client") == EXIT_IP, w)
    podman("restart", "-t", "1", "osada-web-egweb")
    w = until(lambda: (lambda x: x if x.get("client") else None)(wget()), 40)
    check("after a restart (a new network), again", (w or {}).get("client") == EXIT_IP, (w, open(f"{D}/agent.log").read()[-800:]))
    # what the web tab shows
    sites = until(lambda: (lambda v: v if v and any(s["name"] == "egweb" and ((s.get("egress") or {}).get("stats") or {}).get("connections") for s in v["sites"]) else None)(api(b, "GET", "/api/admin/web")[1]), 20)
    st = {s["name"]: s.get("egress") for s in (sites or {}).get("sites", [])}
    check("the web overview: out by exit, counted", (st.get("egweb") or {}).get("exit_name") == "exit" and (st.get("egweb") or {}).get("stats", {}).get("connections", 0) >= 2, st)
    check("... the job's too, its failure said", (st.get("eg") or {}).get("stats", {}).get("failed", 0) >= 1 and "127.0.0.2" in ((st.get("eg") or {}).get("stats", {}).get("last_error") or ""), st.get("eg"))
    check("the job without one: none", st.get("egdirect") is None, st.get("egdirect"))
    # the web tab draws it
    b.load("/admin/web")
    check("the web tab: 'out by exit'", b.wait("document.body.textContent.includes('out by exit')", 15))
    check("no page errors", not [x for x in b.errors if "401" not in x and "placement" not in x], b.errors)
finally:
    b.close()
sys.exit(check.done())
