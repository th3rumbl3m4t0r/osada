"""The authoritative web node (Round 16) on localhost: home is the authoritative web node, e1 an
edge, n2 the node a static site runs on. A file written in admin → web lands at home (the master
copy) and is copied to n2 within 30 s; the edge serves the site from n2; n2 stopped, the edge
serves it from home's copy; an edit made meanwhile shows at once.

    python3 -I tests/web_authority.py [<the osada binary>]       (as root; /root/t-wa, removed after)
"""
import base64, json, os, re, shutil, signal, subprocess, sys, time

B = sys.argv[1] if len(sys.argv) > 1 else "/root/osada-0.2/target/release/osada"
D = "/root/t-wa"
HL, HTTP, EL, HTTPS, PLAIN, NL = 18960, 18961, 18962, 18963, 18964, 18965
failed = []


def check(what, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + what + (f"\n     {detail}" if detail and not cond else ""), flush=True)
    if not cond:
        failed.append(what)


def wait_for(what, f, secs=60):
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


def api(method, path, body=None, cookie=None):
    out = subprocess.run(["curl", "-s", "-X", method, "-H", "x-shelf: 1", "-H", "content-type: application/json", "-w", "\n%{http_code}"]
                         + (["-H", f"cookie: {cookie}"] if cookie else []) + (["--data-binary", json.dumps(body)] if body is not None else [])
                         + [f"http://127.0.0.1:{HTTP}{path}"], capture_output=True, text=True).stdout
    b, _, code = out.rpartition("\n")
    return int(code or 0), b


def site():
    return subprocess.run(["curl", "-sk", "--max-time", "10", "--resolve", f"s1.example:{HTTPS}:127.0.0.1", f"https://s1.example:{HTTPS}/"], capture_output=True, text=True).stdout


