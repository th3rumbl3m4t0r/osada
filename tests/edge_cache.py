#!/usr/bin/python3 -I
"""D-U39/D-U40 on localhost: a drop-in home and two edges (cache, public-cache) that joined by
invitation. The page and its static files from the edge; a file's copy: from home, then from
the other edge (20 MB and 260 MB), one fetch for four requests at once; a new public link
prefetched by both edges; home stopped: the page, the API's offline answer and the link's file
at once.  python3 -I tests/edge_cache.py [<osada binary>]
Ports 18700-18722 on 127.0.0.1, everything in /root/t-ec (removed first)."""
import hashlib, json, os, shutil, signal, subprocess, sys, time, concurrent.futures as cf

B = sys.argv[1] if len(sys.argv) > 1 else "/root/osada-0.2/target/release/osada"
D = "/root/t-ec"
HOME_LINK, HOME_HTTP = 18700, 18701
EDGES = {"e1": (18710, 18711, 18712), "e2": (18720, 18721, 18722)}
HOST = "test.example"
failed = []


def check(what, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + what + (f"\n     {detail}" if detail and not cond else ""), flush=True)
    if not cond:
        failed.append(what)


def curl(url, *args, resolve=None, timeout=120):
    """status, headers (lower case), body, seconds"""
    body_file = f"{D}/.body-{os.getpid()}-{time.monotonic_ns()}"
    cmd = ["curl", "-sk", "-D", "-", "--max-time", str(timeout), "-o", body_file, "-w", "\n%{time_total}"]
    if resolve:
        cmd += ["--resolve", f"{HOST}:{resolve}:127.0.0.1"]
    text = subprocess.run(cmd + list(args) + [url], capture_output=True).stdout.decode(errors="replace")
    try:
        body = open(body_file, "rb").read()
        os.remove(body_file)
    except OSError:
        body = b""
    head, _, t = text.rpartition("\n")
    blocks = [b for b in head.replace("\r", "").split("\n\n") if b.strip()]
    lines = blocks[-1].split("\n") if blocks else []
    status = int(lines[0].split()[1]) if lines and lines[0].startswith("HTTP") else 0
    hdrs = {l.split(":", 1)[0].lower(): l.split(":", 1)[1].strip() for l in lines[1:] if ":" in l}
    return status, hdrs, body, float(t or 0)


def edge_url(e, path):
    return f"https://{HOST}:{EDGES[e][1]}{path}"


def sha(b):
    return hashlib.sha256(b).hexdigest()[:16]


def wait_for(what, f, secs=60):
    end = time.time() + secs
    while time.time() < end:
        try:
            if f():
                return True
        except Exception:
            pass
        time.sleep(0.5)
    check(f"waiting for {what}", False)
    return False


shutil.rmtree(D, ignore_errors=True)
os.makedirs(D)
procs, logs = {}, {}


def start_home():
    env = dict(os.environ, OSADA_DATA=f"{D}/home", OSADA_BIND=f"127.0.0.1:{HOME_HTTP}", OSADA_LINK_BIND=f"127.0.0.1:{HOME_LINK}", OSADA_NODE="home")
    logs["home"] = open(f"{D}/home.log", "a")
    procs["home"] = subprocess.Popen([B, "serve"], env=env, stdout=logs["home"], stderr=subprocess.STDOUT)


def start_edge(e):
    link, https, plain = EDGES[e]
    logs[e] = open(f"{D}/{e}.log", "a")
    procs[e] = subprocess.Popen([B, "serve", "--data", f"{D}/{e}", "--listen", f"127.0.0.1:{link}", "--relay", f"127.0.0.1:{https}", "--relay-http", f"127.0.0.1:{plain}"],
                                stdout=logs[e], stderr=subprocess.STDOUT)


def api(base, method, path, body=None, cookie=None, resolve=None):
    args = ["-X", method, "-H", "x-shelf: 1", "-H", "content-type: application/json"]
    if cookie:
        args += ["-H", f"cookie: {cookie}"]
    if body is not None:
        args += ["--data-binary", json.dumps(body)]
    s, h, b, _ = curl(base + path, *args, resolve=resolve)
    return s, h, b


