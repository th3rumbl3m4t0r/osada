"""FIDO2 in headless Chromium with a virtual authenticator (CDP WebAuthn), on
a fresh node at http://localhost:<port> (the one http origin where browsers
allow WebAuthn): a tier-0 OU, a user in it, the first key registered at the
first sign-in, a sign-in with it, a second key on the account page, the last
key kept; TOTP does not stand in for the key.

    python3 -I tests/ui_fido2.py <data dir> <port>
"""
import http.cookiejar
import json
import os
import sys
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

D, PORT = sys.argv[1], sys.argv[2]
U = cdp.BASE = f"http://localhost:{PORT}"
pw = dict(l.split(": ", 1) for l in open(D + "/first-admin").read().splitlines())["password"]
fails = []


def ok(name, cond, extra=""):
    print(("ok   " if cond else "FAIL ") + name + ("" if cond else f"  {extra}"))
    if not cond:
        fails.append(name)


def call(op, path, body=None):
    req = urllib.request.Request(U + path, data=None if body is None else json.dumps(body).encode(),
                                 headers={"x-shelf": "1", "content-type": "application/json"}, method="GET" if body is None else "POST")
    try:
        with op.open(req, timeout=15) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


adm = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
ok("admin signs in", call(adm, "/api/signin", {"name": "admin", "password": pw})[0] == 200)
s, ov = call(adm, "/api/admin/overview")
p = ov["placement"]
root = next(k for k, o in p["ous"].items() if o.get("parent") is None)
t0 = str(max(int(k) for k in p["ous"]) + 1)
p["ous"][t0] = {"name": "t0", "parent": int(root), "links": {}, "secrets": None, "tier0": True, "second_factor": False}
s, r = call(adm, "/api/admin/placement", p)
ok("a tier-0 OU", s == 200, r)
import time  # noqa: E402
for _ in range(40):  # its directory is made by the next reconcile
    s, r = call(adm, "/api/admin/users", {"name": "tina", "display": "Tina", "ou": int(t0), "password": "tina-password-1"})
    if s == 200 or "does not exist yet" not in str(r):
        break
    time.sleep(0.5)
ok("a user in it", s == 200, r)

# TOTP does not stand in for the key
u = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
s, r = call(u, "/api/signin", {"name": "tina", "password": "tina-password-1"})
ok("the password asks for a key to be registered", r.get("next") == "enrol_fido2", r)
s, r2 = call(u, "/api/signin/totp", {"ticket": r.get("ticket", ""), "code": "123456"})
ok("a TOTP code is no way past it", s == 401, (s, r2))

b = cdp.Browser()
try:
    b.cmd("WebAuthn.enable", True)
    auth = b.cmd("WebAuthn.addVirtualAuthenticator", True, options={
        "protocol": "ctap2", "transport": "usb", "hasResidentKey": False, "hasUserVerification": True, "isUserVerified": True,
    })["authenticatorId"]

    def sign_in():
        b.load("/account")
        if not b.wait("!!document.querySelector('#si-name')"):
            print("page:", b.js("location.href + ' ' + document.body.textContent.slice(0, 200)"))
        b.focus("#si-name")
        b.type("tina")
        b.focus("#si-pw")
        b.type("tina-password-1")
        b.key("Enter")

    sign_in()
    ok("first sign-in: register a key", b.wait("!!document.querySelector('#si-fido2') && document.querySelector('#si-fido2').textContent.startsWith('register')", 8))
    b.js("document.querySelector('#si-fido2').click()")
    ok("registered, signed in", b.wait("!!document.querySelector('#add-key')", 10), b.js("document.querySelector('#app').textContent.slice(0, 300)"))
    ok("one key listed", b.wait("document.querySelectorAll('ul.keys li').length === 1"))
    creds = b.cmd("WebAuthn.getCredentials", True, authenticatorId=auth)["credentials"]
    ok("the key holds one credential", len(creds) == 1, creds)

    b.js("[...document.querySelectorAll('#who button')].find(x => x.textContent === 'sign out').click()")
    b.wait("!!document.querySelector('#si-name')")
    sign_in()
    ok("second sign-in: use the key", b.wait("!!document.querySelector('#si-fido2') && document.querySelector('#si-fido2').textContent.startsWith('use')", 8))
    b.js("document.querySelector('#si-fido2').click()")
    ok("signed in with the key", b.wait("!!document.querySelector('#add-key')", 10), b.js("document.querySelector('#app').textContent.slice(0, 300)"))

    # a second key: another authenticator (the first one refuses: it is registered already)
    b.cmd("WebAuthn.removeVirtualAuthenticator", True, authenticatorId=auth)
    b.cmd("WebAuthn.addVirtualAuthenticator", True, options={
        "protocol": "ctap2", "transport": "usb", "hasResidentKey": False, "hasUserVerification": True, "isUserVerified": True,
    })
    b.js("document.querySelector('#add-key').previousElementSibling.value = 'spare'")
    b.js("document.querySelector('#add-key').click()")
    ok("a second key on the account page", b.wait("document.querySelectorAll('ul.keys li').length === 2", 10), b.js("document.querySelector('.msg.bad') && document.querySelector('.msg.bad').textContent"))

    # take one off, then the last one is kept
    b.js("document.querySelector('ul.keys li button.danger').click()")
    b.wait("!!document.querySelector('#ovl')")
    b.js("[...document.querySelectorAll('#ovl button')].find(x => x.textContent === 'yes').click()")
    ok("one taken off", b.wait("document.querySelectorAll('ul.keys li').length === 1", 8))
    b.js("document.querySelector('ul.keys li button.danger').click()")
    b.wait("!!document.querySelector('#ovl')")
    b.js("[...document.querySelectorAll('#ovl button')].find(x => x.textContent === 'yes').click()")
    ok("the last one stays (tier 0)", b.wait("[...document.querySelectorAll('.msg.bad')].some(m => m.textContent.includes('tier 0'))", 8))

    errors = [e for e in b.errors if "Failed to load resource" not in str(e)]
    ok("no script errors", not errors, errors[:3])
finally:
    b.close()
print("FAILED: " + ", ".join(fails) if fails else "all passed")
sys.exit(1 if fails else 0)
