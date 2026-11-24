# osada 0.2: architecture

Status: draft 0.2, 2026-11-15. The platform is osada, version 0.2.0, the successor of osada 0.1 (`/opt/osada`), which keeps running until 0.2 replaces it. The text says "the platform".
Licence: AGPL-3.0 (osada 0.1 was MIT).

Section 0 lists the decisions of 2026-11-15; the sections they change are amended in place.

This document records the decisions made so far, the design that follows from them, and the decisions still open. Sections 5 to 8 (collections, cluster, authority, replication) are the foundation; everything else sits on them.

---

## 0. Decisions of 2026-11-15

| # | Decision | Sections |
|---|---|---|
| A1 | The product is osada, this is version 0.2.0. One version number for everything; Cargo needs three parts. | header |
| A2 | Licence AGPL-3.0. | 22 |
| A3 | No node is in charge of everything. Authority is assigned per organisational unit and module: home (home) for everything osada 0.1 does today; the edge for mail once it moves in (possibly a new VM later). | 6.1, 7.2, 7.5, 17.2 |
| A4 | The directory is a tree of organisational units (OUs), folders as in a classic directory. Every object sits in one OU; authority, storage, durability and other policies are linked to OUs and inherited downwards. | 10.1, 10.6 |
| A5 | Storage pools managed by administrators behind one interface: a local directory on any drive, S3-compatible storage, SFTP, maybe IPFS. Placed per OU and changed without users noticing. | 9 |
| A6 | Users can still add their own SFTP storage, as in osada 0.1. | 12.3 |
| A7 | The nightly SFTP backup and the earlier versions taken from its snapshots stay as in osada 0.1. | 8.3 |
| A8 | Second factors are TOTP and FIDO2 (security keys, passkeys) only. | 10.2 |
| A9 | The wiki stays a core, built-in module, as now. Another module proves the extension API. | 13, 20 |
| A10 | Osada 0.1's in-browser viewers and editors stay. WOPI (OnlyOffice, Collabora) comes later, on top. | 1, 12.3 |
| A11 | Mail eventually replaces the Stalwart server; until then osada 0.1's mail sync API is kept. | 12.4 |
| A12 | File contents go into the blob store as chunks addressed by hash, not plain files on disk (D11 settled). | 9 |
| A13 | D2 follows from A3: one collection per account per module, so an account's files can be on home and its mail on the edge. | 5.1, 21 |
| A14 | What is the authority for what is switchable in the admin UI (node control): per domain (sign-in and directory, files, wiki, shared links, mail, ...) and per OU, within rules per node: always, may, never. home is the authority for sign-in and the directory. The edge is always the authority for mail and shared links, and never for the cluster log (the ledger) or files. | 6.4 |
| A15 | Node control also chooses which nodes are IdPs. The edge is one: it has its own signing key and a copy of the credentials, so people can sign in at the edge while home is down (D12 settled). | 10.2 |
| A16 | While an authority cannot be reached, another node takes writes for it as deferred writes and delivers them when it is back: applied as they are when nothing changed there; otherwise the authority's version stays and the deferred one is kept as a conflicting version, which the user can merge. | 7.7 |
| A17 | Authentication logs carry session ids that tie every later sign-in, refresh and token to the first login. Retention and verbosity are switches per log in the admin UI. A token that lives longer than 90 % of the authentication log's retention always cautions its user. | 16.3 |
| A18 | Licence details are left for later. The release will be public and must credit the author, th3rumbl3m4t0r. | 22 |
| A19 | Where secrets are kept is set per OU: for some accounts on the edge, for others only on named nodes (tiers, as in a red forest). Only nodes that keep an OU's secrets sign its tokens; sign-in can be proxied through the others, which store nothing. | 6.4, 10.7 |
| A20 | Tier 0 is a flag an administrator turns on for an OU; nothing is tier 0 unless turned on. Accounts in a tier-0 OU and below must sign in with FIDO2; TOTP does not count for them. Outside tier 0, a second factor is required only where an OU turns it on. These rules only tighten down the tree. | 10.2, 10.6, 10.7 |
| A22 | OUs are addressed by full path (`/EMEA/DE`) and shown as a tree. Rights are roles (RBAC, predefined: admin, infrastructure-admin, security-admin, user-admin, ou-admin, auditor, reader, ediscovery-reader) granted to an account or a group (a function) over an OU and everything below it; data follows the OU of its account or space, so an eDiscovery reader over `/AMER/US` reaches all of it there. | 10.5, 10.6 |
| A23 | Every log event carries the session it came from; account creation and password changes are separate events (admin log, auth log). Logs are read through a query builder over all nodes; their settings sit above it. | 16.3 |
| A24 | Every module runs as named instances (`mail-eu`, `wiki-lab`): several of a kind may run in one deployment, each on a node of its own, under the node's rule for the kind. Users get access to instances, directly or through groups; the users table shows "member of" and "access to", managed on the user card or by mass updates (picked users or everything a filter finds, run as jobs on the node). The per-OU domain links keep placing the directory; for module kinds they become "the instance accounts in this OU use by default" once the first module exists. | 12, 10.5 |
| A21 | The edge node will be the host the Stalwart mail server runs on now (mail.example.org, Rocky 10), not edge1 (Rocky 9), which is the edge of osada 0.1. More will change when it is set up. | 17.2 |

---

## 1. Goals and non-goals

### Goals

1. **One binary.** Every node runs the same static executable. What a node does is decided by configuration (roles), never by a different build.
2. **One node is a complete deployment.** A single VPS runs every feature with no external database, cache, queue or search service.
3. **The same design from 1 node to 100 000 users.** Growth means adding nodes and assigning roles. It never means migrating to a different architecture.
4. **Split deployments are first class.** A small always-on public node plus a large home node behind NAT is a supported topology, not a hack.
5. **Uniform patching.** A security fix is one new binary rolled across all nodes with the same procedure everywhere.
6. **Few dependencies.** On the server, a small audited crate set. In the browser, vanilla JS with no framework and no build step.
7. **Extensible under suspicion.** Third-party modules can be loaded, and are given nothing they have not been explicitly granted.

### Non-goals (for now)

- Windows domain join and group policy. Deferred.
- Outlook compatibility (MAPI, EWS, ActiveSync).
- A full office suite. Osada 0.1's in-browser viewers and editors stay (A10); heavier editing goes to an external web office suite through WOPI, later.
- Multi-tenant hosting of unrelated organisations in one cluster. One deployment serves one organisation or household. (Open decision D8.)

### Scale target and where the cost goes

Estimates for 100 000 users, to keep the design honest:

| Load | Estimate | Consequence |
|---|---|---|
| Mail | ~10 million messages/day, ~115/s average | Small. A few nodes. |
| Idle connections | A few hundred thousand (IMAP, WebSocket) | A few GB of RAM in total. |
| Directory | 100 000 users, some hundreds of MB | Fits in memory on every node. |
| Storage | 10 GB/user = 1 PB before replication | The dominant cost. Not solvable by architecture. |
| Call media | 10 000 concurrent video participants ≈ 15 Gbit/s | Needs elastic relay nodes. |

The claim the platform makes is "little compute", not "little storage".

