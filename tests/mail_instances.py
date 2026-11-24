"""Two mail instances (Round 18), run by tests/mail-instances.sh: mail-a (a Stalwart on the node
mail-a) and mail-b (one on mail-b); / has mail-a's node as its mail master, /emea mail-b's. A new
mailbox goes to the instance of its OU's master and is noted on it; each instance's Stalwart gets
only its own accounts; the mail tab picks the instance; a mailbox is not moved away from its mail;
an account of Stalwart's own is adopted on the instance that has it; removal with the mail on
the right one; two instances on one node are refused.

    python3 -I tests/mail_instances.py <work dir> <http://127.0.0.1:port>
"""
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

W, cdp.BASE = sys.argv[1], sys.argv[2]
check = cdp.Check()
pw = dict(l.split(": ", 1) for l in open(W + "/a/node/first-admin").read().splitlines())["password"]


def api(b, method, path, body=None):
    js = f"fetch({json.dumps(path)}, {{method: {json.dumps(method)}, headers: {{'content-type': 'application/json', 'x-shelf': '1'}}{', body: ' + json.dumps(json.dumps(body)) if body is not None else ''}}}).then(async r => [r.status, await r.json().catch(() => null)])"
    return b.js(js)


def until(f, secs=20):
    end = time.time() + secs
    while time.time() < end:
        try:
            v = f()
            if v:
                return v
        except Exception:
            pass
        time.sleep(0.4)
    return False


