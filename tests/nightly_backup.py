"""The nightly backups (Round 17) on localhost: a private sftp-only sshd (chrooted, key only); osada
0.1's nightly backup set the old way (`osada backup set`, its own SFTP target), then a drop-in home
starts: the backup moves into the cluster's settings by itself (its target other storage, the key
along, never shown); a run copies the database, the files, the node's data (a consistent copy of
its store, its keys, the program) and mail (a stand-in for the snapshot helper answers the
request); a second run links what did not change; mail failing makes the run incomplete, not
failed; a second node joined: its data and mail come over the link; a second nightly backup, of the
files alone, to another storage; one per storage; the page.

    python3 -I tests/nightly_backup.py [<the osada binary>]       (as root; /root/t-nb, removed after)
"""
import hashlib, json, os, re, shutil, signal, sqlite3, subprocess, sys, threading, time

B = sys.argv[1] if len(sys.argv) > 1 else "/root/osada-0.2/target/release/osada"
D = "/root/t-nb"
HTTP, LINK, SFTP, NLINK = 18971, 18970, 18972, 18973
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


def j(b):
    try:
        return json.loads(b)
    except Exception:
        return {}


shutil.rmtree(D, ignore_errors=True)
S = f"{D}/sshd"
os.makedirs(f"{S}/root/box")
os.makedirs(f"{S}/root/box2")
os.chmod(S, 0o700)
os.chmod(f"{S}/root", 0o755)
shutil.copy("/usr/sbin/sshd", f"{S}/sshd")
shutil.copy("/usr/libexec/openssh/sshd-session", f"{S}/sshd-session")
for k in ["hostkey", "client"]:
    subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", k, "-f", f"{S}/{k}"], check=True)
shutil.copy(f"{S}/client.pub", f"{S}/authorized_keys")
os.chmod(f"{S}/authorized_keys", 0o600)
open(f"{S}/sshd_config", "w").write(f"""Port {SFTP}
ListenAddress 127.0.0.1
HostKey {S}/hostkey
PidFile {S}/pid
AuthorizedKeysFile {S}/authorized_keys
KbdInteractiveAuthentication no
AllowUsers root
UsePAM no
StrictModes no
Subsystem sftp internal-sftp
ForceCommand internal-sftp
ChrootDirectory {S}/root
SshdSessionPath {S}/sshd-session
""")
key = open(f"{S}/client").read()
procs = {}
mail_mode = {"error": None, "requests": 0}


def mail_helper(nd):
    """Stands in for deploy/stalwart-snapshot.sh: answers each request with a snapshot (or an error)."""
    ms = f"{nd}/mail-snapshot"
    done = ""
    while not stop.is_set():
        try:
            tok = open(f"{ms}/request").read().strip()
        except OSError:
            tok = ""
        if tok and tok != done:
            mail_mode["requests"] += 1
            if mail_mode["error"]:
                open(f"{ms}/done", "w").write(f"{tok} {mail_mode['error']}\n")
            else:
                for p, body in [("data/000001.sst", "sst " * 1000), ("data/CURRENT", "MANIFEST-000002\n"), ("etc/config.json", "{}"), ("bin/stalwart", "#!/bin/sh\n")]:
                    os.makedirs(os.path.dirname(f"{ms}/snap/{p}"), exist_ok=True)
                    open(f"{ms}/snap/{p}", "w").write(body)
                open(f"{ms}/done", "w").write(f"{tok}\n")
            done = tok
        time.sleep(0.2)


