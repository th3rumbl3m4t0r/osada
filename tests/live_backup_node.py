"""A live backup to another node (Round 14) on localhost: a drop-in home and a node joined to it;
the backup's target the node: the files arrive in its files folder (live-backup/<name>/), a
change and a deletion follow.

    python3 -I tests/live_backup_node.py [<the osada binary>]      (as root; /root/t-lbn, removed after)
"""
import json, os, re, shutil, signal, subprocess, sys, time

B = sys.argv[1] if len(sys.argv) > 1 else "/root/osada-0.2/target/release/osada"
D = "/root/t-lbn"
HTTP, LINK, NLINK = 18951, 18950, 18952
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
    body, _, code = out.rpartition("\n")
    return int(code or 0), body


shutil.rmtree(D, ignore_errors=True)
os.makedirs(f"{D}/home")
procs = {}
try:
    env = dict(os.environ, OSADA_DATA=f"{D}/home", OSADA_BIND=f"127.0.0.1:{HTTP}", OSADA_LINK_BIND=f"127.0.0.1:{LINK}", OSADA_NODE="home", OSADA_LIVE_BACKUP_SECS="3")
    procs["home"] = subprocess.Popen([B, "serve"], env=env, stdout=open(f"{D}/home.log", "w"), stderr=subprocess.STDOUT)
    wait_for("home's first administrator", lambda: os.path.exists(f"{D}/home/node/first-admin"))
    pw = open(f"{D}/home/node/first-admin").read().split("password:")[1].strip()
    out = subprocess.run(["curl", "-s", "-D", "-", "-o", "/dev/null", "-H", "x-shelf: 1", "-H", "content-type: application/json", "--data-binary",
                          json.dumps({"name": "admin", "password": pw}), f"http://127.0.0.1:{HTTP}/api/signin"], capture_output=True, text=True).stdout
    admin = re.search(r"set-cookie: ([^;]+)", out, re.I).group(1)
    tok = subprocess.run([B, "token", "--data", f"{D}/home/node", "--node", "n1", "--roles", "store", "--dial", f"127.0.0.1:{NLINK}"], capture_output=True, text=True).stdout.strip()
    j = subprocess.run([B, "join", "--data", f"{D}/n1", "--node", "n1", "--token", tok, "--listen", f"127.0.0.1:{NLINK}", "--wait", "2"], capture_output=True, text=True)
    check("n1 joined", "joined the cluster" in j.stdout, j.stdout + j.stderr)
    procs["n1"] = subprocess.Popen([B, "serve", "--data", f"{D}/n1", "--listen", f"127.0.0.1:{NLINK}"], stdout=open(f"{D}/n1.log", "w"), stderr=subprocess.STDOUT)
    c, b = api("GET", "/api/admin/overview", None, admin)
    o = json.loads(b)
    n1 = wait_for("n1 in node control", lambda: next((i for i, n in json.loads(api("GET", "/api/admin/overview", None, admin)[1])["placement"]["nodes"].items() if i != o["me"]), None), 30)
    wait_for("n1 linked", lambda: n1 in json.loads(api("GET", "/api/admin/overview", None, admin)[1]).get("links", {}), 30)
    c, b = api("POST", "/api/admin/backups", [{"name": "to-n1", "source": "all", "target": f"node:{n1}"}], admin)
    check("a live backup to n1", c == 200, b)
    os.makedirs(f"{D}/home/users/alice/docs", exist_ok=True)
    open(f"{D}/home/users/alice/a.txt", "w").write("first")
    open(f"{D}/home/users/alice/docs/b.txt", "w").write("bee")
    live = f"{D}/n1/live-backup/to-n1/live"
    check("the files arrive at n1", bool(wait_for("the first pass", lambda: open(f"{live}/users/alice/a.txt").read() == "first" and open(f"{live}/users/alice/docs/b.txt").read() == "bee", 60)))
    time.sleep(1.1)
    open(f"{D}/home/users/alice/a.txt", "w").write("changed")
    os.remove(f"{D}/home/users/alice/docs/b.txt")
    check("a change follows", bool(wait_for("the change", lambda: open(f"{live}/users/alice/a.txt").read() == "changed", 30)))
    check("a deletion follows", bool(wait_for("the deletion", lambda: not os.path.exists(f"{live}/users/alice/docs/b.txt"), 30)))
    c, b = api("GET", "/api/backup/live", None, admin)
    check("the status: ok", c == 200 and json.loads(b).get("live", {}).get("to-n1", {}).get("ok") is True, b[:300])
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
