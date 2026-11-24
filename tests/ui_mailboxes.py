"""admin → mail with osada keeping the mailboxes (D-U43, D-U44), in headless Chromium against a
real Stalwart: the mail instance, a domain, a new mailbox from the template (its domain made with
it), its size, other addresses and a catch-all, a new address, off and on, a Stalwart-only account
adopted with its aliases, the user card, removal with the mail. Run by tests/mail-ns.sh.

    python3 -I tests/ui_mailboxes.py <data dir> <http://127.0.0.1:port>
"""
import base64
import json
import os
import sys
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

D, cdp.BASE = sys.argv[1], sys.argv[2]
check = cdp.Check()
pw = dict(l.split(": ", 1) for l in open(D + "/first-admin").read().splitlines())["password"]
SW = "http://127.0.0.1:8080"
AUTH = open(D + "/mail/stalwart.auth").read().strip()


def sw(method, args):
    def req(m, url, body=None):
        r = urllib.request.Request(url, method=m, headers={"Authorization": AUTH, "Content-Type": "application/json"}, data=None if body is None else json.dumps(body).encode())
        with urllib.request.urlopen(r, timeout=20) as f:
            return json.loads(f.read())
    s = req("GET", SW + "/jmap/session")
    args["accountId"] = s["primaryAccounts"]["urn:stalwart:jmap"]
    api = SW + "/" + s["apiUrl"].split("://", 1)[-1].split("/", 1)[1]
    return req("POST", api, {"using": ["urn:ietf:params:jmap:core", "urn:stalwart:jmap"], "methodCalls": [[method, args, "0"]]})["methodResponses"][0][1]


def accounts():
    doms = {d["id"]: d["name"] for d in sw("x:Domain/get", {"properties": ["name"]})["list"]}
    out = {}
    for a in sw("x:Account/get", {"properties": ["emailAddress", "aliases", "quotas", "permissions"]})["list"]:
        out[a["emailAddress"]] = {"aliases": sorted(f'{x["name"]}@{doms[x["domainId"]]}' for x in (a.get("aliases") or {}).values()),
                                  "quota": (a.get("quotas") or {}).get("maxDiskQuota"),
                                  "off": bool(((a.get("permissions") or {}).get("disabledPermissions") or {}).get("authenticate"))}
    return out


def catch_alls():
    return {d["name"]: d.get("catchAllAddress") for d in sw("x:Domain/get", {"properties": ["name", "catchAllAddress"]})["list"]}


def api(b, method, path, body=None):
    js = f"fetch({json.dumps(path)}, {{method: {json.dumps(method)}, headers: {{'content-type': 'application/json', 'x-shelf': '1'}}{', body: ' + json.dumps(json.dumps(body)) if body is not None else ''}}}).then(async r => [r.status, await r.json().catch(() => null)])"
    return b.js(js)


def until(what, f, secs=15):
    end = time.time() + secs
    while time.time() < end:
        try:
            v = f()
            if v:
                return v
        except Exception:
            pass
        time.sleep(0.3)
    return False


def click(b, text, scope=".modal"):
    return b.js(f"(() => {{ const x = [...document.querySelectorAll({json.dumps(scope + ' button')})].find(x => x.textContent === {json.dumps(text)}); if (x) x.click(); return !!x; }})()")


