"""Round 24: passwords for mail programs on the account page, on a drop-in (tests/dropin-ui.sh),
with a mock mail server (JMAP, Stalwart's x:AppPassword) that records what osada sends:

    OSADA_OIDC_ISSUER=http://127.0.0.1:18989 OSADA_PUBLIC_URL=https://osada.test.example \\
    OSADA_MAIL_JMAP_URL=http://127.0.0.1:18986 tests/dropin-ui.sh target/release/osada ui_mail_passwords.py

- no mailbox: the API says why, the window stays hidden;
- with a mailbox: the window shows the user name (the address) and the server; "make one" asks the
  mail server as the person (a token osada signed for the webmail's client, aud maildot, scope with
  Stalwart 1.x's urn:stalwart:oauth:scope:security) and shows the secret once; the list never has
  secrets; "revoke" destroys it there;
- a POST without the X-Shelf header is refused; a name with a line break and an id that is no id
  are refused; when the mail server does not answer, the window says so.
"""
import base64
import http.server
import json
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW, D = sys.argv[1:5]
check = cdp.Check()
sys.stdout.reconfigure(line_buffering=True)
MOCK = 18986
SECRET = "mock-secret-1234567890"
seen = {"calls": [], "tokens": [], "using": []}
pws = {}


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
        if not self.claims():
            self.send_response(401); self.end_headers(); return
        self.answer({"apiUrl": f"http://127.0.0.1:{MOCK}/jmap/", "primaryAccounts": {"urn:ietf:params:jmap:mail": "acc1", "urn:stalwart:jmap": "acc1"}})

    def do_POST(self):
        c = self.claims()
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
        seen["using"].append(body.get("using"))
        out = []
        for name, args, tag in body["methodCalls"]:
            seen["calls"].append((name, args))
            if not c or args.get("accountId") != "acc1":
                out.append(["error", {"type": "forbidden"}, tag])
            elif name == "x:AppPassword/query":
                out.append([name, {"accountId": "acc1", "ids": sorted(pws)}, tag])
            elif name == "x:AppPassword/get":
                out.append([name, {"accountId": "acc1", "list": [dict(pws[i], secret="****") for i in args["ids"] if i in pws]}, tag])
            elif name == "x:AppPassword/set":
                r = {"accountId": "acc1"}
                for k, v in args.get("create", {}).items():
                    i = f"p{len(pws) + 1}"
                    pws[i] = {"id": i, "description": v["description"], "createdAt": "2026-11-24T12:00:00Z", "expiresAt": None}
                    r.setdefault("created", {})[k] = {"id": i, "secret": SECRET}
                for i in args.get("destroy", []):
                    if pws.pop(i, None):
                        r.setdefault("destroyed", []).append(i)
                    else:
                        r.setdefault("notDestroyed", {})[i] = {"type": "notFound"}
                out.append([name, r, tag])
        self.answer({"methodResponses": out})

    def log_message(self, *a):
        pass


srv = http.server.ThreadingHTTPServer(("127.0.0.1", MOCK), Jmap)
threading.Thread(target=srv.serve_forever, daemon=True).start()


def api(b, method, path, body=None, shelf=True):
    hdr = "{'content-type': 'application/json'" + (", 'x-shelf': '1'" if shelf else "") + "}"
    js = f"fetch({json.dumps(path)}, {{method: {json.dumps(method)}, headers: {hdr}{', body: ' + json.dumps(json.dumps(body)) if body is not None else ''}}}).then(async r => [r.status, await r.json().catch(() => null)])"
    return b.js(js)


WIN = "document.querySelector('#mail-passwords')"

