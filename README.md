# osada

Self-hosted files, wiki, sign-in and groupware for a household or an organisation, in one Rust
program: on one machine, or spread over several (a **first node** that keeps the data, at home
or anywhere, and **edges** with public addresses that answer https and relay to it over QUIC).

- **Files**: folders per user and per group, versions, sharing with people (by name or UPN,
  `name@your-domain`) and by public links, sync for the phone app, an editor for text and
  office documents (LibreOffice in the browser), pictures, previews of many formats.
- **Wiki** in several languages, with a built-in guide (`docs/wiki/osada`).
- **Sign-in**: accounts, groups, organisational units and roles; TOTP and security keys; an
  OpenID Connect provider for other programs (webmail, mail clients, your own apps); sign-in
  from outside limited per account to networks you name; fail2ban-like bans on every edge.
- **Chat**: rooms and direct messages, live.
- **Mail** through [Stalwart](https://stalw.art): osada keeps the domains and mailboxes, every
  account's UPN is an address of its mailbox.
- **Sites**: static sites and containers (podman) with their own names and Let's Encrypt
  certificates, served by the edges.
- **Federation** (protocol 1, `docs/FEDERATION.md`): instances that add each other share files
  across, with a public yes/no lookup of a single address and signed requests for the rest.
- **A cluster**: replicated logs per collection, node control (which node keeps what), links
  between nodes over QUIC with the cluster's own CA, edges that cache, logs shipped to one place.

## Install

Rocky Linux, AlmaLinux and RHEL 9 and 10, from the signed repository:

    curl -fsSL -o /etc/yum.repos.d/osada.repo https://repo.th3rumbl3.com/osada.repo
    dnf install -y osada
    osada-setup first --name home --edge --public-name osada.example.org --firewall

The whole guide, every way to start (one VM; a machine at home and an edge in the cloud; more
nodes), mail, certificates, upgrades: [packaging/INSTALL.md](packaging/INSTALL.md), also at
https://repo.th3rumbl3.com/. Other Linux systems (Debian 12, Ubuntu 22.04+, Fedora): the
tarball there, with the same program and `./osada-setup`.

## Build

Rust 1.89 or newer (edition 2024), a C compiler (SQLite is built in):

    cargo build --release          # target/release/osada
    cargo test --release --lib     # the unit tests, the cluster simulation among them

`target/release/osada serve` with `OSADA_DATA=<folder> OSADA_BIND=127.0.0.1:8096
OSADA_NODE=dev` starts a first node; the administrator's password is in
`<folder>/node/first-admin`. `osada --help` lists the commands (init, token, join, cluster,
rescue, dial, web, …). The packages: `packaging/build-repo.sh` (podman; Rocky 9 and 10 builds,
signed, the dnf repositories and their page).

## Layout

| where | what |
|---|---|
| `src/` | the node: cluster log, replication, node control, links, the web front, edges, sites, mail admin |
| `src/osada1/` | files, wiki, sharing, public links, OIDC provider, federation (the modules from 0.1) |
| `static/`, `static1/` | the pages (vanilla JS, no build step) |
| `web/zetaoffice/` | the office page (LibreOffice compiled to WebAssembly) |
| `packaging/` | `osada-setup`, the RPM specs, the repository's build, INSTALL.md |
| `deploy/selinux/` | an SELinux policy (not packaged yet) |
| `docs/` | ARCHITECTURE.md (the design), API.md, OIDC.md, FEDERATION.md, the wiki guide |
| `tests/` | integration tests: throwaway nodes driven over HTTP and through a headless browser |

## Licence

Copyright (C) 2026 th3rumbl3m4t0r

This program is free software: you can redistribute it and/or modify it under the terms of the
GNU Affero General Public License as published by the Free Software Foundation, version 3 of the
License ([LICENSE](LICENSE)). It is distributed in the hope that it will be useful, but WITHOUT
ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR
PURPOSE. Stalwart, which the `stalwart` package repackages, is its authors' work under the
AGPL-3.0.
