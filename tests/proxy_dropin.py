"""The proxy module (apptab generalised, D-U7) end to end, on a drop-in
instance with its https listener and a local upstream:

    python3 -I tests/proxy_dropin.py <http://host:port> <https port> <admin> <password> <user> <password>

The user has access to `apptab` (from osada 0.1's flag, at the import), the
admin not; the admin adds a proxy `graf` (graf.test) and gives themselves
access. The names resolve to 127.0.0.1 (curl's --resolve); the upstream
answers "UPSTREAM-OK".
"""
import json
import subprocess
import sys
import time
import urllib.request
import http.cookiejar

U, TLS, ADM, APW, USR, UPW = sys.argv[1:7]
failed = []


def expect(what, got, want):
    ok = got == want
    print(("ok   " if ok else "FAIL ") + what + ("" if ok else f": got {got!r}, want {want!r}"))
    if not ok:
        failed.append(what)


def client():
    return urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))


def call(op, path, body=None, method=None):
    req = urllib.request.Request(U + path, data=None if body is None else json.dumps(body).encode(),
                                 headers={"x-shelf": "1", "content-type": "application/json"},
                                 method=method or ("GET" if body is None else "POST"))
    try:
        with op.open(req, timeout=15) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def https(url, cookie=None):
    """GET over https at a proxy's name: (status, Set-Cookie pass, body)."""
    host = url.split("/")[2]
    url = url.replace(host, f"{host}:{TLS}", 1)
    cmd = ["curl", "-sk", "-D", "-", "--resolve", f"{host}:{TLS}:127.0.0.1", url]
    if cookie:
        cmd += ["-H", f"cookie: {cookie}"]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=20).stdout.replace("\r\n", "\n")
    head, _, body = out.partition("\n\n")
    status = int(head.split()[1]) if head else 0
    passes = [l.split(":", 1)[1].strip().split(";")[0] for l in head.split("\n") if l.lower().startswith("set-cookie:") and "shelf_apptab=" in l]
    return status, (passes[0] if passes else None), body


def enter(op, name):
    s, t = call(op, f"/api/apptab/ticket?name={name}", {})
    if s != 200:
        return s, None
    s, p, _ = https(t["url"])
    return s, p


adm, usr = client(), client()
expect("admin signs in", call(adm, "/api/signin", {"name": ADM, "password": APW})[0], 200)
expect("user signs in", call(usr, "/api/signin", {"name": USR, "password": UPW})[0], 200)

# apptab, from osada 0.1's environment and flag
s, me = call(usr, "/api/me")
expect("user: a tab for apptab", [p["name"] for p in me.get("proxies", [])], ["apptab"])
expect("admin: no apptab tab", "apptab" in [x["name"] for x in call(adm, "/api/me")[1].get("proxies", [])], False)
expect("admin: no ticket", call(adm, "/api/apptab/ticket?name=apptab", {})[0], 403)
s, mv = enter(usr, "apptab")
expect("user: the ticket makes a pass", (s, bool(mv)), (303, True))
s, _, body = https("https://mv.test/", mv)
expect("user: the upstream through apptab", (s, "UPSTREAM-OK" in body), (200, True))
expect("no pass: refused", https("https://mv.test/")[0], 401)

# a proxy of its own, from the admin portal
s, ov = call(adm, "/api/admin/overview")
p = ov["placement"]
p["modules"]["graf"] = {"kind": "proxy", "title": "Graphs", "node": ov["me"], "host": "graf.test", "upstream": "http://127.0.0.1:18999"}
expect("admin adds the proxy graf", call(adm, "/api/admin/placement", p)[0], 200)
s, acc = call(adm, "/api/account")
expect("admin gets access to graf", call(adm, "/api/admin/access", {"module": "graf", "subject": {"kind": "user", "id": acc["user"]["id"]}})[0], 200)
time.sleep(4)  # the module picks the proxies up every 3 s
expect("admin: a tab for graf", [x["name"] for x in call(adm, "/api/me")[1].get("proxies", [])], ["graf"])
s, gp = enter(adm, "graf")
expect("admin: a pass for graf", (s, bool(gp)), (303, True))
s, _, body = https("https://graf.test/", gp)
expect("admin: the upstream through graf", (s, "UPSTREAM-OK" in body), (200, True))
expect("graf's pass is nothing at apptab", https("https://mv.test/", gp)[0], 401)
expect("apptab's pass is nothing at graf", https("https://graf.test/", mv)[0], 401)
expect("user: no ticket for graf", call(usr, "/api/apptab/ticket?name=graf", {})[0], 403)

# signing out ends the pass
call(usr, "/api/signout", {})
expect("user signed out: the pass is over", https("https://mv.test/", mv)[0], 401)

print("FAILED: " + ", ".join(failed) if failed else "all passed")
sys.exit(1 if failed else 0)
