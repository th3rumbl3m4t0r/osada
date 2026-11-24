#!/usr/bin/python3 -I
"""The web module end to end on localhost (D-U21): a new node that is its own
edge, sites made through the admin API, served by its relay.

    python3 -I tests/web_edge.py [<osada binary>]

Ports 18590-18594 on 127.0.0.1, everything in /root/t-web (removed first).
"""
import http.cookiejar, http.server, json, os, re, shutil, socket, ssl, subprocess, sys, threading, time, urllib.request

B = sys.argv[1] if len(sys.argv) > 1 else "/root/osada-0.2/target/debug/osada"
D = "/root/t-web"
LINK, HTTP, HTTPS, PLAIN, FALLBACK, WS = 18590, 18591, 18592, 18593, 18594, 18595
failed = []


def check(what, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + what + (f"\n     {detail}" if detail and not cond else ""), flush=True)
    if not cond:
        failed.append(what)


def raw(port, host, path="/", tls=False, extra=""):
    s = socket.create_connection(("127.0.0.1", port), timeout=10)
    if tls:
        ctx = ssl.create_default_context()
        ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
        s = ctx.wrap_socket(s, server_hostname=host)
    s.sendall(f"GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n".encode())
    data = b""
    while chunk := s.recv(65536):
        data += chunk
    t = data.decode(errors="replace")
    return int(t.split(" ", 2)[1]), t


