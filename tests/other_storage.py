#!/usr/bin/python3 -I
"""T2: other storage (SFTP, S3) in the cluster's settings, its secrets
write-only; the nightly backup to an SFTP one; what nodes keep of files.

A mock S3 listens on 127.0.0.1:<s3 port> and checks signature version 4 on
its own (Python's hmac). An SFTP server (a private sshd) listens on
127.0.0.1:<sftp port>, user root with the key in <key file>, chrooted to
<sftp root>, whose `box` folder takes the backup.

    python3 -I tests/other_storage.py http://<host:port> <admin> <password> <s3 port> <sftp port> <key file> <sftp root>
"""
import hashlib, hmac, http.cookiejar, http.server, json, os, sys, threading, time, urllib.parse, urllib.request

BASE, NAME, PW = sys.argv[1], sys.argv[2], sys.argv[3]
S3PORT, SFTPPORT, KEYFILE, SFTPROOT = int(sys.argv[4]), int(sys.argv[5]), sys.argv[6], sys.argv[7]
AK, SK = "TESTACCESSKEY", "test-secret-key-0123456789/abc+def"
failed = []
seen = []


def check(what, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + what + (f"\n     {detail}" if detail and not cond else ""), flush=True)
    if not cond:
        failed.append(what)


def hm(k, m):
    return hmac.new(k, m.encode(), hashlib.sha256).digest()


class S3(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        u = urllib.parse.urlsplit(self.path)
        auth = self.headers.get("authorization", "")
        date = self.headers.get("x-amz-date", "")
        ph = self.headers.get("x-amz-content-sha256", "")
        q = sorted(urllib.parse.parse_qsl(u.query, keep_blank_values=True))
        enc = lambda s: urllib.parse.quote(s, safe="-_.~")
        cq = "&".join(f"{enc(k)}={enc(v)}" for k, v in q)
        host = self.headers.get("host", "")
        canon = f"GET\n{u.path}\n{cq}\nhost:{host}\nx-amz-content-sha256:{ph}\nx-amz-date:{date}\n\nhost;x-amz-content-sha256;x-amz-date\n{ph}"
        try:
            cred = auth.split("Credential=")[1].split(",")[0]
            ak, day, region, svc, _ = cred.split("/")
            sig = auth.split("Signature=")[1]
        except Exception:
            ak = day = region = sig = ""
        scope = f"{day}/{region}/s3/aws4_request"
        sts = f"AWS4-HMAC-SHA256\n{date}\n{scope}\n" + hashlib.sha256(canon.encode()).hexdigest()
        k = hm(("AWS4" + SK).encode(), day)
        for part in (region, "s3", "aws4_request"):
            k = hm(k, part)
        good = ak == AK and hmac.compare_digest(hmac.new(k, sts.encode(), hashlib.sha256).hexdigest(), sig)
        seen.append({"path": u.path, "query": dict(q), "good": good, "region": region})
        if good and u.path == "/osada-test":
            body = b"<?xml version='1.0'?><ListBucketResult><Name>osada-test</Name><KeyCount>0</KeyCount></ListBucketResult>"
            self.send_response(200)
        else:
            code = "SignatureDoesNotMatch" if not good else "NoSuchBucket"
            body = f"<?xml version='1.0'?><Error><Code>{code}</Code><Message>mock says no</Message></Error>".encode()
            self.send_response(403 if not good else 404)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


srv = http.server.ThreadingHTTPServer(("127.0.0.1", S3PORT), S3)
threading.Thread(target=srv.serve_forever, daemon=True).start()


def client():
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def call(method, path, body=None, raw=False):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(BASE + path, method=method, data=data, headers={"x-shelf": "1", "content-type": "application/json"})
        try:
            with op.open(req, timeout=60) as r:
                b = r.read()
                if raw:
                    return r.status, b.decode()
                return r.status, (json.loads(b) if b.strip() else None)
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors="replace")
    return call


a = client()
s, _ = a("POST", "/api/signin", {"name": NAME, "password": PW})
check("the administrator signs in", s == 200, s)
s, ov = a("GET", "/api/admin/overview")
check("no other storage at first", s == 200 and ov.get("storages") == {}, ov.get("storages") if s == 200 else ov)

