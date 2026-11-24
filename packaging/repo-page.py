#!/usr/bin/env python3
"""The repository's page (index.html): how to install, the key's fingerprint, what is there.

    python3 -I packaging/repo-page.py <site folder> <site url> <key fingerprint> > index.html
"""
import html, os, re, sys

site, url, fpr = sys.argv[1], sys.argv[2].rstrip("/"), sys.argv[3]
e = html.escape


def rpms(el):
    d = os.path.join(site, f"el{el}", "x86_64")
    out = []
    for f in sorted(os.listdir(d)) if os.path.isdir(d) else []:
        m = re.match(r"(.+)-([^-]+)-([^-]+)\.(x86_64|noarch)\.rpm$", f)
        if m:
            out.append((m.group(1), m.group(2), m.group(3), f, os.path.getsize(os.path.join(d, f))))
    return out


def size(n):
    return f"{n / 1048576:.1f} MB" if n >= 1048576 else f"{n / 1024:.0f} kB"


rows = []
for el in (9, 10):
    for name, ver, rel, f, n in rpms(el):
        rows.append(f"<tr><td>EL{el}</td><td>{e(name)}</td><td>{e(ver)}-{e(rel)}</td>"
                    f"<td><a href=\"el{el}/x86_64/{e(f)}\">{e(f)}</a></td><td>{size(n)}</td></tr>")
tarballs = sorted(os.listdir(os.path.join(site, "tarballs"))) if os.path.isdir(os.path.join(site, "tarballs")) else []
tb = "".join(f"<li><a href=\"tarballs/{e(t)}\">{e(t)}</a></li>" for t in tarballs)

print(f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>osada packages</title>
<style>
:root {{ color-scheme: light dark; --fg:#1b1f24; --dim:#5b6470; --bg:#fbfaf7; --card:#fff; --line:#e3e0d8; --acc:#8a3b12; }}
@media (prefers-color-scheme: dark) {{ :root {{ --fg:#e8e6e1; --dim:#a39f96; --bg:#16181b; --card:#1e2125; --line:#33373d; --acc:#e59866; }} }}
body {{ margin:0; background:var(--bg); color:var(--fg); font:16px/1.55 system-ui, sans-serif; }}
main {{ max-width: 860px; margin: 0 auto; padding: 32px 20px 60px; }}
h1 {{ font-size: 34px; margin: 0 0 4px; letter-spacing: -0.5px; }}
h1 span {{ color: var(--acc); }}
h2 {{ font-size: 20px; margin: 34px 0 8px; }}
p.lead {{ color: var(--dim); margin-top: 0; }}
pre {{ background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 12px 14px; overflow-x: auto; font-size: 14px; }}
code {{ font: 14px ui-monospace, SFMono-Regular, Menlo, monospace; }}
table {{ border-collapse: collapse; width: 100%; font-size: 14px; }}
td, th {{ text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--line); }}
a {{ color: var(--acc); }}
.fpr {{ font: 14px ui-monospace, monospace; background: var(--card); border: 1px solid var(--line); border-radius: 6px; padding: 2px 6px; }}
.dim {{ color: var(--dim); font-size: 14px; }}
</style></head>
<body><main>
<h1><span>osada</span> packages</h1>
<p class="lead">Self-hosted files, wiki, sign-in and groupware for a household or an organisation:
on one machine, or a first node at home with edges in the cloud.</p>

<h2>Rocky Linux, AlmaLinux, RHEL 9 and 10</h2>
<pre><code>curl -fsSL -o /etc/yum.repos.d/osada.repo {e(url)}/osada.repo
dnf install -y osada</code></pre>
<p>dnf asks once to trust the key <a href="RPM-GPG-KEY-osada">RPM-GPG-KEY-osada</a>, fingerprint
<span class="fpr">{e(fpr)}</span>. Then make the node:</p>
<pre><code># one VM in the cloud: the first node and its edge, a certificate for your name
osada-setup first --name home --edge --public-name osada.example.org --firewall

# or: the first node at home …
osada-setup first --name home --firewall
osada-setup token --name edge --at &lt;the cloud VM's address&gt;
# … and on the cloud VM (dnf install osada there first) the line it prints:
osada-setup node --name edge --token osada-join.… --edge --firewall</code></pre>
<p>Then point your name's DNS at the edge, open <code>https://osada.example.org/</code> and sign in as
<code>admin</code> (the password: <code>cat /var/lib/osada/node/first-admin</code>).</p>

<h2>Mail</h2>
<pre><code>dnf install -y osada-mail        # Stalwart, the mail server
osada-setup mail --firewall</code></pre>
<p>then admin → modules (a module of kind mail on that node) and admin → mail.</p>

<h2>The whole guide</h2>
<p><a href="INSTALL.md">INSTALL.md</a>: every way to start (one VM; home and an edge; more nodes),
mail, certificates, upgrading (<code>dnf upgrade osada</code>), where things are, removing.</p>

<h2>Other Linux systems</h2>
<p>Debian 12, Ubuntu 22.04 and newer, Fedora (x86_64, glibc 2.34+): the tarball has the same
program and <code>./osada-setup</code>; signed (<code>.asc</code>) with the same key.</p>
<ul>{tb}</ul>

<h2>Packages</h2>
<table><tr><th>for</th><th>package</th><th>version</th><th>file</th><th>size</th></tr>
{''.join(rows)}
</table>
<p class="dim">osada is AGPL-3.0-only. Stalwart is packaged unchanged from its authors' release
(AGPL-3.0; sources at github.com/stalwartlabs/stalwart). Checksums: <a href="SHA256SUMS">SHA256SUMS</a>.</p>
</main></body></html>""")
