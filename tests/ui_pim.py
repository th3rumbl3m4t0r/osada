"""Round 15 in Chromium on a drop-in instance (an account without a mailbox): contacts and calendar in the header, the people here with one search, a contact card, the month, the no-mailbox notes.

    python3 -I tests/ui_pim.py <base URL> <file with user: / password:>
"""
import os, sys, time
sys.path.insert(0, '/root/osada-0.2/tests')
import cdp
cdp.BASE = sys.argv[1]
kv = dict(l.split(': ', 1) for l in open(sys.argv[2]).read().splitlines() if ': ' in l)
check = cdp.Check()
b = cdp.Browser()
b.load('/signin'); b.wait("!!document.querySelector('#si-name')")
b.focus('#si-name'); b.type(kv['user']); b.focus('#si-pw'); b.type(kv['password']); b.key('Enter')
b.wait("!!document.querySelector('#who a.navlink')", 15)
check("the header: contacts and calendar", b.wait("['contacts', 'calendar'].every(n => [...document.querySelectorAll('#nav a')].some(a => a.textContent === n))", 10), b.text('#nav'))
b.js("[...document.querySelectorAll('#nav a')].find(a => a.textContent === 'contacts').click()")
check("contacts: the people here", b.wait("(document.querySelector('[data-group=\"people here\"] .contact-group-head') || {}).textContent.startsWith('people here (')", 10), b.text('#app')[:300])
check("... the note: no mailbox", 'no mailbox' in b.text('#contacts-groups'))
b.focus('#contacts-q'); b.type('rehearsal')
check("... one search over them", b.wait("document.querySelectorAll('.contact-item').length === 1", 5), b.text('#contacts-groups')[:300])
b.js("document.querySelector('.contact-item').click()")
check("... a contact's card", b.wait("(document.querySelector('#contact-detail .titlebar') || {}).textContent.length > 0 && document.querySelector('#contact-detail').textContent.includes('people here')", 5))
b.js("[...document.querySelectorAll('#nav a')].find(a => a.textContent === 'calendar').click()")
check("calendar: the month drawn", b.wait("document.querySelectorAll('.cal-cell').length === 42", 10))
check("... says there is no mailbox", b.wait("(document.getElementById('cal-day') || {}).textContent.includes('no mailbox')", 10), b.text('#cal-day'))
check("no page errors", not [e for e in b.errors if '401' not in e and '404' not in e], b.errors)
b.close()
sys.exit(check.done())