s3 = {"kind": "s3", "endpoint": f"http://127.0.0.1:{S3PORT}", "region": "", "bucket": "osada-test", "prefix": "nightly/",
      "access_key": AK, "secret": {"secret_key": SK}, "note": "the mock"}
key = open(KEYFILE).read()
sftp = {"kind": "sftp", "host": "127.0.0.1", "port": SFTPPORT, "username": "root", "auth": "key", "root": "/box", "secret": {"key": key}}

# refused
for name, body, why in [
    ("Bad_Name", s3, "a bad name"),
    ("x", {**s3, "endpoint": "http://s3.example.com"}, "http to another machine"),
    ("x", {**s3, "bucket": "No_Such"}, "a bad bucket"),
    ("x", {**s3, "secret": {}}, "no secret key"),
    ("x", {**sftp, "secret": {}}, "no private key"),
    ("x", {**sftp, "host": "bad host"}, "a bad host"),
    ("x", {**sftp, "auth": "magic"}, "an unknown login"),
]:
    s, v = a("PUT", f"/api/admin/storages/{name}", body)
    check(f"refused: {why}", s == 400, (s, v))

s, v = a("PUT", "/api/admin/storages/mock-s3", s3)
check("an S3 storage is added", s == 200, (s, v))
s, v = a("PUT", "/api/admin/storages/local-sftp", sftp)
check("an SFTP storage is added", s == 200, (s, v))

s, raw = a("GET", "/api/admin/overview", raw=True)
ov = json.loads(raw)
st = ov["storages"]
check("both are shown", set(st) == {"mock-s3", "local-sftp"}, st)
check("the secrets are not shown back", SK not in raw and "PRIVATE KEY" not in raw and key.strip()[40:80] not in raw)
check("which secrets are set is shown", st["mock-s3"]["secret_set"] == ["secret_key"] and st["local-sftp"]["secret_set"] == ["key"], st)
check("the region defaults to us-east-1", st["mock-s3"]["region"] == "us-east-1", st["mock-s3"])
check("where it is, for people", st["mock-s3"]["place"] == f"s3://osada-test/nightly/ at http://127.0.0.1:{S3PORT}", st["mock-s3"]["place"])
check("the settings page's settings have no storages", "storages" not in ov["settings"], ov["settings"].keys())

# S3: the mock checks the signature itself
s, v = a("POST", "/api/admin/storages/mock-s3/test")
check("S3 test: the bucket answers to a good signature", s == 200 and "bucket answers" in v["message"], (s, v))
check("S3 test: a ListObjectsV2 of one key under the prefix", seen and seen[-1]["query"] == {"list-type": "2", "max-keys": "1", "prefix": "nightly/"} and seen[-1]["good"], seen[-1:])

# an empty secret keeps it; a wrong one fails the signature
s, v = a("PUT", "/api/admin/storages/mock-s3", {**s3, "secret": {}, "note": "changed"})
check("a change without the secret keeps it", s == 200, (s, v))
s, v = a("POST", "/api/admin/storages/mock-s3/test")
check("... and the test still passes", s == 200, (s, v))
s, v = a("PUT", "/api/admin/storages/mock-s3", {**s3, "secret": {"secret_key": "wrong"}})
s, v = a("POST", "/api/admin/storages/mock-s3/test")
check("a wrong secret key: the bucket's answer is shown", s != 200 and "SignatureDoesNotMatch" in str(v), (s, v))
s, v = a("PUT", "/api/admin/storages/mock-s3", {**s3, "secret": {}, "clear_secret": True})
check("forgetting the secret: refused, S3 needs one", s == 400, (s, v))
s, v = a("PUT", "/api/admin/storages/mock-s3", s3)
check("the right secret again", s == 200, (s, v))

# SFTP: the test pins the server's key
s, v = a("POST", "/api/admin/storages/local-sftp/test")
check("SFTP test: connects, pins the server key", s == 200 and "pinned now" in v.get("message", ""), (s, v))
s, ov = a("GET", "/api/admin/overview")
hk = ov["storages"]["local-sftp"].get("host_key")
check("... the server key is kept", bool(hk) and hk.startswith("SHA256:"), ov["storages"]["local-sftp"])
s, v = a("POST", "/api/admin/storages/local-sftp/test")
check("a second test: the pinned key matches", s == 200 and "pinned now" not in v["message"], (s, v))
s, v = a("PUT", "/api/admin/storages/local-sftp", {**sftp, "secret": {}, "note": "same server"})
s, ov = a("GET", "/api/admin/overview")
check("a change of the note keeps the pinned key and the secret", ov["storages"]["local-sftp"].get("host_key") == hk and ov["storages"]["local-sftp"]["secret_set"] == ["key"], ov["storages"]["local-sftp"])

