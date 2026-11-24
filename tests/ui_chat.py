"""Chat (D-U42) in two headless Chromiums (tests/cdp.py), one per person: a room made with the
other, messages there at once on the other side (the long poll), unread in the header, the room
list and the page title, typing seen and never written, edits and deletions seen live, a direct
message started from the picker, older messages on demand, nothing for someone not in the room.

    python3 -I tests/ui_chat.py <http://host:port> <file of A> <file of B>

Each file is `osada rescue`'s output (user: …, password: …), so no password is on the command line.
"""
import json
import os
import sys
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE = sys.argv[1]


def account(path):
    kv = dict(line.split(': ', 1) for line in open(path).read().splitlines() if ': ' in line)
    return kv['user'], kv['password']


A, B = account(sys.argv[2]), account(sys.argv[3])
check = cdp.Check()
ROOM = f"chat test {int(time.time())}"


def browser(who):
    b = cdp.Browser()
    # both windows count as focused: reading marks what is seen as read
    b.cmd('Emulation.setFocusEmulationEnabled', True, enabled=True)
    b.load('/signin')
    b.wait("!!document.querySelector('#si-name')")
    b.focus('#si-name')
    b.type(who[0])
    b.focus('#si-pw')
    b.type(who[1])
    b.key('Enter')
    check(f'{who[0]} signs in', b.wait("location.pathname !== '/signin' && !!document.querySelector('#who a.navlink')", 15), b.js('location.href'))
    return b


def items(b):
    return b.js("[...document.querySelectorAll('#chat-log .chat-msg .text')].map(e => e.firstChild ? e.childNodes[0].textContent : '')")


def say(b, text):
    b.focus('#chat-compose')
    b.type(text)
    b.key('Enter')


