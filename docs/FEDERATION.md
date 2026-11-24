# Federation between osada instances (Round 22, protocol 1)

Code: `src/osada1/federation.rs`. Test: `tests/federation.sh` (two drop-ins, A and B).

## UPNs

Every account is `<name>@<UPN domain>`. The UPN domain is a cluster setting
(`Settings.upn_domain`; admin → federation, or the mail op `upn_domain`). A drop-in sets it to its
public host at its start when the cluster has none. In production this is `shelf.example.org`.

- **Sign-in:** the name or the UPN, its domain in any case (`Node::login_name`): 0.2's
  `/api/signin`, osada 0.1's `/api/login`, the OIDC login.
- **Shown:** the account page (0.2's `/api/account` `upn`, 0.1's `/api/me` `upn`), the OIDC claims
  (`upn`), and the share window (people by UPN; their mailbox address and its aliases find them too).
- **Mail:** with a mailbox, the UPN is also an alias of it in Stalwart (`mailadmin::plan`). The
  UPN's domain is made in Stalwart when it lacks it. The mailbox's own alias list does not show the
  UPN. A new UPN domain replaces the old aliases. Mail to it arrives once the domain has an MX record.

## Discovery (public)

`GET /.well-known/osada-federation`:

```json
{ "software": "osada", "protocol": 1, "version": "0.2.0", "base": "https://osada.example.org",
  "upn_domain": "osada.example.org", "api": "https://osada.example.org/.well-known/osada-federation/v1",
  "keys": [ { "kty": "RSA", "alg": "RS256", "kid": "…", "n": "…", "e": "AQAB" } ] }
```

The keys are the instance's OIDC signing key.

## Does this UPN exist? (public)

`GET {api}/who/{upn}` answers `{"upn": "…", "exists": true|false}`.

- It answers only for an exact UPN: no prefixes, no lists. Mail addresses are not UPNs.
- Disabled accounts: no.
- A yes and a no take the same 150 ms.
- Unsigned questions: 20 a minute per IPv4 address or IPv6 /64, then 429 with `Retry-After: 60`.
  A federated instance's signed questions are not counted.

## Signed requests (federated instances only)

Both administrators add each other (admin → federation: the other's address; its discovery is
checked, and its `base` must be the address asked). Every request carries:

`Authorization: Osada-Federation <JWT>`

The JWT is RS256, `typ` `osada-fed+jwt`, signed with the asking instance's key. Its claims:

| claim | value |
|---|---|
| `iss` | the asking instance's base; it must be a peer |
| `aud` | the answering instance's base |
| `iat`, `exp` | `exp` at most 300 s after `iat`; `iat` within 300 s of now |
| `jti` | used once |
| `htm` | the method |
| `htu` | the path with its query, as sent |
| `bh` | base64url of the body's SHA-256 (of nothing for GET) |

The answering instance gets the key from the asker's discovery: kept 10 minutes, fetched again for
an unknown `kid` at most once a minute.

| request | who asks | what |
|---|---|---|
| `GET {api}/hello` | anyone federated | `{ok, you, me}`: do you list me? (admin → federation shows it) |
| `POST {api}/shares` | the owner's instance | `{id, owner, owner_display, to, name, dir, size, secret}`: `owner` must be at the asker's UPN domain, `to` a UPN here; 404 if nobody here has it |
| `POST {api}/shares/{id}/revoke` | the owner's instance | the share is gone |
| `POST {api}/shares/{id}/decline` | the recipient's instance | the recipient dropped it |
| `GET {api}/shares/{id}/ls?path=` | the recipient's instance | with `x-shelf-share-secret`: the listing (`entries`, or `file`) |
| `GET {api}/shares/{id}/raw?path=[&dl=1]` | the recipient's instance | with the secret: the file; ranges, ETag |

- **Shares:** view-only. The owner's side keeps only the secret's SHA-256 (`fed_out`); the
  recipient's side keeps the secret (`fed_in`).
- **Paths:** nothing outside the shared item.
- **Offers:** a refused offer leaves nothing behind.
- **Removing a peer:** its shares go both ways.

## For the people

- **Share window:**
  1. this instance;
  2. a federated instance, for an address at its UPN domain: asked with `who`, signed;
  3. public e-mail.

  Sharing with someone at a federated instance is view-only.
- **The shared page:** what people elsewhere share with you, browsed through this instance
  (`/api/federated/{id}/ls|raw`). What you share with them; stop, or drop.
- **Admin → federation:** this instance's address, its UPN domain (editable), the discovery and
  `who` URLs, the peers and whether each lists this one.

## Not yet

- Editing a federated share (write-back).
- Group folders.
- Federated chat or calendars.
- Following a shared item when it is moved or renamed (the share then answers 404).
