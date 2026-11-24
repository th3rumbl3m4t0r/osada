"""D-U29's rest: sharing on from a group's folder, on a throwaway node: public links, shares
with others (read, write), managed by any member, refused to others.

    python3 -I tests/group_sharing.py target/release/osada"""
import http.cookiejar, json, os, shutil, subprocess, sys, tempfile, time, urllib.request

BIN = sys.argv[1]
P = 18906
S = f'http://127.0.0.1:{P}'
USER, PW = 'x-admin', 'gs-admin-pass-12345'
fails = 0


def ok(cond, what):
    global fails
    print(('ok   ' if cond else 'FAIL ') + what, flush=True)
    if not cond:
        fails += 1


def client():
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def call(method, path, body=None, raw=False):
        data = body if isinstance(body, bytes) else (None if body is None else json.dumps(body).encode())
        r = urllib.request.Request(S + path, method=method, data=data, headers={'x-shelf': '1', 'content-type': 'application/octet-stream' if isinstance(body, bytes) else 'application/json'})
        try:
            with op.open(r, timeout=20) as f:
                b = f.read()
                return f.status, (b if raw else (json.loads(b) if b.strip() else None))
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors='replace')
        except OSError as e:
            return 0, str(e)
    return call


T = tempfile.mkdtemp(prefix='gshare-', dir='/var/tmp')
os.makedirs(T + '/data')
env = {'PATH': '/usr/bin:/bin', 'OSADA_DATA': T + '/data'}
subprocess.run([BIN, 'user', 'add', USER, '--admin'], input=PW + '\n', text=True, env=env, check=True, capture_output=True)
node = subprocess.Popen([BIN, 'serve'], env={**env, 'OSADA_BIND': f'127.0.0.1:{P}', 'OSADA_LINK_BIND': f'127.0.0.1:{P + 1}', 'OSADA_COOKIE': 'osada_g', 'OSADA_NODE': 'xg'},
                        stdout=open(T + '/serve.log', 'w'), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
try:
    a = client()
    for _ in range(100):
        if a('POST', '/api/signin', {'name': USER, 'password': PW})[0] == 200:
            break
        time.sleep(0.2)
    s, me = a('GET', '/api/account')
    uid = me['user']['id']
    a('POST', '/api/admin/groups', {'name': 'team', 'display': 'the team', 'ou': 1})
    s, gs = a('POST', '/api/admin/table/groups', {'limit': 50, 'params': {}})
    gid = next(r['id'] for r in gs['rows'] if r['name'] == 'team')
    for name in ('mate', 'outsider'):
        s, v = a('POST', '/api/admin/users', {'name': name, 'display': name, 'ou': 1, 'password': f'{name}-pass-123456'})
    s, us = a('POST', '/api/admin/table/users', {'limit': 50, 'params': {}})
    mate = next(r['id'] for r in us['rows'] if r['name'] == 'mate')
    for who in (uid, mate):
        a('POST', '/api/admin/groups/member', {'group': gid, 'user': who, 'member': True})
    loc = f'g{gid}'
    a('POST', '/api/mkdir', {'loc': loc, 'path': 'notes'})
    s, _ = a('PUT', f'/api/raw/{loc}/notes/hello.txt', b'hello group')
    ok(s == 200, 'a member writes into the group\'s folder')
    # a public link out of it
    s, l = a('POST', '/api/links', {'loc': loc, 'path': 'notes/hello.txt'})
    ok(s == 200 and l['id'] < 0, f'a public link from the group\'s folder ({s} {l})')
    anon = client()
    s, body = anon('GET', f"/s/{l['token']}/raw", raw=True)
    ok(s == 200 and body == b'hello group', f'anyone reads it through the link ({s})')
    # shares with someone outside the group
    out = client()
    out('POST', '/api/signin', {'name': 'outsider', 'password': 'outsider-pass-123456'})
    out('GET', '/api/me')  # osada 0.1's side knows an account from its first visit
    s, sh = a('POST', '/api/shares', {'loc': loc, 'path': 'notes', 'user': 'outsider', 'write': False})
    ok(s == 200 and sh['id'] < 0, f'the notes folder shared with outsider, to read ({s} {sh})')
    s, mine = out('GET', '/api/shares')
    w = [x for x in mine['with_me'] if x['loc'] == f"s{sh['id']}"] if s == 200 else []
    ok(w and w[0]['user'] == USER and w[0]['dir'], f'outsider sees it among what is shared with them ({mine if s == 200 else s})')
    sloc = f"s{sh['id']}"
    s, ls = out('GET', f'/api/ls/{sloc}')
    ok(s == 200 and any(e['name'] == 'hello.txt' for e in ls.get('entries', ls.get('items', []))) if isinstance(ls, dict) else False, f'and lists it ({s} {str(ls)[:160]})')
    s, body = out('GET', f'/api/raw/{sloc}/hello.txt', raw=True)
    ok(s == 200 and body == b'hello group', f'and reads it ({s})')
    s, _ = out('PUT', f'/api/raw/{sloc}/theirs.txt', b'x')
    ok(s == 403, f'but cannot write: read only ({s})')
    s, sh2 = a('POST', '/api/shares', {'loc': loc, 'path': 'notes', 'user': 'outsider', 'write': True})
    s, _ = out('PUT', f'/api/raw/{sloc}/theirs.txt', b'from outside')
    ok(s == 200 and open(os.path.join(T, 'data', 'groups', gid, 'notes', 'theirs.txt'), 'rb').read() == b'from outside', f'with write, it lands in the group\'s folder ({s})')
    s, it = a('GET', f'/api/shares/item?loc={loc}&path=notes')
    ok(s == 200 and [x['user'] for x in it['shares']] == ['outsider'], f'the share dialog shows it ({it if s == 200 else s})')
    # not a member: cannot share from it
    s, v = out('POST', '/api/links', {'loc': loc, 'path': 'notes'})
    ok(s == 404, f'outsider cannot share from the group\'s folder ({s})')
    # another member manages what the first one shared
    m = client()
    m('POST', '/api/signin', {'name': 'mate', 'password': 'mate-pass-123456'})
    m('GET', '/api/me')
    s, v = m('DELETE', f"/api/shares/{sh['id']}")
    ok(s == 200, f'another member ends the share ({s} {v})')
    s, _ = out('GET', f'/api/ls/{sloc}')
    ok(s == 404, f'and outsider no longer gets in ({s})')
    s, v = out('DELETE', f"/api/links/{l['id']}")
    ok(s == 404, f'outsider cannot remove the group\'s link ({s})')
    s, links = a('GET', '/api/links')
    ok(s == 200 and any(x['id'] == l['id'] and x['loc_name'] == 'the team' for x in links), f'the link is among its maker\'s links ({links if s == 200 else s})')
    s, v = a('DELETE', f"/api/links/{l['id']}")
    s2, _ = anon('GET', f"/s/{l['token']}/raw", raw=True)
    ok(s == 200 and s2 == 404, f'removed: the link is gone ({s} {s2})')
finally:
    node.terminate()
    try:
        node.wait(10)
    except subprocess.TimeoutExpired:
        node.kill()
    if fails:
        print('serve.log tail:\n' + ''.join(open(T + '/serve.log').readlines()[-15:]))
    shutil.rmtree(T, ignore_errors=True)
print('FAILED' if fails else 'ALL OK', fails)
sys.exit(1 if fails else 0)
