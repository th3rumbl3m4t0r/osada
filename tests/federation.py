"""Round 22: UPNs and federation, on two drop-ins A and B (tests/federation.sh).

- every account's UPN is name@<the cluster's UPN domain> (set by the admin op upn_domain); sign-in
  takes the name or the UPN (0.2's /api/signin and osada 0.1's /api/login); the account shows it;
- /.well-known/osada-federation: unauthenticated, the base, the UPN domain, the API, the key;
- who/<upn>: yes or no for one exact UPN, nothing else; 20 a minute from an address, then 429;
- unsigned and forged instance requests are refused; an instance that does not list the asking
  one refuses its offers (and the share is not left behind);
- federated both ways: the share window's step 2 finds ben@b.test on B; anna shares a file and a
  folder with him (view only); ben, at B, lists, reads (ranges too) and downloads them through B,
  nothing outside them; another of B cannot; the pages show them (headless Chromium);
- ben drops one (A is told), anna stops sharing the other (B is told); a peer removed.
"""
import http.cookiejar
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

A, PWA, B, PWB, D = sys.argv[1:6]
check = cdp.Check()
sys.stdout.reconfigure(line_buffering=True)
API = "/.well-known/osada-federation/v1"


class S:
    """A session at one instance (its cookies)."""

    def __init__(self, base):
        self.base = base
        self.op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def req(self, method, path, body=None, headers=None, raw=False):
        is_bytes = isinstance(body, bytes)
        data = None if body is None else (body if is_bytes else json.dumps(body).encode())
        h = {"x-shelf": "1", **({"content-type": "application/json"} if body is not None and not is_bytes else {}), **(headers or {})}
        r = urllib.request.Request(self.base + path, data=data, method=method, headers=h)
        try:
            with self.op.open(r, timeout=60) as resp:
                b, st, hd = resp.read(), resp.status, {k.lower(): v for k, v in resp.headers.items()}
        except urllib.error.HTTPError as e:
            b, st, hd = e.read(), e.code, {k.lower(): v for k, v in e.headers.items()}
        if raw:
            return st, b, hd
        try:
            v = json.loads(b) if b else None
        except Exception:
            v = b.decode(errors="replace")
        return st, v

    def signin(self, name, pw):
        return self.req("POST", "/api/signin", {"name": name, "password": pw})


def who(s, upn):
    return s.req("GET", f"{API}/who/" + urllib.parse.quote(upn, safe=""))


a, b = S(A), S(B)
check("A: admin signs in", a.signin("admin", PWA)[0] == 200)
check("B: admin signs in", b.signin("admin", PWB)[0] == 200)
for s, n, disp in [(a, "anna", "Anna Ash"), (b, "ben", "Ben Birch")]:
    r = s.req("POST", "/api/admin/users", {"name": n, "display": disp, "ou": 1, "password": f"{n}-pass-123456"})
    check(f"user {n}", r[0] == 200, r)
r = a.req("POST", "/api/admin/mail/action", {"op": "upn_domain", "domain": "a.test"})
check("A's UPN domain: a.test", r[0] == 200, r)
r = b.req("POST", "/api/admin/mail/action", {"op": "upn_domain", "domain": "B.test"})
check("B's UPN domain: b.test", r[0] == 200, r)
r = a.req("POST", "/api/admin/mail/action", {"op": "upn_domain", "domain": "not a domain"})
check("... not a domain: refused", r[0] != 200, r)
time.sleep(1)

# ---- UPNs ----
r = S(A).signin("anna@a.test", "anna-pass-123456")
check("sign-in with the UPN", r[0] == 200, r)
r = S(A).signin("Anna@A.TEST", "anna-pass-123456")
check("... its domain in any case", r[0] == 200, r)
r = S(A).signin("anna@b.test", "anna-pass-123456")
check("... not with another domain", r[0] != 200, r)
an = S(A)
check("... and still with the name", an.signin("anna", "anna-pass-123456")[0] == 200)
r = S(A).req("POST", "/api/login", {"username": "anna@a.test", "password": "anna-pass-123456"})
check("osada 0.1's log-in takes the UPN too", r[0] == 200, r)
r = an.req("GET", "/api/account")
check("the account shows its UPN", r[1].get("upn") == "anna@a.test", r[1].get("upn"))
r = an.req("GET", "/api/me")
check("... osada 0.1's /api/me too", r[1].get("upn") == "anna@a.test", r[1] if not isinstance(r[1], dict) else r[1].get("upn"))
r = an.req("GET", "/api/people?q=anna")
check("the share window shows people by their UPN", [p["upn"] for p in r[1]["local"]] == ["anna@a.test"], r[1])

