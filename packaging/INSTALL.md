# Installing osada

osada is self-hosted files, a wiki, sign-in (OpenID Connect) and groupware (chat, mail through
Stalwart, calendars through the apps) for a household or an organisation, on one machine or
spread over several: a **first node** that keeps the data, and **edges** with public addresses
that answer https and relay to it.

## The packages (Rocky Linux, AlmaLinux, RHEL 9 and 10)

As root, on every machine that is to run osada:

    curl -fsSL -o /etc/yum.repos.d/osada.repo https://repo.th3rumbl3.com/osada.repo
    dnf install -y osada

`dnf` asks once to trust the repository's key (`RPM-GPG-KEY-osada`, fingerprint on
https://repo.th3rumbl3.com/). The packages:

| package | what |
|---|---|
| `osada` | the program (`/usr/bin/osada`) and `osada-setup`, which makes a node: the first one, an edge, another node, certificates |
| `osada-mail` | mail: pulls in `stalwart` (the mail server, upstream's release in `/opt/stalwart`) and what `osada-setup mail` needs |
| `stalwart` | Stalwart, the mail and groupware server, as released upstream (AGPL-3.0) |

Nothing starts by itself: `osada-setup` (below) makes the node, its settings
(`/etc/osada/osada.env`) and its services. `dnf upgrade` brings new versions and restarts the
services that run.

**Other Linux systems** (Debian 12, Ubuntu 22.04 and newer, Fedora): the tarball
`osada-<version>-linux-x86_64.tar.gz` from https://repo.th3rumbl3.com/ has the same program
and `./osada-setup` (it puts the program at `/usr/local/bin/osada`); use `./osada-setup` where
this guide says `osada-setup`.

## Before you start

- **A public name** for osada, e.g. `osada.example.org`, whose DNS you can set.
- **Firewalls.** `--firewall` opens what a node needs in firewalld (Rocky's default on
  installed systems). Cloud images often have no firewalld but a firewall of the provider's
  (security group, cloud firewall): allow there, on every edge, TCP 80 and 443 and UDP 7441
  (UDP 7442 too on a VM that runs the first node and an edge). `osada-setup` says which.
- SELinux may stay enforcing: osada's services run unconfined.
- Sites with containers (admin → web) need `podman` on the node that runs them
  (`dnf install podman`).

## Where do you start?

| you have | go to |
|---|---|
| one VM in the cloud | **A** |
| a machine at home (behind a router) and a VM in the cloud | **B** |
| more VMs later, either kind | **C** |

The nodes: the **first node** (`home`) keeps the data (files, wiki, accounts, logs) and holds
the cluster's CA; **edges** have public addresses, answer the public names (https) and relay to
home over the node link (QUIC, UDP). Home never needs to be reachable from the internet: it
dials the edges.

### A. One VM in the cloud

    osada-setup first --name home --edge --public-name osada.example.org --firewall

Then point `osada.example.org` at the VM (A record, and AAAA for its IPv6). Within a minute or
two of DNS answering, the edge has a Let's Encrypt certificate: open
`https://osada.example.org/` and sign in as `admin`, with the password from

    cat /var/lib/osada/node/first-admin

(change it on the account page). The VM then runs two nodes: `home` (account `osada`,
`/var/lib/osada`, its pages on 127.0.0.1:8096 only) and `edge` (account `osada-node`,
`/var/lib/osada-node`, ports 443 and 80, its link on UDP 7442). Before the certificate is
there, the pages are reachable over SSH: `ssh -L 8096:127.0.0.1:8096 root@<the VM>`, then
`http://localhost:8096/`.

### B. Home at home, an edge in the cloud

On the machine at home:

    osada-setup first --name home --firewall
    osada-setup token --name edge --at <the cloud VM's public address>

The second line prints the token and the exact line to run on the cloud VM:

    osada-setup node --name edge --token osada-join.… --edge --firewall

The edge waits; home dials it within seconds and lets it in (nothing to forward on the
router). Home keeps that link up from then on. Then:

1. Point `osada.example.org` at the edge (A, AAAA).
2. On home: `osada-setup cert --names osada.example.org` (or admin → nodes → certificates).
3. `https://osada.example.org/`, sign in as `admin` (password: `cat
   /var/lib/osada/node/first-admin` on home). On the local network, the pages are also at
   `http://<home>/` (plain http on port 80; 8096 when something else had port 80).

### C. More nodes

- **Another edge** (a VM with a public address), on the first node:
  `osada-setup token --name edge2 --at <its address>`, then on it the printed `osada-setup
  node … --edge` line. It gets a copy of every certificate by itself; add its addresses to
  the public name's DNS (admin → dns shows what is missing).
- **A node nobody can reach** (at home behind a router) joining a cluster whose first node is
  in the cloud: on the first node `osada-setup token --name nas --roles store,blob,worker`
  (no `--at`), then on the node `osada-setup node --name nas --token … --peer
  home@<the first node's public address>:7441`: it dials the first node (UDP 7441 open there).

## Mail

Mail is [Stalwart](https://stalw.art) 0.16, on one node with a public address (the mail host,
usually an edge). On that VM, after its osada node is installed:

    dnf install -y osada-mail
    osada-setup mail --firewall

It installs Stalwart (`/opt/stalwart`, service `stalwart`, its recovery administrator in
`/opt/stalwart/etc/stalwart.env`), keeps its web listeners on 127.0.0.1 (8080, 8443: the edge
has 443) and gives the osada node on the VM its credential (`<node folder>/mail/stalwart.auth`,
0600). Then, in the admin pages:

1. **modules**: add one of kind `mail` on that node, host `mail.example.org`, upstream
   `tls://127.0.0.1:8443` (the edge passes https for that name on to Stalwart).
2. **mail**: add your domain; its card shows the DNS records Stalwart wants (DKIM, SPF, DMARC,
   MX). Point `mail.example.org` (A, AAAA) at the mail host.
3. **mail → new mailboxes**: the address a new mailbox gets (`{user}` is the account's name,
   e.g. `{user}@example.org`, or `{user}@{user}.mail.example.org` for a domain each), whether
   osada makes a domain Stalwart lacks (the mailbox its catch-all), the default size, and whether
   every new account gets one.
4. **mail → mailboxes**: osada keeps the mailboxes; Stalwart is made to match within a minute
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
  its copy (refreshed in the background), so the site is quick even when home's line is slow;
  while home cannot be reached the page says so and keeps trying. Give an edge the roles
  `cache` (copies of files people fetch through it) and `public-cache` (also copies of new public
  links, fetched before anyone asks); its card sets how many GB it keeps. A file under 1 MB comes
  from home; up to 250 MB from another edge that has it, if one does; above that from whichever
  of home and the edges is quickest and least busy (each link is measured once a week).
- **Where an edge keeps copies.** On a node's card, its drives: "keep files here" on a mounted
  volume of its own. The node's root agent makes `<volume>/osada-<node id>`, lets the service
  write there and restarts it. Home's own files stay in its data folder.
- **Log shipping.** Each node sends its log lines to the logs authority in batches; a node's
  card sets the batch (500 lines by default: about 1,900 lines a second; more for a busy node).
- **The map and what is exposed.** Admin → nodes draws the nodes and their links (round trip,
  what each carries over the last minute and 10 minutes). Its "exposed" column, and a node's
  card, show what listens beyond loopback on each node and what another node could reach at its
  public addresses (checked every hour, or "check now"): osada's own, and the rest (sshd, mail).

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

    dnf upgrade osada

(the services that run are restarted on the new program; `osada-setup upgrade` restarts them
again and shows their state). From a tarball: `./osada-setup upgrade` in the new one.

## Where things are

| what | where |
|---|---|
| the program | `/usr/bin/osada` (the package; a tarball: `/usr/local/bin/osada`), `osada-setup` |
| first node: settings, data | `/etc/osada/osada.env`; `/var/lib/osada` (files, `shelf.db`, `node/`) |
| first node: its pages | `OSADA_BIND` in osada.env: `0.0.0.0:80` at home (the local network, plain http), `127.0.0.1:8096` on a VM with a public address |
| first node: services | `osada`, `osada-web-agent` (as root: containers, disks) |
| other nodes: data, services | `/var/lib/osada-node/node`; `osada-node`, `osada-node-web-agent` (settings in their `ExecStart`) |
| logs of the programs | `journalctl -u osada`, `-u osada-node`, `-u osada-web-agent`, `-u osada-node-web-agent` |
| the dial list | `runuser -u osada -- osada dial --data /var/lib/osada/node` |
| a lost administrator | `runuser -u osada -- env $(grep -v '^#' /etc/osada/osada.env \| xargs) osada rescue <name> --admin` |

The header of every page shows the node you are connected to, then the one that answers when
an edge relays (`// edge → home`); admin → nodes marks it ("you are connected here").

`osada-setup` never deletes anything; running it again keeps existing settings and units (a
new one is written beside as `.new`). `--dry-run` shows what it would do.

## SELinux

osada's services run unconfined (`unconfined_service_t`); on Rocky 9 and 10, enforcing, that gave
no denials. A module of osada's own (osada running confined) is not part of the packages yet.

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
    dnf remove osada osada-mail stalwart   # or, from a tarball: rm /usr/local/bin/osada
    userdel osada; userdel osada-node

and close the ports `--firewall` opened (`firewall-cmd --permanent --remove-port=…`).
