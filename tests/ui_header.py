"""The header (Round 20) in headless Chromium on a drop-in (tests/dropin-ui.sh): "portal // home
files shared …" looks the same on osada 0.2's pages and osada 0.1's (the portal and the files):
the portal's block no wider when it is the page, "// <node>" on the same line as the apps' names
(not higher), as far from the portal as the apps from each other, and further from the first app.

    tests/dropin-ui.sh target/release/osada ui_header.py
"""
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cdp  # noqa: E402

cdp.BASE, NAME, PW, D = sys.argv[1:5]
check = cdp.Check()

# each item of the left side: its text, its box, its text's box (where the letters are)
MEASURE = """(() => [...document.querySelector('#nav').children].filter(e => e.offsetParent).map(e => {
  const b = e.getBoundingClientRect(), r = document.createRange();
  r.selectNodeContents(e);
  const t = [...r.getClientRects()].reduce((a, x) => a ? { left: Math.min(a.left, x.left), right: Math.max(a.right, x.right), top: Math.min(a.top, x.top), bottom: Math.max(a.bottom, x.bottom) } : { left: x.left, right: x.right, top: x.top, bottom: x.bottom }, null);
  const cs = getComputedStyle(e);
  return { text: e.textContent.trim(), cls: e.className, box: [b.left, b.right, b.top, b.bottom].map(Math.round), letters: [t.left, t.right, t.top, t.bottom].map(x => Math.round(x * 10) / 10), font: cs.fontSize + ' ' + cs.fontFamily.split(',')[0] + ' ' + cs.fontWeight };
}))()"""


def gaps(items):
    """the space between one item's letters and the next one's"""
    return [(a["text"], b["text"], round(b["letters"][0] - a["letters"][1], 1)) for a, b in zip(items, items[1:])]


b = cdp.Browser()
try:
    b.load("/signin")
    b.wait("!!document.querySelector('#si-name')", 10)
    b.focus("#si-name"); b.type(NAME); b.focus("#si-pw"); b.type(PW); b.key("Enter")
    check("signed in", b.wait("location.pathname === '/'", 10))
    pages = {}
    for path in ["/", "/files/home/", "/account"]:
        b.load(path)
        b.wait("document.querySelectorAll('#nav > *').length > 2 && [...document.querySelectorAll('#nav > *')].some(e => e.textContent.includes('//'))", 15)
        time.sleep(1)
        pages[path] = b.js(MEASURE)
        print(path, json.dumps(pages[path]))
        print("   gaps", gaps(pages[path]))
    for path, items in pages.items():
        by = {i["text"].split(" ")[0] if not i["text"].startswith("//") else "node": i for i in items}
        node, portal = by.get("node"), by.get("portal")
        apps = [i for i in items if "app" in i["cls"].split()]
        check(f"{path}: portal, the node, apps", node and portal and len(apps) >= 2, items)
        if not (node and portal and len(apps) >= 2):
            continue
        # the node's letters on the line of the apps' letters
        check(f"{path}: '// node' on the apps' line (not higher)", abs(node["letters"][3] - apps[0]["letters"][3]) <= 0.6, (node, apps[0]))
        g = dict(((x, y), d) for x, y, d in gaps(items))
        between_apps = g[(apps[0]["text"], apps[1]["text"])]
        after_node = g[(node["text"], apps[0]["text"])]
        before_node = g[(portal["text"], node["text"])]
        check(f"{path}: the first app further from the node than from the next app", after_node >= between_apps + 4, (after_node, between_apps))
        check(f"{path}: the node as far from portal as the apps from each other", abs(before_node - between_apps) <= 1.5, (before_node, between_apps))
    # the portal's block the same on every page (on or not), the node where it was
    p0 = {k: [i for i in v if i["text"] == "portal"][0] for k, v in pages.items()}
    widths = {k: v["box"][1] - v["box"][0] for k, v in p0.items()}
    check("portal: as wide on every page", max(widths.values()) - min(widths.values()) <= 2, widths)
    n0 = {k: [i for i in v if i["text"].startswith("//")][0]["letters"][0] for k, v in pages.items()}
    check("'// node' at the same place on every page", max(n0.values()) - min(n0.values()) <= 2, n0)
    fonts = {k: [i["font"] for i in v if i["text"] == "portal"][0] for k, v in pages.items()}
    check("the same font on every page", len(set(fonts.values())) == 1, fonts)
    check("no page errors", not [e for e in b.errors if "401" not in e], b.errors)
finally:
    b.close()
sys.exit(check.done())