stop = threading.Event()
try:
    procs["sshd"] = subprocess.Popen([f"{S}/sshd", "-f", f"{S}/sshd_config", "-D", "-e"], stdout=open(f"{S}/log", "w"), stderr=subprocess.STDOUT)
    time.sleep(0.5)
    os.makedirs(f"{D}/home")
    env = dict(os.environ, OSADA_DATA=f"{D}/home", OSADA_BIND=f"127.0.0.1:{HTTP}", OSADA_LINK_BIND=f"127.0.0.1:{LINK}", OSADA_NODE="home")
    # osada 0.1's way: its own SFTP target
    r = subprocess.run([B, "backup", "set", "--target", "root@127.0.0.1:/box", "--port", str(SFTP), "--key", "--time", "03:15"], env=env, input=key, capture_output=True, text=True)
    check("osada 0.1's backup set the old way (its own target)", r.returncode == 0, r.stderr[-400:])
    procs["home"] = subprocess.Popen([B, "serve"], env=env, stdout=open(f"{D}/home.log", "w"), stderr=subprocess.STDOUT)
    wait_for("home's first administrator", lambda: os.path.exists(f"{D}/home/node/first-admin"))
    pw = open(f"{D}/home/node/first-admin").read().split("password:")[1].strip()
    out = subprocess.run(["curl", "-s", "-D", "-", "-o", "/dev/null", "-H", "x-shelf: 1", "-H", "content-type: application/json", "--data-binary",
                          json.dumps({"name": "admin", "password": pw}), f"http://127.0.0.1:{HTTP}/api/signin"], capture_output=True, text=True).stdout
    admin = re.search(r"set-cookie: ([^;]+)", out, re.I).group(1)

    # 1. the move into the cluster's settings
    def moved():
        c, b = api("GET", "/api/admin/overview", None, admin)
        v = j(b)
        return v if c == 200 and "backup-sftp" in (v.get("storages") or {}) and (v.get("settings", {}).get("backups")) else None
    ov = wait_for("the nightly backup to move", moved, 90) or {}
    st = (ov.get("storages") or {}).get("backup-sftp", {})
    check("its own target is other storage now (backup-sftp), with the key and the pinned server key",
          st.get("kind") == "sftp" and st.get("secret_set") == ["key"] and (st.get("host_key") or "").startswith("SHA256:") and st.get("root") == "/box", st)
    nb = (ov.get("settings", {}).get("backups") or [{}])[0]
    check("... and the backup a nightly one in the cluster's settings, at its time, on", nb.get("name") == "nightly" and nb.get("kind") == "nightly" and nb.get("target") == "storage:backup-sftp" and nb.get("time") == "03:15" and not nb.get("off"), nb)
    c, b = api("GET", "/api/admin/overview", None, admin)
    check("the overview never shows the key", key.strip().splitlines()[1][:40] not in b)
    c, b = api("GET", "/api/backup", None, admin)
    bk = j(b)
    check("/api/backup: moved, one nightly backup of everything", bk.get("migrated") is True and len(bk.get("nightly", [])) == 1 and set(bk["nightly"][0]["what"]) == {"files", "wiki", "database", "nodes", "mail"}, b[:400])
    c, b = api("PUT", "/api/backup", {"enabled": True, "time": "03:00", "host": "127.0.0.1", "username": "root", "auth": "key"}, admin)
    check("the old way of setting it is refused now, saying where", c == 400 and "admin → files → backups" in b, (c, b[:200]))
    r = subprocess.run([B, "backup", "run"], env=env, capture_output=True, text=True)
    check("... and `osada backup run` says where to run it", r.returncode != 0 and "cluster's settings" in r.stderr, r.stderr[-300:])

    # a second node, with mail too: its parts come over the link
    tok = subprocess.run([B, "token", "--data", f"{D}/home/node", "--node", "n1", "--roles", "cache", "--dial", f"127.0.0.1:{NLINK}"], capture_output=True, text=True).stdout.strip()
    jn = subprocess.run([B, "join", "--data", f"{D}/n1", "--node", "n1", "--token", tok, "--listen", f"127.0.0.1:{NLINK}", "--wait", "2"], capture_output=True, text=True)
    check("n1 joined", "joined the cluster" in jn.stdout, jn.stdout + jn.stderr)
    procs["n1"] = subprocess.Popen([B, "serve", "--data", f"{D}/n1", "--listen", f"127.0.0.1:{NLINK}"], stdout=open(f"{D}/n1.log", "w"), stderr=subprocess.STDOUT)
    me = j(api("GET", "/api/admin/overview", None, admin)[1]).get("me")
    n1 = wait_for("n1 in node control", lambda: next((i for i in j(api("GET", "/api/admin/overview", None, admin)[1])["placement"]["nodes"] if i != me), None), 30)
    wait_for("n1 linked", lambda: n1 in j(api("GET", "/api/admin/overview", None, admin)[1]).get("links", {}), 40)

    # 2. a run: the database, the files, the nodes' data, mail
    os.makedirs(f"{D}/home/users/alice/docs", exist_ok=True)
    open(f"{D}/home/users/alice/a.txt", "w").write("first")
    open(f"{D}/home/users/alice/docs/b.txt", "w").write("bee")
    for nd in [f"{D}/home/node", f"{D}/n1"]:
        os.makedirs(f"{nd}/mail-snapshot")
        threading.Thread(target=mail_helper, args=(nd,), daemon=True).start()

    def run(name):
        c, b = api("POST", f"/api/backup/run?name={name}", None, admin)
        if c != 200:
            return c, b, None
        def done():
            c, b = api("GET", "/api/backup", None, admin)
            v = j(b)
            return v if c == 200 and not v.get("running") else None
        time.sleep(1)
        return c, b, wait_for(f"run of {name}", done, 120)
    c, b, bk = run("nightly")
    check("run now (?name=nightly)", c == 200, b)
    r1 = [x for x in (bk or {}).get("runs", []) if x["name"] == "nightly"][:1]
    check("the run finished ok, complete", r1 and r1[0]["ok"] is True and not r1[0]["message"].startswith("incomplete"), r1)
    snaps = sorted(x for x in os.listdir(f"{S}/root/box") if x[:2] == "20")
    snap = f"{S}/root/box/{snaps[-1]}" if snaps else "/nonexistent"
    have = set()
    for dp, dn, fn in os.walk(snap):
        for f in fn:
            have.add(os.path.relpath(os.path.join(dp, f), snap))
    want = {"shelf.db", "secret.key", "users/alice/a.txt", "users/alice/docs/b.txt", "nodes/home/store.db", "nodes/home/bin/osada", "mail/home/data/000001.sst", "mail/home/etc/config.json", "mail/home/bin/stalwart", "manifest.json"}
    check("the snapshot has the database, the files, the node's data and mail", want <= have, sorted(want - have))
    want = {"nodes/n1/store.db", "nodes/n1/bin/osada", "mail/n1/data/000001.sst", "mail/n1/data/CURRENT"}
    check("... and the other node's, over the link (its store, its keys, its mail)", want <= have and any(x.startswith("nodes/n1/tls/") for x in have), sorted(want - have) or sorted(x for x in have if "n1" in x)[:20])
    check("... the node's keys and certificates", any(x.startswith("nodes/home/tls/") for x in have), sorted(x for x in have if x.startswith("nodes/"))[:20])
    check("... not its logs, caches or working copies", not any(x.startswith(("nodes/home/logs/", "nodes/home/backup-out/", "nodes/home/mail-snapshot/", "nodes/home/store.db-")) for x in have), sorted(x for x in have if x.startswith("nodes/"))[:30])
    man = j(open(f"{snap}/manifest.json").read()) if os.path.exists(f"{snap}/manifest.json") else {}
    ok = all(hashlib.sha256(open(f"{snap}/{p}", "rb").read()).hexdigest() == man.get("files", {}).get(p, [0, 0, 0, ""])[3] for p in ["nodes/home/store.db", "mail/home/data/000001.sst", "nodes/home/bin/osada", "nodes/n1/store.db", "mail/n1/data/CURRENT"])
    check("... their sha256 as the manifest says", ok)
    try:
        n = sqlite3.connect(f"file:{snap}/nodes/home/store.db?mode=ro", uri=True).execute("SELECT count(*) FROM sqlite_master").fetchone()[0]
    except Exception as e:
        n = str(e)
    check("... the store's copy is a whole SQLite database", isinstance(n, int) and n > 0, n)
    restore = open(f"{S}/root/box/RESTORE.md").read() if os.path.exists(f"{S}/root/box/RESTORE.md") else ""
    check("RESTORE.md: the whole cluster, home, the other nodes, mail", "## Rebuild the whole cluster" in restore and "### Home (home)" in restore and "### The other nodes (n1)" in restore and "### Mail (home, n1)" in restore, restore[:300])

    # 3. again: what did not change is linked; mail failing is noted, the rest done
    mail_mode["error"] = "stalwart did not stop"
    time.sleep(1.1)
    open(f"{D}/home/users/alice/a.txt", "w").write("changed")
    c, b, bk = run("nightly")
    r2 = [x for x in (bk or {}).get("runs", []) if x["name"] == "nightly"][:1]
    check("a second run: ok but incomplete, saying mail failed", r2 and r2[0]["ok"] is True and r2[0]["message"].startswith("incomplete: mail of home: the snapshot of mail: stalwart did not stop"), r2)
    check("... what did not change was linked, not sent", r2 and r2[0]["linked"] > 3 and r2[0]["uploaded"] < 10, r2)

    # 4. a second nightly backup, of the files alone, to another storage; one nightly per storage
    c, b = api("PUT", "/api/admin/storages/box2", {"kind": "sftp", "host": "127.0.0.1", "port": SFTP, "username": "root", "auth": "key", "root": "/box2", "secret": {"key": key}}, admin)
    c2, b2 = api("POST", "/api/admin/storages/box2/test", None, admin)
    check("another SFTP storage, tested", c == 200 and c2 == 200, (b, b2))
    cur = j(api("GET", "/api/admin/overview", None, admin)[1]).get("settings", {}).get("backups", [])
    c, b = api("POST", "/api/admin/backups", cur + [{"name": "dup", "kind": "nightly", "source": "all", "target": "storage:backup-sftp", "time": "04:00"}], admin)
    check("refused: a second nightly backup to the same storage", c == 400 and "goes to backup-sftp already" in b, (c, b))
    c, b = api("POST", "/api/admin/backups", cur + [{"name": "x", "kind": "nightly", "source": "all", "target": "storage:box2", "time": "25:00"}], admin)
    check("refused: a time that is none", c == 400 and "HH:MM" in b, (c, b))
    c, b = api("POST", "/api/admin/backups", cur + [{"name": "x", "kind": "nightly", "source": "all", "target": "storage:box2", "what": ["films"]}], admin)
    check("refused: a part there is not", c == 400 and "films" in b, (c, b))
    c, b = api("POST", "/api/admin/backups", cur + [{"name": "files-only", "kind": "nightly", "source": "all", "target": "storage:box2", "time": "04:00", "what": ["files"]}], admin)
    check("a second nightly backup: the files alone, to box2", c == 200, b)
    mail_requests = mail_mode["requests"]
    c, b, bk = run("files-only")
    r3 = [x for x in (bk or {}).get("runs", []) if x["name"] == "files-only"][:1]
    check("its run: ok", r3 and r3[0]["ok"] is True, r3)
    snaps2 = sorted(x for x in os.listdir(f"{S}/root/box2") if x[:2] == "20")
    top = set(os.listdir(f"{S}/root/box2/{snaps2[-1]}")) if snaps2 else set()
    check("... the files alone: no database, no nodes, no mail", top == {"users", "deleted", "manifest.json"} or top == {"users", "manifest.json"}, top)
    check("... and mail was not asked for", mail_mode["requests"] == mail_requests, mail_mode)
    c, b = api("GET", "/api/backup", None, admin)
    v = j(b)
    check("/api/backup: both, each with its snapshots and runs", {n["name"]: len(n["snapshots"]) for n in v.get("nightly", [])} == {"nightly": 2, "files-only": 1} and {r["name"] for r in v.get("runs", [])} == {"nightly", "files-only"}, b[:500])

    # 5. the admin log: the move, never the key
    c, b = api("POST", "/api/admin/logs/query", {"kind": "admin", "limit": 200}, admin)
    check("the admin log has the move", "storage backup-sftp: added" in b and "moved here" in b and "backups_changed" in b, b[:400])
    check("... and no secret", key.strip().splitlines()[1][:40] not in b)

    # 6. the page
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import cdp
    cdp.BASE = f"http://127.0.0.1:{HTTP}"
    br = cdp.Browser()
    try:
        br.load("/signin")
        br.wait("!!document.querySelector('#si-name')")
        br.focus("#si-name"); br.type("admin"); br.focus("#si-pw"); br.type(pw); br.key("Enter")
        br.wait("!!document.querySelector('#who a.navlink')", 15)
        br.load("/admin/files")
        rows = br.wait("(() => { const g = [...document.querySelectorAll('table.grid')].find(t => t.textContent.includes('backup of what')); return g && g.innerText.includes('files-only') && g.innerText.includes('snapshots') ? g.innerText : null; })()", 20)
        check("the backups: both nightly ones, of what, their state", bool(rows) and "the whole cluster" in rows and "every night at 04:00" in rows, (rows or br.text("#app"))[:700])
        check("... no box of its own SFTP target any more", "its own SFTP target" not in br.text("#app"))
        btn = br.js("[...document.querySelectorAll('button')].some(b => b.textContent === 'add a backup')")
        check("the button: add a backup", btn is True, btn)
        br.js("document.querySelector('[data-add-backup]').click()")
        k = br.wait("(() => { const s = document.querySelector('select[data-kind]'); return s ? [...s.options].map(o => o.value).join(',') + '|' + s.value : null; })()", 10)
        check("... a new one: kind nightly or live", k == "nightly,live|nightly", k)
        parts = br.js("[...document.querySelectorAll('[data-what] input[data-part]')].map(i => i.dataset.part + ':' + i.checked).join(',')")
        check("... backup of what: each part, all ticked", parts == "files:true,wiki:true,database:true,nodes:true,mail:true", parts)
        to = br.js("[...document.querySelector('select[data-to]').options].map(o => o.value + ':' + o.disabled).join(',')")
        check("... to: SFTP storage, those with a nightly backup not offered", to == "storage:backup-sftp:true,storage:box2:true", to)
        br.js("document.querySelector('select[data-kind]').value = 'live'; document.querySelector('select[data-kind]').dispatchEvent(new Event('change'))")
        live = br.js("!document.querySelector('[data-what]') && !!document.querySelector('select[data-kind]')")
        check("... live instead: of all files or a node's, no parts", live is True, live)
        br.js("[...document.querySelectorAll('.modal button')].find(b => b.textContent === 'cancel').click()")
        cell = br.wait("(document.querySelector('[data-for=\"storage:box2\"] .mpick-text') || {}).textContent", 10)
        check("box2's \"for files\": nightly backup here", cell == "nightly backup here", cell)
        check("no page errors", not [e for e in br.errors if "401" not in e and "404" not in e], br.errors)
    finally:
        br.close()
finally:
    stop.set()
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
