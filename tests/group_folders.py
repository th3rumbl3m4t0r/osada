#!/usr/bin/python3 -I
"""Group folders (D-U29) through the files API of a drop-in node.

    python3 -I tests/group_folders.py http://<host:port> <admin> <password> <data folder>
"""
import http.cookiejar, json, os, sys, urllib.request

BASE, NAME, PW, DATA = sys.argv[1:5]
failed = []


def check(what, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + what + (f"\n     {detail}" if detail and not cond else ""), flush=True)
    if not cond:
        failed.append(what)


def client():
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def call(method, path, body=None, raw=False):
        data = body if isinstance(body, bytes) else (None if body is None else json.dumps(body).encode())
        req = urllib.request.Request(BASE + path, method=method, data=data, headers={"x-shelf": "1", "content-type": "application/octet-stream" if isinstance(body, bytes) else "application/json"})
        try:
            with op.open(req, timeout=20) as r:
                b = r.read()
                return r.status, (b if raw else (json.loads(b) if b.strip() else None))
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors="replace")
    return call


a = client()
s, _ = a("POST", "/api/signin", {"name": NAME, "password": PW})
check("the administrator signs in", s == 200, s)
s, me = a("GET", "/api/account")
uid = me["user"]["id"] if s == 200 else None
s, g = a("POST", "/api/admin/groups", {"name": "team", "display": "the team", "ou": 1})
check("a group made", s == 200, (s, g))
s, gs = a("POST", "/api/admin/table/groups", {"limit": 50, "params": {}})
rows = gs.get("rows", []) if isinstance(gs, dict) else []
gid = next((r.get("id") for r in rows if r.get("name") == "team"), None)
check("its id", bool(gid), gs if not gid else "")
s, v = a("POST", "/api/admin/groups/member", {"group": gid, "user": uid, "member": True})
check("the administrator put into it", s == 200, (s, v))
s, mine = a("GET", "/api/groups/mine")
check("files: the group's folder among one's places", s == 200 and any(x["id"] == gid and x["name"] == "the team" for x in mine), (s, mine))
loc = f"g{gid}"
s, v = a("POST", "/api/mkdir", {"loc": loc, "path": "notes"})
check("a folder made in it", s in (200, 201, 204), (s, v))
s, v = a("PUT", f"/api/raw/{loc}/notes/hello.txt", b"hello group")
check("a file written into it", s in (200, 201, 204), (s, v))
s, ls = a("GET", f"/api/ls/{loc}/notes")
names = [e.get("name") for e in (ls.get("entries") or ls.get("items") or [])] if isinstance(ls, dict) else ls
check("its list", s == 200 and "hello.txt" in json.dumps(ls), (s, names))
s, body = a("GET", f"/api/raw/{loc}/notes/hello.txt", raw=True)
check("read back", s == 200 and body == b"hello group", (s, body[:60] if isinstance(body, bytes) else body))
check("kept with osada's files: groups/<id>/", open(os.path.join(DATA, "groups", gid, "notes", "hello.txt"), "rb").read() == b"hello group")
s, v = a("POST", "/api/links", {"loc": loc, "path": "notes/hello.txt"})
check("shared on by a public link (D-U29's rest: tests/group_sharing.py)", s == 200 and v.get("id", 0) < 0, (s, v))
# someone not in the group
s, v = a("POST", "/api/admin/users", {"name": "outsider", "display": "outsider", "ou": 1, "password": "outsider-pass-123456"})
check("another account", s == 200, (s, v))
b = client()
s, _ = b("POST", "/api/signin", {"name": "outsider", "password": "outsider-pass-123456"})
s2, v = b("GET", f"/api/ls/{loc}")
s3, mine2 = b("GET", "/api/groups/mine")
check("not a member: no folder, not in the places", s == 200 and s2 == 404 and mine2 == [], (s, s2, mine2))
print("all passed" if not failed else f"{len(failed)} failed: " + "; ".join(failed))
sys.exit(1 if failed else 0)
