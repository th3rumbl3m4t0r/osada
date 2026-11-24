"""The drop-in upgrade in headless Chromium (tests/cdp.py): osada 0.1's pages
hand sign-in over to /signin and get the user back; the files, the account
page and the admin portal link to each other; another program's sign-in
(OIDC) goes through /signin; logging out of either ends the session.

    python3 -I tests/ui_dropin.py <http://host:port> <admin name> <password> [<oidc authorize URL>]
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW = sys.argv[1], sys.argv[2], sys.argv[3]
AUTHORIZE = sys.argv[4] if len(sys.argv) > 4 else None
fails = []


def ok(name, cond, extra=""):
    print(("ok   " if cond else "FAIL ") + name + ("" if cond else f"  {extra}"))
    if not cond:
        fails.append(name)


def came_back(b, since, secs=10):
    """Whether the browser went to the program's redirect URI with a code (it
    does not answer: the page is Chrome's error page, the request is seen)."""
    end = time.time() + secs
    while time.time() < end:
        b.js("1")
        if any("127.0.0.1:9/cb?code=" in u for u in b.requests[since:]):
            return True
        time.sleep(0.2)
    return False


def sign_in(b):
    b.focus("#si-name")
    b.type(NAME)
    b.focus("#si-pw")
    b.type(PW)
    b.key("Enter")