# ---- the public part ----
pub = S(A)
st, doc = pub.req("GET", "/.well-known/osada-federation")
check("discovery, unauthenticated: a osada, its base, UPN domain, API, an RS256 key",
      st == 200 and doc["software"] == "osada" and doc["base"] == A and doc["upn_domain"] == "a.test" and doc["api"] == A + API and doc["keys"][0]["alg"] == "RS256", doc)
check("who anna@a.test: yes, nothing else", who(pub, "anna@a.test") == (200, {"upn": "anna@a.test", "exists": True}))
check("who ANNA@A.TEST: yes", who(pub, "ANNA@A.TEST")[1]["exists"] is True)
check("who nobody@a.test: no", who(pub, "nobody@a.test") == (200, {"upn": "nobody@a.test", "exists": False}))
check("who anna, ann@a.test, *@a.test, anna@b.test: no (exact UPNs only)", all(who(pub, x)[1]["exists"] is False for x in ["anna", "ann@a.test", "*@a.test", "anna@b.test"]))

an.req("PUT", "/api/raw/home/hello.txt", b"hello from A")
an.req("PUT", "/api/raw/home/docs/a.txt?mkdirs=1", b"A's a")
an.req("PUT", "/api/raw/home/docs/sub/b.txt?mkdirs=1", b"A's b")
r = an.req("GET", "/api/people?q=ben@b.test")
check("no federation yet: 'none', public e-mail offered", r[1]["federation"] == "none" and r[1]["federated"] == [] and (r[1]["email"] or {}).get("address") == "ben@b.test", r[1])

st, v = S(A).req("POST", f"{API}/shares", {"id": 1, "owner": "x@b.test", "to": "anna@a.test", "name": "x", "dir": False, "secret": "s" * 40})
check("an unsigned offer: 401", st == 401, (st, v))
forged = "eyJhbGciOiJSUzI1NiIsInR5cCI6InNoZWxmLWZlZCtqd3QifQ.eyJpc3MiOiJodHRwOi8vMTI3LjAuMC4xOjE4OTIzIn0.AAAA"
st, v = S(A).req("GET", f"{API}/shares/1/ls", headers={"authorization": "Osada-Federation " + forged})
check("a forged one: refused", st in (401, 403), (st, v))

# ---- federating ----
r = a.req("POST", "/api/federation/peers", {"url": B})
check("A federates with B", r[0] == 200 and r[1]["upn_domain"] == "b.test", r)
r = a.req("POST", "/api/federation/peers", {"url": A})
check("... not with itself", r[0] == 400, r)
r = a.req("GET", "/api/federation")
p = r[1]["peers"]
check("... B does not list A yet", len(p) == 1 and p[0]["ok"] is False and "does not list" in p[0]["state"], r[1])
r = an.req("POST", "/api/shares", {"loc": "home", "path": "hello.txt", "user": "ben@b.test", "write": False})
check("sharing before B lists A: B refuses, nothing left behind",
      r[0] == 400 and "does not federate" in json.dumps(r[1]) and an.req("GET", "/api/shares/item?loc=home&path=hello.txt")[1]["federated"] == [], r)
r = b.req("POST", "/api/federation/peers", {"url": "127.0.0.1:18921"})
check("B federates with A (no scheme: https, refused)", r[0] == 400, r)
r = b.req("POST", "/api/federation/peers", {"url": A + "/.well-known/osada-federation"})
check("B federates with A (its discovery URL)", r[0] == 200, r)
r = a.req("GET", "/api/federation")
check("... both ways now", r[1]["peers"][0]["ok"] is True and r[1]["peers"][0]["state"] == "federated both ways", r[1])

