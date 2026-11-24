#!/usr/bin/python3 -I
"""T3: the logs out as JSON over POST, the secret write-only, against a node
started with OSADA_SHIP_ROUND=1. A collector listens on 127.0.0.1:<port>.

    python3 -I tests/ship_logs.py http://<host:port> <admin> <password> <collector port>
"""
import http.cookiejar, http.server, json, sys, threading, time, urllib.request

BASE, NAME, PW, PORT = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
URL = f"http://127.0.0.1:{PORT}/ingest"
failed = []
got = []          # (authorization header, lines)
answer = [200]    # what the collector answers


def check(what, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + what + (f"\n     {detail}" if detail and not cond else ""), flush=True)
    if not cond:
        failed.append(what)


class Collector(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        code = answer[0]
        if code == 200:
            got.append((self.headers.get("authorization"), json.loads(body), self.headers.get("content-type")))
        self.send_response(code)
        self.send_header("content-length", "0")
        self.end_headers()

    def log_message(self, *a):
        pass


srv = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Collector)
threading.Thread(target=srv.serve_forever, daemon=True).start()


def client():
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def call(method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(BASE + path, method=method, data=data, headers={"x-shelf": "1", "content-type": "application/json"})
        try:
            with op.open(req, timeout=20) as r:
                b = r.read()
                return r.status, (json.loads(b) if b.strip() else None)
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors="replace")
    return call


def wait(cond, secs=25):
    end = time.time() + secs
    while time.time() < end:
        if cond():
            return True
        time.sleep(0.5)
    return False


def lines():
    return [l for _, b, _ in got for l in b]


a = client()
s, _ = a("POST", "/api/signin", {"name": NAME, "password": PW})
check("the administrator signs in", s == 200, s)
s, ov = a("GET", "/api/admin/overview")
settings = ov["settings"]
check("shipping off at first", not settings["ship"]["enabled"] and ov["ship_secret_set"] is False, settings.get("ship"))

for bad in ["ftp://x/", "http://example.com/in", "https://", "https://user@host/x"]:
    st = dict(settings, ship={"enabled": True, "url": bad, "secret": "", "logs": ["auth"]})
    s, v = a("POST", "/api/admin/settings", st)
    check(f"refused: {bad}", s == 400, (s, v))
st = dict(settings, ship={"enabled": True, "url": URL, "secret": "", "logs": ["access"]})
s, v = a("POST", "/api/admin/settings", st)
check("refused: a log not shipped", s == 400, (s, v))

SECRET = "first-secret-0123456789"
st = dict(settings, ship={"enabled": True, "url": URL, "secret": SECRET, "logs": ["auth", "admin"]})
s, v = a("POST", "/api/admin/settings", st)
check("shipping on, with a secret", s == 200, (s, v))
s, ov = a("GET", "/api/admin/overview")
check("the secret never comes back", ov["settings"]["ship"]["secret"] == "" and ov["ship_secret_set"] is True and SECRET not in json.dumps(ov), ov["settings"]["ship"])

check("lines arrive", wait(lambda: any(l["log"] == "admin" for l in lines()) and any(l["log"] == "auth" for l in lines())), len(lines()))
auths = {h for h, _, _ in got}
check("with the bearer token, as JSON", auths == {f"Bearer {SECRET}"} and {c for _, _, c in got} == {"application/json"}, (auths, {c for _, _, c in got}))
ls = lines()
l0 = next((l for l in ls if l["log"] == "admin" and l["event"].get("event") == "settings_changed"), None)
check("the change of the settings among them", l0 is not None, [l["event"].get("event") for l in ls][:20])
need = {"@timestamp", "id", "node", "node_name", "log", "seq", "time_ms", "actor", "event"}
check("each line whole", all(need <= set(l) for l in ls) and all(l["@timestamp"].endswith("Z") for l in ls), [sorted(set(l)) for l in ls[:2]])
check("ids unique", len({l["id"] for l in ls}) == len(ls))
check("the secret in no line", SECRET not in json.dumps(ls))
check("no session tokens", not any("token" in l["event"] for l in ls))

# a save without the secret keeps it
s, ov = a("GET", "/api/admin/overview")
st = dict(ov["settings"], session_days=ov["settings"]["session_days"])
s, v = a("POST", "/api/admin/settings", st)
n = len(got)
check("saved without the secret", s == 200, (s, v))
check("it is kept", wait(lambda: len(got) > n) and got[-1][0] == f"Bearer {SECRET}", got[-1][0] if got else None)

# the collector failing: nothing lost, status says so
answer[0] = 503
b = client()
b("POST", "/api/signin", {"name": NAME, "password": "wrong-password-xyz"})
time.sleep(4)
s, info = a("GET", "/api/admin/nodes/info")
me = ov["me"]
stt = (info.get(me) or {}).get("ship") or {}
check("the node says it is failing", stt.get("state") == "failing" and "503" in (stt.get("error") or ""), stt)
answer[0] = 200
check("then it catches up", wait(lambda: any(l["log"] == "auth" and l["event"].get("event") in ("signin_failed", "sign_in_failed") for l in lines()) or any(l["log"] == "auth" and "wrong" in json.dumps(l["event"]) for l in lines())),
      [l["event"].get("event") for l in lines() if l["log"] == "auth"][-5:])
for log in ("auth", "admin"):
    seqs = sorted(l["seq"] for l in lines() if l["log"] == log)
    check(f"{log}: no seq missing", seqs == list(range(seqs[0], seqs[0] + len(seqs))) if seqs else False, seqs[-10:])
time.sleep(3)
s, info = a("GET", "/api/admin/nodes/info")
stt = (info.get(me) or {}).get("ship") or {}
check("then ok", stt.get("state") == "ok" and stt.get("sent", 0) >= len(lines()) and all(v == 0 for v in (stt.get("backlog") or {}).values()), stt)

# a new secret, then none
s, ov = a("GET", "/api/admin/overview")
st = dict(ov["settings"])
st["ship"] = dict(st["ship"], secret="second-secret-987654")
a("POST", "/api/admin/settings", st)
n = len(got)
check("a new secret used", wait(lambda: len(got) > n) and got[-1][0] == "Bearer second-secret-987654", got[-1][0] if got else None)
st["ship"] = dict(st["ship"], secret="", clear_secret=True)
a("POST", "/api/admin/settings", st)
s, ov = a("GET", "/api/admin/overview")
n = len(got)
check("forgotten: no Authorization", ov["ship_secret_set"] is False and wait(lambda: len(got) > n) and got[-1][0] is None, (ov["ship_secret_set"], got[-1][0] if got else None))
admin_events = [l["event"] for l in lines() if l["log"] == "admin" and l["event"].get("event") == "settings_changed"]
check("the admin log says replaced/cleared, not the secret", any("secret cleared" in json.dumps(e) for e in admin_events) and "second-secret" not in json.dumps(admin_events), admin_events[-2:])

# off
st["ship"] = dict(st["ship"], enabled=False, clear_secret=False)
a("POST", "/api/admin/settings", st)
time.sleep(3)
n = len(got)
a("GET", "/api/admin/overview")
b("POST", "/api/signin", {"name": NAME, "password": "wrong-password-again"})
time.sleep(4)
s, info = a("GET", "/api/admin/nodes/info")
check("off: nothing more sent, status off", len(got) == n and ((info.get(me) or {}).get("ship") or {}).get("state") == "off", (len(got), n, (info.get(me) or {}).get("ship")))
srv.shutdown()
print("all passed" if not failed else f"{len(failed)} failed: " + "; ".join(failed))
sys.exit(1 if failed else 0)
