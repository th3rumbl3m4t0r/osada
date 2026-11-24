#!/usr/bin/python3 -I
"""One issuer per certificate (D-U26) and sites that run on another node
(D-U27), on localhost: node A (home), edge B joined to it; Pebble (Let's
Encrypt's test server, every challenge taken as valid) as the ACME server.

    python3 -I tests/web_cluster.py <pebble dir> [<osada binary>]

<pebble dir>: pebble-linux-amd64/linux/amd64/pebble, localhost-cert.pem,
localhost-key.pem, certs-pebble.minica.pem. Ports 18790-18799 and 14000/15000
on 127.0.0.1; everything in /root/t-cl (removed first).
"""
import http.cookiejar, json, os, shutil, socket, ssl, subprocess, sys, time, urllib.request

P = sys.argv[1]
B = sys.argv[2] if len(sys.argv) > 2 else "/root/osada-0.2/target/debug/osada"
D = "/root/t-cl"
A_LINK, A_HTTP, B_LINK, B_HTTPS, B_PLAIN = 18790, 18791, 18792, 18793, 18794
failed, procs = [], []


def check(what, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + what + (f"\n     {detail}" if detail and not cond else ""), flush=True)
    if not cond:
        failed.append(what)
    return cond


def wait_for(f, secs):
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


def start(name, args, env=None):
    log = open(f"{D}/{name}.log", "w")
    procs.append(subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT, env=env, cwd=D))


def raw(port, host, path="/", tls=False):
    s = socket.create_connection(("127.0.0.1", port), timeout=10)
    cert = None
    if tls:
        ctx = ssl.create_default_context()
        ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
        s = ctx.wrap_socket(s, server_hostname=host)
        cert = s.getpeercert(binary_form=True)
    s.sendall(f"GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n".encode())
    data = b""
    while chunk := s.recv(65536):
        data += chunk
    t = data.decode(errors="replace")
    return int(t.split(" ", 2)[1]), t, cert


