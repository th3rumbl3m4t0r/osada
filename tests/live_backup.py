"""The live backup (Round 14) on localhost: a private sftp-only sshd (chrooted, key only) as other
storage, a drop-in home; the live backup set to it: the files arrive in live/, a change and a
deletion follow within a pass, the status says so.

    python3 -I tests/live_backup.py [<the osada binary>]       (as root; /root/t-lb, removed after)
"""
import json, os, re, shutil, signal, subprocess, sys, time

B = sys.argv[1] if len(sys.argv) > 1 else "/root/osada-0.2/target/release/osada"
D = "/root/t-lb"
HTTP, LINK, SFTP = 18941, 18940, 18932
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
S = f"{D}/sshd"
os.makedirs(f"{S}/root/box")
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
procs = {}
try:
    procs["sshd"] = subprocess.Popen([f"{S}/sshd", "-f", f"{S}/sshd_config", "-D", "-e"], stdout=open(f"{S}/log", "w"), stderr=subprocess.STDOUT)
    os.makedirs(f"{D}/home")
    env = dict(os.environ, OSADA_DATA=f"{D}/home", OSADA_BIND=f"127.0.0.1:{HTTP}", OSADA_LINK_BIND=f"127.0.0.1:{LINK}", OSADA_NODE="home", OSADA_LIVE_BACKUP_SECS="3")
    procs["home"] = subprocess.Popen([B, "serve"], env=env, stdout=open(f"{D}/home.log", "w"), stderr=subprocess.STDOUT)
    wait_for("home's first administrator", lambda: os.path.exists(f"{D}/home/node/first-admin"))
    pw = open(f"{D}/home/node/first-admin").read().split("password:")[1].strip()
    out = subprocess.run(["curl", "-s", "-D", "-", "-o", "/dev/null", "-H", "x-shelf: 1", "-H", "content-type: application/json", "--data-binary",
                          json.dumps({"name": "admin", "password": pw}), f"http://127.0.0.1:{HTTP}/api/signin"], capture_output=True, text=True).stdout
    admin = re.search(r"set-cookie: ([^;]+)", out, re.I).group(1)
    key = open(f"{S}/client").read()
    c, b = api("PUT", "/api/admin/storages/box", {"kind": "sftp", "host": "127.0.0.1", "port": SFTP, "username": "root", "auth": "key", "root": "/box", "secret": {"key": key}}, admin)
    check("the SFTP storage", c == 200, b)
    c, b = api("POST", "/api/admin/storages/box/test", None, admin)
    check("... tested (its key pinned)", c == 200, b)
    c, b = api("POST", "/api/admin/backups", [{"name": "box-live", "source": "all", "target": "storage:box"}], admin)
    check("a live backup to it", c == 200, b)
    c, b = api("POST", "/api/admin/backups", [{"name": "x", "source": "all", "target": "storage:nowhere"}], admin)
    check("... none to a storage that is not there", c != 200, b)
    os.makedirs(f"{D}/home/users/alice/docs", exist_ok=True)
    open(f"{D}/home/users/alice/a.txt", "w").write("first")
    open(f"{D}/home/users/alice/docs/b.txt", "w").write("bee")
    live = f"{S}/root/box/live"
    check("the files arrive", bool(wait_for("the first pass", lambda: open(f"{live}/users/alice/a.txt").read() == "first" and open(f"{live}/users/alice/docs/b.txt").read() == "bee", 60)))
    time.sleep(1.1)
    open(f"{D}/home/users/alice/a.txt", "w").write("changed")
    os.remove(f"{D}/home/users/alice/docs/b.txt")
    check("a change follows", bool(wait_for("the change", lambda: open(f"{live}/users/alice/a.txt").read() == "changed", 30)))
    check("a deletion follows", bool(wait_for("the deletion", lambda: not os.path.exists(f"{live}/users/alice/docs/b.txt"), 30)))
    c, b = api("GET", "/api/backup/live", None, admin)
    st = json.loads(b).get("live", {}).get("box-live", {}) if c == 200 else {}
    check("the status: ok, the files counted", st.get("ok") is True and st.get("files", 0) >= 1, b[:300])
    # the page: the backups of both kinds; the storage's "for files" multi-pick
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
        rows = br.wait("(() => { const g = [...document.querySelectorAll('table.grid')].find(t => t.textContent.includes('backup of what')); return g && g.innerText.includes('up to date') ? g.innerText : null; })()", 20)
        check("the page: the backups (Round 17: of what), the live one up to date", bool(rows) and "all files" in rows, br.text("#app")[:600])
        cell = br.wait("(document.querySelector('[data-for=\"storage:box\"] .mpick-text') || {}).textContent", 10)
        check("the storage's \"for files\": live backup here", cell == "live backup here", cell)
        br.js("document.querySelector('[data-for=\"storage:box\"] .mpick-btn').click()")
        opts = br.js("[...document.querySelectorAll('[data-for=\"storage:box\"] .mpick-row')].map(r => r.textContent + ':' + r.querySelector('input').checked)")
        check("... its options, a tick box each", opts == ["live backup here:true", "nightly backup here:false"], opts)
        br.js("document.querySelector('[data-for=\"storage:box\"] input[data-value=live]').click()")
        time.sleep(2)
        c, b = api("GET", "/api/admin/overview", None, admin)
        check("... unticked: no live backup", c == 200 and not json.loads(b).get("settings", {}).get("backups"), b[:200])
        check("no page errors", not [e for e in br.errors if "401" not in e and "404" not in e], br.errors)
    finally:
        br.close()
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