---

## 2. Principles

- **A single node is a cluster of one.** There is no "standalone mode" with different code paths.
- **Single writer per collection.** Every piece of mutable user data has exactly one authority node at a time. No multi-master conflict resolution.
- **The log is the source of truth.** Tables, indexes and search are derived and can be rebuilt.
- **Same binary, different privilege.** A process holds only the secrets and access its role needs.
- **Default deny for anything that is not core.** Extensions declare what they need and get nothing else.
- **Native model first, compatibility as front ends.** The data model is JMAP-shaped. IMAP, CalDAV, CardDAV and WebDAV are translators in front of it.
- **Every release ships everything.** One version number for the whole binary and all built-in modules.

---

## 3. System overview

```
                 clients
   browser · mail apps · calendar apps · sync clients
                    │
   ┌────────────────┴─────────────────────────────────┐
   │ protocol front ends                               │
   │ HTTP (JMAP, WebDAV, CalDAV, CardDAV, OIDC, SAML) │
   │ SMTP · IMAP · LDAP (read-only)                    │
   ├───────────────────────────────────────────────────┤
   │ built-in modules                                  │
   │ identity · directory · chat · calendar · contacts │
   │ files · mail                                      │
   ├───────────────────────────────────────────────────┤
   │ core services                                     │
   │ authn/authz · collections · blobs · search        │
   │ events/push · jobs · audit · config               │
   ├───────────────────────────────────────────────────┤
   │ cluster                                           │
   │ cluster log (Raft) · placement · node links       │
   ├───────────────────────────────────────────────────┤
   │ local storage                                     │
   │ embedded key-value store · blob directory         │
   └───────────────────────────────────────────────────┘
        ▲                                   ▲
        │ narrow brokered API               │ node links
   extension modules                    other nodes
   (sandboxed: wiki, Matrix, ...)
```

---

## 4. The binary: roles and processes

### 4.1 Roles

A node enables one or more roles in its config file. A single-node deployment enables all of them.

| Role | Does | Holds state | Notes |
|---|---|---|---|
| `voter` | Member of the cluster log (Raft) | Yes | 1, 3 or 5 per cluster |
| `store` | Authority or replica for collections | Yes | Metadata and logs |
| `blob` | Stores blob chunks | Yes | Can be combined with `store` or separate |
| `idp` | Issues tokens, holds signing keys | Keys only | Small number of trusted nodes |
| `edge-http` | Serves web UI and HTTP protocols | No | Scales out freely |
| `edge-mail` | SMTP in and out, IMAP | Queue only | Needs a public address with clean reputation |
| `worker` | Parsing, indexing, scanning, previews | No | Sandboxed, no keys |
| `modhost` | Runs extension modules | No | Sandboxed, brokered access |
| `media` | Call relays (later) | No | Needs UDP and public addresses |

### 4.2 Process model on one host

One binary does not mean one process. Following OpenBSD-style privilege separation, the binary re-executes itself into several processes with different restrictions:

```
supervisor    starts and restarts children; no network, no data access
├─ core       tokio runtime: listeners, routing, collection logs, replication
├─ keyholder  holds signing keys; answers "sign this" requests and nothing else
├─ worker ×N  untrusted-input parsing, indexing, scanning; no network, no keys
└─ modhost ×N WASM runtime for extension modules; brokered access only
```

- It is still one systemd unit, or one container.
- Children drop privileges at start using what the kernel offers (seccomp filters, Landlock, namespaces). Inside restricted containers some mechanisms are unavailable; the binary must report the sandbox level it actually achieved and refuse to run extension modules below a configured minimum.
- `keyholder` exists so that a compromise of `core` yields the ability to request signatures while the process is compromised, and not the keys themselves.

### 4.3 The core runtime

- Async runtime: tokio.
- HTTP: axum, as one listener among several. SMTP, IMAP and LDAP have their own listeners on the same runtime.
- TLS: one implementation for all listeners, with built-in ACME certificate issuance.
- Every listener gets authentication, rate limiting, audit and metrics from the core, not from the module behind it.

---

## 5. Data model: collections and logs

### 5.1 Collections

A **collection** is the unit of ownership, replication and authority.

| Collection type | Examples |
|---|---|
| Account | One user's data in one module: their files, their mailbox, their calendar, their contacts, their settings (one collection each) |
| Organisational unit | The directory objects of one OU (section 10.6) |
| Room | One chat room and its history |
| Shared space | A shared folder, a group calendar, a shared address book, a wiki space |
| Cluster | Membership, the OU tree and its policy links, placement, uniqueness, cluster configuration (see section 6) |

Each collection has: an id, a type, an authority node, an epoch, a replica set, a durability mode and a blob placement policy.

An account is one collection per module, so different modules of one account can have different authorities (A3, A13). Modules address data by collection id.

### 5.2 The log

Every collection is an append-only log of entries:

```
entry = (collection, epoch, seq, time, actor, module, operation, payload, prev_hash)
```

- `epoch` increases every time authority changes. `seq` increases with every entry.
- `prev_hash` chains entries, so two replicas can verify they hold the same history by comparing one hash.
- Only the authority node appends. Replicas receive entries in order and reject any entry from a stale epoch.

### 5.3 Derived state

- **Object tables and indexes** are materialised from the log into the local key-value store.
- **Sync state.** The opaque state string a JMAP client holds is `(epoch, seq)`. "What changed since X" is a log range scan.
- **Search indexes** are built by workers from the log and live beside the replica.
- **Compaction.** Old log entries are folded into a snapshot after a retention horizon (for example 30 days). Clients older than the horizon do a full resync. The horizon also provides undelete.

### 5.4 Operations across collections

