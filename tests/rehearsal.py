#!/usr/bin/python3 -I
"""A rehearsal of deploy/UPGRADE.md on localhost, as root on home.

Phase 1: a copy of osada 0.1's database (sqlite .backup of a read-only
connection; no files, the backup setting and sessions taken out) started by
`osada serve` as the drop-in: the node gets a GUID as its id and the host's
name (home) as its name (D-U19). Phase 2: a token for `mail-1`, the join
made at home by address only, the node folder moved (what swrap copy does),
the edge serving without --relay-to, home dialing it by name, Stalwart (a
stand-in, tests/fake_stalwart.py) as the mail instance by the edge's name;
then home renamed through the admin API while the edge keeps relaying, a
name taken refused, and the name put back.

    python3 -I tests/rehearsal.py [<osada binary>]

Ports 18490-18495 on 127.0.0.1; everything in /root/rehearsal (left for a
look; removed at the next run). The live services are not touched.
"""
import http.cookiejar, json, os, re, shutil, socket, ssl, subprocess, sys, time, urllib.request

B = sys.argv[1] if len(sys.argv) > 1 else "/root/osada-0.2/target/release/osada"
R = "/root/rehearsal"
HERE = os.path.dirname(os.path.abspath(__file__))
HOME_HTTP, LINK_HOME, LINK_EDGE, EDGE_HTTPS, EDGE_HTTP, STALWART = 18490, 18491, 18492, 18493, 18494, 18495
procs = []
failed = []


def check(what, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + what + (f"\n     {detail}" if detail and not cond else ""), flush=True)
    if not cond:
        failed.append(what)
    return cond


def run(*a, env=None):
    return subprocess.run(a, capture_output=True, text=True, env=env)


def start(name, args, env=None):
    out = open(f"{R}/{name}.log", "a")
    procs.append((name, subprocess.Popen(args, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, env=env, start_new_session=True)))


def stop(name):
    for n, p in list(procs):
        if n == name:
            p.terminate()
            p.wait(15)
            procs.remove((n, p))


def log(name):
    try:
        return open(f"{R}/{name}.log").read()
    except OSError:
        return ""


def wait_for(f, secs=60):
    end = time.time() + secs
    while time.time() < end:
        try:
            v = f()
            if v:
                return v
        except Exception:
            pass
        time.sleep(0.5)
    return None


def home_env(extra=None):
    e = {"PATH": "/usr/bin:/bin", "OSADA_DATA": f"{R}/data", "OSADA_BIND": f"127.0.0.1:{HOME_HTTP}",
         "OSADA_LINK_BIND": f"127.0.0.1:{LINK_HOME}", "OSADA_COOKIE": "osada_rehearsal",
         "OSADA_OIDC_ISSUER": f"http://127.0.0.1:{HOME_HTTP}", "OSADA_PUBLIC_URL": "https://shelf.example.org",
         "OSADA_APPTAB_HOST": "apptab.example.org", "OSADA_APPTAB_UPSTREAM": "http://127.0.0.1:9"}
    e.update(extra or {})
    return e


def https(host, path="/"):
    """Through the edge's https port with this server name: status and text."""
    ctx = ssl.create_default_context()
    ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
    with ctx.wrap_socket(socket.create_connection(("127.0.0.1", EDGE_HTTPS), timeout=10), server_hostname=host) as t:
        t.sendall(f"GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n".encode())
        data = b""
        while chunk := t.recv(65536):
            data += chunk
    text = data.decode(errors="replace")
    return int(text.split(" ", 2)[1]), text


def cert(dir, *names):
    os.makedirs(dir, exist_ok=True)
    run("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
        "-keyout", f"{dir}/privkey.pem", "-out", f"{dir}/fullchain.pem", "-days", "2", "-subj", f"/CN={names[0]}",
        "-addext", "subjectAltName=" + ",".join("DNS:" + n for n in names))
    return f"{dir}/fullchain.pem", f"{dir}/privkey.pem"


def api(opener, method, path, body=None):
    req = urllib.request.Request(f"http://127.0.0.1:{HOME_HTTP}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(),
                                 headers={"x-shelf": "1", "content-type": "application/json"})
    try:
        with opener.open(req, timeout=20) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")


def cluster():
    return run(B, "cluster", "--data", f"{R}/data/node").stdout


