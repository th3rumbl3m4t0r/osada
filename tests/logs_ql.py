"""D-U31: the cluster's logs on a throwaway node: http, auth and files lines reach the logs
store, queried with the pipes over the API and in admin → logs (headless Chromium).

    python3 -I tests/logs_ql.py target/release/osada"""
import http.cookiejar
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

BIN = sys.argv[1]
P = 18896
S = f'http://127.0.0.1:{P}'
USER, PW = 'x-admin', 'logs-admin-pass-12345'
fails = 0


def ok(cond, what):
    global fails
    print(('ok   ' if cond else 'FAIL ') + what, flush=True)
    if not cond:
        fails += 1


T = tempfile.mkdtemp(prefix='logs-ql-', dir='/var/tmp')
os.makedirs(T + '/data')
env = {'PATH': '/usr/bin:/bin', 'OSADA_DATA': T + '/data'}
subprocess.run([BIN, 'user', 'add', USER, '--admin'], input=PW + '\n', text=True, env=env, check=True, capture_output=True)
node = subprocess.Popen([BIN, 'serve'], env={**env, 'OSADA_BIND': f'127.0.0.1:{P}', 'OSADA_LINK_BIND': f'127.0.0.1:{P + 1}', 'OSADA_COOKIE': 'osada_l',
                                             'OSADA_NODE': 'xl', 'OSADA_LOGS_ROUND': '1'},
                        stdout=open(T + '/serve.log', 'w'), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
b = None


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


def ql(a, q, since=3600):
    now = int(time.time() * 1000)
    return a('POST', '/api/admin/logs/ql', {'q': q, 'from': now - since * 1000, 'to': now + 5000})


try:
    a = client()
    for _ in range(100):
        s, _ = a('POST', '/api/signin', {'name': USER, 'password': PW})
        if s == 200:
            break
        time.sleep(0.2)
    ok(s == 200, 'node up, the administrator signs in')
    # traffic: pages, scanners, a secret in a query string, a failed sign-in, a file
    anon = client()
    for path in ['/', '/wp-login.php', '/.env?token=secret123&x=1', '/cgi-bin/luci']:
        anon('GET', path)
    s, _ = anon('POST', '/api/signin', {'name': USER, 'password': 'wrong-password-1'})
    ok(s in (401, 403), f'a wrong password is refused ({s})')
    s, r = a('PUT', '/api/raw/home/t-logs.txt', raw=b'hello logs', headers={'content-type': 'text/plain'})
    ok(s == 200, f'a file written ({s} {str(r)[:100]})')
    seen = None
    for _ in range(40):
        s, seen = ql(a, 'dataset = http | filter path = "/wp-login.php" | fields client, status, front, method')
        if s == 200 and seen['rows']:
            break
        time.sleep(0.5)
    ok(s == 200 and seen['rows'] and seen['rows'][0][0] == '127.0.0.1' and seen['rows'][0][2] == 'lan' and seen['rows'][0][3] == 'GET',
       f'http: the scanner\'s request is there ({seen if s != 200 else seen["rows"]})')
    s, r = ql(a, 'dataset = http | filter path = "/.env" | fields query')
    ok(s == 200 and r['rows'] == [['token=…&x=1']], f'http: the token in the query string is not kept ({r if s != 200 else r["rows"]})')
    s, r = ql(a, 'search "secret123"')
    ok(s == 200 and r['rows'] == [], 'the secret is nowhere in the logs')
    for _ in range(20):
        s, r = ql(a, 'dataset = auth | filter event = "sign_in_failed" | fields name, addr, why')
        if s == 200 and r['rows']:
            break
        time.sleep(0.5)
    ok(s == 200 and r['rows'] and r['rows'][0][:2] == [USER, '127.0.0.1'], f'auth: the failed sign-in ({r if s != 200 else r["rows"]})')
    for _ in range(20):
        s, r = ql(a, 'dataset = files | fields op, user, path, bytes, how')
        if s == 200 and r['rows']:
            break
        time.sleep(0.5)
    ok(s == 200 and ['create', USER, '/t-logs.txt', 10, 'web'] in r['rows'], f'files: the new file ({r if s != 200 else r["rows"]})')
    s, r = ql(a, 'dataset = http | stats count() as n, values(path) as paths by client | filter n >= 2')
    ok(s == 200 and r['kind'] == 'table' and r['rows'] and r['rows'][0][0] == '127.0.0.1', f'stats over http ({r if s != 200 else r["rows"]})')
    s, r = ql(a, 'dataset in (http, auth) | filter client = "127.0.0.1" or addr = "127.0.0.1" | stats count() as n by _dataset | sort _dataset')
    ok(s == 200 and [x[0] for x in r['rows']] == ['auth', 'http'], f'two datasets at once ({r if s != 200 else r["rows"]})')
    s, r = ql(a, 'dataset = http | timechart span=1m count() as n')
    ok(s == 200 and r['kind'] == 'timechart' and r['columns'] == ['_time', 'n'], 'timechart')
    s, r = ql(a, 'dataset = http | filter nothere = 1')
    ok(s == 400 and 'no field' in str(r), f'a wrong query says why ({s} {str(r)[:100]})')
    s, r = a('GET', '/api/admin/logs/info')
    ok(s == 200 and 'http' in r['datasets'] and r['sizes']['http']['rows'] >= 4 and r['authority'], f'info: datasets and sizes ({str(r)[:160]})')
    s, r = anon('POST', '/api/admin/logs/ql', {'q': 'dataset = http', 'from': 0, 'to': 1})
    ok(s in (401, 403), f'not signed in: refused ({s})')

    # the page
    b = cdp.Browser()
    b.load(S + '/signin')
    b.wait("!!document.querySelector('#si-name')")
    b.js(f"document.querySelector('#si-name').value = {json.dumps(USER)}; document.querySelector('#si-pw').value = {json.dumps(PW)};"
         "document.querySelector('#app form button[type=submit]').click()")
    b.wait("location.pathname != '/signin'", 10)
    b.errors.clear()
    b.load(S + '/admin/logs')
    ok(b.wait("!!document.querySelector('textarea.ql')", 10), 'admin → logs: the query window')
    b.js("const t = document.querySelector('textarea.ql'); t.value = 'dataset = http | top 5 path';"
         "[...document.querySelectorAll('button')].find(x => x.textContent === 'run').click()")
    ok(b.wait("[...document.querySelectorAll('#ql-out td')].some(td => td.textContent === '/wp-login.php')", 10), 'the result in the grid')
    ok('rows in' in b.text('#app'), 'says how many and how long')
    b.js("[...document.querySelectorAll('#ql-out td span')].find(s => s.textContent === '/wp-login.php').dispatchEvent(new MouseEvent('dblclick', {bubbles: true}))")
    ok(b.wait("document.querySelector('textarea.ql').value.endsWith('| filter path = \"/wp-login.php\"')", 5), 'a double-click filters on the value')
    ok(b.wait("document.querySelectorAll('#ql-out tbody tr').length === 1", 10), 'and runs it again')
    b.js("document.querySelector('textarea.ql').value = 'dataset = http | timechart span=1m count() as n';"
         "[...document.querySelectorAll('button')].find(x => x.textContent === 'run').click()")
    ok(b.wait("!!document.querySelector('svg.ql-bars rect')", 10), 'a timechart draws bars')
    b.js("document.querySelector('details summary').click()")
    ok(b.wait("[...document.querySelectorAll('dl.ql-datasets dt')].some(d => d.textContent.startsWith('files'))", 5), 'help lists the datasets')
    ok(b.wait("[...document.querySelectorAll('.lbl')].some(l => l.textContent === 'http requests')", 5), 'log settings: http requests and files')
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
        print('serve.log tail:\n' + ''.join(open(T + '/serve.log').readlines()[-20:]))
    shutil.rmtree(T, ignore_errors=True)
print('FAILED' if fails else 'ALL OK', fails)
sys.exit(1 if fails else 0)
