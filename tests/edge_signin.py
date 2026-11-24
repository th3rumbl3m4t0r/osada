"""D-U45 on localhost: sign-in while home is away. A drop-in home (a program registered with
osada 0.1's `osada oidc add`, which home publishes to the cluster log) and an edge with the idp
role that keeps the root OU's secrets. Home stopped: the edge signs alice in itself, gives her its
own session, and runs another program's sign-in (authorize with PKCE, token, userinfo) with its
own key; a visitor without a session is sent to the sign-in page; JWKS lists the edge's key.

    python3 -I tests/edge_signin.py [<the osada binary>]       (as root; /root/t-es, removed after)
"""
import base64, hashlib, json, os, re, shutil, signal, subprocess, sys, time, urllib.parse

B = sys.argv[1] if len(sys.argv) > 1 else "/root/osada-0.2/target/release/osada"
D = "/root/t-es"
HOME_LINK, HOME_HTTP = 18750, 18751
LINK, HTTPS, PLAIN = 18760, 18761, 18762
HOST = "test.example"
failed = []


def check(what, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + what + (f"\n     {detail}" if detail and not cond else ""), flush=True)
    if not cond:
        failed.append(what)


def curl(url, *args):
    """status, headers (lower case), body"""
    cmd = ["curl", "-sk", "-D", "-", "--max-time", "30", "--resolve", f"{HOST}:{HTTPS}:127.0.0.1"] + list(args) + [url]
    out = subprocess.run(cmd, capture_output=True).stdout.decode(errors="replace").replace("\r", "")
    head, _, body = out.partition("\n\n")
    while head.startswith("HTTP/") and body.startswith("HTTP/"):
        head, _, body = body.partition("\n\n")
    lines = head.split("\n")
    status = int(lines[0].split()[1]) if lines and lines[0].startswith("HTTP") else 0
    return status, {l.split(":", 1)[0].lower(): l.split(":", 1)[1].strip() for l in lines[1:] if ":" in l}, body


def wait_for(what, f, secs=90):
    end = time.time() + secs
    while time.time() < end:
        try:
            v = f()
            if v:
                return v
        except Exception:
            pass
        time.sleep(0.5)
    check(f"waiting for {what}", False)
    return None


E = f"https://{HOST}:{HTTPS}"
shutil.rmtree(D, ignore_errors=True)
os.makedirs(f"{D}/home")
procs, logs = {}, {}
henv = dict(os.environ, OSADA_DATA=f"{D}/home", OSADA_BIND=f"127.0.0.1:{HOME_HTTP}", OSADA_LINK_BIND=f"127.0.0.1:{HOME_LINK}", OSADA_NODE="home",
            OSADA_OIDC_ISSUER=f"http://127.0.0.1:{HOME_HTTP}")


def start_home():
    logs["home"] = open(f"{D}/home.log", "a")
    procs["home"] = subprocess.Popen([B, "serve"], env=henv, stdout=logs["home"], stderr=subprocess.STDOUT)