a = browser(A)
b = browser(B)
try:
    # the header and the portal have chat
    check('the header links chat', a.wait("!!document.querySelector('#nav a.chatlink')"))
    check('the portal has a chat card', a.wait("!!document.getElementById('chat-card-unread')"))
    a.js("document.querySelector('#nav a.chatlink').click()")
    check('chat opens in the page', a.wait("location.pathname === '/chat' && !!document.getElementById('chat-rooms')"))

    # A makes a room with B
    a.js("[...document.querySelectorAll('.chat-side button')].find(x => x.textContent === 'new room').click()")
    a.wait("!!document.querySelector('.modal input')")
    a.focus('.modal input')
    a.type(ROOM)
    a.focus('.modal .refpick input')
    a.type(B[0])
    check('the picker finds B', a.wait(f"[...document.querySelectorAll('.modal .refpick .opt')].some(o => o.textContent.includes({json.dumps(B[0])}))", 10))
    a.js(f"[...document.querySelectorAll('.modal .refpick .opt')].find(o => o.textContent.includes({json.dumps(B[0])})).dispatchEvent(new MouseEvent('mousedown', {{bubbles: true}}))")
    check('B is chosen', a.wait("document.querySelectorAll('.modal .chip').length === 1"))
    a.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'make the room').click()")
    check('the room is made and open', a.wait(f"(document.querySelector('#chat-pane .titlebar') || {{}}).textContent === '# ' + {json.dumps(ROOM)}", 15), a.text('#chat-pane'))
    check('its first line says who made it with whom', a.wait("[...document.querySelectorAll('.chat-event')].some(e => e.textContent.includes('you made the room'))"))
    say(a, 'hello from A')
    check('A sees the message', a.wait("[...document.querySelectorAll('#chat-log .chat-msg')].some(m => m.textContent.includes('hello from A'))"))
    check('the box is empty again', a.js("document.getElementById('chat-compose').value === ''"))

    # B: unread in the header and the title, before opening chat
    b.load('/')
    check('B: unread in the header', b.wait("(document.querySelector('#nav a.chatlink') || {}).textContent === 'chat (1)'", 15), b.text('#nav'))
    check('B: and in the title', b.wait("document.title.startsWith('(1) ')"))
    check('B: and on the portal', b.wait("(document.getElementById('chat-card-unread') || {}).textContent === '1 unread message'"))
    b.js("document.querySelector('#nav a.chatlink').click()")
    check('B: the room in the list, unread', b.wait(f"[...document.querySelectorAll('.chat-room-item.unread')].some(r => r.textContent.includes({json.dumps(ROOM)}))", 10), b.text('#chat-rooms'))
    b.js(f"[...document.querySelectorAll('.chat-room-item')].find(r => r.textContent.includes({json.dumps(ROOM)})).click()")
    check('B: the message', b.wait("[...document.querySelectorAll('#chat-log .chat-msg')].some(m => m.textContent.includes('hello from A'))"))
    check('B: marked new', b.js("!!document.querySelector('.chat-new')"))
    check('B: read, so nothing unread', b.wait("(document.querySelector('#nav a.chatlink') || {}).textContent === 'chat' && !document.title.startsWith('(')", 10), b.text('#nav'))

    # live: typing, then a message, without reloading A's page
    b.focus('#chat-compose')
    b.type('hi A')
    check('A sees B typing', a.wait("(document.getElementById('chat-typing') || {}).textContent.includes('typing')", 10), a.text('#chat-typing'))
    b.key('Enter')
    check('A gets the message live', a.wait("[...document.querySelectorAll('#chat-log .chat-msg')].some(m => m.textContent.includes('hi A'))", 10))
    check('the typing line goes', a.wait("(document.getElementById('chat-typing') || {}).textContent === ''", 10))
    check('A: nothing unread with the room open', a.wait("(document.querySelector('#nav a.chatlink') || {}).textContent === 'chat'", 10), a.text('#nav'))

    # A edits and deletes; B sees both live
    a.js("[...document.querySelectorAll('#chat-log .chat-msg.mine')].find(m => m.textContent.includes('hello from A')).querySelector('.acts a').click()")
    a.wait("!!document.querySelector('.modal textarea')")
    a.js("document.querySelector('.modal textarea').value = 'hello from A, edited'")
    a.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'save').click()")
    check('B sees the edit', b.wait("[...document.querySelectorAll('#chat-log .chat-msg')].some(m => m.textContent.includes('hello from A, edited') && m.textContent.includes('(edited)'))", 10))
    say(a, 'to be deleted')
    check('B sees the next one', b.wait("[...document.querySelectorAll('#chat-log .chat-msg')].some(m => m.textContent.includes('to be deleted'))", 10))
    a.js("[...document.querySelectorAll('#chat-log .chat-msg.mine')].find(m => m.textContent.includes('to be deleted')).querySelectorAll('.acts a')[1].click()")
    a.wait("!!document.querySelector('.modal')")
    a.js("[...document.querySelectorAll('.modal button')].find(x => x.textContent === 'yes').click()")
    check('B sees it deleted', b.wait("![...document.querySelectorAll('#chat-log .chat-msg')].some(m => m.textContent.includes('to be deleted'))", 10))
    check('B cannot edit A\'s message', not b.js("[...document.querySelectorAll('#chat-log .chat-msg')].filter(m => !m.classList.contains('mine')).some(m => [...m.querySelectorAll('.acts a')].some(x => x.textContent === 'edit'))"))

    # a direct message from the picker; B's list shows it live
    a.js("[...document.querySelectorAll('.chat-side button')].find(x => x.textContent === 'message someone').click()")
    a.wait("!!document.querySelector('.modal .refpick input')")
    a.focus('.modal .refpick input')
    a.type(B[0])
    a.wait(f"[...document.querySelectorAll('.modal .refpick .opt')].some(o => o.textContent.includes({json.dumps(B[0])}))", 10)
    a.js(f"[...document.querySelectorAll('.modal .refpick .opt')].find(o => o.textContent.includes({json.dumps(B[0])})).dispatchEvent(new MouseEvent('mousedown', {{bubbles: true}}))")
    check('the DM is open', a.wait("(document.querySelector('#chat-pane .titlebar') || {}).textContent.startsWith('@ ')", 15), a.text('#chat-pane'))
    say(a, 'psst')
    check('B: the DM in the list with one unread', b.wait("[...document.querySelectorAll('.chat-room-item.unread')].some(r => r.textContent.startsWith('@ ') && r.querySelector('.badge').textContent === '1')", 10), b.text('#chat-rooms'))
    check('B: unread in the header while in the room', b.wait("(document.querySelector('#nav a.chatlink') || {}).textContent === 'chat (1)'", 10))

    # many messages: the newest 60 shown, older ones on demand
    for i in range(70):
        a.js(f"fetch(location.origin + '/api/chat/rooms/' + new URLSearchParams(location.search).get('room') + '/do', {{method: 'POST', headers: {{'content-type': 'application/json', 'x-shelf': '1'}}, body: JSON.stringify({{op: 'say', text: 'n{i}'}})}}).then(r => r.status)")
    b.js("[...document.querySelectorAll('.chat-room-item')].find(r => r.textContent.startsWith('@ ')).click()")
    check('B: the latest of the DM', b.wait("(() => { const t = [...document.querySelectorAll('#chat-log .chat-msg .text')].map(e => e.childNodes[0].textContent); return t.length === 60 && t[59] === 'n69'; })()", 10), items(b)[-3:])
    b.js("document.querySelector('.chat-older button').click()")
    # 60 more (the DM of these two keeps what earlier runs said), in order
    check('B: and the older ones', b.wait("(() => { const s = [...document.querySelectorAll('#chat-log .chat-msg')].map(m => +m.dataset.seq); return s.length > 60 && s.length <= 120 && s.every((x, i) => !i || s[i - 1] < x); })()", 10), len(items(b)))

    # someone not in the room: no history
    room_id = a.js("new URLSearchParams(location.search).get('room')")
    check('a DM is not a room of others', a.js(f"fetch('/api/chat/rooms/{room_id}x').then(r => r.status)") == 404)
    # what the sign-in page asks before signing in, and the refusal above, are expected
    errors = [e for e in a.errors + b.errors if not ('401' in e and e.endswith('/api/account')) and not e.endswith(f'/api/chat/rooms/{room_id}x')]
    check('no page errors', not errors, errors)
finally:
    a.close()
    b.close()
sys.exit(check.done())
