"""Round 21: the share window in headless Chromium, on a drop-in (tests/dropin-ui.sh), with a mock
JMAP server (the mail server) that records what osada sends:

    OSADA_OIDC_ISSUER=http://127.0.0.1:18989 OSADA_PUBLIC_URL=https://osada.test.example \\
    OSADA_MAIL_JMAP_URL=http://127.0.0.1:18987 tests/dropin-ui.sh target/release/osada ui_share.py

- the search is on top; it finds people of this instance by name, display name and UPN (their
  mailbox's address, else name@<osada's public host>); federated instances: none set up; an address
  nobody has is public e-mail;
- sharing with a person (can edit), shown with their UPN;
- public e-mail: a view-only link, mailed from the person's mailbox (JMAP Email/set and
  EmailSubmission/set with a token osada signed for the webmail's client, aud maildot, their address),
  listed as "public link sent to …"; the link opens without an account; a send that fails leaves
  no link; a user's own address is refused as public e-mail.
"""
import base64
import http.server
import json
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW, D = sys.argv[1:5]
check = cdp.Check()
sys.stdout.reconfigure(line_buffering=True)
MOCK = 18987
seen = {"session": [], "calls": [], "tokens": []}


class Jmap(http.server.BaseHTTPRequestHandler):
    def claims(self):
        auth = self.headers.get("authorization", "")
        tok = auth.split(" ", 1)[1] if auth.startswith("Bearer ") else ""
        try:
            p = tok.split(".")[1]
            c = json.loads(base64.urlsafe_b64decode(p + "=" * (-len(p) % 4)))
        except Exception:
            c = {}
        seen["tokens"].append(c)
        return c

    def answer(self, v):
        b = json.dumps(v).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        c = self.claims()
        seen["session"].append(self.path)
        if not c:
            self.send_response(401); self.end_headers(); return
        self.answer({"apiUrl": f"http://127.0.0.1:{MOCK}/jmap/", "primaryAccounts": {"urn:ietf:params:jmap:mail": "acc1", "urn:ietf:params:jmap:submission": "acc1"}})

    def do_POST(self):
        c = self.claims()
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
        out = []
        for name, args, tag in body["methodCalls"]:
            seen["calls"].append((name, args))
            if not c:
                out.append(["error", {"type": "forbidden"}, tag])
            elif name == "Identity/get":
                out.append([name, {"list": [{"id": "idx", "email": "other@test.example"}, {"id": "id1", "email": c.get("email")}]}, tag])
            elif name == "Mailbox/get":
                out.append([name, {"list": [{"id": "mbi", "role": "inbox"}, {"id": "mbd", "role": "drafts"}, {"id": "mbs", "role": "sent"}]}, tag])
            elif name == "Email/set":
                to = args["create"]["e"]["to"][0]["email"]
                if to.startswith("fail@"):
                    out.append([name, {"notCreated": {"e": {"type": "invalidProperties", "description": "the mock refuses"}}}, tag])
                else:
                    out.append([name, {"created": {"e": {"id": "m1"}}}, tag])
            elif name == "EmailSubmission/set":
                out.append([name, {"created": {"s": {"id": "sub1"}}}, tag])
        self.answer({"methodResponses": out})

    def log_message(self, *a):
        pass


srv = http.server.ThreadingHTTPServer(("127.0.0.1", MOCK), Jmap)
threading.Thread(target=srv.serve_forever, daemon=True).start()


def api(b, method, path, body=None):
    js = f"fetch({json.dumps(path)}, {{method: {json.dumps(method)}, headers: {{'content-type': 'application/json', 'x-shelf': '1'}}{', body: ' + json.dumps(json.dumps(body)) if body is not None else ''}}}).then(async r => [r.status, await r.json().catch(() => null)])"
    return b.js(js)


DLG = "document.querySelector('.share')"
RES = DLG + ".querySelector('.share-results')"