b = cdp.Browser()
try:
    b.load("/")
    b.wait("!!document.querySelector('#si-name')")
    b.focus("#si-name"); b.type("admin"); b.focus("#si-pw"); b.type(pw); b.key("Enter")
    check("signed in", b.wait("!!document.querySelector('#nav a[href=\"/admin\"]')", 10))
    ov = api(b, "GET", "/api/admin/overview")[1]
    p = ov["placement"]
    ids = {(n.get("name") or i): i for i, n in p["nodes"].items()}
    check("both nodes", until(lambda: len((api(b, "GET", "/api/admin/overview")[1]["placement"]["nodes"])) == 2), p["nodes"])
    p = api(b, "GET", "/api/admin/overview")[1]["placement"]
    ids = {(n.get("name") or i): i for i, n in p["nodes"].items()}
    na, nb = ids.get("mail-a"), ids.get("mail-b")
    check("the nodes by name", na and nb, ids)
    # the OU /emea, the masters for mail, two instances
    root = [k for k, o in p["ous"].items() if o.get("parent") is None][0]
    emea = str(max(int(k) for k in p["ous"]) + 1)
    p["ous"][emea] = {"name": "emea", "parent": int(root), "links": {}, "secrets": None, "tier0": False, "second_factor": False}
    p["ous"][root]["links"]["mail"] = {"node": na, "enforced": False}
    p["ous"][emea]["links"]["mail"] = {"node": nb, "enforced": False}
    p["modules"]["mail-a"] = {"kind": "mail", "title": "Mail A", "node": na, "host": "mail-a.example.org", "upstream": "tls://127.0.0.1:8443"}
    p["modules"]["mail-b"] = {"kind": "mail", "title": "Mail B", "node": nb, "host": "mail-b.example.org", "upstream": "tls://127.0.0.1:8443"}
    r = api(b, "POST", "/api/admin/placement", p)
    check("two instances, /emea's master mail-b", r[0] == 200, r)
    q = json.loads(json.dumps(p))
    q["modules"]["mail-a2"] = {"kind": "mail", "title": "Mail A2", "node": na, "host": "mail-a2.example.org", "upstream": "tls://127.0.0.1:8444"}
    r = api(b, "POST", "/api/admin/placement", q)
    check("a second instance on mail-a is refused", r[0] != 200 and "one Stalwart a node" in json.dumps(r[1]), r)

    for n, ou in [("anna", int(root)), ("bob", int(emea)), ("carl", int(emea))]:
        # a new OU's directory is made within seconds of saving it
        r = until(lambda: (lambda x: x if x[0] == 200 else None)(api(b, "POST", "/api/admin/users", {"name": n, "display": n.title(), "ou": ou, "password": "a long password 1"})), 30)
        check(f"user {n}", r and r[0] == 200, r)
    uid = {u["name"]: u["id"] for u in api(b, "POST", "/api/admin/table/users", {"limit": 50})[1]["rows"]}
    r = api(b, "POST", "/api/admin/mail/action", {"op": "mail_settings", "settings": {"template": "{user}@{user}.example.org", "make_domains": True, "quota": None, "auto": False}})
    check("the settings of new mailboxes", r[0] == 200, r)

    def state(inst):
        r = api(b, "GET", "/api/admin/mail?instance=" + inst)
        return r[1] if r[0] == 200 else {}

    def emails(inst):
        return sorted(a["email"] for a in (state(inst).get("result") or {}).get("accounts", []))

    check("mail-a answers", until(lambda: state("mail-a").get("result") is not None), state("mail-a"))
    check("mail-b answers (over the link)", until(lambda: state("mail-b").get("result") is not None, 40), state("mail-b"))
    by_ou = (state("mail-a").get("result") or {}).get("instance_by_ou", {})
    check("new mailboxes by OU: / → mail-a, /emea → mail-b", by_ou.get("/") == "mail-a" and by_ou.get("/emea") == "mail-b", by_ou)

    for n in ["anna", "bob"]:
        r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid[n]})
        check(f"{n}'s mailbox", r[0] == 200 and not r[1].get("error"), r)
    mb = lambda n: api(b, "GET", f"/api/admin/users/{uid[n]}")[1]["user"].get("mailbox") or {}
    check("anna's is on mail-a", mb("anna").get("instance") == "mail-a", mb("anna"))
    check("bob's is on mail-b", mb("bob").get("instance") == "mail-b", mb("bob"))
    check("mail-a's Stalwart has anna only", until(lambda: emails("mail-a") == ["anna@anna.example.org"]), emails("mail-a"))
    check("mail-b's Stalwart has bob only", until(lambda: emails("mail-b") == ["bob@bob.example.org"]), emails("mail-b"))
    sb = state("mail-b")
    check("mail-b's tab: bob is osada's", any(a["email"] == "bob@bob.example.org" and a.get("managed") for a in sb["result"]["accounts"]), sb)
    check("mail-a's tab: no pending bob", not any(x["address"] == "bob@bob.example.org" for x in state("mail-a")["result"]["pending"]), state("mail-a")["result"]["pending"])
    check("the user card asks bob's instance", api(b, "GET", f"/api/admin/mail?user={uid['bob']}")[1]["instance"]["name"] == "mail-b")

    # moving bob's mailbox away from its mail: refused
    r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid["bob"], "instance": "mail-a"})
    check("moving a mailbox from its mail is refused", r[0] == 400 and "not built yet" in json.dumps(r[1]), r)

    # an account of mail-a's Stalwart's own, adopted for carl (of /emea): it stays on mail-a
    r = api(b, "POST", "/api/admin/mail/action", {"op": "domain_add", "name": "example.net", "instance": "mail-a"})
    check("a domain on mail-a", r[0] == 200, r)
    dom = [d for d in state("mail-a")["result"]["domains"] if d["name"] == "example.net"]
    check("mail-a has example.net, mail-b not", dom and not any(d["name"] == "example.net" for d in state("mail-b")["result"]["domains"]))
    # the account made in Stalwart A directly (not osada's): through the node's own management call is not offered,
    # so a mailbox for carl on mail-a by the API, then osada lets go of it, then carl adopts it without naming the instance
    r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid["carl"], "address": "carl@example.net", "instance": "mail-a"})
    check("carl's mailbox on mail-a, asked for", r[0] == 200 and mb("carl").get("instance") == "mail-a", r)
    check("in mail-a's Stalwart", until(lambda: "carl@example.net" in emails("mail-a")), emails("mail-a"))
    r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_remove", "user": uid["carl"]})
    check("osada lets go of it (the account stays)", r[0] == 200 and "carl@example.net" in emails("mail-a"), r)
    r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid["carl"], "address": "carl@example.net"})
    check("adopted again without an instance: on mail-a, where its account is", r[0] == 200 and mb("carl").get("instance") == "mail-a", (r, mb("carl")))
    check("nothing made on mail-b for it", "carl@example.net" not in emails("mail-b"), emails("mail-b"))

    # the mail tab: the picker, the other instance
    b.load("/admin/mail")
    check("the instance picker", b.wait("[...document.querySelectorAll('select')].some(s => [...s.options].map(o => o.value).join() === 'mail-a,mail-b')", 15), b.text())
    b.js("(() => { const s = [...document.querySelectorAll('select')].find(s => [...s.options].map(o => o.value).join() === 'mail-a,mail-b'); s.value = 'mail-b'; s.dispatchEvent(new Event('change')); })()")
    check("mail-b's mailboxes on the page", b.wait("[...document.querySelectorAll('table.grid td')].some(t => t.textContent === 'bob@bob.example.org')", 15), b.text())
    check("anna not among them", not b.js("[...document.querySelectorAll('table.grid td')].some(t => t.textContent === 'anna@anna.example.org')"))
    check("where new mailboxes go, by OU", "/emea → mail-b" in b.text().replace("\n", " ") or "/emea" in b.text(), b.text())

    # bob's removed with his mail: gone from mail-b
    r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_remove", "user": uid["bob"], "delete": True})
    check("bob's mailbox removed with its mail", r[0] == 200, r)
    check("gone from mail-b's Stalwart", until(lambda: "bob@bob.example.org" not in emails("mail-b")), emails("mail-b"))
    check("mail-a untouched", "anna@anna.example.org" in emails("mail-a"))
    r = api(b, "POST", "/api/admin/mail/action", {"op": "reconcile"})
    check("a reconcile over both, no errors", r[0] == 200 and not r[1].get("errors"), r)
finally:
    b.close()
sys.exit(check.done())