b = cdp.Browser()
try:
    b.load("/files/home/")
    ok("osada 0.1's pages send to /signin", b.wait("location.pathname === '/signin' && !!document.querySelector('#si-name')", 10), b.js("location.href"))
    ok("with the way back", b.js("new URLSearchParams(location.search).get('next')") in ("/", "/files/home/"))
    sign_in(b)
    ok("back at the files", b.wait("location.pathname === '/files/home/' && !!document.querySelector('#nav a[data-sec=files]')", 10), b.js("location.href"))
    # one header (D-U13): portal at the left, the apps, admin and the user at the right
    ok("files: portal at the left", b.wait("document.querySelector('#nav a').getAttribute('href') === '/' && document.querySelector('#nav a').textContent === 'portal'"))
    ok("files: the apps", b.js("['files', 'wiki'].every(s => !!document.querySelector(`#nav a[data-sec=${s}]`))"))
    ok("files: admin at the left, the account at the right", b.js("!!document.querySelector('#nav a.admin') && !!document.querySelector('#who a.navlink')"))
    # a narrow window: app links that would come within 20 px of the user's part go (they are on the portal)
    FITS = "(() => { const v = [...document.querySelectorAll('#nav > a')].filter(a => !a.hidden).pop(); return v.getBoundingClientRect().right + 20 <= document.querySelector('#who').getBoundingClientRect().left; })()"
    ok("files: wide, every app in the header", b.js("[...document.querySelectorAll('#nav a.app')].every(a => !a.hidden)") and b.js(FITS))
    b.cmd("Emulation.setDeviceMetricsOverride", True, width=560, height=800, deviceScaleFactor=1, mobile=False)
    b.js("window.dispatchEvent(new Event('resize'))")
    time.sleep(0.5)
    ok("files: narrow, some apps go, the rest keeps 20 px", b.js("[...document.querySelectorAll('#nav a.app')].some(a => a.hidden)") and b.js(FITS),
       b.js("[...document.querySelectorAll('#nav > a')].map(a => a.textContent + (a.hidden ? '-' : '+')).join(' ')"))
    ok("files: narrow, admin stays at the left", b.js("!!document.querySelector('#nav a.admin') && !document.querySelector('#nav a.admin').hidden"))
    b.js("document.querySelector('#nav a.portal').click()")
    b.wait("location.pathname === '/' && document.querySelectorAll('.portal .card').length >= 3", 10)
    time.sleep(0.3)
    # (when even portal and admin alone come closer than 20 px, every app is hidden: nothing more to take away)
    ok("portal: narrow, the same", b.js("[...document.querySelectorAll('#nav a.app')].some(a => a.hidden)")
       and (b.js(FITS) or b.js("[...document.querySelectorAll('#nav a.app')].every(a => a.hidden)")) and b.js("!document.querySelector('#nav a.admin').hidden"),
       b.js("[...document.querySelectorAll('#nav > a')].map(a => a.textContent + (a.hidden ? '-' : '+')).join(' ')"))
    ok("portal: narrow, every app still a card", b.js("[...document.querySelectorAll('#nav a.app')].every(a => [...document.querySelectorAll('.portal .card .titlebar')].some(t => t.textContent.startsWith(a.textContent)))"))
    b.cmd("Emulation.clearDeviceMetricsOverride", True)
    b.js("window.dispatchEvent(new Event('resize'))")
    time.sleep(0.3)
    ok("wide again: every app back", b.js("[...document.querySelectorAll('#nav a.app')].every(a => !a.hidden)"))
    b.js("document.querySelector('#nav a.portal').click()")
    ok("the portal", b.wait("location.pathname === '/' && document.querySelectorAll('.portal .card').length >= 3", 10), b.js("location.href"))
    ok("portal: a card for files, with storage", b.js("[...document.querySelectorAll('.portal .card')].some(c => c.textContent.startsWith('files') && /of|in your home/.test(c.textContent))"))
    ok("portal: the same header", b.js("document.querySelector('#nav a').textContent === 'portal' && !!document.querySelector('#nav a[href=\"/files/home/\"]')"))
    b.js("document.querySelector('#nav a[href=\"/admin\"]').click()")
    ok("the admin page", b.wait("location.pathname.startsWith('/admin') && !!document.querySelector('.tabs')", 10), b.js("location.href"))
    ok("admin: tabs for the modules", b.js("['files', 'proxies', 'apps'].every(t => !!document.querySelector(`.tabs a[data-tab=${t}]`))"))
    b.js("document.querySelector('.tabs a[data-tab=files]').click()")
    ok("admin: files, storage limits", b.wait("!!document.querySelector('button.set-limit')", 10))
    ok("admin: files, nightly backup", b.wait("[...document.querySelectorAll('.titlebar')].some(t => t.textContent.includes('nightly backup'))", 10))
    b.js("document.querySelector('.tabs a[data-tab=apps]').click()")
    ok("admin: sign-in for apps", b.wait("[...document.querySelectorAll('.titlebar')].some(t => t.textContent.includes('OpenID'))", 10))
    b.js("document.querySelector('.tabs a[data-tab=users]').click()")
    ok("admin: users with their outside networks", b.wait("[...document.querySelectorAll('.gridbox th')].some(th => th.textContent.includes('from outside'))", 10))
    b.load("/osada-admin")
    ok("osada 0.1's admin page is the admin page's files tab", b.wait("location.pathname === '/admin/files'", 10), b.js("location.href"))
    b.js("document.querySelector('#who a[href=\"/account\"]').click()")
    ok("the account page", b.wait("location.pathname === '/account' && document.querySelector('#app').textContent.includes('session')", 10))
    b.js("document.querySelector('#nav a[href=\"/files/home/\"]').click()")
    ok("and back to the files", b.wait("location.pathname === '/files/home/' && !!document.querySelector('#nav a[data-sec=files]')", 10))

    if AUTHORIZE:
        n = len(b.requests)
        b.load(AUTHORIZE)
        ok("OIDC: signed in already, straight back to the program", came_back(b, n), b.requests[n:])

    # out on osada 0.1's side: the session is gone for both
    b.load("/files/home/")
    b.wait("!!document.querySelector('#who button')")
    b.js("[...document.querySelectorAll('#who button')].pop().click()")
    ok("logged out: at /signin", b.wait("location.pathname === '/signin' && !!document.querySelector('#si-name')", 10), b.js("location.href"))
    b.load("/account")
    ok("logged out: the account page asks to sign in", b.wait("!!document.querySelector('#si-name')"))

    if AUTHORIZE:
        b.load(AUTHORIZE)
        ok("OIDC: /signin, for the program", b.wait("location.pathname === '/signin' && document.querySelector('#app').textContent.includes('to continue to')", 10), b.js("location.href"))
        n = len(b.requests)
        sign_in(b)
        ok("OIDC: back to the program with a code", came_back(b, n), b.requests[n:])

    errors = [e for e in b.errors if "Failed to load resource" not in str(e) and "127.0.0.1:9" not in str(e)]
    ok("no script errors", not errors, errors[:3])
finally:
    b.close()
print("FAILED: " + ", ".join(fails) if fails else "all passed")
sys.exit(1 if fails else 0)