def main():
    shutil.rmtree(D, ignore_errors=True)
    os.makedirs(D)
    with open(f"{D}/pebble.json", "w") as f:
        json.dump({"pebble": {"listenAddress": "127.0.0.1:14000", "managementListenAddress": "127.0.0.1:15000", "certificate": f"{P}/localhost-cert.pem",
                              "privateKey": f"{P}/localhost-key.pem", "httpPort": 5002, "tlsPort": 5001, "ocspResponderURL": "", "externalAccountBindingRequired": False}}, f)
    pe = dict(os.environ, PEBBLE_VA_ALWAYS_VALID="1", PEBBLE_VA_NOSLEEP="1", PEBBLE_WFE_NONCEREJECT="0")
    start("pebble", [f"{P}/pebble-linux-amd64/linux/amd64/pebble", "-config", f"{D}/pebble.json"], pe)
    env = dict(os.environ, OSADA_ACME_DIRECTORY="https://127.0.0.1:14000/dir", OSADA_ACME_ROOT=f"{P}/certs-pebble.minica.pem", OSADA_CERT_ROUND="3")
    subprocess.run([B, "init", "--data", f"{D}/a", "--node", "home-a"], check=True, capture_output=True)
    start("a", [B, "serve", "--data", f"{D}/a", "--listen", f"127.0.0.1:{A_LINK}", "--http", f"127.0.0.1:{A_HTTP}", "--peer", f"127.0.0.1:{B_LINK}"], env)
    time.sleep(2)
    tok = subprocess.run([B, "token", "--data", f"{D}/a", "--node", "edge-b", "--roles", "edge-http,worker"], capture_output=True, text=True).stdout.strip()
    j = subprocess.run([B, "join", "--data", f"{D}/b", "--token", tok, "--peer", f"127.0.0.1:{A_LINK}"], capture_output=True, text=True)
    check("edge-b joined", j.returncode == 0, j.stderr)
    with open(f"{D}/b/cloudflare-ips", "w") as f:
        f.write("127.0.0.1/32\n")
    start("b", [B, "serve", "--data", f"{D}/b", "--listen", f"127.0.0.1:{B_LINK}", "--relay", f"127.0.0.1:{B_HTTPS}", "--relay-http", f"127.0.0.1:{B_PLAIN}"], env)
    check("A and B linked", wait_for(lambda: "links: " in open(f"{D}/b.log").read() and "none" not in open(f"{D}/b.log").read().split("links: ")[-1].split("\n")[0], 30), open(f"{D}/b.log").read()[-500:])

    pw = open(f"{D}/a/first-admin").read().split()[-1]
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def api(method, path, body=None):
        req = urllib.request.Request(f"http://127.0.0.1:{A_HTTP}{path}", method=method, data=None if body is None else json.dumps(body).encode(),
                                     headers={"x-shelf": "1", "content-type": "application/json"})
        try:
            with op.open(req, timeout=20) as r:
                return r.status, json.loads(r.read() or b"null")
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors="replace")

    api("POST", "/api/signin", {"name": "admin", "password": pw})
    s, ov = api("GET", "/api/admin/overview")
    pl = ov["placement"]
    ids = {(n.get("name") or i): i for i, n in pl["nodes"].items()}
    a, b = ids["home-a"], ids["edge-b"]
    pl["certs"] = {"t": {"names": ["tls.example.org"], "issuer": a, "copies": [b]}}
    pl["modules"]["far"] = {"kind": "web", "title": "", "node": a, "site": {"names": ["far.example.org"], "exposed_on": [b], "content": {"kind": "static"}, "front": "cloudflare"}}
    pl["modules"]["tlsfar"] = {"kind": "web", "title": "", "node": a, "site": {"names": ["tls.example.org"], "exposed_on": [b], "content": {"kind": "static"}, "front": "tls"}}
    s, body = api("POST", "/api/admin/placement", pl)
    check("a certificate and two sites that run on A, exposed on B", s == 200, body)
    for site, text in (("far", "FAR AT HOME"), ("tlsfar", "TLS AT HOME")):
        os.makedirs(f"{D}/a/web/{site}")
        open(f"{D}/a/web/{site}/index.html", "w").write(text)
    st = wait_for(lambda: (lambda r: r if r[0] == 200 else None)(raw(B_PLAIN, "far.example.org")), 20)
    check("D-U27: a site that runs on A, through B's plain http", st and st[1].endswith("FAR AT HOME"), st and st[1][:300])
    m = wait_for(lambda: json.load(open(f"{D}/a/certs/t/meta.json")), 60)
    check("D-U26: A issued certificate t by ACME (Pebble)", m and m["names"] == ["tls.example.org"], open(f"{D}/a.log").read()[-800:])
    mb = wait_for(lambda: (lambda x: x if m and x["serial"] == m["serial"] else None)(json.load(open(f"{D}/b/certs/t/meta.json"))), 30)
    check("B got a copy (the same serial)", mb, open(f"{D}/b.log").read()[-800:])
    st = wait_for(lambda: (lambda r: r if r[0] == 200 else None)(raw(B_HTTPS, "tls.example.org", tls=True)), 40)
    issuer_ok = bool(st) and b"Pebble" in (st[2] or b"")
    check("B answers https for the name with that certificate, the site from A", st and st[1].endswith("TLS AT HOME") and issuer_ok, st and (st[1][:200], issuer_ok))
    os.makedirs(f"{D}/a/acme/.well-known/acme-challenge", exist_ok=True)
    open(f"{D}/a/acme/.well-known/acme-challenge/tok123", "w").write("tok123.thumbprint")
    st = raw(B_PLAIN, "tls.example.org", "/.well-known/acme-challenge/tok123")
    check("an http-01 token at B, from its issuer A", st[0] == 200 and st[1].endswith("tok123.thumbprint"), st[1][:300])
    st = raw(B_PLAIN, "far.example.org", "/.well-known/acme-challenge/nope")
    check("an unknown token: 404", st[0] == 404, st[1][:200])

    # T13: a site's files, through A's admin, on the node it runs on (B)
    import base64
    s, ov = api("GET", "/api/admin/overview")
    pl = ov["placement"]
    pl["modules"]["onb"] = {"kind": "web", "title": "", "node": b, "site": {"names": ["onb.example.org"], "exposed_on": [b], "content": {"kind": "static"}, "front": "cloudflare"}}
    s, body = api("POST", "/api/admin/placement", pl)
    check("a site that runs on B", s == 200, body)
    time.sleep(1)
    f = lambda **k: api("POST", "/api/admin/web/files", {"site": "onb", **k})
    s, v = f(op="write", path="index.html", data=base64.b64encode(b"<h1>WRITTEN FROM A</h1>").decode())
    check("T13: a file written through A lands on B", s == 200, v)
    st = wait_for(lambda: (lambda r: r if r[0] == 200 else None)(raw(B_PLAIN, "onb.example.org")), 15)
    check("B serves it", st and st[1].endswith("<h1>WRITTEN FROM A</h1>"), st and st[1][:200])
    f(op="mkdir", path="img")
    s, v = f(op="list", path="")
    names = sorted(e["name"] for e in v.get("entries", [])) if s == 200 else v
    check("the folder's list", names == ["img", "index.html"], names)
    req = urllib.request.Request(f"http://127.0.0.1:{A_HTTP}/api/admin/web/file?site=onb&path=index.html")
    with op.open(req, timeout=10) as r:
        check("download", r.read() == b"<h1>WRITTEN FROM A</h1>" and "attachment" in r.headers.get("content-disposition", ""))
    s, v = f(op="rename", path="index.html", to="img/old.html")
    s2, v2 = f(op="list", path="img")
    check("rename into a folder", s == 200 and [e["name"] for e in v2.get("entries", [])] == ["old.html"], (v, v2))
    s, v = f(op="read", path="../../a/tls/node.key")
    check("a path out of the folder is refused", s == 400, (s, v))
    s, v = f(op="delete", path="img", all=True)
    s2, v2 = f(op="list", path="")
    check("delete a folder and what is in it", s == 200 and v2.get("entries") == [], (v, v2))


try:
    main()
finally:
    for p in procs:
        p.terminate()
        try:
            p.wait(10)
        except Exception:
            p.kill()
    print("all passed" if not failed else f"{len(failed)} failed: " + "; ".join(failed))
    sys.exit(1 if failed else 0)
