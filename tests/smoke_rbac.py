"""End-to-end smoke test of OUs, groups, roles and the logs through the HTTP
API, against a fresh node: `osada init` + `osada serve --http`.

    python3 -I tests/smoke_rbac.py <data dir> <http://host:port>
"""
import http.cookiejar
import json
import sys
import time
import urllib.request

D, U = sys.argv[1], sys.argv[2]
pw = dict(l.split(": ", 1) for l in open(D + "/first-admin").read().splitlines())["password"]
failed = []


def client():
    return urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))


def call(op, path, body=None):
    req = urllib.request.Request(U + path, data=None if body is None else json.dumps(body).encode(),
                                 headers={"x-shelf": "1", "content-type": "application/json"},
                                 method="GET" if body is None else "POST")
    try:
        with op.open(req, timeout=15) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def expect(what, got, want):
    ok = got == want
    print(("ok   " if ok else "FAIL ") + what + ("" if ok else f": got {got!r}, want {want!r}"))
    if not ok:
        failed.append(what)


def ou(name, parent):
    return {"name": name, "parent": parent, "links": {}, "secrets": None, "tier0": False, "second_factor": False}


adm = client()
expect("admin signs in", call(adm, "/api/signin", {"name": "admin", "password": pw})[0], 200)
p = call(adm, "/api/admin/overview")[1]["placement"]
p["ous"].update({"2": ou("EMEA", 1), "3": ou("DE", 2), "4": ou("CZ", 2)})
expect("EMEA, EMEA/DE and EMEA/CZ in one save", call(adm, "/api/admin/placement", p)[0], 200)
time.sleep(2.5)
o = call(adm, "/api/admin/overview")[1]
expect("paths", sorted(x["path"] for x in o["ous"]), ["/", "/EMEA", "/EMEA/CZ", "/EMEA/DE"])
ids = {x["path"]: x["id"] for x in o["ous"]}
expect("group de-helpdesk", call(adm, "/api/admin/groups", {"name": "de-helpdesk", "display": "DE helpdesk", "ou": ids["/EMEA/DE"]})[0], 200)
for name, where in [("hans", "/EMEA/DE"), ("jana", "/EMEA/CZ"), ("helen", "/")]:
    expect(f"user {name} in {where}", call(adm, "/api/admin/users", {"name": name, "display": name.title(), "ou": ids[where], "password": name + " password"})[0], 200)
time.sleep(0.5)
o = call(adm, "/api/admin/overview")[1]
rows = lambda op, name, body=None: call(op, f"/api/admin/table/{name}", body or {"limit": 1000})[1]["rows"]
uid = {u["name"]: u["id"] for u in rows(adm, "users")}
gid = {g["name"]: g["id"] for g in rows(adm, "groups")}
expect("the users table: a filter, a sort, a page and the count",
       (lambda p: ([u["name"] for u in p["rows"]], p["total"]))(call(adm, "/api/admin/table/users", {"filter": {"groups": [[{"col": "ou", "op": "startswith", "vals": ["/EMEA"]}]]}, "sort": "name", "desc": True, "limit": 1})[1]),
       (["jana"], 2))
expect("a regexp the node cannot read is refused", call(adm, "/api/admin/table/users", {"filter": {"groups": [[{"col": "name", "op": "regexp", "vals": ["("]}]]}})[0], 400)
expect("helen into de-helpdesk", call(adm, "/api/admin/groups/member", {"group": gid["de-helpdesk"], "user": uid["helen"], "member": True})[0], 200)
expect("de-helpdesk: user-admin over /EMEA/DE",
       call(adm, "/api/admin/grants", {"scope": ids["/EMEA/DE"], "role": "user-admin", "subject": {"kind": "group", "id": gid["de-helpdesk"]}})[0], 200)
time.sleep(0.5)

he = client()
expect("helen signs in", call(he, "/api/signin", {"name": "helen", "password": "helen password"})[0], 200)
expect("helen holds a role", call(he, "/api/account")[1]["staff"], True)
expect("helen sets hans's password (DE)", call(he, "/api/admin/users/update", {"id": uid["hans"], "password": "new hans password"})[0], 200)
expect("helen may not set jana's (CZ)", call(he, "/api/admin/users/update", {"id": uid["jana"], "password": "new jana password"})[0], 403)
op = o["placement"]
brno = {**op, "ous": {**op["ous"], "9": ou("Brno", ids["/EMEA/CZ"])}}
expect("helen may not make OUs", call(he, "/api/admin/placement", brno)[0], 403)
expect("helen sees only DE's users", sorted(u["name"] for u in rows(he, "users")), ["hans"])
expect("helen sees no collections", call(he, "/api/admin/table/collections", {})[0], 403)
expect("the objects of /EMEA, below included", sorted((r["type"], r["name"]) for r in rows(adm, "objects", {"params": {"ou": str(ids["/EMEA"]), "below": "1"}, "limit": 100}) if r["type"] != "collection"),
       [("group", "de-helpdesk"), ("role", "user-admin"), ("user", "hans"), ("user", "jana")])
expect("helen's roles, through her group", [(r["role"], r["scope"], r["via"]) for r in rows(adm, "grants", {"params": {"subject_user": uid["helen"]}})], [("user-admin", "/EMEA/DE", "group de-helpdesk")])
expect("the group's members", [u["name"] for u in rows(adm, "users", {"params": {"member_of": gid["de-helpdesk"]}})], ["helen"])
expect("helen may not grant herself admin", call(he, "/api/admin/grants", {"scope": ids["/"], "role": "admin", "subject": {"kind": "user", "id": uid["helen"]}})[0], 403)
expect("helen may not read logs", call(he, "/api/admin/logs/query", {"kind": "auth", "conds": [], "limit": 5})[0], 403)
expect("hans signs in with the new password", call(client(), "/api/signin", {"name": "hans", "password": "new hans password"})[0], 200)

card = call(adm, "/api/admin/users/" + uid["helen"])[1]
expect("helen's card: roles", [(g["role"], g["scope"], g["via"]) for g in card["grants"]], [("user-admin", "/EMEA/DE", "group de-helpdesk")])
expect("helen's card: her auth events", [l["event"]["event"] for l in card["sign_ins"]], ["sign_in", "password_changed"])

sid = call(adm, "/api/account")[1]["sid"]
q = lambda kind, field, value: call(adm, "/api/admin/logs/query", {"kind": kind, "conds": [{"field": field, "op": "is", "value": value}], "limit": 100})[1]["lines"]
admin_events = [l["event"]["event"] for l in q("admin", "session", sid)][::-1]
expect("admin log of the admin's session",
       admin_events, ["node_control_changed", "group_created", "user_created", "user_created", "user_created", "member_added", "role_granted"])
expect("auth log of the admin's session", [l["event"]["event"] for l in q("auth", "session", sid)][::-1],
       ["sign_in", "password_changed", "password_changed", "password_changed"])
hans = [(l["event"]["event"], l["event"].get("actor_name")) for l in q("auth", "user", "hans")][::-1]
expect("auth log about hans", hans, [("password_changed", "admin"), ("password_changed", "helen"), ("sign_in", "hans")])
print("FAILED: " + ", ".join(failed) if failed else "all passed")
sys.exit(1 if failed else 0)