try:
    os.makedirs(f"{D}/home")
    start_home()
    wait_for("home's first administrator", lambda: os.path.exists(f"{D}/home/node/first-admin"))
    pw = open(f"{D}/home/node/first-admin").read().split("password:")[1].strip()
    hb = f"http://127.0.0.1:{HOME_HTTP}"
    s, h, _ = api(hb, "POST", "/api/signin", {"name": "admin", "password": pw})
    admin = h.get("set-cookie", "").split(";")[0]
    s, _, b = api(hb, "POST", "/api/admin/users", {"name": "alice", "display": "Alice", "ou": 1, "password": "alice-pass-123456"}, cookie=admin)
    check("alice made", s == 200, b[:200])
    # the edges: a token for each with --dial, joined by invitation, a certificate for the name
    for e, (link, https, plain) in EDGES.items():
        tok = subprocess.run([B, "token", "--data", f"{D}/home/node", "--node", e, "--roles", "edge-http,idp,store,blob,worker,cache,public-cache",
                              "--dial", f"127.0.0.1:{link}"], capture_output=True, text=True).stdout.strip()
        j = subprocess.run([B, "join", "--data", f"{D}/{e}", "--node", e, "--token", tok, "--listen", f"127.0.0.1:{link}", "--wait", "2"], capture_output=True, text=True)
        check(f"{e} joined by invitation", "joined the cluster" in j.stdout, j.stdout + j.stderr)
        os.makedirs(f"{D}/{e}/edge-tls")
        subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", f"{D}/{e}/edge-tls/privkey.pem",
                        "-out", f"{D}/{e}/edge-tls/fullchain.pem", "-days", "2", "-subj", f"/CN={HOST}", "-addext", f"subjectAltName=DNS:{HOST}"], check=True, capture_output=True)
        start_edge(e)
    wait_for("both edges answering", lambda: all(curl(edge_url(e, "/"), resolve=EDGES[e][1])[0] == 200 for e in EDGES))
    wait_for("the edges linked to each other (the mesh)", lambda: "e1" in open(f"{D}/e2.log").read() or all(
        any(l.startswith("links:") and l.count("ms") >= 2 for l in open(f"{D}/{e}.log").read().splitlines()[-5:]) for e in EDGES), 90)
    # alice once at home (osada 0.1's side knows her from her first visit), her files
    s, h, _ = api(hb, "POST", "/api/signin", {"name": "alice", "password": "alice-pass-123456"})
    ac = h.get("set-cookie", "").split(";")[0]
    api(hb, "GET", "/api/me", cookie=ac)
    os.makedirs(f"{D}/home/users/alice", exist_ok=True)
    data = {}
    for name, mb in [("a20", 20), ("c40", 40), ("p30", 30), ("big", 260)]:
        b = os.urandom(mb << 20)
        open(f"{D}/home/users/alice/{name}.bin", "wb").write(b)
        data[name] = sha(b)
        del b

    # the page and its static files: from the edge
    s1, h1, page, _ = curl(edge_url("e1", "/"), resolve=EDGES["e1"][1])
    s2, h2, _, _ = curl(edge_url("e1", "/admin/nodes"), resolve=EDGES["e1"][1])
    check("page: 200, then from the edge's copy", s1 == 200 and s2 == 200 and h2.get("x-edge-cache") == "hit", f"{s1} {s2} {h2}")
    other = curl(f"https://other.example:{EDGES['e1'][1]}/", "--resolve", f"other.example:{EDGES['e1'][1]}:127.0.0.1")
    other2 = curl(f"https://other.example:{EDGES['e1'][1]}/", "--resolve", f"other.example:{EDGES['e1'][1]}:127.0.0.1")
    # copies are per host: another name's first answer is home's (a proxy's page there is never osada's)
    check("another host: not from test.example's copy", other[0] == 200 and other[1].get("x-edge-cache") != "hit", f"{other[0]} {other[1]}")
    asset = page.decode().split('src="')[1].split('"')[0] if b'src="/static2/' in page else None
    s3, h3, b3, _ = curl(edge_url("e1", asset), "-H", "accept-encoding: gzip", resolve=EDGES["e1"][1])
    s4, h4, b4, _ = curl(edge_url("e1", asset), "-H", "accept-encoding: gzip", resolve=EDGES["e1"][1])
    check("static file: gzip from home, then from the edge", s3 == 200 and h3.get("content-encoding") == "gzip" and h4.get("x-edge-cache") == "hit" and b3 == b4, f"{h3} {h4}")
    s5, h5, b5, _ = curl(edge_url("e1", asset), resolve=EDGES["e1"][1])
    import gzip
    check("... plain for a client without gzip", s5 == 200 and "content-encoding" not in h5 and gzip.decompress(b3) == b5, str(h5))

    # alice through the edges
    cookies = {}
    for e in EDGES:
        s, h, _ = api(f"https://{HOST}:{EDGES[e][1]}", "POST", "/api/signin", {"name": "alice", "password": "alice-pass-123456"}, resolve=EDGES[e][1])
        cookies[e] = h.get("set-cookie", "").split(";")[0]

    def raw(e, name, *extra):
        return curl(edge_url(e, f"/api/raw/home/{name}.bin"), "-H", f"cookie: {cookies[e]}", *extra, resolve=EDGES[e][1])

    s, h, b, _ = raw("e1", "a20")
    check("20 MB at e1: from home", s == 200 and sha(b) == data["a20"] and h.get("x-edge-source") == "home", str(h))
    s, h, b, _ = raw("e1", "a20")
    check("... again: e1's copy", s == 200 and sha(b) == data["a20"] and h.get("x-edge-cache") == "hit", str(h))
    s, h, b, _ = raw("e2", "a20")
    check("20 MB at e2: from e1's copy (ask any edge first)", s == 200 and sha(b) == data["a20"] and h.get("x-edge-source") == "peer", str(h))
    s, h, b, _ = raw("e2", "a20", "-H", "range: bytes=1000-1999")
    check("... a range from e2's copy now", s == 206 and len(b) == 1000 and h.get("x-edge-cache") == "hit", str(h))

    # four at once: one fetch, the others follow it (or find the copy)
    with cf.ThreadPoolExecutor(4) as ex:
        rs = list(ex.map(lambda _: raw("e1", "c40"), range(4)))
    srcs = [r[1].get("x-edge-source") or r[1].get("x-edge-cache") for r in rs]
    check("40 MB, four requests at once at e1: all whole", all(r[0] == 200 and sha(r[2]) == data["c40"] for r in rs), str(srcs))
    check("... one fetch: the others followed it or found the copy", srcs.count("home") == 1 and all(x in ("home", "follow", "hit") for x in srcs), str(srcs))

    # big: weighed (the links' rates and how busy each is), e1's copy over home's line
    s, h, b, _ = raw("e1", "big")
    check("260 MB at e1: from home", s == 200 and sha(b) == data["big"], str(h))
    del b
    s, h, b, _ = raw("e2", "big")
    check("260 MB at e2: from e1 (weighed against home)", s == 200 and sha(b) == data["big"] and h.get("x-edge-source") == "peer", str(h))
    del b

    # a new public link: both edges have the file before anyone asks
    s, _, b = api(hb, "POST", "/api/links", {"loc": "home", "path": "p30.bin"}, cookie=ac)
    token = json.loads(b).get("token") if s == 200 else None
    check("a public link", bool(token), b[:200])
    ok = wait_for("both edges to prefetch the new link", lambda: all(
        "prefetched" in open(f"{D}/{e}.log").read() for e in EDGES), 60)
    for e in EDGES:
        s, h, b, _ = curl(edge_url(e, f"/s/{token}/raw"), resolve=EDGES[e][1])
        check(f"link at {e}: from its copy at the first visit", s == 200 and sha(b) == data["p30"] and h.get("x-edge-cache") == "hit", str(h))

    # home away: the edge answers at once
    procs["home"].send_signal(signal.SIGTERM)
    procs["home"].wait(timeout=30)
    time.sleep(0.5)
    s, h, _, t = curl(edge_url("e1", "/"), resolve=EDGES["e1"][1])
    check(f"home stopped: the page at once ({t:.2f} s)", s == 200 and t < 1.0, f"{s} {h}")
    s, h, _, t = curl(edge_url("e1", "/api/account"), "-H", "x-shelf: 1", "-H", f"cookie: {cookies['e1']}", resolve=EDGES["e1"][1])
    check(f"... the API: offline at once ({t:.2f} s)", s == 503 and h.get("x-shelf-offline") == "1" and t < 2.0, f"{s} {h}")
    s, h, b, t = curl(edge_url("e2", f"/s/{token}/raw"), resolve=EDGES["e2"][1])
    check(f"... the link's file from the copy, marked offline ({t:.2f} s)", s == 200 and sha(b) == data["p30"] and h.get("x-shelf-offline") == "1" and t < 3.0, f"{s} {h}")
    start_home()
    wait_for("home back, through the edge", lambda: curl(edge_url("e1", "/api/account"), "-H", "x-shelf: 1", "-H", f"cookie: {cookies['e1']}", resolve=EDGES["e1"][1])[0] == 200, 60)
finally:
    for p in procs.values():
        p.send_signal(signal.SIGTERM)
    for p in procs.values():
        try:
            p.wait(timeout=20)
        except Exception:
            p.kill()
print("FAILED: " + ", ".join(failed) if failed else "all passed")
sys.exit(1 if failed else 0)