b = cdp.Browser()
try:
    b.load("/")
    b.wait("!!document.querySelector('#si-name')")
    b.focus("#si-name"); b.type("admin"); b.focus("#si-pw"); b.type(pw); b.key("Enter")
    check("signed in", b.wait("!!document.querySelector('#nav a[href=\"/admin\"]')", 10))
    # users, the mail instance on this node, a domain, and an account of Stalwart's own
    for n in ["anna", "ben"]:
        check(f"user {n}", api(b, "POST", "/api/admin/users", {"name": n, "display": n.title(), "ou": 1, "password": "a long password 1"})[0] == 200)
    ov = api(b, "GET", "/api/admin/overview")[1]
    p = ov["placement"]
    node = list(p["nodes"])[0]
    p["modules"]["mail"] = {"kind": "mail", "title": "Mail", "node": node, "host": "mail.example.org", "upstream": "tls://127.0.0.1:8443"}
    r = api(b, "POST", "/api/admin/placement", p)
    check("the mail instance", r[0] == 200, r)
    check("a domain", api(b, "POST", "/api/admin/mail/action", {"op": "domain_add", "name": "example.org"})[0] == 200)
    did = [d for d in sw("x:Domain/get", {"properties": ["name"]})["list"] if d["name"] == "example.org"][0]["id"]
    sw("x:Account/set", {"create": {"a": {"@type": "User", "name": "ben", "domainId": did, "aliases": {"0": {"name": "benny", "domainId": did}}}}})

    # the settings of new mailboxes, on the page
    b.load("/admin/mail")
    check("the mail tab", b.wait("!!document.getElementById('mail-boxes') || document.querySelectorAll('.gridbox table.grid').length >= 2", 15), b.text())
    b.js("[...document.querySelectorAll('input')].find(i => i.placeholder === '{user}@example.org').value = '{user}@{user}.ca.example.org'")
    b.js("[...document.querySelectorAll('label')].find(l => l.textContent.includes('make the address')).querySelector('input').click()")
    click(b, "save", "section")
    check("settings saved", until("settings", lambda: api(b, "GET", "/api/admin/mail")[1]["result"]["settings"]["template"] == "{user}@{user}.ca.example.org"))

    # a new mailbox for anna, by the API the page uses: from the template, its domain made
    uid = {u["name"]: u["id"] for u in api(b, "POST", "/api/admin/table/users", {"limit": 50})[1]["rows"]}
    r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid["anna"], "quota": 1 << 30})
    check("anna's mailbox", r[0] == 200 and not r[1].get("error"), r)
    check("in Stalwart, with its size", until("anna", lambda: accounts().get("anna@anna.ca.example.org", {}).get("quota") == 1 << 30), accounts())

    # the page: anna's card, other addresses and a catch-all
    b.load("/admin/mail")
    b.wait("[...document.querySelectorAll('table.grid td')].some(t => t.textContent === 'anna@anna.ca.example.org')", 15)
    b.js("[...document.querySelectorAll('table.grid tr.data')].find(r => [...r.cells].some(c => c.textContent === 'anna@anna.ca.example.org') && r.closest('table').textContent.includes('other addresses')).click()")
    check("anna's card", b.wait("!!document.querySelector('.modal textarea')", 5))
    check("its other addresses can be changed", until("enabled", lambda: b.js("!document.querySelector('.modal textarea').disabled"), 3))
    b.js("document.querySelector('.modal textarea').value = 'a@example.org\\n*@example.org'")
    click(b, "save")
    check("Stalwart has the alias", until("alias", lambda: accounts()["anna@anna.ca.example.org"]["aliases"] == ["a@example.org"]), accounts())
    check("and the catch-all", catch_alls().get("example.org") == "anna@anna.ca.example.org", catch_alls())
    check("ben's alias is refused for anna", api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid["ben"], "address": "a@example.org"})[0] == 400)

    # a new address: renamed in Stalwart
    r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid["anna"], "address": "anna@example.org"})
    check("anna's new address", r[0] == 200, r)
    acc = accounts()
    check("the account renamed, aliases kept", "anna@example.org" in acc and acc["anna@example.org"]["aliases"] == ["a@example.org"] and "anna@anna.ca.example.org" not in acc, acc)
    check("the catch-all follows", until("catch-all", lambda: catch_alls().get("example.org") == "anna@example.org"), catch_alls())
    # off, then on
    api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid["anna"], "off": True})
    check("off in Stalwart", until("off", lambda: accounts()["anna@example.org"]["off"]))
    api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid["anna"], "off": False})
    check("on again", until("on", lambda: not accounts()["anna@example.org"]["off"]))

    # ben's account of Stalwart's: suggested, adopted with its alias
    b.load("/admin/mail")
    check("adopt suggested", b.wait("[...document.querySelectorAll('button')].some(x => x.textContent === 'adopt 1')", 15), b.text())
    b.js("[...document.querySelectorAll('button')].find(x => x.textContent === 'adopt 1').click()")
    b.wait("!!document.querySelector('.modal')", 5)
    click(b, "yes")
    check("ben's mailbox adopted", until("adopted", lambda: (api(b, "GET", f"/api/admin/users/{uid['ben']}")[1]["user"].get("mailbox") or {}).get("address") == "ben@example.org"))
    api(b, "POST", "/api/admin/mail/action", {"op": "reconcile"})
    check("its alias taken from Stalwart", until("import", lambda: (api(b, "GET", f"/api/admin/users/{uid['ben']}")[1]["user"]["mailbox"].get("aliases")) == ["benny@example.org"]),
          api(b, "GET", f"/api/admin/users/{uid['ben']}")[1]["user"])
    check("Stalwart unchanged", accounts()["ben@example.org"]["aliases"] == ["benny@example.org"])

    # the user card
    b.load(f"/admin/users/{uid['anna']}")
    check("the card shows the mailbox", b.wait("(document.getElementById('mailbox-of') || {}).textContent.includes('anna@example.org (+2 other)')", 15), b.text())

    # removed with its mail: typed address
    b.js("document.getElementById('mailbox-size').click()")
    b.wait("!!document.querySelector('.modal')", 5)
    click(b, "remove…")
    b.wait("[...document.querySelectorAll('.modal button')].some(x => x.textContent === 'remove with its mail')", 5)
    click(b, "remove with its mail")
    check("not without the typed address", "anna@example.org" in accounts())
    b.js("document.querySelector('.modal input').value = 'anna@example.org'")
    click(b, "remove with its mail")
    check("gone from Stalwart", until("gone", lambda: "anna@example.org" not in accounts()), accounts())
    check("and from anna", until("no mailbox", lambda: api(b, "GET", f"/api/admin/users/{uid['anna']}")[1]["user"].get("mailbox") is None))
    # the sign-in page's 401, and the refusal asked for above
    check("no page errors", not [e for e in b.errors if "401" not in e and not ("400" in e and e.endswith("/api/admin/mail/action"))], b.errors)
finally:
    b.close()
sys.exit(check.done())
