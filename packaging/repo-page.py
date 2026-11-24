#!/usr/bin/env python3
"""The repository's page (index.html): how to install, the key's fingerprint, what is there.

    python3 -I packaging/repo-page.py <site folder> <site url> <key fingerprint> > index.html
"""
import html, os, re, sys

site, url, fpr = sys.argv[1], sys.argv[2].rstrip("/"), sys.argv[3]
e = html.escape


def size(n):
    return f"{n / 1048576:.1f} MB" if n >= 1048576 else f"{n / 1024:.0f} kB"


def listing(rel, pattern):
    d = os.path.join(site, rel)
    out = []
    for root, _, files in os.walk(d):
        for f in sorted(files):
            m = re.match(pattern, f)
            if m:
                p = os.path.join(root, f)
                out.append((m.group(1), m.group(2), os.path.relpath(p, site), os.path.getsize(p)))
    return sorted(out)


RPM = r"(.+)-([^-]+-[^-]+)\.(?:x86_64|noarch)\.rpm$"
DEB = r"([^_]+)_([^_]+)_(?:amd64|all)\.deb$"
groups = [("EL 9", "el9/x86_64", RPM), ("EL 10", "el10/x86_64", RPM)]
fedora = sorted(os.listdir(os.path.join(site, "fedora"))) if os.path.isdir(os.path.join(site, "fedora")) else []
groups += [(f"Fedora {v}", f"fedora/{v}/x86_64", RPM) for v in fedora]
groups += [("Debian, Ubuntu", "deb/pool", DEB)]
rows = []
for label, rel, pat in groups:
    for name, ver, path, n in listing(rel, pat):
        rows.append(f"<tr><td>{e(label)}</td><td>{e(name)}</td><td>{e(ver)}</td>"
                    f"<td><a href=\"{e(path)}\">{e(os.path.basename(path))}</a></td><td>{size(n)}</td></tr>")
tarballs = sorted(os.listdir(os.path.join(site, "tarballs"))) if os.path.isdir(os.path.join(site, "tarballs")) else []
tb = "".join(f"<li><a href=\"tarballs/{e(t)}\">{e(t)}</a></li>" for t in tarballs)
u = e(url)

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
<p>Everything here is signed with one key, fingerprint <span class="fpr">{e(fpr)}</span>
(<a href="RPM-GPG-KEY-osada">armored</a>, <a href="osada.gpg">binary for apt</a>). As root:</p>

<h2>Rocky Linux, AlmaLinux, RHEL, CentOS Stream 9 and 10</h2>
<pre><code>curl -fsSL -o /etc/yum.repos.d/osada.repo {u}/osada.repo
dnf install -y osada</code></pre>

<h2>Fedora {e(", ".join(fedora)) or ""}</h2>
<pre><code>curl -fsSL -o /etc/yum.repos.d/osada.repo {u}/osada-fedora.repo
dnf install -y osada</code></pre>

<h2>Debian 12, 13; Ubuntu 22.04, 24.04, 26.04</h2>
<pre><code>apt-get update &amp;&amp; apt-get install -y curl ca-certificates
curl -fsSL -o /usr/share/keyrings/osada.gpg {u}/osada.gpg
curl -fsSL -o /etc/apt/sources.list.d/osada.sources {u}/osada.sources
apt-get update &amp;&amp; apt-get install -y osada</code></pre>

<h2>Then make the node</h2>
<pre><code># one machine: the first node and an edge, a certificate for your name
osada-setup first --edge --public-name osada.example.org --firewall

# more machines, anywhere: on the first node …
osada-setup token --name edge2 --at &lt;its address&gt;
# … and on that machine (osada installed there first) the line it prints:
osada-setup node --name edge2 --token osada-join.… --edge --firewall</code></pre>
<p>Then point your name's DNS at the edges, open <code>https://osada.example.org/</code> and sign in as
<code>admin</code> (the password: <code>cat /var/lib/osada/node/first-admin</code>).
<code>--firewall</code> opens the ports in firewalld or ufw, whichever runs.</p>

<h2>Mail</h2>
<pre><code>dnf install -y osada-mail        # or: apt-get install -y osada-mail (Stalwart, the mail server)
osada-setup mail --firewall</code></pre>
<p>It sets up Stalwart, its certificate and DKIM, the webmail (<code>https://mail.&lt;domain&gt;/maildot/</code>, signing in with osada) and the mail instance, and prints the DNS records to publish.</p>

<h2>The whole guide</h2>
<p><a href="INSTALL.md">INSTALL.md</a>: every way to start (one VM; home and an edge; more nodes),
mail, certificates, upgrading, where things are, removing.</p>

<h2>Other Linux systems</h2>
<p>Any x86_64 Linux with systemd and glibc 2.34 or newer: the tarball has the same program and
<code>./osada-setup</code>; signed (<code>.asc</code>) with the same key.</p>
<ul>{tb}</ul>

<h2>Packages</h2>
<table><tr><th>for</th><th>package</th><th>version</th><th>file</th><th>size</th></tr>
{''.join(rows)}
</table>
<p class="dim">osada is AGPL-3.0-only. Stalwart is packaged unchanged from its authors' release
(AGPL-3.0; sources at github.com/stalwartlabs/stalwart). Checksums: <a href="SHA256SUMS">SHA256SUMS</a>.</p>
</main></body></html>""")