b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    check("signed in", b.wait("location.pathname === '/'", 10))
    b.errors.clear()
    ov = api(b, "GET", "/api/admin/overview")[1]
    p, me = ov["placement"], ov["me"]
    us = api(b, "POST", "/api/admin/table/users", {"limit": 50, "params": {}})[1]["rows"]
    uid = {r["name"]: r["id"] for r in us}
    p["modules"]["mail"] = {"kind": "mail", "title": "mail", "node": me, "host": "mail.test.example", "upstream": "tls://127.0.0.1:9"}
    r = api(b, "POST", "/api/admin/placement", p)
    check("a mail instance", r[0] == 200, r)
    r = api(b, "POST", "/api/oidc/clients", {"name": "maildot", "redirect_uris": "https://mail.test.example/maildot/", "public": True})
    check("the webmail's sign-in client", r[0] == 200, r)
    r = api(b, "GET", "/api/account/mail-passwords")
    check("no mailbox yet: not available, and why", r[0] == 200 and r[1] == {"available": False, "why": "you have no mailbox"}, r)
    b.load("/account")
    b.wait("!!document.querySelector('.win')", 10)
    time.sleep(1)
    check("... the window stays hidden", b.js(f"!!{WIN} && {WIN}.hidden") is True, b.js(f"{WIN} && {WIN}.outerHTML.slice(0, 200)"))
    r = api(b, "POST", "/api/admin/mail/action", {"op": "mailbox_set", "user": uid["admin"], "address": "admin@test.example"})
    check("a mailbox admin@test.example", r[0] == 200, r)
    r = api(b, "GET", "/api/account/mail-passwords")
    check("with a mailbox: available, the user name and the server, none yet", r[0] == 200 and r[1] == {"available": True, "login": "admin@test.example", "host": "mail.test.example", "list": []}, r)
    tok = seen["tokens"][-1] if seen["tokens"] else {}
    check("... asked as the person: aud maildot, their address, Stalwart's security scope, short",
          tok.get("aud") == "maildot" and tok.get("email") == "admin@test.example" and tok.get("iss") == "https://osada.test.example"
          and "urn:stalwart:oauth:scope:security" in tok.get("scope", "").split() and 0 < tok.get("exp", 0) - tok.get("iat", 0) <= 300, tok)
    check("... with Stalwart's JMAP capability", seen["using"] and "urn:stalwart:jmap" in seen["using"][-1], seen["using"][-1:])
    n = len(seen["calls"])
    r = api(b, "POST", "/api/account/mail-passwords", {"description": "sneaky"}, shelf=False)
    check("a POST without X-Shelf: refused, nothing asked", r[0] == 403 and len(seen["calls"]) == n, r)
    r = api(b, "POST", "/api/account/mail-passwords", {"description": "two\nlines"})
    check("a name on two lines: refused", r[0] == 400 and "one line" in json.dumps(r[1]), r)
    r = api(b, "DELETE", "/api/account/mail-passwords/a%20b")
    check("an id that is no id: refused", r[0] == 400, r)
    # the page
    b.load("/account")
    check("the window: user name, IMAP and SMTP settings", b.wait(f"!!{WIN} && !{WIN}.hidden && {WIN}.textContent.includes('admin@test.example') && {WIN}.textContent.includes('993') && {WIN}.textContent.includes('No app passwords yet')", 10),
          b.js(f"{WIN} && {WIN}.textContent.slice(0, 500)"))
    b.js(f"(() => {{ const i = {WIN}.querySelector('input'); i.value = 'phone'; document.querySelector('#make-mail-password').click(); }})()")
    check("make one: the secret shown once, the list has it", b.wait(f"!!{WIN}.querySelector('.fresh-secret') && {WIN}.querySelector('.fresh-secret code').textContent === {json.dumps(SECRET)} && {WIN}.querySelector('ul.keys') && {WIN}.querySelector('ul.keys').textContent.includes('phone')", 10),
          b.js(f"{WIN}.textContent.slice(0, 700)"))
    sets = [a for nm, a in seen["calls"] if nm == "x:AppPassword/set" and "create" in a]
    check("... made at the mail server with its name", sets and list(sets[-1]["create"].values())[0] == {"description": "phone"}, sets[-1:])
    r = api(b, "GET", "/api/account/mail-passwords")
    check("the list has no secrets", r[0] == 200 and len(r[1]["list"]) == 1 and SECRET not in json.dumps(r[1]) and "****" not in json.dumps(r[1]) and r[1]["list"][0]["description"] == "phone", r)
    b.load("/account")
    check("after a reload the secret is gone", b.wait(f"!!{WIN} && !{WIN}.hidden && {WIN}.textContent.includes('phone') && !{WIN}.querySelector('.fresh-secret')", 10))
    b.js(f"[...{WIN}.querySelectorAll('button')].find(x => x.textContent === 'revoke').click()")
    b.wait("!!document.querySelector('.modal')", 5)
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'yes').click()")
    check("revoke: gone from the list", b.wait(f"{WIN}.textContent.includes('No app passwords yet')", 10), b.js(f"{WIN}.textContent.slice(0, 500)"))
    check("... destroyed at the mail server", not pws and any(nm == "x:AppPassword/set" and a.get("destroy") == ["p1"] for nm, a in seen["calls"]), pws)
    r = api(b, "DELETE", "/api/account/mail-passwords/p1")
    check("revoking it again: the mail server's refusal shown", r[0] == 502 and "notFound" in json.dumps(r[1]), r)
    check("no page errors", not [x for x in b.errors if "400" not in x and "403" not in x and "502" not in x], b.errors)
    # the mail server gone
    srv.shutdown(); srv.server_close()
    r = api(b, "GET", "/api/account/mail-passwords")
    check("the mail server not answering: 502, in words", r[0] == 502 and "mail server" in json.dumps(r[1]), r)
    b.load("/account")
    check("... and the window says so", b.wait(f"!!{WIN} && !{WIN}.hidden && {WIN}.querySelector('.msg.bad') && {WIN}.querySelector('.msg.bad').textContent.includes('mail server')", 10),
          b.js(f"{WIN} && {WIN}.outerHTML.slice(0, 400)"))
finally:
    b.close()
    try:
        srv.shutdown()
    except Exception:
        pass
sys.exit(check.done())