There are no distributed transactions. An operation that touches two collections (deliver a local mail from A to B, move a file between two shared spaces, post an invitation into another user's calendar) is written as:

1. An entry in the source log that records the intent with an idempotency key.
2. A delivery job that applies the effect to the target collection through its authority, retrying until acknowledged.
3. The target ignores duplicates by idempotency key.

This is the only cross-collection mechanism. Modules must not invent their own.

### 5.5 Local storage

- One embedded ordered key-value store per node, keyed by collection prefix. Accessed through a storage trait so the engine can be swapped. (Open decision D1.)
- One blob directory per node (section 9).
- One data directory in total. Backup, move and restore of a node are operations on that directory.

---

## 6. Cluster

### 6.1 Cluster log

A small replicated log, run with Raft among `voter` nodes, holds everything that must be globally agreed:

- Node membership, roles and addresses.
- The placement map: for every collection, its authority, epoch and replica set. Most of it follows from the policies linked to OUs; explicit placements override them.
- The OU tree (ids, names, parents) and the policy links on each OU (section 10.6). The objects inside an OU are not here; they are in the OU's own collection.
- Global uniqueness: user names, mail addresses, aliases.
- Cluster-wide configuration and the active cluster version.
- Revocations.

With one voter, Raft degenerates to a local log. With three or five, the cluster log survives node loss. Every node, voter or not, keeps a read-only replica of the cluster log in memory, and of every OU collection, which is what makes directory lookups and routing local everywhere.

The cluster log must stay small and low-traffic. Per-user data never goes in it.

### 6.2 Node identity and joining

- `init` on the first node creates a cluster certificate authority, the first voter and the first administrator. The CA's key stays on that node (`tls/ca.key`, 0600; later in its keyholder).
- `join` on a new node uses a one-time token issued by an existing node, receives a node certificate and syncs the cluster log. The token (`osada token` on the CA's node, `osada-join.<secret>.<CA fingerprint>`) is for one named node and its roles, works once and runs out (an hour unless set). The new node keeps its private key: it connects without a certificate, trusts the other side only if its CA has the token's fingerprint, and sends its public key. The CA's node signs it and adds the node to node control in the cluster log, with the rules proposed for its roles.
- All node-to-node traffic is mutually authenticated with cluster certificates.
- A node that has just joined does not know node control yet; it takes the cluster log from the node it joined through until the cluster log names the voters.

### 6.3 Node links

- One long-lived, multiplexed, mutually authenticated connection per node pair, carrying replication, request forwarding and blob transfer.
- **Either side may initiate.** A node behind NAT dials out and keeps the link open; the public node uses the same link in the other direction. No inbound port is needed at home.
- Transport: QUIC (D4), one UDP port per node, ALPN `osada-node/1`, keep-alives every 10 s (which also keep a NAT's mapping open) and a 30 s idle timeout. A TLS-over-TCP fallback for networks that block UDP comes later, behind the same interface.
- Both ends present cluster certificates naming their node (as a DNS name). The dialer checks that it reached the node it meant; the accepting side checks that the dialer is the node its hello names. When two nodes dial each other at once, both keep the connection dialed by the node whose name sorts first.

### 6.4 Node control

What each node may do, and what it is the authority for, is set in the admin UI under node control (and with the CLI), and kept in the cluster log (A14).

- **Domains.** Authority is placed per domain: `directory` (the OU objects and the credentials: sign-in data), `files`, `wiki`, `links` (shared links), `mail`, `chat`, `calendar`, `contacts`; and `ledger`, the cluster log itself, whose authorities are the voters.
- **Node rules.** For every node and domain: `always` (the node is the authority for that domain everywhere, and OU placements do not apply), `may` (the default), or `never`. When a node with an edge role is added, node control proposes `never` for `ledger` and `files`; a single node that is everything (section 17.1) keeps `may`.
- **Placement.** For a domain without an `always` node: a cluster default, overridden per OU (inherited down the tree, section 10.6) and per collection. The UI shows, for every OU and domain, which node is the authority and why.
- **Roles.** Which nodes are voters, which are `idp` (sign tokens, keep credentials), `store`, `blob`, edge, worker.
- **Secrets.** Per OU, which `idp` nodes keep its credentials and sign its tokens; the others proxy its sign-ins (section 10.7).
- Every change is checked as a whole before it is committed: no authority on a node whose rule is `never` or that has no `store` role, at most one `always` per domain, at least one voter and one `idp`, no OU link that an `always` rule or an enforced link above makes pointless. A change that moves authority starts planned handovers (section 7.3), whose progress the UI shows.

In this deployment home is the voter and the authority for `directory`, `files` and `wiki`; the edge is `always` for `mail` and `links`, `never` for `ledger` and `files`, and an `idp` that keeps the secrets of ordinary accounts; tier-0 OUs keep theirs on home or on nodes of their own.

---

## 7. Authority

### 7.1 Request routing

Any node can accept any client request.

- **Writes** are forwarded over a node link to the collection's authority, found in the local copy of the placement map.
- **Reads** are served locally if the node holds a replica at least as new as the state token the client presents; otherwise they are forwarded. This gives read-your-writes without always going to the authority.
- **Push.** Edge nodes subscribe to the collections their connected clients care about and fan changes out over WebSocket.

### 7.2 Leases and fencing

- The authority holds a time-bounded lease, renewed through the voters.
- A node whose lease has lapsed must stop accepting writes, even if it believes it is healthy.
- Every append carries the epoch. Replicas and the cluster log reject anything from an older epoch. This is what makes a slow or partitioned old authority harmless.
- **No automatic failover, no lease.** Where a collection's placement allows no automatic failover (one voter, section 7.5), its authority needs no lease and keeps accepting writes while the voters are unreachable. Authority then changes only by an explicit `promote`, which raises the epoch; whatever the old authority wrote after that is a divergent tail (section 7.4). This keeps mail on the edge working while home is down (A3).

### 7.3 Planned handover

Used by rebalancing, by node drain during upgrades and by administrators.

1. A transfer intent for collection C from node A to node B is committed to the cluster log.
2. B becomes a replica if it is not one, and catches up to near the head.
3. A stops accepting writes for C and reports its final sequence number. Clients see a short retry.
4. B confirms it holds everything up to that number.
5. The cluster log commits "authority of C is B, epoch + 1".
6. A becomes a replica. Edge nodes pick up the new placement and retry in-flight writes against B.

No acknowledged write can be lost in a planned handover, in any durability mode.

### 7.4 Unplanned failover

1. A's lease expires and the voters agree A is unreachable.
2. The voters fence a majority of the collection's members at the new epoch, so that none of them takes anything more from A, choose the most caught-up of those, and commit the new epoch with it as authority. (With fewer than a majority reachable, only an explicit `promote` can go on, and `sync` writes may then be lost.)
3. When A returns, any entries it holds beyond what the new authority had are a divergent tail. They are cut off, kept in a recovery area, and re-applied automatically where that is safe (for example a received mail) or presented to an administrator where it is not.

Whether step 3 can ever find a divergent tail depends on the durability mode (section 8).

### 7.5 The two-node rule

Two nodes cannot fail over automatically without risking both believing they are in charge. Supported choices:

- **One voter.** One node is the only voter. Authority is still per OU and module, so the other node can be the authority for some things (A3); those keep working while the voter is away (section 7.2), but nothing that needs the cluster log does (new accounts, renames, placement changes). Losing the voter for good is recovered by an explicit `promote` on a surviving node.
- **Three voters.** Add a third, very small node as a witness that holds only the cluster log. Recommended once a third machine exists (the mail VM of A3).

### 7.6 Rebalancing

A background job on the cluster-log leader, run daily.

- **Input:** per-collection access statistics by entry node, decayed over one to two weeks.
- **Goal:** place authority on a `store` node near where the collection is mostly used. For shared collections, weight by participant activity.
- **Constraints, in priority order:** residency pins (an account pinned to a region never leaves it), node capacity, maximum moves per run.
- **Damping:** a collection moves only if the predicted gain exceeds a threshold on several consecutive runs, so a week of travel does not move an account.
- The job only produces transfer intents. The mechanism is always the planned handover above.

### 7.7 Deferred writes while the authority is away

A node that cannot reach a collection's authority can still take writes for it, so that work goes on at the edge while home is down (A16). It is the one case where a node other than the authority takes a write, and it never writes to the collection's log itself.

- Only where the module allows it (files and wiki first) and the node holds a replica of the collection. Directory, permission and policy changes wait until the authority answers.
- The write goes into the node's **outbox**, a collection of its own: the operation, the version of the object it was made on (its base), the new content (chunks in the node's own pool), the actor and session id, an idempotency key. The user sees the change at once, marked as not yet at home.
- When the authority answers again, the outbox is delivered in order, each operation as a write conditional on its base (section 5.4):
  - **changed only on the deferring node:** applied as it is (written through);
  - **changed on both:** the authority's version stays, and the deferred one is kept as a **conflicting version** of the object (of a file: a version marked as a conflict, with the node and time; of a wiki article: a conflicting revision). The user is told, and can compare them, keep either, or merge them;
  - **removed on one side, changed on the other:** nothing is lost. The changed version is kept, as the object or as a conflicting version, and the user decides.
- Delivery is retried until acknowledged and is idempotent. Each outbox entry is closed with its outcome, which is logged (`node` log, section 16.3).

---

## 8. Replication, durability and backup

### 8.1 Durability modes

Set per collection type, overridable per collection.

| Mode | Write is acknowledged after | Loss on authority failure |
|---|---|---|
| `local` | Local durable write | Everything since last backup, if the disk is lost |
| `async` | Local durable write; replicas follow | The unreplicated tail (typically seconds) |
| `sync` | A majority of the collection's members have it durably (of three: the authority and one replica) | None |

Defaults: `local` on one node, `async` on two, `sync` once there are three or more `store` nodes.

### 8.2 Mail acceptance

Inbound SMTP only confirms receipt to the sending server after the message is durably queued on the edge node. Delivery into the account collection is then a retried job. Mail is therefore never lost between acceptance and the authority, even when the authority is briefly unreachable.

### 8.3 Backup

Replication is not backup: a deletion replicates.

- **Retention horizon.** Log compaction keeps deleted data recoverable for a configured period.
- **Snapshots.** A consistent checkpoint of the local store plus continuous log shipping to a backup target (a directory, another node, or S3-compatible storage).
- **Restore granularity.** Because each collection is a log, a single account or room can be restored to a point in time without touching others.
- **Nightly SFTP backup, as in osada 0.1 (A7).** At a set time, and once when the UPS leaves line power, a complete snapshot goes to an SFTP server: a consistent copy of the store, the keys, the binary and its configuration, and the blobs. Content already in a kept snapshot is hard-linked instead of sent. Three snapshots are kept (the newest, one up to a week older, one one to four weeks older); pruning waits for line power. `RESTORE.md` beside them says how to rebuild.
- **Earlier versions, as in osada 0.1 (A7).** Users look through the snapshots and put back files and folders from them, each checked against the snapshot's manifest.

---

## 9. Blob storage

- Message bodies, attachments and file contents are split into chunks, stored by content hash and referenced from log entries (D11). Metadata and blobs replicate independently.
- **Storage pools (A5).** Administrators define pools: a local directory on any drive, S3-compatible storage, an SFTP server, IPFS maybe later. A local or SFTP pool belongs to a node; S3 and IPFS can be reached by several. All of them store content by hash behind one trait.
- **Placement policy**, linked to an OU (inherited) or set on a collection, decides which pools hold which content, for example "everything on home's big disk; the last 30 days and anything pinned also on the edge". Administrators change it at any time; content moves in the background and users notice nothing.
- **Unavailable blobs are a normal state.** When the node holding a cold chunk is offline, metadata is still served and the client shows the item as temporarily unavailable. Modules must handle this case explicitly.
- **Backends** behind one trait: local directory first, then SFTP and S3-compatible storage, maybe IPFS.
- **Encryption at rest** with per-collection keys wrapped by a cluster key, so chunks can sit on storage the operator trusts less than the node itself. (Open decision D7.)
- **Garbage collection** by reference tracking from the logs, delayed by the retention horizon.

---

## 10. Identity and directory

Identity and directory are tier zero: every other module depends on them, and they are the first things built.

### 10.1 Directory

- Objects: users, groups, domains, devices, OAuth/SAML clients, service principals (one per module), roles.
- Organised in organisational units (section 10.6). The objects of each OU are a collection with its own authority; every node replicates all of them and holds them in memory. Lookups never leave the node.
- Credentials (password hashes, TOTP secrets, FIDO2 public keys) are a separate collection per OU, replicated only to the nodes its secrets policy names (section 10.7).
- Writes go to the OU's authority. Whatever must be unique across the cluster (user names, mail addresses, aliases) is reserved in the cluster log first, through the voters. Such writes are rare, so the latency of consensus is acceptable even across regions.

### 10.2 Authentication

- Passwords with a memory-hard hash (argon2, as in osada 0.1). Second factors are TOTP and FIDO2 only: security keys and passkeys, through WebAuthn. A FIDO2 credential with user verification may also sign in on its own. No SMS, mail or push factors (A8). Nothing beyond a first factor is required unless an OU turns it on; tier-0 OUs require FIDO2 (section 10.7, A20). ID tokens say which factors a sign-in used (`amr`).
- **TOTP (done).** The secret is a credential (kept where the OU's secrets are). A sign-in that needs a second factor goes in two steps: the password, then a ticket (five minutes, five tries) for the code. An account without TOTP in an OU that asks for a second factor sets it up during that sign-in (the secret and its QR code shown once, kept when its first code is right), so turning the rule on locks nobody out. A code never counts twice: each sign-in records the step it used, and the node reads that back at start. Users set TOTP up and take it off on their account page (not while their OU asks for it); administrators with password-set rights reset it.
- App-specific passwords for protocols that cannot do modern authentication (IMAP, CalDAV, CardDAV, WebDAV clients).
- Access tokens are short-lived and signed. Any node verifies them locally with public keys from the cluster log. Refresh tokens are held by `idp` nodes only.
- Every `idp` node has a signing key of its own, held by its keyholder. All of them are published together (JWKS), so a token from either node is good everywhere, and one node's key can be withdrawn on its own (A15).
- An `idp` node that keeps an OU's credentials (section 10.7) can sign its people in while the directory's authority is away. What a sign-in changes (TOTP steps used, FIDO2 signature counters, failed attempts, sessions) is written to that node's own collection and merged into the directory's view: counters by maximum, sessions per node.
- Revocation: short token lifetime plus a revocation list in the cluster log.
- Signing happens in the `keyholder` process.

### 10.3 Protocols

| Protocol | Purpose | When |
|---|---|---|
| OIDC / OAuth 2 | Single sign-on for the platform's own UI and for external apps | First |
| SCIM | Provisioning users and groups from or to other systems | Before public release |
| SAML | Single sign-on for applications that do not speak OIDC | Before public release |
| LDAP, read-only | A directory view for legacy applications | Before public release |

### 10.4 Linux hosts

Linux machines authenticate against the platform through a small agent, shipped separately from the server binary because it consists of system plug-ins:

- PAM and NSS modules for login and user/group resolution.
- An SSH certificate authority, so servers trust short-lived certificates instead of static keys.
- Centrally managed sudo rules.
- An offline cache so a laptop can log in without network.

### 10.5 Authorisation

One model for every module.

- Permissions are relationship tuples `(subject, relation, object)`, for example `(group:finance, editor, space:budget)`.
- Tuples for a collection live with that collection; group membership comes from the directory. Any node holding both can answer a permission check locally.
- Modules never implement their own access control. They call the core check.
- Extension modules are subjects too: a service principal with explicitly granted tuples.
- **Roles over OU subtrees (A22).** In practice the tuples are role grants: `(subject, role, ou)`, where the subject is an account or a group (a function, such as "EMEA helpdesk") and the grant covers the OU and everything below it. A grant is kept in that OU's directory collection, so it replicates with it; what someone may do in an OU is the union of the roles granted to them and to their groups on the way from the root down. Predefined roles: `admin` (everything), `infrastructure-admin` (node control, handovers, settings), `security-admin` (OU security, locking accounts, the logs), `user-admin` (users, groups, passwords), `ou-admin` (OUs below and where their domains go), `auditor` (directory and logs), `reader` (directory), `ediscovery-reader` (directory and the accounts' data). Cluster-wide rights (node control, handovers, settings) only come from a grant at the root. One can only grant a role whose rights one holds there oneself.
- **Data follows the OU.** An account's collections (files, mail, calendar, …) take the account's OU, a shared space its own; so a role over `/AMER/US` reaches everything there, which is what an eDiscovery case needs.
- **Node control changes are checked part by part**: the nodes, the defaults and the default secrets need cluster rights; a new OU, a rename or new links need `ou-manage` there; tier 0, the second factor and the secrets need `ou-security` there.

### 10.6 Organisational units

The directory is a tree of organisational units (OUs), like the folders of a classic directory (A4).

- Every directory object (user, group, device, OAuth/SAML client, service principal, shared space) sits in exactly one OU. The root OU is the organisation. Objects can move between OUs; their ids do not change.
- An OU has a path for people (`/EMEA/DE`; the root is `/`) and a permanent id for everything else. Names are unique among siblings and have no `/`. The admin UI shows the OUs as a tree.
- **Policies are linked to OUs** and inherited down the tree. The nearest link wins; a link can be marked enforced so that OUs below cannot override it. What can be linked:
  - authority and replicas per module, e.g. "under /home, files on home, mail on the edge" (A3);
  - storage placement (section 9) and durability mode (section 8.1);
  - sign-in rules: required second factor, tier 0 (section 10.7), outside networks (CIDR blocks and AS numbers, as in osada 0.1). The security rules only tighten down the tree: once an OU requires a second factor or is tier 0, every OU below it does too, and no OU below can turn it off;
  - storage limits, and defaults for new accounts.
- An account's collections follow the policies of the user's OU unless placed explicitly. Moving a user to another OU can change where their data lives; that is a planned handover (section 7.3), never a copy by hand.
- An OU's own objects are a collection whose authority is linked like any other: one node is authoritative for a folder. The tree itself (OU ids, names, parents) and the policy links are in the cluster log, because every node needs them to route.
- **Delegated administration.** An administrator role is a tuple on an OU, `(user:anna, admin, ou:/home/family)`, and covers everything below it (section 10.5).
- LDAP (section 10.3) shows OUs as `ou=` entries.

### 10.7 Where secrets are kept

Administrators decide per OU which nodes keep the credentials of its accounts (A19), like the tiers of a red forest: ordinary accounts can have theirs on the edge, so they sign in there while home is down; administrators (tier 0) only on a few hardened nodes.

- An OU's **secrets policy** is a set of `idp` nodes, linked to the OU and inherited like other links (the nearest wins, an enforced link above decides); the cluster default applies where no OU sets one. The OU's credentials are replicated to those nodes only, and the OU's directory authority, which takes credential changes, must be one of them.
- **Signing follows the secrets.** Only a node in the set signs tokens for the OU's accounts, and every node checks this when it verifies a token: the key must belong to a node in the subject's set. Otherwise a node with a signing key and no secret at all could still make tokens for a tier-0 account.
- **Sign-in is proxied** where the secrets are not kept. A node outside the set passes the exchange (password, TOTP code, FIDO2 challenge and answer) over the node link to a node in the set, which checks it, starts the session and signs; the proxy keeps nothing of it. While no node of the set can be reached, those accounts cannot sign in or refresh; access tokens they hold work until they expire.
- **Tier 0** is a flag an administrator turns on for an OU (A20); nothing is tier 0 unless turned on. It covers the OU and everything below it. Its accounts must sign in with FIDO2: a security key or passkey with the password, or a passkey that verifies the user on its own; TOTP does not count for them. Turning it on warns about accounts in it without a FIDO2 credential (they could not sign in) and ends the sessions of its accounts that were started without FIDO2. FIDO2 also keeps proxied sign-ins safe from the proxy: nothing reusable crosses it, unlike a typed password.
- Node control shows per OU where its secrets are and who signs. The check refuses an empty set, a node that is not an `idp`, and a set without the OU's directory authority.

---

## 11. Protocol surface

### 11.1 Native

- **JMAP** (RFC 8620) is the native API for mail, calendars, contacts, files and the platform's own extensions such as chat. The web client uses nothing else.
- Push uses JMAP over WebSocket (RFC 8887).
- Standards status to track: mail is RFC 8621 and contacts is RFC 9610. The calendar extension was still an Internet-Draft as of October 2026, and file storage (FileNode) is an earlier-stage draft. Where a draft is used, the platform pins a revision and versions its capability string.

### 11.2 Compatibility front ends

Each is a translator onto the same collections, and none has its own storage.

| Front end | Serves |
|---|---|
| IMAP | Native mail apps |
| SMTP submission | Sending from native mail apps |
| CalDAV | Native calendar apps |
| CardDAV | Native contact apps |
| WebDAV | File access from operating systems and tools |

### 11.3 Server to server

- SMTP for mail, with SPF, DKIM, DMARC and ARC.
- Calendar invitations to external participants travel as mail (iTIP over email), so external scheduling depends on the mail module.
- Chat federation only through the Matrix module (section 14).

---

## 12. Built-in modules

**Instances (A24).** A module runs as instances: a kind (files, wiki, mail, …) under a name of its own, on a node, kept in node control. A deployment may run several instances of a kind (a mail module per region, a wiki per department), meant to sit on different nodes; the node's rule for the kind applies to where an instance may go. An instance's data lives with it, and moving it to another node is a planned handover. People reach an instance through access granted to them or to a group they are in, kept in the directory of their OU. Mass updates (put into a group, give or take access, disable, enable) work on users picked in the table or on everything a filter finds; they run on the node as jobs, a batch per entry, each user checked against the actor's rights.

All built-in modules are compiled into the binary and implement one internal `Module` trait: they register routes, object types, permissions, jobs, search schemas and UI assets with the core. **They use the same host API that extension modules use**, in-process and without the sandbox. This keeps the extension API honest: if a built-in needs something the API lacks, the API is extended instead of bypassed.

### 12.1 Chat

- A room is a collection with a linear log. Membership and permissions use the core model.
- Direct messages are rooms with two members.
- Presence and typing indicators are ephemeral, carried over node links and never written to a log.
- A room has a flag: local or federated. Federated rooms are handled together with the Matrix module (section 14). Local rooms never touch it.
- Calls are a later module on top of rooms, using the `media` role.
- **Built (D-U42, 2026-11-17).** A room is a collection of domain chat, made by a `create`
  command (its authority by node control's placement of chat for the maker's OU) and kept by
  every store node. Its entries (`src/chat.rs`, module `chat`): create, rename, topic, add, remove,
  leave, say, edit, delete, read. The authority checks each against the folded room (members;
  owners take people out; authors edit; authors and owners delete; a DM is fixed at two). A DM's
  id is derived from the two user ids, so a pair has one. The folded state keeps where each
  message is and who said it, edited texts and read markers, not the texts; history is read from
  the log. The pages ask the node that answers; a long poll (`/api/chat/wait`) wakes on any change
  to a room kept there or on typing. Typing lives in that node's memory; carrying it over the
  links comes with more than one answering node. `federated` is refused until §14.

### 12.2 Calendar and contacts

- Data model follows JSCalendar and JSContact.
- Free/busy, scheduling between local users and resource booking are internal operations across collections (section 5.4).
- Group calendars and shared address books are shared-space collections.

### 12.3 Files

- A folder tree per account and per shared space; contents in the blob store; versions kept by reference, so a version costs only its changed chunks.
- Sharing by the core permission model, plus public links as capability tokens.
- **The user's own storage (A6).** As in osada 0.1, users can add SFTP servers of their own (password or key, sealed). They appear next to the home, are served by the node that holds the credentials, and are neither replicated nor backed up. They are not storage pools.
- **Viewers and editors (A10).** Osada 0.1's in-browser viewers and editors carry over: spreadsheets, documents, presentations, mail files, archives and the rest.
- Later, office documents can also be opened in an external web office suite (OnlyOffice or Collabora) through WOPI. This is the one deliberate exception to "one binary": the office suite is an optional separate service, and the platform works without it.
- A desktop sync client is a separate project. Until it exists, WebDAV is the access path. (Open decision D9.)

### 12.4 Mail

- SMTP in and out on `edge-mail` nodes, with a durable queue.
- Parsing, spam scoring and virus scanning of inbound mail run in `worker` processes, never in `core`.
- Filtering rules with Sieve.
- Storage is the account's mail collection; JMAP and IMAP are two views of it.
- Mail replaces the Stalwart server at mail.example.org (A11), with the edge as its authority. Until then osada 0.1's mail sync API (`/api/mail/users`) is kept, so Stalwart keeps getting its accounts and addresses from osada.
- **Mailboxes are osada's (D-U43, 2026-11-17).** Who has a mailbox, its address, size and on/off
  are kept in the directory (a user's `mailbox`) and set in admin → mail or on the user card; the
  voter makes Stalwart's accounts match once a minute and after each change (and makes a domain
  Stalwart lacks when the settings say so). It never removes an account on its own; removing a
  mailbox with its mail is an administrator's act. The external stalwart-osada-sync is no longer
  needed; the sync API stays for compatibility. The `email` claim of tokens is the mailbox's
  address.
- Reuse: Stalwart reached 1.0 in October 2026 and is AGPL-3.0, so it is licence-compatible. Its server has its own storage and clustering, which do not fit the collection model, and its test suites are no longer public. The recommendation is to reuse its standalone protocol and parsing libraries where their licences allow, and to write the storage integration here. (Open decision D5.)

---

## 13. Extension modules

Two modules prove the extension system, and they test opposite properties:

- **An expressive module** (to be chosen; the wiki stays built in, A9) tests expressiveness: can a real feature with its own data, UI, search and permissions be built purely on the extension API?
- **Matrix federation** tests restriction: can code that parses hostile input from the open internet be contained?

### 13.1 Runtime

- An extension is a WebAssembly component with a manifest.
- Components run inside a `modhost` process, which is itself sandboxed by the operating system. There are two walls: the WASM runtime, and the process sandbox around the runtime.
- A component has no ambient authority: no filesystem, no sockets, no clock or randomness beyond what the host hands it, no access to other modules.
- Resource limits per component: memory, CPU budget per call, storage quota, outbound request rate.
- A crashing or misbehaving component is killed and restarted without affecting `core`.

### 13.2 Capabilities

The manifest declares every capability. An administrator approves them at install time. Nothing can be requested later at runtime; a new capability needs a new manifest and a new approval. Examples:

| Capability | Meaning |
|---|---|
| `collections.own` | Create and use collections of the module's own type |
| `collections.read(type, filter)` | Read specific existing collections, narrowed by a filter |
| `http.in(prefix)` | Receive requests under one path prefix |
| `http.out(policy)` | Make outbound requests through the host, under a destination policy |
| `directory.read(fields)` | Read named directory fields of specific subjects |
| `sign(key)` | Ask the keyholder to sign with a named key the module never sees |
| `jobs` | Schedule background work within its budget |
| `search` | Index and query its own objects |
| `ui(slots)` | Contribute UI in named places |

Rules:

- Outbound HTTP is always performed by the host: the host resolves names, does TLS, and refuses private and internal address ranges. The component never opens a connection.
- Every host call by an extension is authorised against its service principal and written to the audit log.
- Data access goes through the same permission check as user access. A module cannot read a collection its principal has no relation to.

### 13.3 UI of extensions

- Built-in module UI is trusted code served by the binary.
- Extension UI is loaded in a sandboxed iframe with an opaque origin. It talks to the shell by message passing, and the shell applies the module's capabilities to every request. Extension script never runs in the shell's origin and never sees the user's token.

### 13.4 Wiki

The wiki is a built-in module (A9), carried over from osada 0.1: articles in several languages, pictures, references, family trees, revisions. Each wiki space is a shared-space collection; pages are objects, and revisions come from the log. Like every built-in it uses the same host API as extensions.

---

## 14. Matrix federation module

### 14.1 Scope

- Only rooms flagged as federated are visible to the module. Local rooms, mail, files and the rest of the platform are outside its reach by capability.
- To the user, a federated room is a room. The web client talks to the chat module with the same API either way.
- To the Matrix network, the whole deployment appears as one homeserver under one server name, however many nodes it has.

### 14.2 How it works

- The chat module owns the room as the user sees it. For a federated room it forwards local events to the Matrix module and accepts remote events from it.
- The Matrix module owns the Matrix view of that room: the event graph, state resolution, signatures, and the conversation with remote servers.
- The two meet at a narrow interface: "local event to send", "remote event accepted", "membership changed".

### 14.3 Capabilities granted

| Capability | Narrowing |
|---|---|
| `http.in` | The Matrix federation paths and the server discovery document only |
| `http.out` | Matrix federation requests only, through the host |
| `collections.own` | Its own Matrix-state collections |
| Chat bridge interface | Federated rooms only |
| `directory.read` | Display name and avatar of users who are members of a federated room |
| `sign` | The server's Matrix signing key, held by the keyholder |

It gets no access to credentials, tokens, mail, files, calendars or local rooms.

### 14.4 End-to-end encryption

Routing is server-side and does not affect the client. Encryption is the one place a choice has to be made, because many Matrix rooms are end-to-end encrypted:

- **Chosen default:** the module is the cryptographic endpoint for local users in encrypted federated rooms. The client stays untouched and vanilla. The consequence is that these rooms are encrypted between the remote users' devices and this module, not all the way to the local user's device, and the module holds room keys. The UI must label such rooms accurately.
- **Alternative, not planned:** true end-to-end encryption would require the Matrix cryptographic protocol in the browser.

Because the module holds room keys under the default, its key storage is one of its own collections, encrypted, and the capability table above is the complete list of what it can reach.

---

## 15. Web client

- **Vanilla JS.** ES modules, custom elements, no framework, no bundler required to run. Types by JSDoc annotations checked in development only.
- **Shell and modules.** A small shell provides sign-in, navigation, the JMAP client, the push connection and a local cache. Each module contributes custom elements and routes.
- **One data layer.** All modules read and write through the shared JMAP client, which keeps a local cache keyed by state tokens and applies pushed changes. No module makes its own network calls.
- **Assets** are embedded in the binary, content-hashed and cached immutably. The client and server always match because they ship together.
- **Security headers.** A strict content security policy: same-origin scripts only, no inline script, no dynamic code evaluation.
- **Installable** as a progressive web app with push notifications. Native mobile apps are later work.
- **A million rows per table.** Every table of the admin pages is filtered, sorted and paged by the node; the page only ever holds one page, and quick filters apply on Enter, not as one types. Pickers (users, groups, OUs, nodes) ask for at most 15 matches once typing pauses, and open a full table for the rest. Indexes come where a linear pass gets too slow; logs page on by cursor.
- **The stated exception.** A rich-text composer for mail and wiki is the one place where a small, audited third-party component may be justified. (Open decision D6.)

---

## 16. Security

### 16.1 Threat model, in order of likelihood

1. Hostile input from the network: mail, attachments, federation traffic, uploaded files.
2. A vulnerability in a dependency reachable from a listener.
3. A malicious or buggy extension module.
4. A compromised node in a multi-node cluster.

### 16.2 Measures

- **Privilege separation** (section 4.2). Hostile input is parsed in workers with no network and no keys.
- **Secrets by role.** A node receives only the keys its roles need. An `edge-http` node holds no signing keys and no collection data.
- **Node compromise.** A compromised `store` node can corrupt the collections it is authority for; hash-chained logs make this detectable by replicas. It cannot forge tokens or change the placement map.
- **Supply chain.** Pinned and vendored dependencies, licence and advisory checks in CI, a reviewed allow-list of crates, minimal feature flags, reproducible static builds, signed releases.
- **Audit log.** Authentication events, permission changes, administrative actions and every extension host call. Append-only and replicated like any other log (section 16.3).

### 16.3 Logs

**Session ids (A17).** Every sign-in with credentials (password, second factor, FIDO2) starts a session with an id, `sid`: random, 128 bits, not a secret, unrelated to the cookie or any token. Everything that follows from that sign-in carries it as its origin: the session's refreshes; sign-ins to other programs through OIDC (each gets a `sid` of its own, with the first as its origin, so a chain of programs stays traceable); tokens; API keys, app passwords and shared links made in it; a later step-up. ID tokens carry `sid` (the OIDC claim), so programs can log it too. From any event the logs lead back to the first sign-in, and from a sign-in to everything that came of it.

**What is logged.** Each node writes its own logs, as collections it is the authority for, so the edge keeps logging while home is down; they are replicated to the nodes that keep logs (home). Every entry has the time, the node, the actor, the client's address, the `sid` and its origin where there is one, and what happened. Never secrets: no passwords, cookies or tokens; a token is named by its id.

| Log | Holds |
|---|---|
| `auth` | Sign-ins and failures, second factors, sessions (start, refresh, end, revocation), tokens issued, sign-ins to other programs, changes of passwords, TOTP and FIDO2 credentials |
| `admin` | Every administrative change: users, OUs, policies, node control, settings |
| `access` | Reads and writes of files, the wiki and mail; shares and shared links used |
| `node` | Cluster events: membership, handovers, failovers, outcomes of deferred writes |
| `ops` | The program's own diagnostics |

**Switches in the admin UI**, per log:

- **Verbosity:** `off`, `minimal`, `standard`, `detailed` or `debug`. For `auth`: minimal is sign-ins, failures and credential changes; standard adds sessions, refreshes, tokens and sign-ins to other programs; detailed adds every use of a session or token (summed up per minute); debug adds protocol detail, still without secrets. `auth` and `admin` cannot be `off`.
- **Retention:** how long entries are kept, in days. Older entries are removed from the front of the log daily; the chain stays verifiable from a checkpoint.

A change of a switch is itself an `admin` event.

**Events carry their session (A23).** Every event names the session it came from (`sid`) and who acted (`actor`): a sign-in, its password change, the accounts it created, the roles it granted, the node control it changed. Creating an account is `user_created` in the admin log; its first password is a separate `password_changed` in the auth log; both carry the session, so they can be put together. Ending another session names the ended one (`ended`) and the session that ended it.

**Reading the logs.** Each node keeps its own auth and admin logs; a query runs on every node (or the one it names), with conditions on the event, the user, who acted, the session, the address, the node, the OU, the role or free text, and a time range, and the results are cut to what the reader may see: entries about accounts in OUs where they have `logs-read`, the rest with `logs-read` at the root. In the admin UI the settings sit above the query; clicking a value in the results narrows the query to it.

**Tokens that outlive the logs.** Whatever grants access for a time (a session with all its refreshes, refresh tokens, API keys, app passwords, phone tokens, shared links) has a lifetime, unlimited when it has no expiry. If that lifetime is longer than 90 % of the `auth` log's retention, its user is cautioned: when it is made, wherever it is listed, and when a lower retention brings it over the line. The caution says that once the logs are gone, it can no longer be traced to the sign-in it came from. Admins see a list of all such tokens.

---

## 17. Deployment topologies

### 17.1 One node

```
[ VPS: all roles ]
```

One binary, one config file, one data directory, built-in certificates. Durability `local`, plus snapshots to a backup target.

### 17.2 Public node plus home node

```
internet ──▶ [ edge (VPS):   idp, edge-http, edge-mail, store (mail, links, replicas), small blob ]
                          ▲
                          │ node link, dialled from home
                          │
             [ home (home): voter, idp, store (directory, files, wiki), large blob ]
```

- home is the only voter and the authority for the directory, files and the wiki; the edge is the authority for mail and shared links (A3, A14).
- home dials out and keeps the link open; nothing at home needs to be reachable from outside.
- The edge holds replicas of the directory, the credentials of the OUs whose secrets policy names it (it is an `idp`, A15, A19) and of the file and wiki metadata, with recent and pinned chunks, as placement says. Unlike osada 0.1, the edge holds data that needs a login.
- Home offline: sign-in (for accounts whose secrets the edge keeps), mail and shared links work on the edge; files and the wiki can be read where the edge has the chunks, and changed as deferred writes (section 7.7); directory and policy changes wait for home.
- The edge lost: explicit `promote` of mail and links onto home or a new VM, then point DNS at a new public node.

### 17.3 Small high-availability cluster

Three nodes, all voters and stores. Durability `sync`. Automatic failover. Rolling upgrades with no downtime.

### 17.4 Large organisation

```
bare metal (few):   voter · idp · store · blob
elastic (many):     edge-http · edge-mail · worker · modhost · media
```

- The elastic tier runs as containers on OpenShift or any Kubernetes; it holds no durable state.
- SMTP and call media need fixed public addresses and non-HTTP ports, so those roles run on dedicated edge nodes or with host networking.
- At this size the blob backend is S3-compatible storage.

### 17.5 Multiple regions

Voters spread over three sites; `store` and `blob` nodes in each region; rebalancing moves authority toward users; residency pins keep collections where they must stay.

### 17.6 Growth path

| Step | Change | Migration needed |
|---|---|---|
| 1 → 2 nodes | `join`, assign roles and placement policy | None |
| 2 → 3 nodes | Add voters, switch durability to `sync` | None |
| Split roles | Move edge and worker roles to new nodes | None |
| Local blobs → S3 | Add backend, change placement policy, let chunks migrate | Background copy |
| Add a region | Add nodes, let the rebalancer run | None |

---

## 18. Operations

- **Configuration.** A minimal local file (node identity, roles, listen addresses, data directory). Everything else lives in the cluster log and is changed through the admin API or CLI.
- **Version skew.** A cluster must run correctly with versions N and N+1 mixed. Node-link messages and log entries are versioned and forward-compatible. New behaviour that needs all nodes is gated behind the cluster version, which is raised explicitly once every node is upgraded.
- **Rolling upgrade.** For each node: drain (stop accepting new connections, hand its authorities over with the planned handover of section 7.3), replace the binary, restart, rejoin. Clients reconnect on their own. On a single node this is a restart of a few seconds.
- **Observability.** Structured logs, metrics and traces from the core for every listener and module, with no module-specific setup.
- **Admin surface.** One CLI, built into the binary, speaking the same admin API as the web UI.

---

## 19. Testing

- **Deterministic simulation** of the cluster from the first commit: many nodes in one process, a simulated network and clock, and scripted faults (partitions, crashes, slow disks, clock drift). Invariants checked on every run:
  1. At most one node accepts writes for a collection in any epoch.
  2. A write acknowledged in `sync` mode is never lost.
  3. Replicas of a collection converge to the same hash chain.
  4. A planned handover loses nothing in any mode.

  In every mode, an acknowledged write that is not in the final chain must be in a recovery area: nothing vanishes without a trace. The harness has teeth: with `sync` broken to acknowledge on the authority alone, it finds a lost write within two seeds.
- **Protocol conformance** suites for JMAP, IMAP, CalDAV, CardDAV, WebDAV, SMTP and, for the Matrix module, the Matrix federation test suites.
- **Footprint budget** enforced in CI: the full single-node deployment must start and idle within a fixed memory budget on a small VPS. The number is to be set once a baseline exists.
- **Upgrade tests** that run a mixed-version cluster through the fault schedule.

---

## 20. Build order

| Milestone | Content | Done when |
|---|---|---|
| M0 Spine | Storage trait, collection log, cluster log, node links, authority, replication, admin CLI, simulation harness | Simulated clusters pass the fault schedule; a real VPS and a real home node replicate a test collection |
| M1 Identity | Directory, IdP with OIDC and passkeys, permission model, web shell, admin UI | A user can sign in on any node and manage accounts |
| M2 Chat | Local rooms, push, presence | Two users on two nodes chat; a room survives a handover mid-conversation |
| M3 Calendar and contacts | JMAP, CalDAV, CardDAV, internal scheduling | A phone's built-in apps sync |
| M4 Files | Blob placement, sharing, WebDAV, office integration | The split topology serves files with the home node online and degrades correctly with it offline |
| M5 Mail | SMTP, JMAP mail, IMAP, filtering, spam handling | Daily personal use starts here |
| M6 Extensions | Module host, capabilities, sandboxed UI, a first extension | An extension with its own data and UI works using only the extension API |
| M7 Matrix | Federation module | Federated rooms work; the module passes a hostile-module review against its capability list |
| M8 Public release | SAML, SCIM, LDAP view, Linux agent, upgrade tests, documentation, external security review | First release to others |

**Order for osada 0.2.** Replacing osada 0.1 comes first: M0, M1, M4 (files, users' SFTP storage, nightly backup and versions) and the wiki, then the switch from 0.1 with a migration of its data. Chat (M2), calendar and contacts (M3) and mail (M5) follow.

Later: calls, native mobile apps, desktop sync client, large-organisation features (retention policies, legal hold, discovery), packaging for OpenShift, Windows domain support.

One ordering constraint: the extension host API has to be drafted during M0 and M1, even though extensions ship in M6, because built-in modules are written against it from M2 onward.

---

## 21. Open decisions

| # | Decision | Options | Leaning |
|---|---|---|---|
| D1 | Embedded key-value engine | RocksDB (proven, C++ dependency); a pure-Rust engine (fewer dependencies, less field history); SQLite | SQLite first, behind the trait (proven in osada 0.1, compiled in); evaluate others against the simulation suite |
| D2 | Collection granularity for accounts | One collection per account; one per account per module | Settled: one per account per module (A13) |
| D3 | Raft implementation | Existing Rust library; own implementation | Existing library; verify maturity before committing |
| D4 | Node-link transport | QUIC; TLS over TCP with multiplexing | Settled: QUIC first (quinn on rustls with ring); a TCP fallback for networks that block UDP later |
| D5 | Mail code reuse | Stalwart's libraries only; embed its server; write everything | Libraries only, after checking each crate's licence |
| D6 | Rich-text composer | One audited dependency; own minimal editor | Decide at M5 |
| D7 | Blob encryption at rest | On by default; optional | On by default if the performance cost is acceptable on a small VPS |
| D8 | Multi-tenancy | One organisation per cluster; tenants within a cluster | One per cluster |
| D9 | Desktop file sync | Own client; rely on WebDAV and third-party tools | Defer until after M5 |
| D10 | WASM runtime and component interface | To be evaluated at M0 for API stability and resource limiting | — |
| D11 | Unit of file storage | Content-defined chunks; whole files by hash | Settled: chunks (A12) |
| D12 | Sign-in while home is away | `idp` on the edge too (signing key and credentials on the VPS); `idp` on home only | Settled: `idp` on the edge, with its own signing key (A15) |

Library names in this document are candidates. Their current maturity, maintenance and licences have to be checked at the point of adoption.

---

## 22. Licence

- Copyright © 2026 th3rumbl3m4t0r. The platform is AGPL-3.0 for now; the rest is decided before the public release, which credits the author (A18).
- Dependencies must be compatible with AGPL-3.0; this is enforced in CI.
- Extension modules run in a separate sandbox behind a defined interface. Whether third-party modules are bound by the platform's licence is a question for a lawyer before the public release.

---

## References

- JMAP core: RFC 8620. JMAP mail: RFC 8621. JMAP over WebSocket: RFC 8887. JMAP contacts: RFC 9610.
- JMAP calendars (Internet-Draft): https://datatracker.ietf.org/doc/html/draft-ietf-jmap-calendars
- JMAP working group documents and milestones: https://datatracker.ietf.org/group/jmap/about/
- Stalwart 1.0.0 release notes: https://newreleases.io/project/github/stalwartlabs/stalwart/release/v1.0.0
- Matrix server implementations: https://www.matrix.org/ecosystem/servers/
