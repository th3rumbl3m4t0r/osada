"""D-U30: OU policies on a throwaway node: a storage limit for accounts without their own and
the longest sign-in, inherited downwards and only tightened below.

    python3 -I tests/ou_policies.py target/release/osada"""
import http.cookiejar, json, os, shutil, subprocess, sys, tempfile, time, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

BIN = sys.argv[1]
P = 18902
S = f'http://127.0.0.1:{P}'
USER, PW = 'x-admin', 'ou-admin-pass-12345'
fails = 0


def ok(cond, what):
    global fails
    print(('ok   ' if cond else 'FAIL ') + what, flush=True)
    if not cond:
        fails += 1


def client():
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def call(method, path, body=None, raw=None, headers=None):
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        r = urllib.request.Request(S + path, method=method, data=data, headers={'content-type': 'application/json', 'x-shelf': '1', **(headers or {})})
        try:
            with op.open(r, timeout=30) as f:
                t = f.read()
                return f.status, (json.loads(t) if t.strip().startswith((b'{', b'[')) else t)
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors='replace')
        except OSError as e:
            return 0, str(e)
    return call


T = tempfile.mkdtemp(prefix='oupol-', dir='/var/tmp')
os.makedirs(T + '/data')
env = {'PATH': '/usr/bin:/bin', 'OSADA_DATA': T + '/data'}
subprocess.run([BIN, 'user', 'add', USER, '--admin'], input=PW + '\n', text=True, env=env, check=True, capture_output=True)
node = subprocess.Popen([BIN, 'serve'], env={**env, 'OSADA_BIND': f'127.0.0.1:{P}', 'OSADA_LINK_BIND': f'127.0.0.1:{P + 1}', 'OSADA_COOKIE': 'osada_o', 'OSADA_NODE': 'xo'},
                        stdout=open(T + '/serve.log', 'w'), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
b = None
try:
    a = client()
    for _ in range(100):
        if a('POST', '/api/signin', {'name': USER, 'password': PW})[0] == 200:
            break
        time.sleep(0.2)
    s, ov = a('GET', '/api/admin/overview')
    pl = ov['placement']
    root = next(k for k, o in pl['ous'].items() if o['parent'] is None)
    pl['ous'][root]['storage_limit'] = 1 << 20
    pl['ous'][root]['session_days'] = 2
    s, r = a('POST', '/api/admin/placement', pl)
    ok(s == 200, f'the root OU: 1 MiB for accounts without a limit, sign-ins of 2 days at most ({s} {r})')
    s, me = a('GET', '/api/me')
    ok(s == 200 and me['storage']['limit'] == 1 << 20, f'the account without its own limit has the OU\'s ({me.get("storage") if s == 200 else me})')
    s, r = a('PUT', '/api/raw/home/big.bin', raw=b'x' * (2 << 20), headers={'content-type': 'application/octet-stream'})
    ok(s == 507, f'2 MiB do not fit ({s})')
    s, r = a('PUT', '/api/raw/home/small.txt', raw=b'fits', headers={'content-type': 'text/plain'})
    ok(s == 200, f'a small file does ({s})')
    b2 = client()
    b2('POST', '/api/signin', {'name': USER, 'password': PW})
    s, acc = b2('GET', '/api/account')
    sess = acc.get('sessions', []) if s == 200 else []
    newest = max(sess, key=lambda x: x['started']) if sess else None
    life = (newest['expires'] - newest['started']) / 86_400_000 if newest else None
    ok(life is not None and abs(life - 2) < 0.01, f'a new sign-in lasts 2 days ({life})')
    # a child OU that loosens: refused; one that tightens: fine
    nid = str(max(int(k) for k in pl['ous']) + 1)
    pl['ous'][nid] = {'name': 'kids', 'parent': int(root), 'links': {}, 'secrets': None, 'tier0': False, 'second_factor': False, 'storage_limit': 2 << 20}
    s, r = a('POST', '/api/admin/placement', pl)
    ok(s == 400 and 'storage limit above' in str(r), f'a child OU with a higher limit is refused ({s} {str(r)[:120]})')
    pl['ous'][nid]['storage_limit'] = 1 << 19
    pl['ous'][nid]['session_days'] = 30
    s, r = a('POST', '/api/admin/placement', pl)
    ok(s == 400 and 'longer than' in str(r), f'longer sign-ins below are refused ({s})')
    pl['ous'][nid]['session_days'] = 1
    s, r = a('POST', '/api/admin/placement', pl)
    ok(s == 200, f'tighter below is fine ({s} {r})')
    # the page
    b = cdp.Browser()
    b.load(S + '/signin')
    b.wait("!!document.querySelector('#si-name')")
    b.js(f"document.querySelector('#si-name').value = {json.dumps(USER)}; document.querySelector('#si-pw').value = {json.dumps(PW)};"
         "document.querySelector('#app form button[type=submit]').click()")
    b.wait("location.pathname != '/signin'", 10)
    b.errors.clear()
    b.load(S + '/admin/ous')
    ok(b.wait("[...document.querySelectorAll('.lbl')].some(l => l.textContent === 'storage limit of its accounts')", 10), 'the OU card has the policies')
    ok(b.wait("[...document.querySelectorAll('input')].some(i => i.value === '1.0 MiB' || i.value === '1 MiB' || i.value.startsWith('1'))", 5), 'with the root\'s limit')
    errs = [e for e in b.errors if 'favicon' not in e]
    ok(not errs, f'no page errors ({errs[:4]})')
finally:
    if b:
        b.close()
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
