"""T6: links to Stalwart's own pages, in headless Chromium against a throwaway node.
The portal's mail card links the mail server's account pages (app passwords, encryption at
rest); the mail module's window in admin and the DNS page's Stalwart window link its
administration and those account pages.

    python3 -I tests/ui_mail_links.py target/release/osada"""
import http.cookiejar
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__))))
import cdp  # noqa: E402

BIN = sys.argv[1]
P = 18894
S = f'http://127.0.0.1:{P}'
USER, PW = 'x-admin', 't6-admin-pass-12345'
SQLITE = shutil.which('sqlite3') or '/opt/android-sdk/platform-tools/sqlite3'
fails = 0


def ok(cond, what):
    global fails
    print(('ok   ' if cond else 'FAIL ') + what, flush=True)
    if not cond:
        fails += 1


T = tempfile.mkdtemp(prefix='t6-ui-', dir='/var/tmp')
os.makedirs(T + '/data')
env = {'PATH': '/usr/bin:/bin', 'OSADA_DATA': T + '/data'}
subprocess.run([BIN, 'user', 'add', USER, '--admin'], input=PW + '\n', text=True, env=env, check=True, capture_output=True)
node = subprocess.Popen([BIN, 'serve'], env={**env, 'OSADA_BIND': f'127.0.0.1:{P}', 'OSADA_LINK_BIND': f'127.0.0.1:{P + 1}', 'OSADA_COOKIE': 'osada_x',
                                             'OSADA_NODE': 'xt', 'OSADA_MAILDOT_URL': 'https://mail.example.org/maildot/'},
                        stdout=open(T + '/serve.log', 'w'), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
b = None
try:
    jar = http.cookiejar.CookieJar()
    op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))

    def api(method, path, body=None):
        r = urllib.request.Request(S + path, method=method, data=json.dumps(body).encode() if body is not None else None,
                                   headers={'content-type': 'application/json', 'x-shelf': '1'})
        try:
            with op.open(r, timeout=30) as f:
                return f.status, json.loads(f.read() or b'null')
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors='replace')
        except OSError as e:
            return 0, str(e)
    for _ in range(100):
        s, _ = api('POST', '/api/signin', {'name': USER, 'password': PW})
        if s == 200:
            break
        time.sleep(0.2)
    ok(s == 200, 'the node is up, the administrator signs in')
    s, ov = api('GET', '/api/admin/overview')
    pl = ov['placement']
    me_id = ov['me']
    pl['modules']['mail'] = {'kind': 'mail', 'title': 'mail', 'node': me_id, 'host': 'mail.example.org', 'upstream': 'tls://127.0.0.1:4430'}
    s, body = api('POST', '/api/admin/placement', pl)
    ok(s == 200, f'a mail module with its name (mail.example.org) ({s} {str(body)[:200]})')
    # Round 24: a mail module is the mail instance (osada-setup mail makes one), so access to it
    # decides who has the webmail (D-U16): the administrator gets a mailbox and access
    s, us = api('POST', '/api/admin/table/users', {'limit': 50, 'params': {}})
    uid = {r['name']: r['id'] for r in us['rows']} if s == 200 else {}
    s, body = api('POST', '/api/admin/mail/action', {'op': 'mailbox_set', 'user': uid.get(USER), 'address': 'x-admin@example.org'})
    ok(s == 200, f'a mailbox x-admin@example.org ({s} {str(body)[:200]})')
    s, body = api('POST', '/api/admin/access', {'module': 'mail', 'subject': {'kind': 'user', 'id': uid.get(USER)}})
    ok(s == 200, f'access to the mail instance ({s} {str(body)[:200]})')
    s, me = api('GET', '/api/me')
    mail = [a for a in (me.get('apps') or []) if a.get('name') == 'mail'] if s == 200 else []
    ok(mail and mail[0].get('account') == 'https://mail.example.org/account/', f'/api/me: the mail app with its account pages ({mail})')

    b = cdp.Browser()
    b.load(S + '/signin')
    b.wait("!!document.querySelector('#si-name')")
    b.js(f"document.querySelector('#si-name').value = {json.dumps(USER)}; document.querySelector('#si-pw').value = {json.dumps(PW)};"
         "document.querySelector('#app form button[type=submit]').click()")
    b.wait("location.pathname != '/signin'", 10)
    b.errors.clear()
    b.load(S + '/')
    ok(b.wait("!!document.querySelector('#mail-account')", 10), 'portal: the mail card links the account pages')
    ok(b.js("document.querySelector('#mail-account').href") == 'https://mail.example.org/account/', 'portal: to https://mail.example.org/account/')
    ok(b.js("document.querySelector('#mail-account').target") == '_blank', 'portal: in a tab of its own')

    # admin → modules → the mail module's window
    b.load(S + '/admin/modules')
    ok(b.wait("[...document.querySelectorAll('table.grid td')].some(td => td.textContent === 'mail')", 10), 'modules: the mail module')
    b.js("[...document.querySelectorAll('table.grid tr')].find(tr => [...tr.cells].some(td => td.textContent === 'mail')).click()")
    ok(b.wait("document.querySelectorAll('.stalwart-link').length == 2", 5), "the mail module's window: Stalwart's pages")
    hrefs = b.js("[...document.querySelectorAll('.stalwart-link')].map(a => a.href)")
    ok(hrefs == ['https://mail.example.org/admin/', 'https://mail.example.org/account/'], f'administration and account pages ({hrefs})')

    # a module of another kind: none
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
