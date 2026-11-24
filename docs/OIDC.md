# Sign-in with osada (OpenID Connect)

osada can be the login of other programs on the network: a program sends the
browser to osada, the person logs in there (or already is), and the program
gets back who they are. This is OpenID Connect, so any program or library
that speaks it works: oauth2-proxy in front of a site that has no login of its
own, Grafana, Gitea, a Flask or axum app of your own, a phone app.

- [Turning it on](#turning-it-on)
- [Adding a program](#adding-a-program)
- [What a program needs](#what-a-program-needs)
- [Example: oauth2-proxy in front of nginx](#example-oauth2-proxy-in-front-of-nginx)
- [Example: your own program](#example-your-own-program)
- [Limits](#limits)

## Turning it on

In `/etc/osada/osada.env`, set the address browsers and programs reach osada
at, without a path, and restart osada:

    OSADA_OIDC_ISSUER=http://192.168.1.10

This is the *issuer*. It has to be exactly the same everywhere: in the tokens,
in the discovery document and in each program's settings. On the first start
with it, osada makes its signing key (an RSA key, kept sealed in the
database, so the backup has it). By the public name (through the edge, or the
https listener at home) the issuer is the public address instead
(`https://shelf.example.org`): a program outside uses that one throughout.

## Adding a program

Admin → **sign-in for other programs** → **add program**, or from the shell:

    runuser -u osada -- env OSADA_DATA=/home/osada \
        /opt/osada/target/release/osada oidc add tracker \
        --redirect http://192.168.1.11/oauth2/callback [--users rob,eve] [--public]

- **name**: the program's `client_id` (a-z 0-9 . _ -). It cannot be renamed.
- **redirect URIs**: where osada may send the browser back to, one per line.
  They are matched exactly, query included. The program's documentation says
  what its callback address is.
- **who may log in**: usernames; empty means every account. Someone else gets
  `access_denied`.
- **public**: for an app that cannot keep a secret (on a phone, or all in the
  browser). It gets no secret and has to use PKCE. Everything else gets a
  **client secret**, shown once. If it is lost, **new secret** makes another
  one and the old one stops working.

Removing a program ends every token it was given. So do changing a password
and disabling an account, for that person's tokens.

## What a program needs

| setting | value |
|---|---|
| issuer / discovery | `http://192.168.1.10`, discovery at `http://192.168.1.10/.well-known/openid-configuration` |
| client_id, client_secret | from the admin page |
| scopes | `openid profile email` (others are ignored) |
| flow | authorization code; PKCE S256 recommended, required for public clients |
| client authentication | `client_secret_basic` or `client_secret_post`; `none` for public clients |
| signing | RS256 (key in `/oidc/jwks`) |
| user name claim | `preferred_username` (the osada username) |
| user id claim | `sub`: random per account and never reused, the right one to store |

Programs that find things through the discovery document need only the first
three rows. For the others, the endpoints are:

| | |
|---|---|
| authorization | `http://192.168.1.10/oidc/authorize` |
| token | `http://192.168.1.10/oidc/token` |
| userinfo | `http://192.168.1.10/oidc/userinfo` |
| keys (JWKS) | `http://192.168.1.10/oidc/jwks` |

The ID token has `iss`, `sub`, `aud`, `azp`, `iat`, `exp`, `auth_time`,
`nonce` (when sent) and `at_hash`, with scope `profile` also
`preferred_username` and `name`, and with scope `email` `email` and
`email_verified` when the account has a mail address (the mail server's sync
sets it, README "Mail accounts"; most accounts have none). Userinfo answers
the same claims for the access token.

The access token is a JWT too (RFC 9068, header `typ` `at+jwt`, same key and
`kid`), so a server can check it with the keys from `/oidc/jwks` alone:
`iss`, `sub`, `aud` and `client_id` (both the program's name), `scope` (as
granted), `iat`, `exp`, `auth_time`, `jti`, and the `profile`/`email` claims
as above. A server taking tokens for one program should require its `aud`
(Stalwart: `requireAudience`) and a scope (`requireScopes`); an ID token has
no `scope`, so it is refused as an access token. A server checking only the
signature sees a revocation (password changed, account disabled, program
removed) when the token runs out, within the hour; userinfo sees it at once.

ID and access tokens last an hour. Every code exchange also gives a refresh
token, good for 30 days. Each one works once and the answer brings the next
one. A code works once, within 60 seconds, from the redirect URI it was asked
for. If someone uses a code twice, the tokens it gave are revoked.

`prompt=login` and `max_age` make the person log in again. `prompt=none`
never shows the login page and answers `login_required` instead. The answer
on the redirect URI carries `iss` (RFC 9207) besides `code` and `state`.

There is no consent page. The programs are the admin's own, so a person who
is logged in to osada goes straight back with a code.

## Example: oauth2-proxy in front of nginx

For sites without a login of their own (cgit, the tracker, the status page on
apptab), oauth2-proxy can do the OpenID Connect part and nginx asks it about
each request (`auth_request`). One program in osada covers every site behind
that nginx.

In osada: add a program `apptab` with the redirect URI
`http://192.168.1.11/oauth2/callback`.

`/etc/oauth2-proxy.cfg` on apptab:

    provider = "oidc"
    oidc_issuer_url = "http://192.168.1.10"
    client_id = "apptab"
    client_secret = "<from osada>"
    redirect_url = "http://192.168.1.11/oauth2/callback"
    scope = "openid profile"
    code_challenge_method = "S256"
    # most accounts have no email address; the username stands in for one
    oidc_email_claim = "preferred_username"
    email_domains = ["*"]
    http_address = "127.0.0.1:4180"
    reverse_proxy = true
    set_xauthrequest = true
    skip_provider_button = true
    upstreams = ["static://202"]
    # plain http on the LAN
    cookie_secure = false
    cookie_secret = "<python3 -c 'import os,base64; print(base64.urlsafe_b64encode(os.urandom(32)).decode())'>"

nginx, in the server block:

    location /oauth2/ {
        proxy_pass http://127.0.0.1:4180;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Auth-Request-Redirect $request_uri;
    }
    location = /oauth2/auth {
        proxy_pass http://127.0.0.1:4180;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header Content-Length "";
        proxy_pass_request_body off;
    }

and in each location to protect:

    auth_request /oauth2/auth;
    error_page 401 = /oauth2/sign_in;
    # who it is, for the program behind
    auth_request_set $user $upstream_http_x_auth_request_preferred_username;
    proxy_set_header X-User $user;

Leave out locations that programs call rather than people, such as an API an
app uses or a status probe. Otherwise they get a login page instead of their
answer.

## Example: your own program

Any OpenID Connect library works. With Authlib in Flask:

    oauth.register("osada", client_id="tracker", client_secret="…",
                   server_metadata_url="http://192.168.1.10/.well-known/openid-configuration",
                   client_kwargs={"scope": "openid profile", "code_challenge_method": "S256"})

    @app.route("/login")
    def login():
        return oauth.osada.authorize_redirect(url_for("callback", _external=True))

    @app.route("/callback")
    def callback():
        user = oauth.osada.authorize_access_token()["userinfo"]
        session["user"] = user["preferred_username"]   # store user["sub"] to recognise them
        return redirect("/")

## Limits

- Over plain http on the LAN, like the rest of osada there: anyone who can
  watch the network can read codes and tokens, as they can passwords.
- Email addresses only for accounts with a mailbox, and no groups; who may
  use a program is set per program.
- No logout endpoint: logging out of a program ends that program's session,
  not the one in osada.
- No implicit or hybrid flow, no request objects, no dynamic client
  registration, no consent page.
- One signing key, never rotated. Removing `oidc_key` from the `settings`
  table and restarting makes a new one; programs pick it up from the JWKS.