time.sleep(2.1)  # the search's cache of the earlier answer is per address; a new address below
r = an.req("GET", "/api/people?q=ben@b.test")
check("search 2: ben@b.test, on B; no public e-mail", [x["upn"] for x in r[1]["federated"]] == ["ben@b.test"] and r[1]["email"] is None and r[1]["federation"] == "on", r[1])
r = an.req("GET", "/api/people?q=nobody@b.test")
check("nobody@b.test: not there; public e-mail instead", r[1]["federated"] == [] and (r[1]["email"] or {}).get("address") == "nobody@b.test", r[1])
r = an.req("POST", "/api/shares", {"loc": "home", "path": "hello.txt", "user": "ben@b.test", "write": True})
check("anna shares hello.txt with ben@b.test", r[0] == 200 and r[1].get("federated") is True, r)
r = an.req("POST", "/api/shares", {"loc": "home", "path": "docs", "user": "ben@b.test", "write": False})
check("... and the folder docs", r[0] == 200 and r[1].get("federated") is True, r)
r = an.req("POST", "/api/shares", {"loc": "home", "path": "hello.txt", "user": "nobody@b.test", "write": False})
check("someone B does not have: refused", r[0] == 400 and "nobody here is" in json.dumps(r[1]), r)
it = an.req("GET", "/api/shares/item?loc=home&path=hello.txt")[1]
check("the item lists ben@b.test on B", [(f["upn"], f["instance"]) for f in it["federated"]] == [("ben@b.test", B.split("://")[1])], it)

be = S(B)
check("ben signs in at B with his UPN", be.signin("ben@b.test", "ben-pass-123456")[0] == 200)
inc = be.req("GET", "/api/federated")[1]["incoming"]
check("ben: two shares from anna@a.test (Anna Ash)", sorted(x["name"] for x in inc) == ["docs", "hello.txt"] and all(x["from"] == "anna@a.test" and x["from_display"] == "Anna Ash" for x in inc), inc)
hi = next(x for x in inc if x["name"] == "hello.txt")
dd = next(x for x in inc if x["name"] == "docs")
st, body, hd = be.req("GET", f"/api/federated/{hi['id']}/raw", raw=True)
check("ben reads hello.txt through B", st == 200 and body == b"hello from A", (st, body[:80]))
st, body, hd = be.req("GET", f"/api/federated/{hi['id']}/raw?dl=1", raw=True)
check("... a download", st == 200 and "attachment" in hd.get("content-disposition", ""), hd)
st, body, hd = be.req("GET", f"/api/federated/{hi['id']}/raw", headers={"range": "bytes=6-9"}, raw=True)
check("... a range", st == 206 and body == b"from", (st, body))
r = be.req("GET", f"/api/federated/{dd['id']}/ls")
check("ben lists docs", r[0] == 200 and [e["name"] for e in r[1]["entries"]] == ["sub", "a.txt"], r)
r = be.req("GET", f"/api/federated/{dd['id']}/ls?path=sub")
check("... docs/sub", r[0] == 200 and [e["name"] for e in r[1]["entries"]] == ["b.txt"], r)
st, body, _ = be.req("GET", f"/api/federated/{dd['id']}/raw?path=sub/b.txt", raw=True)
check("... reads docs/sub/b.txt", st == 200 and body == b"A's b", (st, body))
r = be.req("GET", f"/api/federated/{dd['id']}/raw?path=../hello.txt")
check("... nothing outside docs", r[0] in (400, 404), r)
r = be.req("GET", f"/api/federated/{hi['id']}/ls?path=x")
check("... nothing under a file", r[0] in (400, 404), r)
r = b.req("GET", f"/api/federated/{hi['id']}/raw")
check("B's admin cannot open ben's", r[0] == 404, r)

# ---- the pages ----
cdp.BASE = B
br = cdp.Browser()
try:
    br.load("/signin")
    br.wait("!!document.querySelector('#si-name')", 10)
    check("B's sign-in page: 'user name or UPN'", "user name or UPN" in br.js("document.body.textContent"))
    # (cdp's key() sends '.' as the Delete key: the name is set, not typed)
    br.js("(() => { const i = document.querySelector('#si-name'); i.value = 'ben@b.test'; i.dispatchEvent(new Event('input')); return 1; })()")
    br.focus("#si-pw"); br.type("ben-pass-123456"); br.key("Enter")
    check("ben signs in on the page with his UPN", br.wait("location.pathname === '/'", 10))
    br.errors.clear()  # the sign-in page's /api/account before it: 401
    br.load("/shared")
    check("shared with me: hello.txt and docs from anna@a.test", br.wait("[...document.querySelectorAll('tr.fed')].filter(r => r.textContent.includes('anna@a.test')).length === 2", 15), br.js("document.body.textContent.slice(0, 400)"))
    br.js("[...document.querySelectorAll('tr.fed a')].find(x => x.textContent === 'docs/').click()")
    check("... docs opens: sub/ and a.txt", br.wait("(() => { const b = document.querySelector('.fed-browse'); return !!b && b.textContent.includes('sub/') && b.textContent.includes('a.txt'); })()", 15))
    br.js("[...document.querySelectorAll('.fed-browse a')].find(x => x.textContent === 'sub/').click()")
    check("... and sub: b.txt", br.wait("(() => { const b = document.querySelector('.fed-browse'); return !!b && b.textContent.includes('b.txt') && b.textContent.includes('docs/sub'); })()", 15))
    errs = [e for e in br.errors if "favicon" not in e]
    check("no page errors (B)", not errs, errs)