def main():
    shutil.rmtree(R, ignore_errors=True)
    os.makedirs(f"{R}/data", mode=0o700)
    run("sqlite3", "file:/home/osada/shelf.db?mode=ro", f".backup {R}/data/shelf.db")
    shutil.copy2("/home/osada/secret.key", f"{R}/data/secret.key")
    for d in ("users", "wiki", "thumbs", "tmp", "deleted"):
        os.makedirs(f"{R}/data/{d}")
    run("sqlite3", f"{R}/data/shelf.db", "DELETE FROM settings WHERE key = 'backup'; DELETE FROM sessions;")
    with_mail = int(run("sqlite3", f"{R}/data/shelf.db", "SELECT count(*) FROM users WHERE mail IS NOT NULL AND mail != '' AND sub IS NOT NULL").stdout.strip() or 0)

    # ---- phase 1: the drop-in, no OSADA_NODE
    start("home", [B, "serve"], home_env())
    check("phase 1: home starts as the drop-in", wait_for(lambda: "node of the cluster" in log("home")), log("home")[-1500:])
    m = re.search(r"home \(([0-9a-f-]{36})\): a new cluster", log("home"))
    check("its id is a GUID, its name the host's: home", m, log("home")[:800])
    home_id = m.group(1) if m else "?"
    imported = open(f"{R}/data/node/imported").read() if os.path.exists(f"{R}/data/node/imported") else ""
    check("osada 0.1's accounts imported", len([l for l in imported.splitlines() if l.startswith("imported ")]) >= 8, imported)
    c = cluster()
    check("node control: the GUID, named home", f'"{home_id}"' in c and '"name": "home"' in c, c[:600])

    # ---- phase 2: the edge mail-1, joined at home by address, its folder moved
    tok = run(B, "token", "--data", f"{R}/data/node", "--node", "mail-1", "--roles", "edge-http,edge-mail,idp,store,blob")
    t = tok.stdout.strip()
    check("a join token for mail-1", t.startswith("osada-join."), tok.stderr)
    j = run(B, "join", "--data", f"{R}/stage/node", "--token", t, "--peer", f"127.0.0.1:{LINK_HOME}")
    m = re.search(r"\(([0-9a-f-]{36})\)", j.stdout)
    check("the join, at home, by address only: a GUID for mail-1", j.returncode == 0 and m, j.stdout + j.stderr)
    edge_id = m.group(1) if m else "?"
    os.rename(f"{R}/stage/node", f"{R}/edge")
    cert(f"{R}/edge/edge-tls", "shelf.example.org", "apptab.example.org")
    sc, sk = cert(f"{R}/stalwart", "mail.example.org")
    start("stalwart", ["python3", "-I", f"{HERE}/fake_stalwart.py", str(STALWART), sc, sk])
    start("edge", [B, "serve", "--data", f"{R}/edge", "--listen", f"127.0.0.1:{LINK_EDGE}",
                   "--relay", f"127.0.0.1:{EDGE_HTTPS}", "--relay-http", f"127.0.0.1:{EDGE_HTTP}"])
    check("the edge serves (no --relay-to)", wait_for(lambda: "relays https" in log("edge"), 20), log("edge"))
    stop("home")
    start("home", [B, "serve"], home_env({
        "OSADA_PEERS": f"mail-1@127.0.0.1:{LINK_EDGE}", "OSADA_MAIL_NODE": "mail-1",
        "OSADA_MAIL_HOST": "mail.example.org", "OSADA_MAIL_UPSTREAM": f"tls://127.0.0.1:{STALWART}"}))
    got = wait_for(lambda: re.search(r"mail: mail\.example\.org on mail-1 \(([0-9a-f-]{36})\).*access for (\d+) of (\d+)", log("home")), 90)
    check(f"the mail instance on mail-1 by its name, access for the {with_mail} with an address",
          got and got.group(1) == edge_id and int(got.group(2)) == with_mail, log("home")[-1500:])
    st = wait_for(lambda: (lambda s: s if s[0] != 503 else None)(https("shelf.example.org")), 60)
    check("the edge relays osada's name home", st and st[0] in (200, 301, 302, 303), st and st[1][:300])
    sw = wait_for(lambda: (lambda s: s if "STALWART-OK from 127.0.0.1" in s[1] else None)(https("mail.example.org")), 30)
    check("mail.example.org passed on to Stalwart, with the visitor's address", sw, log("stalwart"))
    c = cluster()
    check("node control: mail-1 by its GUID", f'"{edge_id}"' in c and '"name": "mail-1"' in c, c[:800])

    # ---- names are editable: rename home through the admin API
    r = run(B, "rescue", "rehearsal-admin", "--admin", "--create", "--data", f"{R}/data/node")
    pw = re.search(r"\b[0-9a-f]{20}\b", r.stdout)
    check("rescue: an administrator for the rehearsal", r.returncode == 0 and pw, r.stderr)
    wait_for(lambda: "rescue: rehearsal-admin" in log("home"), 20)
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    s, _ = api(op, "POST", "/api/signin", {"name": "rehearsal-admin", "password": pw.group(0) if pw else ""})
    check("signed in", s == 200, s)
    s, ov = api(op, "GET", "/api/admin/overview")
    p = ov["placement"] if s == 200 else {}
    p["nodes"][home_id]["name"] = "home-renamed"
    s, body = api(op, "POST", "/api/admin/placement", p)
    check("home renamed (home → home-renamed)", s == 200, body)
    check("node control says so", '"name": "home-renamed"' in cluster())
    st = https("shelf.example.org")
    check("the edge relays home after the rename", st[0] in (200, 301, 302, 303), st[1][:300])
    s, ov = api(op, "GET", "/api/admin/overview")
    p = ov["placement"]
    p["nodes"][home_id]["name"] = "MAIL-1"
    s, body = api(op, "POST", "/api/admin/placement", p)
    check("a name another node has is refused", s != 200 and "another node is MAIL-1" in str(body), (s, body))
    p["nodes"][home_id]["name"] = "home"
    s, body = api(op, "POST", "/api/admin/placement", p)
    check("the name put back: home", s == 200 and '"name": "home"' in cluster(), body)
    s, me = api(op, "GET", "/api/account")
    check("the account page names the node", s == 200 and me.get("node_name") == "home" and me.get("node") == home_id, me if s == 200 else s)


try:
    main()
finally:
    for n, _ in list(procs):
        stop(n)
    print(("all passed" if not failed else f"{len(failed)} failed: " + "; ".join(failed)) + f" (logs in {R})")
    sys.exit(1 if failed else 0)