shutil.rmtree(D, ignore_errors=True)
os.makedirs(D)
procs = {}
try:
    subprocess.run([B, "init", "--data", f"{D}/home", "--node", "home"], check=True, capture_output=True)
    procs["home"] = subprocess.Popen([B, "serve", "--data", f"{D}/home", "--listen", f"127.0.0.1:{HL}", "--http", f"127.0.0.1:{HTTP}"], stdout=open(f"{D}/home.log", "w"), stderr=subprocess.STDOUT)
    wait_for("home's first administrator", lambda: os.path.exists(f"{D}/home/first-admin"))
    pw = open(f"{D}/home/first-admin").read().split("password:")[1].strip()
    out = subprocess.run(["curl", "-s", "-D", "-", "-o", "/dev/null", "-H", "x-shelf: 1", "-H", "content-type: application/json", "--data-binary",
                          json.dumps({"name": "admin", "password": pw}), f"http://127.0.0.1:{HTTP}/api/signin"], capture_output=True, text=True).stdout
    admin = re.search(r"set-cookie: ([^;]+)", out, re.I).group(1)
    for n, link, roles in [("e1", EL, "edge-http"), ("n2", NL, "store")]:
        tok = subprocess.run([B, "token", "--data", f"{D}/home", "--node", n, "--roles", roles, "--dial", f"127.0.0.1:{link}"], capture_output=True, text=True).stdout.strip()
        j = subprocess.run([B, "join", "--data", f"{D}/{n}", "--node", n, "--token", tok, "--listen", f"127.0.0.1:{link}", "--wait", "2"], capture_output=True, text=True)
        check(f"{n} joined", "joined the cluster" in j.stdout, j.stdout + j.stderr)
    os.makedirs(f"{D}/e1/edge-tls")
    subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", f"{D}/e1/edge-tls/privkey.pem",
                    "-out", f"{D}/e1/edge-tls/fullchain.pem", "-days", "2", "-subj", "/CN=s1.example", "-addext", "subjectAltName=DNS:s1.example"], check=True, capture_output=True)
    procs["e1"] = subprocess.Popen([B, "serve", "--data", f"{D}/e1", "--listen", f"127.0.0.1:{EL}", "--relay", f"127.0.0.1:{HTTPS}", "--relay-http", f"127.0.0.1:{PLAIN}"], stdout=open(f"{D}/e1.log", "w"), stderr=subprocess.STDOUT)
    procs["n2"] = subprocess.Popen([B, "serve", "--data", f"{D}/n2", "--listen", f"127.0.0.1:{NL}"], stdout=open(f"{D}/n2.log", "w"), stderr=subprocess.STDOUT)
    ov = wait_for("the three nodes", lambda: (lambda o: o if len(o["placement"]["nodes"]) == 3 and len(o.get("links", {})) >= 2 else None)(json.loads(api("GET", "/api/admin/overview", None, admin)[1])), 60)
    ids = {(n.get("name") or i): i for i, n in ov["placement"]["nodes"].items()}
    home, e1, n2 = ov["me"], next(i for i, n in ov["placement"]["nodes"].items() if "edge-http" in n["roles"] and i != ov["me"]), None
    n2 = next(i for i in ov["placement"]["nodes"] if i not in (home, e1))
    p = ov["placement"]
    p["modules"]["s1"] = {"kind": "web", "title": "s1", "node": n2, "site": {"names": ["s1.example"], "exposed_on": [e1], "content": {"kind": "static"}, "front": "tls"}}
    p["web"] = {"authority": home}
    c, b = api("POST", "/api/admin/placement", p, admin)
    check("the site on n2, home authoritative", c == 200, b)
    c, b = api("POST", "/api/admin/web/files", {"site": "s1", "op": "write", "path": "/index.html", "data": base64.b64encode(b"<p>one</p>").decode()}, admin)
    check("a file written in admin → web", c == 200, b)
    check("... it is at home (the master copy)", open(f"{D}/home/web/s1/index.html").read() == "<p>one</p>")
    check("... and copied to n2", bool(wait_for("the copy", lambda: open(f"{D}/n2/web/s1/index.html").read() == "<p>one</p>", 45)))
    check("the edge serves it (from n2)", bool(wait_for("the site", lambda: "<p>one</p>" in site(), 30)), site()[:200])
    procs["n2"].send_signal(signal.SIGTERM)
    procs["n2"].wait(timeout=20)
    c, b = api("POST", "/api/admin/web/files", {"site": "s1", "op": "write", "path": "/index.html", "data": base64.b64encode(b"<p>two</p>").decode()}, admin)
    check("n2 away: an edit goes to home", c == 200, b)
    check("... the edge serves home's copy", bool(wait_for("the fallback", lambda: "<p>two</p>" in site(), 40)), site()[:200])
    # taken over (Round 17): a site whose files are only on the node it runs on; the authoritative
    # node fetches them itself, in the background
    procs["n2"] = subprocess.Popen([B, "serve", "--data", f"{D}/n2", "--listen", f"127.0.0.1:{NL}"], stdout=open(f"{D}/n2b.log", "w"), stderr=subprocess.STDOUT)
    os.makedirs(f"{D}/n2/web/s2/img", exist_ok=True)
    open(f"{D}/n2/web/s2/index.html", "w").write("<p>s2 at n2</p>")
    open(f"{D}/n2/web/s2/img/x.txt", "w").write("x")
    ov = json.loads(api("GET", "/api/admin/overview", None, admin)[1])
    p = ov["placement"]
    p["modules"]["s2"] = {"kind": "web", "title": "s2", "node": n2, "site": {"names": ["s2.example"], "exposed_on": [e1], "content": {"kind": "static"}, "front": "tls"}}
    c, b = api("POST", "/api/admin/placement", p, admin)
    check("a site with files only at n2", c == 200, b)
    check("... home takes them over by itself", bool(wait_for("the takeover", lambda: open(f"{D}/home/web/s2/index.html").read() == "<p>s2 at n2</p>" and open(f"{D}/home/web/s2/img/x.txt").read() == "x", 60)))
    c, b = api("POST", "/api/admin/web/files", {"site": "s2", "op": "list", "path": "/"}, admin)
    check("... admin → web lists them (from home)", c == 200 and "index.html" in b, b[:200])
    check("... n2 keeps them", open(f"{D}/n2/web/s2/index.html").read() == "<p>s2 at n2</p>")
finally:
    for pr in procs.values():
        pr.send_signal(signal.SIGTERM)
    for pr in procs.values():
        try:
            pr.wait(timeout=20)
        except Exception:
            pr.kill()
print("FAILED: " + ", ".join(failed) if failed else "all passed")
if not failed:
    shutil.rmtree(D, ignore_errors=True)
sys.exit(1 if failed else 0)