finally:
    br.close()
cdp.BASE = A
br = cdp.Browser()
try:
    br.load("/signin")
    br.wait("!!document.querySelector('#si-name')", 10)
    br.focus("#si-name"); br.type("anna"); br.focus("#si-pw"); br.type("anna-pass-123456"); br.key("Enter")
    br.wait("location.pathname === '/'", 10)
    br.errors.clear()
    br.load("/files/home/")
    br.wait("[...document.querySelectorAll('table.files tbody tr')].some(r => r.textContent.includes('hello.txt'))", 15)
    br.js("(() => { withPart('share', x => x.shareDialog('home', 'hello.txt', 'hello.txt')); return 1; })()")
    check("anna's share window: ben@b.test on B, view only", br.wait("(() => { const t = document.querySelector('.share table.share-with'); return !!t && t.textContent.includes('ben@b.test') && t.textContent.includes('view only'); })()", 10),
          br.js("(document.querySelector('.share') || document.body).textContent.slice(0, 400)"))
    br.js("(() => { const i = document.querySelector('.share .share-find'); i.value = 'ben@b.test'; i.dispatchEvent(new Event('input')); return 1; })()")
    check("... the search: 2. federated instances: ben@b.test, already shared",
          br.wait("(() => { const r = document.querySelector('.share .share-results'); return !!r && !r.hidden && r.querySelector('tr.fed') && r.querySelector('tr.fed').textContent.includes('shared'); })()", 10),
          br.js("(document.querySelector('.share .share-results') || document.body).textContent.slice(0, 400)"))
    errs = [e for e in br.errors if "favicon" not in e]
    check("no page errors (A)", not errs, errs)
finally:
    br.close()
br = cdp.Browser()
try:
    br.load("/signin")
    br.wait("!!document.querySelector('#si-name')", 10)
    br.focus("#si-name"); br.type("admin"); br.focus("#si-pw"); br.type(PWA); br.key("Enter")
    br.wait("location.pathname === '/'", 10)
    br.errors.clear()
    br.load("/admin/federation")
    check("admin → federation: the UPN domain, B federated both ways",
          br.wait("document.body.textContent.includes('federated instances') && document.body.textContent.includes('federated both ways') && [...document.querySelectorAll('input')].some(i => i.value === 'a.test')", 20),
          br.js("document.body.textContent.slice(0, 600)"))
    errs = [e for e in br.errors if "favicon" not in e]
    check("no page errors (admin)", not errs, errs)
finally:
    br.close()

# ---- ending ----
r = be.req("DELETE", f"/api/federated/{dd['id']}")
check("ben drops docs; A is told", r[0] == 200 and r[1]["owner_told"] is True, r)
out = an.req("GET", "/api/federated")[1]["outgoing"]
check("... anna's list: hello.txt left", [x["name"] for x in out] == ["hello.txt"], out)
r = an.req("DELETE", f"/api/fedshares/{out[0]['id']}")
check("anna stops sharing hello.txt; B is told", r[0] == 200 and r[1]["recipient_told"] is True, r)
check("... ben's list is empty", be.req("GET", "/api/federated")[1]["incoming"] == [])
check("... it no longer opens", be.req("GET", f"/api/federated/{hi['id']}/raw")[0] == 404)
r = b.req("DELETE", "/api/federation/peers/" + urllib.parse.quote(A.split("://")[1], safe=""))
check("B stops federating with A", r[0] == 200, r)
r = a.req("GET", "/api/federation")
check("... A sees it", r[1]["peers"][0]["ok"] is False and "does not list" in r[1]["peers"][0]["state"], r[1])
codes = [who(S(B), f"x{i}@b.test")[0] for i in range(25)]
check("who: 20 a minute from one address, then 429", codes.count(200) <= 20 and codes[-1] == 429, codes)
sys.exit(check.done())
