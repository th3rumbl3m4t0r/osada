"""Headless Chromium over the DevTools protocol (--remote-debugging-pipe), for driving
the test osada. Records page errors and console errors, and the requests made."""
import json
import os
import shutil
import subprocess
import tempfile
import time

BASE = 'http://127.0.0.1:18096'


class Browser:
    def __init__(self, args=()):
        r_cmd, self.w = os.pipe()
        self.r, w_ev = os.pipe()
        self.prof = tempfile.mkdtemp(prefix='cdp-')

        def child():
            os.dup2(r_cmd, 3)
            os.dup2(w_ev, 4)
        self.proc = subprocess.Popen(
            ['/usr/lib64/chromium-browser/headless_shell', '--remote-debugging-pipe', '--no-sandbox', '--disable-gpu',
             '--window-size=1280,900', f'--user-data-dir={self.prof}', *args, 'about:blank'],
            preexec_fn=child, pass_fds=(3, 4), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.close(r_cmd)
        os.close(w_ev)
        self.buf, self.n, self.events = b'', 0, []
        self.errors, self.requests = [], []
        t = next(t for t in self.cmd('Target.getTargets')['targetInfos'] if t['type'] == 'page')
        self.s = self.cmd('Target.attachToTarget', targetId=t['targetId'], flatten=True)['sessionId']
        for d in ('Page', 'Network', 'Runtime', 'Log'):
            self.cmd(d + '.enable', session=True)

    def _read(self):
        while b'\0' not in self.buf:
            c = os.read(self.r, 1 << 20)
            if not c:
                raise RuntimeError('chrome closed the pipe')
            self.buf += c
        m, self.buf = self.buf.split(b'\0', 1)
        return json.loads(m)

    def _event(self, m):
        meth, p = m.get('method'), m.get('params', {})
        if meth == 'Runtime.exceptionThrown':
            d = p['exceptionDetails']
            self.errors.append('exception: ' + (d.get('exception', {}).get('description') or d.get('text', '')).split('\n')[0])
        elif meth == 'Runtime.consoleAPICalled' and p['type'] == 'error':
            self.errors.append('console: ' + ' '.join(str(a.get('value', a.get('description', ''))) for a in p['args']))
        elif meth == 'Log.entryAdded' and p['entry']['level'] == 'error' and 'favicon' not in p['entry'].get('url', ''):
            self.errors.append('log: ' + p['entry']['text'] + ' ' + p['entry'].get('url', ''))
        elif meth == 'Network.requestWillBeSent':
            self.requests.append(p['request']['url'])
        self.events.append(m)

    def cmd(self, method, session=False, **params):
        self.n += 1
        msg = {'id': self.n, 'method': method, 'params': params}
        if session:
            msg['sessionId'] = self.s
        os.write(self.w, json.dumps(msg).encode() + b'\0')
        while True:
            m = self._read()
            if m.get('id') == self.n:
                if 'error' in m:
                    raise RuntimeError(f'{method}: {m["error"]}')
                return m.get('result', {})
            self._event(m)

    def js(self, expr):
        r = self.cmd('Runtime.evaluate', True, expression=expr, awaitPromise=True, returnByValue=True)
        if 'exceptionDetails' in r:
            d = r['exceptionDetails']
            raise RuntimeError((d.get('exception', {}).get('description') or d.get('text', '')).split('\n')[0])
        return r['result'].get('value')

    def wait(self, expr, secs=10):
        end = time.time() + secs
        last = None
        while time.time() < end:
            try:
                v = self.js(expr)
                if v:
                    return v
            except RuntimeError as e:
                last = e
            time.sleep(0.1)
        return False

    def load(self, url):
        self.cmd('Page.navigate', True, url=url if url.startswith('http') else BASE + url)
        self.wait("document.readyState === 'complete'")

    def login(self, user='alice', pw='alice-pass-1'):
        self.load('/login')
        s = self.js(f"fetch('/api/login', {{method: 'POST', headers: {{'content-type': 'application/json', 'x-shelf': '1'}}, "
                    f"body: JSON.stringify({{username: '{user}', password: '{pw}'}})}}).then(r => r.status)")
        assert s == 200, s

    def go(self, path):
        """an SPA navigation, then wait for the page to settle"""
        self.js(f"navigate({json.dumps(path)})")
        self.wait("!document.querySelector('#busy') || document.querySelector('#busy').hidden")
        time.sleep(0.3)

    def text(self, sel='#app'):
        return self.js(f"(document.querySelector({json.dumps(sel)}) || {{}}).textContent || ''")

    KEYS = {'Enter': 13, 'Escape': 27, 'Tab': 9, 'Backspace': 8, 'ArrowDown': 40, 'ArrowUp': 38, 'ArrowLeft': 37, 'ArrowRight': 39, 'F2': 113}

    def key(self, k, mods=0):
        """one key press, as the keyboard sends it; mods: 2 ctrl, 8 shift"""
        code = self.KEYS.get(k, ord(k.upper()) if len(k) == 1 else 0)
        base = dict(key=k, windowsVirtualKeyCode=code, nativeVirtualKeyCode=code, modifiers=mods)
        text = k if len(k) == 1 and not mods & 2 else ('\r' if k == 'Enter' else '')
        self.cmd('Input.dispatchKeyEvent', True, type='keyDown' if text else 'rawKeyDown', text=text, **base)
        self.cmd('Input.dispatchKeyEvent', True, type='keyUp', **base)

    def type(self, text):
        for ch in text:
            self.key(ch)

    def focus(self, sel):
        self.js(f"document.querySelector({json.dumps(sel)}).focus()")

    def screenshot(self, path):
        import base64
        d = self.cmd('Page.captureScreenshot', True, format='png')['data']
        open(path, 'wb').write(base64.b64decode(d))

    def close(self):
        self.proc.kill()
        self.proc.wait()
        shutil.rmtree(self.prof, ignore_errors=True)


class Check:
    def __init__(self):
        self.fails = 0

    def __call__(self, name, cond, extra=''):
        print(('ok   ' if cond else 'FAIL ') + name + (('  -- ' + str(extra)[:600]) if not cond and extra != '' else ''))
        if not cond:
            self.fails += 1
        return cond

    def done(self):
        print(f'\n{self.fails} failed' if self.fails else '\nall ok')
        return 1 if self.fails else 0