# a nightly backup to the SFTP storage (Round 17: in the cluster's settings, kind nightly)
def backups(list_):
    return a("POST", "/api/admin/backups", list_)


s, v = backups([{"name": "nb", "kind": "nightly", "source": "all", "target": "storage:mock-s3", "time": "03:15", "what": ["files", "database"]}])
check("a nightly backup refuses an S3 storage (with A12)", s == 400 and "S3" in str(v), (s, v))
s, v = backups([{"name": "nb", "kind": "nightly", "source": "all", "target": "storage:local-sftp", "time": "03:15", "what": ["files", "database"], "off": True}])
check("a nightly backup goes to the SFTP storage", s == 200, (s, v))
s, bk = a("GET", "/api/backup")
check("... and says so", s == 200 and bk["nightly"][0]["target"] == "storage:local-sftp", bk.get("nightly") if s == 200 else bk)
s, v = a("POST", "/api/backup/test?name=nb")
check("its test: the pinned key", s == 200 and v.get("host_key") == hk, (s, v))
s, v = a("POST", "/api/backup/run?name=nb")
check("run now", s == 200, (s, v))


def run_done():
    end = time.time() + 120
    while time.time() < end:
        s, bk = a("GET", "/api/backup")
        if s == 200 and not bk["running"]:
            return bk
        time.sleep(1)
    return None


bk = run_done()
check("the run finished ok", bk and bk["runs"] and bk["runs"][0]["ok"] is True and bk["runs"][0]["name"] == "nb", bk and bk["runs"][:1])
snaps = [x for x in os.listdir(os.path.join(SFTPROOT, "box")) if not x.startswith(".")]
check("a snapshot and RESTORE.md are on the storage", "RESTORE.md" in snaps and any(x[:2] == "20" for x in snaps), snaps)
restore = open(os.path.join(SFTPROOT, "box", "RESTORE.md")).read()
check("RESTORE.md names the storage's server", "127.0.0.1" in restore, restore[:300])

# a storage a backup goes to, removed: the run says why
s, v = a("DELETE", "/api/admin/storages/local-sftp")
check("a storage is removed", s == 200, (s, v))
s, v = a("POST", "/api/backup/run?name=nb")
check("the backup to a removed storage says why", s == 400 and "no other storage local-sftp" in str(v), (s, v))
s, v = a("DELETE", "/api/admin/storages/local-sftp")
check("removing it again: there is none", s != 200, (s, v))
s, v = backups([])
check("the nightly backup removed", s == 200, (s, v))

# what the nodes keep of files
s, u = a("GET", "/api/backup/usage")
check("usage: this node's files, by part", s == 200 and u["node"] and any(p["name"] == "shelf.db" for p in u["parts"]) and u["total"] > 0, u)
s, info = a("GET", "/api/admin/nodes/info")
check("nodes/info: what each node keeps of files", s == 200 and all("files" in i for i in info.values() if "error" not in i), info)

# the admin log: the changes, never a secret
s, v = a("POST", "/api/admin/logs/query", {"kind": "admin", "limit": 200})
txt = json.dumps(v)
check("the admin log has the storage changes", "storage_changed" in txt and "storage mock-s3: added" in txt and "host key pinned" in txt and "storage local-sftp: removed" in txt, txt[:600])
check("... and no secret", SK not in txt and "wrong" not in txt and key.strip()[40:80] not in txt)

# the settings page still saves (it never touches the storages)
s, ov = a("GET", "/api/admin/overview")
s, v = a("POST", "/api/admin/settings", ov["settings"])
s, ov2 = a("GET", "/api/admin/overview")
check("saving the log settings keeps other storage", s == 200 and set(ov2["storages"]) == {"mock-s3"}, (s, v, ov2.get("storages")))

print("\n" + ("ALL PASS" if not failed else f"{len(failed)} FAILED: " + "; ".join(failed)))
sys.exit(1 if failed else 0)