def search(b, text):
    b.js(f"(() => {{ const i = {DLG}.querySelector('.share-find'); i.value = {json.dumps(text)}; i.dispatchEvent(new Event('input')); }})()")
    b.js(f"[...{DLG}.querySelectorAll('button')].find(x => x.textContent === 'search').click()")
    time.sleep(0.8)
    return b.js(f"(() => {{ const r = {RES}; return r && !r.hidden ? r.textContent : ''; }})()")


b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    check("signed in", b.wait("location.pathname === '/'", 10))
    b.errors.clear()
    ov = api(b, "GET", "/api/admin/overview")[1]
    p, me = ov["placement"], ov["me"]
    for n, disp in [("bob", "Bob Builder"), ("carol", "Carol Singer")]:
        r = api(b, "POST", "/api/admin/users", {"name": n, "display": disp, "ou": 1, "password": f"{n}-pass-123456"})
        check(f"user {n}", r[0] == 200, r)
    us = api(b, "POST", "/api/admin/table/users", {"limit": 50, "params": {}})[1]["rows"]
    uid = {r["name"]: r["id"] for r in us}
    # a mail instance (no Stalwart behind it: the mock answers JMAP), mailboxes for admin and bob
    p["modules"]["mail"] = {"kind": "mail", "title": "mail", "node": me, "host": "mail.test.example", "upstream": "tls://127.0.0.1:9"}
    r = api(b, "POST", "/api/admin/placement", p)
    check("a mail instance", r[0] == 200, r)
    for n in ["admin", "bob"]:
        r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid[n], "address": f"{n}@test.example"})
        check(f"{n}'s mailbox {n}@test.example", r[0] == 200, r)
    r = api(b, "POST", "/api/oidc/clients", {"name": "maildot", "redirect_uris": "https://mail.test.example/maildot/", "public": True})
    check("the webmail's sign-in client", r[0] == 200, r)
    b.js("fetch('/api/raw/home/hello.txt', { method: 'PUT', headers: { 'x-shelf': '1' }, body: 'hello there' }).then(r => r.status)")
    b.load("/files/home/")
    b.wait("[...document.querySelectorAll('table.files tbody tr')].some(r => r.textContent.includes('hello.txt'))", 15)
    # (the dialog's promise ends when it closes: not waited for)
    b.js("(() => { withPart('share', x => x.shareDialog('home', 'hello.txt', 'hello.txt')); return 1; })()")
    check("the share window: the search on top, focused", b.wait(f"!!{DLG} && {DLG}.firstElementChild.classList.contains('share-top') && document.activeElement === {DLG}.querySelector('.share-find')", 10),
          b.js(f"{DLG} && {DLG}.innerHTML.slice(0, 300)"))
    t = search(b, "bob")
    check("'bob': on this server, with his UPN", "1. on this server" in t and "Bob Builder" in t and "bob@osada.test.example" in t, t)
    check("... federated instances: none set up", "2. federated instances" in t and "none set up" in t, t)
    check("... no public e-mail for a name", "3. public e-mail" not in t, t)
    t = search(b, "Build")
    check("by display name", "bob@osada.test.example" in t, t)
    t = search(b, "carol")
    check("everyone: name@the UPN domain (the public host)", "carol@osada.test.example" in t, t)
    t = search(b, "admin")
    check("yourself: (you)", "(you)" in t, t)
    t = search(b, "bob@test.example")
    check("his mailbox address: found here too, not public e-mail", "Bob Builder" in t and "3. public e-mail" not in t, t)
    b.js(f"(() => {{ const row = [...{RES}.querySelectorAll('tr.person')].find(r => r.textContent.includes('bob@osada.test.example')); row.querySelector('input[type=checkbox]').click(); [...row.querySelectorAll('button')].find(x => x.textContent === 'share').click(); }})()")
    check("shared with bob, who can edit; shown by UPN", b.wait(f"(() => {{ const t = {DLG}.querySelector('table.share-with'); return !!t && t.textContent.includes('bob@osada.test.example') && t.querySelector('input[type=checkbox]').checked; }})()", 10),
          b.js(f"{DLG}.textContent.slice(0, 600)"))
    t = search(b, "x@y")
    check("'x@y' is not an address", "not an e-mail address" in t and "3. public e-mail" not in t, t)
    # public e-mail
    t = search(b, "Stranger@Elsewhere.org")
    check("an address nobody has: 3. public e-mail, view only, from your mailbox", "3. public e-mail" in t and "stranger@elsewhere.org is nobody here" in t and "view only" in t, t)
    b.js(f"(() => {{ {RES}.querySelector('input[type=number]').value = '7'; [...{RES}.querySelectorAll('button')].find(x => x.textContent === 'send link').click(); }})()")
    check("listed: public link sent to stranger@elsewhere.org, until a date", b.wait(f"/public link sent to\\s*stranger@elsewhere\\.org.*until \\d/.test({DLG}.querySelector('table.share-with').textContent)", 15),
          b.js(f"{DLG}.textContent.slice(0, 900)"))
    sets = [a for n, a in seen["calls"] if n == "Email/set"]
    e = sets[-1]["create"]["e"] if sets else {}
    check("mailed through JMAP: Email/set from your address to theirs", e.get("from", [{}])[0].get("email") == "admin@test.example" and e.get("to", [{}])[0].get("email") == "stranger@elsewhere.org", e)
    link = next((x for x in it["links"] if x.get("sent_to") == "stranger@elsewhere.org"), None) if (it := api(b, "GET", "/api/shares/item?loc=home&path=hello.txt")[1]) else None
    check("... the letter: who shared what, the link", link and "shared \u201chello.txt\u201d" in e.get("subject", "") and f"https://osada.test.example/s/{link['token']}" in e.get("bodyValues", {}).get("t", {}).get("value", ""), (e.get("subject"), link))
    subs = [a for n, a in seen["calls"] if n == "EmailSubmission/set"]
    check("... submitted as your identity, then out of Drafts into Sent", subs and subs[-1]["create"]["s"]["identityId"] == "id1" and subs[-1]["onSuccessUpdateEmail"]["#s"] == {"keywords/$draft": None, "mailboxIds/mbd": None, "mailboxIds/mbs": True}, subs[-1:] or None)
    tok = seen["tokens"][-1] if seen["tokens"] else {}
    check("... with a token osada signed for the webmail's client: aud maildot, your address, short", tok.get("aud") == "maildot" and tok.get("email") == "admin@test.example" and tok.get("iss") == "https://osada.test.example" and tok.get("scope") == "openid email" and 0 < tok.get("exp", 0) - tok.get("iat", 0) <= 300, tok)
    st = subprocess.run(["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", f"{cdp.BASE}/s/{link['token']}/raw"], capture_output=True, text=True).stdout if link else ""
    check("the link opens without an account", st == "200", st)
    # a send that fails leaves no link; a user's address is no public e-mail
    n_links = len(api(b, "GET", "/api/shares/item?loc=home&path=hello.txt")[1]["links"])
    r = api(b, "POST", "/api/links", {"loc": "home", "path": "hello.txt", "send_to": "fail@elsewhere.org"})
    check("a send that fails: an error, no link left", r[0] == 400 and "not sent to fail@elsewhere.org" in json.dumps(r[1]) and len(api(b, "GET", "/api/shares/item?loc=home&path=hello.txt")[1]["links"]) == n_links, r)
    r = api(b, "POST", "/api/links", {"loc": "home", "path": "hello.txt", "send_to": "bob@test.example"})
    check("a user's own address is refused as public e-mail", r[0] == 400 and "share with them" in json.dumps(r[1]), r)
    r = api(b, "POST", "/api/shares", {"loc": "home", "path": "hello.txt", "user": "carol@osada.test.example", "write": False})
    check("sharing by UPN works too", r[0] == 200, r)
    check("no page errors", not [x for x in b.errors if "401" not in x and "400" not in x], b.errors)
finally:
    b.close()
    srv.shutdown()
sys.exit(check.done())
