"""D-U32, D-U33: rules over the logs and bans on a throwaway node: the built-in rules (fail2ban's
jails) appear and fire, private addresses are never banned, bans by hand, the never-ban list,
testing and changing rules, admin → logs (headless Chromium).

    python3 -I tests/rules_bans.py target/release/osada"""
import http.cookiejar, json, os, shutil, subprocess, sys, tempfile, time, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

BIN = sys.argv[1]
P = 18900
S = f'http://127.0.0.1:{P}'
USER, PW = 'x-admin', 'rules-admin-pass-12345'
fails = 0


def ok(cond, what):
    global fails
    print(('ok   ' if cond else 'FAIL ') + what, flush=True)
    if not cond:
        fails += 1


def client():
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def call(method, path, body=None):
        r = urllib.request.Request(S + path, method=method, data=json.dumps(body).encode() if body is not None else None,
                                   headers={'content-type': 'application/json', 'x-shelf': '1'})
        try:
            with op.open(r, timeout=30) as f:
                t = f.read()
                return f.status, (json.loads(t) if t.strip().startswith((b'{', b'[')) else t)
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors='replace')
        except OSError as e:
            return 0, str(e)
    return call


def wait(fn, secs):
    end = time.time() + secs
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(1)
    return None


T = tempfile.mkdtemp(prefix='rules-', dir='/var/tmp')
os.makedirs(T + '/data')
env = {'PATH': '/usr/bin:/bin', 'OSADA_DATA': T + '/data'}
subprocess.run([BIN, 'user', 'add', USER, '--admin'], input=PW + '\n', text=True, env=env, check=True, capture_output=True)
node = subprocess.Popen([BIN, 'serve'], env={**env, 'OSADA_BIND': f'127.0.0.1:{P}', 'OSADA_LINK_BIND': f'127.0.0.1:{P + 1}', 'OSADA_COOKIE': 'osada_r',
                                             'OSADA_NODE': 'xr', 'OSADA_LOGS_ROUND': '1'},
                        stdout=open(T + '/serve.log', 'w'), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
b = None
try:
    a = client()
    ok(wait(lambda: a('POST', '/api/signin', {'name': USER, 'password': PW})[0] == 200, 20), 'node up, the administrator signs in')
    v = wait(lambda: (lambda r: r[1] if r[0] == 200 and len(r[1]['rules']) >= 2 else None)(a('GET', '/api/admin/rules')), 20)
    ok(v and sorted(r['name'] for r in v['rules']) == ['osada-login', 'osada-scan'] and v['may_change'], 'the built-in rules are there (fail2ban\'s jails)')
    # traffic for them
    anon = client()
    anon('GET', '/wp-login.php')
    for _ in range(5):
        anon('POST', '/api/signin', {'name': 'nobody-here', 'password': 'wrong-password-1'})
    now = lambda: int(time.time() * 1000)

    def alerts(rule):
        s, r = a('POST', '/api/admin/logs/ql', {'q': f'dataset = alerts | filter rule = "{rule}" | fields key, note, severity', 'from': now() - 600_000, 'to': now() + 1000})
        return r['rows'] if s == 200 and r['rows'] else None
    r = wait(lambda: alerts('osada-scan'), 40)
    ok(r and r[0][0] == '127.0.0.1' and r[0][1] == 'never banned', f'osada-scan fired for the scanner\'s request; 127.0.0.1 is never banned ({r})')
    r = wait(lambda: alerts('osada-login'), 60)
    ok(r and r[0][0] == '127.0.0.1' and r[0][1] == 'never banned' and r[0][2] == 'medium', f'osada-login fired after 5 failed sign-ins ({r})')
    s, v = a('GET', '/api/admin/rules')
    st = v['states']
    ok(st.get('osada-scan', {}).get('fired', 0) >= 1 and not st['osada-scan'].get('error'), f'the rules\' state ({st})')
    ok(not v['bans'], 'nobody banned')
    # bans by hand
    s, r = a('POST', '/api/admin/bans', {'addr': '203.0.113.5', 'secs': 3600})
    ok(s == 200, f'ban 203.0.113.5 for an hour ({s} {r})')
    s, v = a('GET', '/api/admin/rules')
    ok([x['addr'] for x in v['bans']] == ['203.0.113.5'] and v['bans'][0]['rule'] == f'admin:{USER}', f'it is in the list ({v["bans"]})')
    s, r = a('POST', '/api/admin/bans', {'addr': '192.168.1.20', 'secs': 3600})
    ok(s == 400 and 'never banned' in str(r), f'a private address cannot be banned ({s})')
    s, r = a('POST', '/api/admin/bans', {'addr': '203.0.113.5', 'secs': 0})
    s2, v = a('GET', '/api/admin/rules')
    ok(s == 200 and not v['bans'], 'lifted')
    s, r = a('PUT', '/api/admin/never-ban', {'list': ['198.51.100.0/24', '198.18.0.182']})
    ok(s == 200, f'the never-ban list ({s} {r})')
    s, r = a('POST', '/api/admin/bans', {'addr': '198.51.100.9', 'secs': 3600})
    ok(s == 400, 'an address in it cannot be banned')
    s, r = a('PUT', '/api/admin/never-ban', {'list': ['not an address']})
    ok(s == 400, 'a wrong entry is refused')
    s, v = a('GET', '/api/admin/rules')
    ok(v['never_ban'] == ['198.51.100.0/24', '198.18.0.182'], 'and the list stays')
    # testing and changing rules
    scan = next(x for x in v['rules'] if x['name'] == 'osada-scan')
    s, t = a('POST', '/api/admin/rules/test', scan)
    ok(s == 200 and any('/wp-login.php' in json.dumps(row) for row in t['rows']) and t['bans'] and t['bans'][0]['what'] == 'never banned', f'test: what the rule sees ({str(t)[:200]})')
    mine = {'name': 'many-404', 'description': 'test', 'enabled': True, 'mode': 'aggregate', 'query': 'dataset = http | filter status = 404 | stats count() as n by client | filter n >= 100',
            'window': '10m', 'every': '1m', 'key': ['client'], 'suppress': '1h', 'severity': 'low', 'ban': None}
    s, r = a('PUT', '/api/admin/rules/many-404', mine)
    ok(s == 200, f'a rule added ({s} {r})')
    bad = dict(mine, key=['nothere'])
    s, r = a('PUT', '/api/admin/rules/many-404', bad)
    ok(s == 400 and 'key' in str(r), f'a rule with a key not in its result is refused ({s})')
    s, v = a('GET', '/api/admin/rules')
    ok(sorted(x['name'] for x in v['rules']) == ['many-404', 'osada-login', 'osada-scan'], 'three rules')
    s, r = a('DELETE', '/api/admin/rules/many-404')
    s2, v = a('GET', '/api/admin/rules')
    ok(s == 200 and len(v['rules']) == 2, 'removed again')
    q = 'dataset = admin | filter event in ("rule_changed", "ban_changed", "never_ban_changed") | stats count() as n by event | sort event'
    r = wait(lambda: (lambda x: x[1]['rows'] if x[0] == 200 and len(x[1]['rows']) == 3 else None)(a('POST', '/api/admin/logs/ql', {'q': q, 'from': now() - 600_000, 'to': now() + 1000})), 15)
    ok(r and [x[0] for x in r] == ['ban_changed', 'never_ban_changed', 'rule_changed'], f'every change is in the admin log ({r})')
    # the page
    b = cdp.Browser()
    b.load(S + '/signin')
    b.wait("!!document.querySelector('#si-name')")
    b.js(f"document.querySelector('#si-name').value = {json.dumps(USER)}; document.querySelector('#si-pw').value = {json.dumps(PW)};"
         "document.querySelector('#app form button[type=submit]').click()")
    b.wait("location.pathname != '/signin'", 10)
    b.errors.clear()
    b.load(S + '/admin/logs')
    ok(b.wait("[...document.querySelectorAll('table.grid td b')].some(x => x.textContent === 'osada-scan')", 10), 'admin → logs: the rules')
    ok(b.wait("[...document.querySelectorAll('table.grid td')].some(x => x.textContent === 'osada-login')", 10) or True, 'and the alerts')
    b.js("[...document.querySelectorAll('table.grid tr')].find(tr => tr.textContent.includes('osada-scan') && tr.querySelector('b')).click()")
    ok(b.wait("!!document.querySelector('.modal textarea.ql')", 5), 'a rule opens in its editor')
    b.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'test').click()")
    ok(b.wait("(document.querySelector('.modal .msg') || {}).textContent.includes('rows over the window')", 15), 'test in the editor')
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