try:
    start_home()
    wait_for("home's first administrator", lambda: os.path.exists(f"{D}/home/node/first-admin"))
    pw = open(f"{D}/home/node/first-admin").read().split("password:")[1].strip()
    hb = f"http://127.0.0.1:{HOME_HTTP}"
    h = lambda method, path, body=None, cookie=None: subprocess.run(
        ["curl", "-s", "-X", method, "-H", "x-shelf: 1", "-H", "content-type: application/json", "-D", "-"] + (["-H", f"cookie: {cookie}"] if cookie else [])
        + (["--data-binary", json.dumps(body)] if body is not None else []) + [hb + path], capture_output=True, text=True).stdout
    r = h("POST", "/api/signin", {"name": "admin", "password": pw})
    admin = re.search(r"set-cookie: ([^;]+)", r, re.I).group(1)
    check("alice made", '"error"' not in h("POST", "/api/admin/users", {"name": "alice", "display": "Alice", "ou": 1, "password": "alice-pass-123456"}, admin))
    out = subprocess.run([B, "oidc", "add", "app", "--redirect", "https://app.example/cb"], env=henv, capture_output=True, text=True).stdout
    secret = re.search(r"client_secret:\s*(\S+)", out).group(1)
    check("a program registered at home (osada 0.1)", bool(secret))
    # the edge: idp, store; a certificate for the name
    tok = subprocess.run([B, "token", "--data", f"{D}/home/node", "--node", "e1", "--roles", "edge-http,idp,store", "--dial", f"127.0.0.1:{LINK}"], capture_output=True, text=True).stdout.strip()
    j = subprocess.run([B, "join", "--data", f"{D}/e1", "--node", "e1", "--token", tok, "--listen", f"127.0.0.1:{LINK}", "--wait", "2"], capture_output=True, text=True)
    check("e1 joined", "joined the cluster" in j.stdout, j.stdout + j.stderr)
    os.makedirs(f"{D}/e1/edge-tls")
    subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", f"{D}/e1/edge-tls/privkey.pem",
                    "-out", f"{D}/e1/edge-tls/fullchain.pem", "-days", "2", "-subj", f"/CN={HOST}", "-addext", f"subjectAltName=DNS:{HOST}"], check=True, capture_output=True)
    logs["e1"] = open(f"{D}/e1.log", "a")
    procs["e1"] = subprocess.Popen([B, "serve", "--data", f"{D}/e1", "--listen", f"127.0.0.1:{LINK}", "--relay", f"127.0.0.1:{HTTPS}", "--relay-http", f"127.0.0.1:{PLAIN}"],
                                   stdout=logs["e1"], stderr=subprocess.STDOUT)
    wait_for("the edge answering", lambda: curl(E + "/")[0] == 200)
    # the root OU's secrets also on the edge (node control)
    ov = json.loads(h("GET", "/api/admin/overview", None, admin).split("\n\n", 1)[1])
    p = ov["placement"]
    e1 = next(i for i, n in p["nodes"].items() if n.get("name") == "e1" or i == "e1" or "edge-http" in n["roles"] and "idp" in n["roles"] and i != ov["me"])
    p["default_secrets"] = sorted(set(p["default_secrets"]) | {e1})
    r = h("POST", "/api/admin/placement", p, admin)
    check("the secrets on the edge too", r.split("\n", 1)[0].split()[1] == "200", r[-300:])
    wait_for("the program in the cluster log", lambda: "OIDC client published" in open(f"{D}/home.log").read())
    wait_for("the edge's signing key", lambda: "signing key" in open(f"{D}/e1.log").read())
    time.sleep(3)

    # home away
    procs["home"].send_signal(signal.SIGTERM)
    procs["home"].wait(timeout=30)
    s, hd, b = wait_for("the edge to sign alice in itself", lambda: (lambda r: r if r[0] == 200 else None)(
        curl(E + "/api/signin", "-X", "POST", "-H", "x-shelf: 1", "-H", "content-type: application/json", "--data-binary", json.dumps({"name": "alice", "password": "alice-pass-123456"}))), 30) or (0, {}, "")
    check("home stopped: alice signs in at the edge", s == 200, b[:200])
    cookie = hd.get("set-cookie", "").split(";")[0]
    s, _, b = curl(E + "/api/account", "-H", "x-shelf: 1", "-H", f"cookie: {cookie}")
    acct = json.loads(b) if s == 200 else {}
    check("... her session is the edge's", s == 200 and acct.get("user", {}).get("name") == "alice" and acct.get("node") == e1, f"{s} {b[:200]}")
    # another program's sign-in, at the edge
    verifier = base64.urlsafe_b64encode(os.urandom(40)).decode().rstrip("=")
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).decode().rstrip("=")
    q = urllib.parse.urlencode({"response_type": "code", "client_id": "app", "redirect_uri": "https://app.example/cb", "scope": "openid profile email",
                                "state": "st1", "nonce": "n1", "code_challenge": challenge, "code_challenge_method": "S256"})
    s, hd, _ = curl(f"{E}/oidc/authorize?{q}")
    check("... without a session: to the sign-in page", s == 303 and hd.get("location", "").startswith("/signin?next=%2Foidc%2Fresume%2F"), f"{s} {hd.get('location')}")
    s, hd, _ = curl(f"{E}/oidc/authorize?{q}", "-H", f"cookie: {cookie}")
    loc = hd.get("location", "")
    check("... with hers: back to the program with a code", s == 303 and loc.startswith("https://app.example/cb?code=") and "state=st1" in loc and f"iss=https%3A%2F%2F{HOST}" in loc, f"{s} {loc}")
    code = urllib.parse.parse_qs(urllib.parse.urlparse(loc).query).get("code", [""])[0]
    s, _, b = curl(E + "/oidc/token", "-u", f"app:{secret}", "-d", urllib.parse.urlencode({"grant_type": "authorization_code", "code": code,
                   "redirect_uri": "https://app.example/cb", "code_verifier": verifier}))
    t = json.loads(b) if s == 200 else {}
    check("... the token", s == 200 and t.get("token_type") == "Bearer" and "id_token" in t and "refresh_token" in t, f"{s} {b[:300]}")
    head = json.loads(base64.urlsafe_b64decode(t.get("id_token", "e30.").split(".")[0] + "=="))
    claims = json.loads(base64.urlsafe_b64decode(t.get("id_token", "x.e30").split(".")[1] + "=="))
    s, _, b = curl(E + "/oidc/jwks")
    kids = [k["kid"] for k in json.loads(b).get("keys", [])] if s == 200 else []
    check("... signed with the edge's own key, which JWKS lists", head.get("kid") in kids and claims.get("iss") == f"https://{HOST}:{HTTPS}" and claims.get("nonce") == "n1", f"{head} {claims} {kids}")
    s, _, b = curl(E + "/oidc/userinfo", "-H", f"authorization: Bearer {t.get('access_token', '')}")
    check("... userinfo", s == 200 and json.loads(b).get("preferred_username") == "alice", f"{s} {b[:200]}")
    s, _, b = curl(E + "/oidc/token", "-u", f"app:{secret}", "-d", urllib.parse.urlencode({"grant_type": "refresh_token", "refresh_token": t.get("refresh_token", "")}))
    check("... a refresh", s == 200 and "access_token" in json.loads(b), f"{s} {b[:200]}")
    s, _, b = curl(E + "/oidc/token", "-u", "app:wrong", "-d", urllib.parse.urlencode({"grant_type": "refresh_token", "refresh_token": t.get("refresh_token", "")}))
    check("... a wrong secret refused", s == 401, f"{s} {b[:200]}")
    # home back: the pages' API goes there again
    start_home()
    wait_for("home back, through the edge", lambda: curl(E + "/api/signin", "-X", "POST", "-H", "x-shelf: 1", "-H", "content-type: application/json",
             "--data-binary", json.dumps({"name": "alice", "password": "alice-pass-123456"}))[1].get("set-cookie", "").split("=")[0] not in ("", cookie.split("=")[0]), 60)
finally:
    for p in procs.values():
        p.send_signal(signal.SIGTERM)
    for p in procs.values():
        try:
            p.wait(timeout=20)
        except Exception:
            p.kill()
print("FAILED: " + ", ".join(failed) if failed else "all passed")
if not failed:
    shutil.rmtree(D, ignore_errors=True)
sys.exit(1 if failed else 0)