class Fallback(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = f"FALLBACK host={self.headers.get('Host')} xff={self.headers.get('X-Forwarded-For')} path={self.path}".encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


def ws_echo():
    """A WebSocket server as pplc.example.net's scanner: the handshake, then every byte back."""
    import base64, hashlib
    ls = socket.socket()
    ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    ls.bind(("127.0.0.1", WS))
    ls.listen(4)

    def one(c):
        req = b""
        while b"\r\n\r\n" not in req:
            req += c.recv(4096)
        key = re.search(rb"(?i)sec-websocket-key: *(\S+)", req).group(1)
        acc = base64.b64encode(hashlib.sha1(key + b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest())
        path = req.split(b" ")[1]
        c.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + acc + b"\r\nX-Path: " + path + b"\r\n\r\n")
        while d := c.recv(4096):
            c.sendall(d)

    def serve():
        while True:
            c, _ = ls.accept()
            threading.Thread(target=one, args=(c,), daemon=True).start()
    threading.Thread(target=serve, daemon=True).start()


def main():
    ws_echo()
    shutil.rmtree(D, ignore_errors=True)
    subprocess.run([B, "init", "--data", D, "--node", "web-edge"], check=True, capture_output=True)
    os.makedirs(f"{D}/edge-tls")
    subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", f"{D}/edge-tls/privkey.pem",
                    "-out", f"{D}/edge-tls/fullchain.pem", "-days", "2", "-subj", "/CN=tls.example.org", "-addext", "subjectAltName=DNS:tls.example.org,DNS:osada.example.org"],
                   check=True, capture_output=True)
    with open(f"{D}/cloudflare-ips", "w") as f:
        f.write("127.0.0.1/32\n")  # this test is Cloudflare
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", FALLBACK), Fallback)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    log = open(f"{D}.log", "w")
    p = subprocess.Popen([B, "serve", "--data", D, "--listen", f"127.0.0.1:{LINK}", "--http", f"127.0.0.1:{HTTP}", "--relay", f"127.0.0.1:{HTTPS}",
                          "--relay-http", f"127.0.0.1:{PLAIN}", "--fallback-http", f"http://127.0.0.1:{FALLBACK}"], stdout=log, stderr=subprocess.STDOUT)
    try:
        time.sleep(3)
        pw = open(f"{D}/first-admin").read().split()[-1]
        op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

        def api(method, path, body=None):
            req = urllib.request.Request(f"http://127.0.0.1:{HTTP}{path}", method=method, data=None if body is None else json.dumps(body).encode(),
                                         headers={"x-shelf": "1", "content-type": "application/json"})
            try:
                with op.open(req, timeout=20) as r:
                    return r.status, json.loads(r.read() or b"null")
            except urllib.error.HTTPError as e:
                return e.code, e.read().decode(errors="replace")

        s, _ = api("POST", "/api/signin", {"name": "admin", "password": pw})
        check("signed in as the first administrator", s == 200, s)
        s, ov = api("GET", "/api/admin/overview")
        pl = ov["placement"]
        me = next(iter(pl["nodes"]))
        pl["modules"]["kosto"] = {"kind": "web", "title": "kosto", "node": me, "site": {"names": ["kosto.example.org", "www.kosto.example.org"], "exposed_on": [me],
                                  "content": {"kind": "static", "not_found": "418.html"}, "front": "cloudflare",
                                  "routes": [{"prefix": "/api/scan", "upstream": f"http://127.0.0.1:{WS}"}]}}
        pl["modules"]["tls-site"] = {"kind": "web", "title": "", "node": me, "site": {"names": ["tls.example.org"], "exposed_on": [me], "content": {"kind": "static", "spa": True},
                                     "front": "tls", "routes": [{"prefix": "/api/", "upstream": f"http://127.0.0.1:{FALLBACK}/back/"}]}}
        s, body = api("POST", "/api/admin/placement", pl)
        check("two sites saved in node control", s == 200, body)
        os.makedirs(f"{D}/web/kosto/media")
        open(f"{D}/web/kosto/index.html", "w").write("<h1>KOSTO</h1>")
        open(f"{D}/web/kosto/media/a.txt", "w").write("A")
        open(f"{D}/web/kosto/418.html", "w").write("TEAPOT")
        os.makedirs(f"{D}/web/tls-site")
        open(f"{D}/web/tls-site/index.html", "w").write("<h1>SPA</h1>")
        time.sleep(6)  # the edge looks at node control every 5 s
        st, t = raw(PLAIN, "kosto.example.org", "/", extra="CF-Connecting-IP: 198.51.100.7\r\n")
        check("behind Cloudflare: plain http serves the site's folder", st == 200 and "<h1>KOSTO</h1>" in t, t[:300])
        st, t = raw(PLAIN, "www.kosto.example.org", "/media/a.txt")
        check("its second name, a file below", st == 200 and t.endswith("A"), t[:300])
        st, t = raw(PLAIN, "kosto.example.org", "/missing")
        check("its own not-found page (nginx's error_page 404 /418.html)", st == 404 and t.endswith("TEAPOT"), t[:300])
        s = socket.create_connection(("127.0.0.1", PLAIN), timeout=10)
        s.sendall(b"GET /api/scan?x=1 HTTP/1.1\r\nHost: kosto.example.org\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                  b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n")
        head = b""
        while b"\r\n\r\n" not in head:
            head += s.recv(4096)
        s.sendall(b"ping through the edge")
        echo = b""
        while len(echo) < 21:
            echo += s.recv(4096)
        s.close()
        check("a WebSocket through a route (101, the accept key, bytes both ways)",
              head.startswith(b"HTTP/1.1 101") and b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo=" in head and b"/api/scan?x=1" in head and echo == b"ping through the edge", (head[:300], echo))
        st, t = raw(HTTPS, "tls.example.org", "/some/app/route", tls=True)
        check("its own certificate: https, a single-page app's route", st == 200 and "<h1>SPA</h1>" in t, t[:300])
        st, t = raw(PLAIN, "tls.example.org", "/x")
        check("its own certificate: plain http goes to https", st == 301 and "https://tls.example.org/x" in t, t[:300])
        st, t = raw(HTTPS, "tls.example.org", "/api/v1?q=2", tls=True)
        check("a route to an upstream, nginx's way", st == 200 and "path=/back/v1?q=2" in t and "xff=127.0.0.1" in t, t[:400])
        st, t = raw(PLAIN, "other.example.net", "/old")
        check("a name without a site or certificate: the fallback, Host kept", st == 200 and "host=other.example.net" in t and "path=/old" in t, t[:300])
        st, t = raw(PLAIN, "osada.example.org", "/")
        check("a name of the edge's certificate: not the fallback (https)", st == 301, t[:300])
        s, v = api("GET", "/api/admin/web")
        names = sorted(x["name"] for x in v.get("sites", [])) if s == 200 else v
        check("admin → web lists the sites", names == ["kosto", "tls-site"], names)
        s, body = api("POST", "/api/admin/web/action", {"site": "kosto", "action": "upgrade"})
        check("a container action on a static site is refused", s == 400, (s, body))
    finally:
        p.terminate()
        p.wait(10)
        srv.shutdown()
    print("all passed" if not failed else f"{len(failed)} failed: " + "; ".join(failed))
    sys.exit(1 if failed else 0)


main()
