# Installing osada

osada is self-hosted files, a wiki, sign-in (OpenID Connect) and groupware (chat, mail through
Stalwart, calendars through the apps) for a household or an organisation, on one machine or
spread over several, wherever they are: a **first node** that keeps the data, **edges** that
answer https and relay to it, and other nodes as you like.

## The packages

As root, on every machine that is to run osada. **Rocky Linux, AlmaLinux, RHEL, CentOS Stream
9 and 10:**

    curl -fsSL -o /etc/yum.repos.d/osada.repo https://repo.th3rumbl3.com/osada.repo
    dnf install -y osada

**Fedora** (44):

    curl -fsSL -o /etc/yum.repos.d/osada.repo https://repo.th3rumbl3.com/osada-fedora.repo
    dnf install -y osada

`dnf` asks once to trust the repository's key (`RPM-GPG-KEY-osada`, fingerprint on
https://repo.th3rumbl3.com/). **Debian 12, 13 and Ubuntu 22.04, 24.04, 26.04:**

    apt-get update && apt-get install -y curl ca-certificates
    curl -fsSL -o /usr/share/keyrings/osada.gpg https://repo.th3rumbl3.com/osada.gpg
    curl -fsSL -o /etc/apt/sources.list.d/osada.sources https://repo.th3rumbl3.com/osada.sources
    apt-get update && apt-get install -y osada

The packages:

| package | what |
|---|---|
| `osada` | the program (`/usr/bin/osada`) and `osada-setup`, which makes a node: the first one, an edge, another node, certificates |
| `osada-mail` | mail: pulls in `stalwart` (the mail server, upstream's release in `/opt/stalwart`) and what `osada-setup mail` needs |
| `stalwart` | Stalwart, the mail and groupware server, as released upstream (AGPL-3.0) |

Nothing starts by itself: `osada-setup` (below) makes the node, its settings
(`/etc/osada/osada.env`) and its services. `dnf upgrade` / `apt-get upgrade` bring new versions
and restart the services that run.

**Other Linux systems** (x86_64, systemd, glibc 2.34 or newer): the tarball
`osada-<version>-linux-x86_64.tar.gz` from https://repo.th3rumbl3.com/ has the same program
and `./osada-setup` (it puts the program at `/usr/local/bin/osada`); use `./osada-setup` where
this guide says `osada-setup`.

## Before you start

- **A public name** for osada, e.g. `osada.example.org`, whose DNS you can set.
- **Firewalls.** `--firewall` opens what a node needs in firewalld (Rocky's and Fedora's) or
  ufw (Ubuntu's), whichever runs; with neither running it opens nothing. Cloud providers often
  have a firewall of their own (security group, cloud firewall): allow there, on every edge, TCP
  80 and 443 and UDP 7441 (UDP 7442 too on a VM that runs the first node and an edge), and on the
  mail host TCP 25, 465, 587 and 993. `osada-setup` says which.
- SELinux may stay enforcing: osada's services run unconfined.
- Sites with containers (admin → web) need `podman` on the node that runs them
  (`dnf install podman`, `apt-get install podman`).

## Making the nodes

Nodes can be anywhere: VMs at a provider, machines on a local network, several on one network.
osada does not mind which is where; you decide what each does, and name each as you like:

- the **first node** keeps the data (files, wiki, accounts, logs) and holds the cluster's CA
  (`--name`; the machine's name by default);
- **edges** answer the public names (https on 443, http on 80) and relay to the nodes that hold
  the data; one can run on the first node's machine (`--edge`);
- **other nodes** store, cache or run sites.

A node joins with a one-time token made on the first node: the first node dials it (`--at <its
address>`), or it dials the first node (`--peer`) when only that way is open.

### One machine

    osada-setup first --edge --public-name osada.example.org --firewall

It makes two nodes: the first node (named after the machine; account `osada`, `/var/lib/osada`,
its pages on 127.0.0.1:8096) and its edge (`<name>-edge`; account `osada-node`,
`/var/lib/osada-node`, ports 443 and 80, its link on UDP 7442). Point `osada.example.org` at the
machine (A, and AAAA for its IPv6). Within a minute or two of DNS answering, the edge has a Let's
Encrypt certificate: open `https://osada.example.org/` and sign in as `admin`, with the password from

    cat /var/lib/osada/node/first-admin

(change it on the account page). Before that, the pages are reachable over SSH: `ssh -L
8096:127.0.0.1:8096 root@<the machine>`, then `http://localhost:8096/`.

### More machines

On the first node, for each new one:

    osada-setup token --name <its name> --at <its address>

It prints the token and the line to run on the new machine, such as

    osada-setup node --name edge2 --token osada-join.… --edge --firewall

The new node waits; the first node dials it within seconds and lets it in (nothing to forward on
a router), and keeps that link from then on. An edge gets a copy of every certificate by itself;
add its addresses to the public name's DNS (admin → dns shows what is missing). For a node that
the first node cannot reach but that can reach the first node: `token` without `--at`, then on the
node `osada-setup node … --peer <first node's name>@<its address>:7441` (UDP 7441 open there).

The first node's pages straight on a local network (plain http, no edge between):
`osada-setup first --http 0.0.0.0:80 …`. A first node without an edge of its own: an edge on
another machine (above), DNS, then on the first node

    osada-setup cert --names osada.example.org

(or admin → nodes → certificates). The first name of the first certificate becomes the instance's
name, `OSADA_PUBLIC_URL` and `OSADA_OIDC_ISSUER` in `/etc/osada/osada.env` (`first
--public-name` sets them too): its address in links and federation, its UPN domain, its sign-in
issuer, and the header's `osada // <instance>`.

A new cluster starts with the OUs `users`, `web` (where new sites go) and `services`, under the
organisation; admin → IdP makes more.

## Mail

Mail is [Stalwart](https://stalw.art) 0.16, on one node the internet reaches on 25, 465, 587, 993
and 443 (an edge). On that machine:

    dnf install -y osada-mail        # Debian, Ubuntu: apt-get install -y osada-mail
    osada-setup mail --firewall

That sets it all up:

- Stalwart (`/opt/stalwart`, service `stalwart`, its recovery administrator in
  `/opt/stalwart/etc/stalwart.env`), its web listeners on 127.0.0.1 (8080, 8443: the edge has
  443 and passes `mail.<domain>`'s TLS on to it);
- the domain (the instance's name, or `--domain example.org`), the server's name
  (`mail.<domain>`, or `--host`), DKIM (Stalwart's keys, rotated by it), and Let's Encrypt for the
  server's name (tls-alpn-01 through the edge);
- the webmail at `https://mail.<domain>/maildot/`, signing in through osada (Stalwart's OIDC
  directory; the client `maildot` appears in admin → sign-in for apps). Mail apps (IMAP, SMTP,
  CalDAV, CardDAV) then use app passwords, which each person makes and revokes on their account
  page (the "mail programs" window). `--no-oidc` keeps Stalwart's own passwords;
- the mail instance in the cluster (admin → mail), with this node's credential for Stalwart;
- the DNS records to publish: A/AAAA of `mail.<domain>`, MX, SPF, DKIM, DMARC and the apps' SRV
  records, printed and kept in `/opt/stalwart/etc/dns-records.txt`. Set reverse DNS (PTR) of the
  machine's addresses to `mail.<domain>` at your provider too.

Then, in admin → mail:

1. **new mailboxes**: the address a new mailbox gets (`{user}` is the account's name,
   e.g. `{user}@example.org`, or `{user}@{user}.mail.example.org` for a domain each), whether
   osada makes a domain Stalwart lacks (the mailbox its catch-all), the default size, and whether
   every new account gets one.
2. **mailboxes**: osada keeps the mailboxes; Stalwart is made to match within a minute
   (or at once: "sync now"). "new mailbox", or "make one" on a user's card, makes one; click a
   mailbox to change its address (the account is renamed, its mail stays), its other addresses
   (one a line; `*@domain` makes it that domain's catch-all), its size, turn it off, or remove it
   (keeping Stalwart's account, or with its mail: type the address). Accounts Stalwart already
   had are shown as "Stalwart only" until adopted (a user's card, or "adopt" for those the page
   can match to a user): adopting changes nothing in Stalwart, and osada takes their other
   addresses and catch-alls from it first.

Many providers block port 25 until asked; check yours. An existing Stalwart works as well: put
an API key of its (or `Basic <base64 of user:password>`) in `<node folder>/mail/stalwart.auth`,
owned by the node's account, mode 0600. A script that made Stalwart's accounts before (such as
a sync timer) should be stopped once the mailboxes are adopted: osada does it now.

## How the nodes find each other

- `token --at` puts the new node's address on the first node's own dial list (`osada dial
  --data /var/lib/osada/node` shows it; `--add`, `--remove` change it). The first node keeps
  dialing it, invites the new node there for the join, and keeps the link afterwards.
- The address a node was reached at becomes its **link** in node control (admin → nodes → its
  card), and for an edge also its public IPv4. Every node keeps a link to every node that has
  a link: the edges reach each other directly (certificates, sites on another node).
- `--peer name@host:port` makes a node dial that address itself (as the first node's
  `OSADA_PEERS`).

## The edges, files and logs

- **What the edges keep.** Every edge answers osada's own pages and their static files from
  its copy (refreshed in the background), so the site is quick even when the first node's line is slow;
  while the first node cannot be reached the page says so and keeps trying. Give an edge the roles
  `cache` (copies of files people fetch through it) and `public-cache` (also copies of new public
  links, fetched before anyone asks); its card sets how many GB it keeps. A file under 1 MB comes
  from the first node; up to 250 MB from another edge that has it, if one does; above that from whichever
  of the first node and the edges is quickest and least busy (each link is measured once a week).
- **Where an edge keeps copies.** On a node's card, its drives: "keep files here" on a mounted
  volume of its own. The node's root agent makes `<volume>/osada-<node id>`, lets the service
  write there and restarts it. Home's own files stay in its data folder.
- **Log shipping.** Each node sends its log lines to the logs authority in batches; a node's
  card sets the batch (500 lines by default: about 1,900 lines a second; more for a busy node).
- **The map and what is exposed.** Admin → nodes draws the nodes and their links (round trip,
  what each carries over the last minute and 10 minutes). Its "exposed" column, and a node's
  card, show what listens beyond loopback on each node and what another node could reach at its
  public addresses (checked every hour, or "check now"): osada's own, and the rest (sshd, mail).

## Federation

Every account is also `<name>@<UPN domain>`; the UPN domain is the instance's name unless set in
admin → federation. Instances federate over `https://<instance>/.well-known/osada-federation`:
people find people of the other instance by their UPN in the share window and share with them
(view only). Admin → federation:

- **closed** (the default): only the instances listed there, each side adding the other;
- **open federation** (the switch there): every osada instance, but those on the blocklist that
  then shows instead of the list; blocking a name blocks its subdomains too. An instance is only
  believed for UPNs of its own name or a domain above it.

## Chat

Everyone signed in has chat (the header, the portal): rooms with the people they pick, and
direct messages. Messages, edits and who is typing show at once on the other side; unread
messages show in the header, the page's title and the portal. A room is kept by every node with
the role `store`.

## Signing in from outside

An account's **outside networks** (admin → users) are the networks it may be used from through
an edge: CIDR blocks or AS numbers. **Empty means from anywhere**, which is how new accounts
start. Give an account networks to limit it to them. Repeated failed sign-ins get the address
banned on every edge (admin → logs → rules: `osada-login`, `osada-scan`). For administrators,
turn on a second factor (account page: TOTP, or a security key).

## Certificates

One node runs ACME (Let's Encrypt, http-01) for each certificate: the **issuer**, an edge whose
port 80 the names reach; the other edges get copies over their links and answer the
challenges for it too. Admin → nodes → certificates lists them with their state (issued when,
on how many nodes). The issuer tries within a minute of a change; after a failure, again in an
hour or when the certificate changes (its names, issuer or copies). Why it failed:
`journalctl -u osada-node` on the issuer (Let's Encrypt's own words).

## Upgrading

On every node, the edges first and the first node last:

    dnf upgrade osada                                    # Debian, Ubuntu:
    apt-get update && apt-get install --only-upgrade osada

(the services that run are restarted on the new program; `osada-setup upgrade` restarts them
again and shows their state). From a tarball: `./osada-setup upgrade` in the new one.

## Where things are

| what | where |
|---|---|
| the program | `/usr/bin/osada` (the package; a tarball: `/usr/local/bin/osada`), `osada-setup` |
| first node: settings, data | `/etc/osada/osada.env`; `/var/lib/osada` (files, `shelf.db`, `node/`) |
| first node: its pages | `OSADA_BIND` in osada.env: `127.0.0.1:8096`, or what `--http` gave (e.g. `0.0.0.0:80` for a local network) |
| first node: services | `osada`, `osada-web-agent` (as root: containers, disks) |
| other nodes: data, services | `/var/lib/osada-node/node`; `osada-node`, `osada-node-web-agent` (settings in their `ExecStart`) |
| logs of the programs | `journalctl -u osada`, `-u osada-node`, `-u osada-web-agent`, `-u osada-node-web-agent` |
| the dial list | `runuser -u osada -- osada dial --data /var/lib/osada/node` |
| a lost administrator | `runuser -u osada -- env $(grep -v '^#' /etc/osada/osada.env \| xargs) osada rescue <name> --admin` |

The header of every page reads `osada // <instance>`; hovering it shows the node you are connected
to and, when an edge relays, the one that answers (admin → nodes marks it: "you are connected here").

`osada-setup` never deletes anything; running it again keeps existing settings and units (a
new one is written beside as `.new`). `--dry-run` shows what it would do.

## SELinux

osada's services run unconfined (`unconfined_service_t`); on Rocky 9 and 10, enforcing, that gave
no denials. (Debian and Ubuntu have no SELinux; their AppArmor leaves osada alone.) A module of osada's own (osada running confined) is not part of the packages yet.

## When something does not work

- **The join waits.** On the first node, `journalctl -u osada` says `dialing edge at …`: the
  new node's UDP port (7441, or 7442) is not reachable from the first node. Check the
  provider's firewall. A token is good for an hour; make a new one with the same `--at`.
- **No certificate.** DNS must point the names at the edges (all of them, if several);
  `journalctl -u osada-node` on the issuer has Let's Encrypt's reason; `systemctl restart
  osada-node` there tries again at once.
- **Locked out.** `osada rescue` (above) prints a new password.

## Removing

    systemctl disable --now osada osada-web-agent osada-node osada-node-web-agent
    rm -f /etc/systemd/system/osada{,-web-agent,-node,-node-web-agent}.service; systemctl daemon-reload
    rm -rf /var/lib/osada /var/lib/osada-node /etc/osada   # the data too
    dnf remove osada osada-mail stalwart   # apt-get remove osada osada-mail stalwart; a tarball: rm /usr/local/bin/osada
    userdel osada; userdel osada-node

and close the ports `--firewall` opened (`firewall-cmd --permanent --remove-port=…`; with ufw,
`ufw status numbered` lists them, marked `osada`, and `ufw delete <number>` removes one).
