//! The web side of a node: the admin and account pages (static/, built
//! into the binary) and the API they use. This is the start of §15 and of
//! the admin API (§18); JMAP comes with the modules that need it.
//!
//! - Static files are served under their content hash, kept for good.
//! - The page's CSP allows scripts from the node only.
//! - The session is a cookie: `osada` inside the drop-in upgrade of osada
//!   0.1 (shared with its pages, D-U5), `shelf2_<node>` for a node of its own
//!   (cookies ignore ports). HttpOnly, SameSite=Lax (other programs' sign-in
//!   through OIDC comes back by a top-level redirect), Secure over TLS; every
//!   request that changes something must carry `x-shelf: 1`, which a form on
//!   another site cannot.
//! - The pages are /signin, /account and /admin…; the API is /api/signin…,
//!   /api/account… and /api/admin/…; static files /static2/<hash>/<name>.
//! - Administration needs an administrator's session. What it changes goes
//!   to the voter (node control, settings, handovers) or to the authority of
//!   the collection (users), over the links when that is another node.

// The guards below return the error page itself (a Response), axum's usual way.
#![allow(clippy::result_large_err)]

use std::{
    collections::BTreeMap,
    convert::Infallible,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, FromRequestParts, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::{any, get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    audit::Settings,
    directory::User,
    log::CollectionId,
    node::{Actor, BulkAction, Fido2Answer, LogQuery, Node, Selection, Session, SignInStep},
    placement::{NodeId, OuId, Placement},
    rbac::Subject,
};

const FILES: [(&str, &str, &str); 6] = [
    ("x11.css", "text/css; charset=utf-8", include_str!("../static/x11.css")),
    ("x11.js", "text/javascript; charset=utf-8", include_str!("../static/x11.js")),
    ("app.css", "text/css; charset=utf-8", include_str!("../static/app.css")),
    ("app.js", "text/javascript; charset=utf-8", include_str!("../static/app.js")),
    ("grid.js", "text/javascript; charset=utf-8", include_str!("../static/grid.js")),
    // the node map (D-U34): Cytoscape.js 3.34.4, MIT (static/vendor/cytoscape-LICENSE), loaded when the map is shown
    ("cytoscape.js", "text/javascript; charset=utf-8", include_str!("../static/vendor/cytoscape-3.34.4.min.js")),
];
const INDEX: &str = include_str!("../static/index.html");
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                   connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

struct Asset {
    mime: &'static str,
    body: &'static str,
    /// the body gzipped (empty: not)
    gz: Vec<u8>,
}

/// What the embedding server wants told (the drop-in upgrade keeps osada
/// 0.1's users table in step with it, D-U2).
pub trait Hooks: Send + Sync {
    /// A user's password was set (by them or an administrator).
    fn password_set(&self, user: &str, password: &str);
    /// Users' mail addresses (user id, address), as the mail server's sync gave them (D-U38).
    fn mail_users(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The issuer other programs know osada by (its public https URL), for tokens osada signs
    /// itself (Round 15).
    fn issuer(&self) -> Option<String> {
        None
    }
}

/// How the pages are served.
#[derive(Clone)]
pub struct Options {
    /// The session cookie's name.
    pub cookie: String,
    /// Inside osada 0.1's server: its pages are at /files…, ours get a link there.
    pub files: bool,
    pub hooks: Option<Arc<dyn Hooks>>,
    /// Sign-in for other programs (OIDC) by this node (D-U45): the scheme of its issuer (`https`
    /// at an edge; its name is the request's). None where osada 0.1's provider answers (home).
    pub oidc: Option<&'static str>,
}

impl Options {
    /// A node of its own: a cookie named after it.
    pub fn standalone(node: &NodeId) -> Options {
        Options { cookie: format!("shelf2_{}", node.0.replace(['.', '-'], "_")), files: false, hooks: None, oidc: None }
    }
}

struct App {
    node: Arc<Node>,
    opts: Options,
    index: String,
    assets: BTreeMap<String, Asset>,
    limiter: Limiter,
    oidc: crate::oidc::Provider,
}

/// Failed sign-ins in the last quarter hour: per name and address (a
/// stranger cannot lock someone out from everywhere), and per address.
#[derive(Default)]
struct Limiter(std::sync::Mutex<std::collections::HashMap<String, (u32, std::time::Instant)>>);

const WINDOW: std::time::Duration = std::time::Duration::from_secs(15 * 60);
const FAILS_PER_NAME: u32 = 10;
const FAILS_PER_ADDR: u32 = 50;

impl Limiter {
    fn keys(name: &str, ip: IpAddr) -> [(String, u32); 2] {
        [(format!("{name}@{ip}"), FAILS_PER_NAME), (ip.to_string(), FAILS_PER_ADDR)]
    }

    fn allowed(&self, name: &str, ip: IpAddr) -> bool {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, (_, at)| at.elapsed() < WINDOW);
        Self::keys(name, ip).iter().all(|(k, max)| m.get(k).is_none_or(|(n, _)| n < max))
    }

    fn fail(&self, name: &str, ip: IpAddr) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for (k, _) in Self::keys(name, ip) {
            let e = m.entry(k).or_insert((0, std::time::Instant::now()));
            e.0 += 1;
        }
    }

    fn clear(&self, name: &str, ip: IpAddr) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&format!("{name}@{ip}"));
    }
}

/// Who asks: the address, and whether the request came over TLS (then the
/// cookie is Secure). Through osada 0.1's edge it is the visitor's address
/// the edge saw.
pub struct Peer(pub IpAddr, pub bool);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        use crate::osada1::{edge, tls};
        let tls = parts.extensions.get::<tls::Https>().is_some();
        if let Some(edge::Remote(ip)) = parts.extensions.get::<edge::Remote>() {
            return Ok(Peer(*ip, true));
        }
        if let Some(ConnectInfo(edge::PeerAddr(a))) = parts.extensions.get::<ConnectInfo<edge::PeerAddr>>() {
            return Ok(Peer(a.ip().to_canonical(), tls));
        }
        if let Some(ConnectInfo(a)) = parts.extensions.get::<ConnectInfo<SocketAddr>>() {
            return Ok(Peer(a.ip().to_canonical(), tls));
        }
        Ok(Peer(IpAddr::V6(Ipv6Addr::UNSPECIFIED), tls))
    }
}

/// The node the browser is connected to when it is not this one: the edge that relayed the
/// request (relay::Via).
pub struct Entry(pub Option<NodeId>);

impl<S: Send + Sync> FromRequestParts<S> for Entry {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        Ok(Entry(parts.extensions.get::<crate::relay::Via>().map(|v| v.0.clone())))
    }
}

/// Serves the pages and the API on `addr` until the process ends (a node of
/// its own; inside osada 0.1's server it is `router`).
pub async fn serve(node: Arc<Node>, addr: SocketAddr) -> Result<(), String> {
    let opts = Options::standalone(node.me());
    let app = app(node, opts);
    // "/" is the account page here
    let router = routes(app.clone()).fallback(page).with_state(app);
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("{addr}: {e}"))?;
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await.map_err(|e| e.to_string())
}

/// The pages and the API, to merge into another server's router.
pub fn router<S: Clone + Send + Sync + 'static>(node: Arc<Node>, opts: Options) -> Router<S> {
    let app = app(node, opts);
    routes(app.clone()).with_state(app)
}

fn app(node: Arc<Node>, opts: Options) -> Arc<App> {
    let mut assets = BTreeMap::new();
    let mut index = INDEX
        // its name (at the start; the header shows the current one), not its GUID
        .replace("{{node}}", &node.cluster_state().placement.node_label(node.me()))
        .replace("{{version}}", crate::VERSION)
        .replace("{{files}}", if opts.files { "1" } else { "" });
    // One hash over all of them: the scripts import each other by relative
    // path, so they change together.
    let mut all = Sha256::new();
    for (name, _, body) in FILES {
        all.update(name.as_bytes());
        all.update(body.as_bytes());
    }
    let hash = &hex::encode(all.finalize())[..12];
    for (name, mime, body) in FILES {
        index = index.replace(&format!("{{{{{name}}}}}"), &format!("/static2/{hash}/{name}"));
        // compressed once, at start: an edge keeps and sends this (load test 2026-11-17: app.js 171 KB raw)
        let gz = {
            use std::io::Write;
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            let _ = e.write_all(body.as_bytes());
            e.finish().unwrap_or_default()
        };
        assets.insert(format!("{hash}/{name}"), Asset { mime, body, gz });
    }
    Arc::new(App { node, opts, index, assets, limiter: Limiter::default(), oidc: crate::oidc::Provider::default() })
}

fn routes(app: Arc<App>) -> Router<Arc<App>> {
    let r = if app.opts.oidc.is_some() {
        Router::new()
            .route("/.well-known/openid-configuration", get(oidc_discovery))
            .route("/oidc/jwks", get(oidc_jwks))
            .route("/oidc/authorize", get(oidc_authorize_get).post(oidc_authorize_post))
            .route("/oidc/resume/{id}", get(oidc_resume))
            .route("/oidc/token", post(oidc_token).options(oidc_preflight))
            .route("/oidc/userinfo", get(oidc_userinfo).post(oidc_userinfo).options(oidc_preflight))
    } else {
        Router::new()
    };
    r
        .route("/", get(page))
        .route("/signin", get(page))
        .route("/account", get(page))
        .route("/chat", get(page))
        .route("/contacts", get(page))
        .route("/calendar", get(page))
        .route("/.well-known/caldav", get(dav_discovery))
        .route("/.well-known/carddav", get(dav_discovery))
        // a site at /web/<site>/ on osada's own names (Round 18)
        .route("/web/{site}", any(web_site))
        .route("/web/{site}/", any(web_site))
        .route("/web/{site}/{*rest}", any(web_site))
        .route("/api/pim/contacts", get(pim_contacts))
        .route("/api/pim/calendar", get(pim_calendar))
        .route("/api/pim/calendar/event", post(pim_event))
        .route("/admin", get(page))
        .route("/admin/{*rest}", get(page))
        .route("/static2/{hash}/{name}", get(asset))
        .route("/api/signin", post(sign_in))
        .route("/api/signin/totp", post(sign_in_totp))
        .route("/api/signin/fido2", post(sign_in_fido2))
        .route("/api/account/fido2/begin", post(fido2_begin))
        .route("/api/account/fido2/confirm", post(fido2_confirm))
        .route("/api/account/fido2/remove", post(fido2_remove))
        .route("/api/admin/users/reset-fido2", post(reset_fido2))
        .route("/api/account/totp/begin", post(totp_begin))
        .route("/api/account/totp/confirm", post(totp_confirm))
        .route("/api/account/totp/remove", post(totp_remove))
        .route("/api/admin/users/reset-totp", post(reset_totp))
        .route("/api/signout", post(sign_out))
        .route("/api/account", get(me))
        .route("/api/chat/rooms", get(chat_rooms).post(chat_create))
        .route("/api/chat/dm", post(chat_dm))
        .route("/api/chat/people", get(chat_people))
        .route("/api/chat/wait", get(chat_wait))
        .route("/api/chat/rooms/{id}", get(chat_room))
        .route("/api/chat/rooms/{id}/do", post(chat_do))
        .route("/api/chat/rooms/{id}/read", post(chat_read))
        .route("/api/chat/rooms/{id}/typing", post(chat_typing))
        .route("/api/account/password", post(change_password))
        .route("/api/account/end-session", post(end_session))
        .route("/api/admin/overview", get(overview))
        .route("/api/admin/placement", post(set_placement))
        .route("/api/admin/mail", get(mail_state))
        .route("/api/admin/mail/action", post(mail_action))
        .route("/api/admin/web", get(web_sites))
        .route("/api/admin/web/action", post(web_action))
        .route("/api/admin/web/files", post(web_files))
        .route("/api/admin/nodes/info", get(nodes_info))
        .route("/api/admin/dns", get(dns_check))
        .route("/api/admin/nodes/disk", post(disk_format))
        .route("/api/admin/nodes/exposure", post(exposure_now))
        .route("/api/admin/nodes/withdraw-key", post(withdraw_key))
        .route("/api/admin/backups", post(set_backups))
        .route("/api/admin/web/file", get(web_file))
        .route("/api/admin/settings", post(set_settings))
        .route("/api/admin/storages/{name}", put(put_storage).delete(delete_storage))
        .route("/api/admin/storages/{name}/test", post(test_storage))
        .route("/api/admin/handover", post(handover))
        .route("/api/admin/users", post(create_user))
        .route("/api/admin/users/update", post(update_user))
        .route("/api/admin/users/{id}", get(user_card))
        .route("/api/admin/users/end-session", post(end_user_session))
        .route("/api/admin/groups", post(create_group))
        .route("/api/admin/groups/member", post(set_member))
        .route("/api/admin/grants", post(grant))
        .route("/api/admin/grants/revoke", post(revoke))
        .route("/api/admin/logs/query", post(query_logs))
        .route("/api/admin/logs/ql", post(logs_ql))
        .route("/api/admin/logs/info", get(logs_info))
        .route("/api/admin/rules", get(rules_view))
        .route("/api/admin/rules/test", post(test_rule))
        .route("/api/admin/rules/{name}", put(put_rule).delete(delete_rule))
        .route("/api/admin/bans", post(set_ban))
        .route("/api/admin/never-ban", put(set_never_ban))
        .route("/api/admin/table/{name}", post(table))
        .route("/api/admin/access", post(grant_access))
        .route("/api/admin/access/revoke", post(revoke_access))
        .route("/api/admin/users/bulk", post(bulk))
        .route("/api/admin/jobs/{id}", get(job))
}

fn secured(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert("content-security-policy", HeaderValue::from_static(CSP));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    r
}

fn ok(v: Value) -> Response {
    let mut r = Json(v).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    secured(r)
}

fn fail(status: StatusCode, msg: impl Into<String>) -> Response {
    let mut r = (status, Json(json!({ "error": msg.into() }))).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    secured(r)
}

async fn page(State(app): State<Arc<App>>, uri: axum::http::Uri) -> Response {
    if uri.path().starts_with("/api/") {
        return fail(StatusCode::NOT_FOUND, "no such call");
    }
    let mut r = Response::new(Body::from(app.index.clone()));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    secured(r)
}

async fn asset(State(app): State<Arc<App>>, Path((hash, name)): Path<(String, String)>, headers: HeaderMap) -> Response {
    let Some(a) = app.assets.get(&format!("{hash}/{name}")) else {
        return fail(StatusCode::NOT_FOUND, "no such file");
    };
    let gzip = !a.gz.is_empty() && headers.get_all(header::ACCEPT_ENCODING).iter().filter_map(|v| v.to_str().ok()).any(|v| v.contains("gzip"));
    let mut r = Response::new(if gzip { Body::from(a.gz.clone()) } else { Body::from(a.body) });
    let h = r.headers_mut();
    if gzip {
        h.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(a.mime));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    secured(r)
}

impl App {
    fn token<'a>(&self, headers: &'a HeaderMap) -> Option<&'a str> {
        headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            (k == self.opts.cookie).then_some(v)
        })
    }

    fn current(&self, headers: &HeaderMap) -> Option<(String, Session, User)> {
        let token = self.token(headers)?;
        let (s, u) = self.node.session(token)?;
        Some((token.to_string(), s, u))
    }

    fn cookie(&self, token: &str, max_age: u64, secure: bool) -> HeaderValue {
        let secure = if secure { "; Secure" } else { "" };
        HeaderValue::from_str(&format!("{}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}", self.opts.cookie))
            .expect("a cookie of hex")
    }

    fn password_set(&self, user: &str, password: &str) {
        if let Some(h) = &self.opts.hooks {
            h.password_set(user, password);
        }
    }
}

/// Requests that change something carry `x-shelf: 1`.
fn csrf(headers: &HeaderMap) -> Result<(), Response> {
    match headers.get("x-shelf") {
        Some(v) if v == "1" => Ok(()),
        _ => Err(fail(StatusCode::FORBIDDEN, "missing x-shelf header")),
    }
}

fn signed_in(app: &App, headers: &HeaderMap) -> Result<(String, Session, User), Response> {
    app.current(headers).ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "not signed in"))
}

/// Who acts: the signed-in user, their session and address.
fn actor(app: &App, headers: &HeaderMap, peer: IpAddr) -> Result<Actor, Response> {
    let (_, s, u) = signed_in(app, headers)?;
    Ok(Actor { user: u, sid: s.sid, addr: peer.to_string() })
}

/// The admin page and its calls need some role; what each may do, the node
/// checks.
fn staff(app: &App, headers: &HeaderMap, peer: IpAddr) -> Result<Actor, Response> {
    let a = actor(app, headers, peer)?;
    if !app.node.has_any_role(&a.user) {
        return Err(fail(StatusCode::FORBIDDEN, "you hold no role"));
    }
    Ok(a)
}

macro_rules! try_resp {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}

/// What a refusal is: the node says "you have no … right" or "you cannot
/// …" when the actor's roles do not allow it (403); anything else is a
/// request that cannot be done (400).
fn refusal(e: &str) -> StatusCode {
    if e.starts_with("you have no") || e.starts_with("you cannot") { StatusCode::FORBIDDEN } else { StatusCode::BAD_REQUEST }
}

fn done(r: Result<impl serde::Serialize, String>) -> Response {
    match r {
        Ok(v) => ok(json!(v)),
        Err(e) => fail(refusal(&e), e),
    }
}


#[derive(Deserialize)]
struct SignInBody {
    name: String,
    password: String,
}

async fn sign_in(State(app): State<Arc<App>>, Peer(peer, tls): Peer, headers: HeaderMap, Json(b): Json<SignInBody>) -> Response {
    try_resp!(csrf(&headers));
    let name = b.name.trim().to_lowercase();
    if !app.limiter.allowed(&name, peer) {
        return fail(StatusCode::TOO_MANY_REQUESTS, "too many failed sign-ins: try again in 15 minutes");
    }
    match app.node.sign_in_step(&name, &b.password, &peer.to_string()).await {
        Ok(SignInStep::Done { token, session }) => {
            app.limiter.clear(&name, peer);
            signed_in_now(&app, &token, &session, tls)
        }
        Ok(SignInStep::More { ticket, need, enrol }) => {
            app.limiter.clear(&name, peer);
            ok(json!({ "next": need, "ticket": ticket, "enrol": enrol }))
        }
        Err(e) => {
            app.limiter.fail(&name, peer);
            fail(StatusCode::UNAUTHORIZED, e)
        }
    }
}

fn signed_in_now(app: &App, token: &str, s: &Session, tls: bool) -> Response {
    let max_age = s.expires.saturating_sub(s.started) / 1000;
    let mut r = ok(json!({ "sid": s.sid }));
    r.headers_mut().insert(header::SET_COOKIE, app.cookie(token, max_age, tls));
    r
}

#[derive(Deserialize)]
struct TotpBody {
    ticket: String,
    code: String,
}

async fn sign_in_totp(State(app): State<Arc<App>>, Peer(peer, tls): Peer, headers: HeaderMap, Json(b): Json<TotpBody>) -> Response {
    try_resp!(csrf(&headers));
    match app.node.sign_in_totp(&b.ticket, &b.code, &peer.to_string()).await {
        Ok((token, session)) => signed_in_now(&app, &token, &session, tls),
        Err(e) => fail(StatusCode::UNAUTHORIZED, e),
    }
}

#[derive(Deserialize)]
struct CodeBody {
    code: String,
}

/// The relying party of a WebAuthn ceremony: the name the page was asked
/// for (an edge passes it on), and its origins.
fn rp(headers: &HeaderMap) -> crate::webauthn::Rp {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let (name, port) = match host.rsplit_once(':') {
        Some((n, p)) if !n.ends_with(']') || !n.contains('[') => (n.to_string(), p.parse().ok()),
        _ => (host.clone(), None),
    };
    crate::webauthn::Rp { origins: crate::webauthn::origins(&name, port), id: name }
}

#[derive(Deserialize)]
struct Fido2Body {
    ticket: String,
    #[serde(flatten)]
    answer: Fido2Answer,
}

async fn sign_in_fido2(State(app): State<Arc<App>>, Peer(peer, tls): Peer, headers: HeaderMap, Json(b): Json<Fido2Body>) -> Response {
    try_resp!(csrf(&headers));
    match app.node.sign_in_fido2(&b.ticket, &rp(&headers), &b.answer, &peer.to_string()).await {
        Ok((token, session)) => signed_in_now(&app, &token, &session, tls),
        Err(e) => fail(StatusCode::UNAUTHORIZED, e),
    }
}

async fn fido2_begin(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    done(app.node.begin_fido2(&a))
}

async fn fido2_confirm(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<Fido2Answer>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    done(app.node.confirm_fido2(&a, &rp(&headers), &b).await)
}

#[derive(Deserialize)]
struct KeyId {
    id: String,
}

async fn fido2_remove(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<KeyId>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    done(app.node.remove_fido2(&a, &b.id).await)
}

async fn reset_fido2(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<KeyId>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.reset_fido2(&a, &b.id).await)
}

async fn totp_begin(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    done(app.node.begin_totp(&a))
}

async fn totp_confirm(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<CodeBody>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    done(app.node.confirm_totp(&a, &b.code).await)
}

async fn totp_remove(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<CodeBody>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    done(app.node.remove_totp(&a, &b.code).await)
}

#[derive(Deserialize)]
struct UserId {
    id: String,
}

async fn reset_totp(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<UserId>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.reset_totp(&a, &b.id).await)
}

async fn sign_out(State(app): State<Arc<App>>, Peer(peer, tls): Peer, headers: HeaderMap) -> Response {
    try_resp!(csrf(&headers));
    if let Some(t) = app.token(&headers) {
        app.node.sign_out(t, &peer.to_string());
    }
    let mut r = ok(json!({}));
    r.headers_mut().insert(header::SET_COOKIE, app.cookie("", 0, tls));
    // what the browser kept of osada's names (a /web/ site's pages too) goes with the session (Round 20)
    r.headers_mut().insert(header::HeaderName::from_static("clear-site-data"), HeaderValue::from_static("\"cache\""));
    r
}

async fn me(State(app): State<Arc<App>>, Entry(via): Entry, headers: HeaderMap) -> Response {
    let (_, s, u) = try_resp!(signed_in(&app, &headers));
    let state = app.node.cluster_state();
    let p = &state.placement;
    let rule = p.sign_in_rule(u.ou).unwrap_or_default();
    let holders: Vec<NodeId> = p.secret_holders(u.ou).map(|h| h.nodes.into_iter().collect()).unwrap_or_default();
    let auth = state.settings.logs.get(&crate::audit::LogKind::Auth).map_or(0, |l| l.retention_days);
    ok(json!({
        "user": u,
        "staff": app.node.has_any_role(&u),
        "totp": app.node.has_totp(&u),
        "fido2": app.node.fido2_keys(&u).map(|l| l.into_iter().map(|k| json!({ "id": k.id, "name": k.name, "added": k.added })).collect::<Vec<_>>()),
        "ou_path": p.ou_path(u.ou),
        // Round 22: the account's UPN, a sign-in name too
        "upn": app.node.upn_domain().map(|d| format!("{}@{d}", u.name.to_lowercase())),
        // the node that answers (where the session is), and the one the browser is connected to
        // when that is another (the edge that relayed it)
        "node": app.node.me(),
        "node_name": p.node_label(app.node.me()),
        "via": via,
        "via_name": via.as_ref().map(|v| p.node_label(v)),
        "files": app.opts.files,
        "sid": s.sid,
        "rule": { "tier0": rule.tier0.map(|o| p.ou_path(o)), "second_factor": rule.second_factor.map(|o| p.ou_path(o)) },
        "secrets_on": holders,
        "sessions": app.node.sessions_of(&u.id),
        "session_days": state.settings.session_days,
        "auth_retention_days": auth,
        "caution": state.settings.sessions_outlive_logs(),
    }))
}

#[derive(Deserialize)]
struct PasswordBody {
    old: String,
    new: String,
}

async fn change_password(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<PasswordBody>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    let r = app.node.change_password(&a, &b.old, &b.new).await;
    if r.is_ok() {
        app.password_set(&a.user.id, &b.new);
    }
    done(r)
}

#[derive(Deserialize)]
struct SidBody {
    sid: String,
}

async fn end_session(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<SidBody>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    let user = a.user.id.clone();
    done(app.node.end_session(&a, &user, &b.sid))
}

async fn overview(State(app): State<Arc<App>>, Entry(via): Entry, Peer(peer, _): Peer, headers: HeaderMap) -> Response {
    let a = try_resp!(staff(&app, &headers, peer));
    let mut v = app.node.overview(&a.user);
    // the node the browser is connected to, when an edge relayed it
    v["via"] = json!(via);
    ok(v)
}

async fn set_placement(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(p): Json<Placement>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_placement(&a, p).await)
}

/// admin → mail (D-U38): the mail instance, its domains and mailboxes, each mailbox with the
/// account it belongs to (by the address the sync gave the account, else by the same name).
#[derive(Deserialize, Default)]
struct MailQuery {
    /// The mail instance (Round 18: several); none: the first, or the one of `user`'s mailbox.
    #[serde(default)]
    instance: Option<String>,
    #[serde(default)]
    user: Option<String>,
}

async fn mail_state(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, axum::extract::Query(q): axum::extract::Query<MailQuery>) -> Response {
    let a = try_resp!(staff(&app, &headers, peer));
    let mut op = json!({ "op": "state" });
    if let Some(i) = q.instance.filter(|i| !i.is_empty()) {
        op["instance"] = json!(i);
    }
    if let Some(u) = q.user.filter(|u| !u.is_empty()) {
        op["user"] = json!(u);
    }
    let mut v = match app.node.mail(&a, op).await {
        Ok(v) => v,
        Err(e) => return fail(StatusCode::FORBIDDEN, &e),
    };
    let by_mail: BTreeMap<String, String> = app.opts.hooks.as_ref().map(|h| h.mail_users()).unwrap_or_default().into_iter().map(|(u, m)| (m.to_lowercase(), u)).collect();
    // osada's mailboxes (D-U43): an account one names is osada's; for another, the account the old
    // sync gave it (or of the same name) is a suggestion to adopt
    let users = app.node.users();
    // the mailboxes on this instance (Round 18)
    let this = v["instance"]["name"].as_str().map(str::to_string);
    let boxes: BTreeMap<String, &User> =
        users.iter().filter(|u| app.node.mailbox_instance(u) == this).filter_map(|u| u.mailbox.as_ref().map(|m| (m.address.clone(), u))).collect();
    let mut have = std::collections::BTreeSet::new();
    if let Some(accounts) = v["result"]["accounts"].as_array_mut() {
        for acct in accounts {
            let email = acct["email"].as_str().unwrap_or("").to_lowercase();
            have.insert(email.clone());
            if let Some(u) = boxes.get(&email) {
                acct["user"] = json!({ "id": u.id, "name": u.name });
                acct["managed"] = json!(true);
                acct["off"] = json!(u.mailbox.as_ref().is_some_and(|m| m.off));
                acct["mailbox"] = json!(u.mailbox);
                continue;
            }
            let user = by_mail.get(&email).and_then(|id| app.node.find_user(id)).or_else(|| acct["name"].as_str().and_then(|n| app.node.find_user_by_name(n)));
            if let Some(u) = user.filter(|u| u.mailbox.is_none()) {
                acct["user"] = json!({ "id": u.id, "name": u.name });
            }
            acct["managed"] = json!(false);
        }
    }
    if v["result"].is_object() {
        let errors = app.node.mail_errors();
        let pending: Vec<Value> = boxes
            .iter()
            .filter(|(a, _)| !have.contains(*a))
            .map(|(a, u)| json!({ "address": a, "user": { "id": u.id, "name": u.name }, "error": errors.get(&u.id), "mailbox": u.mailbox }))
            .collect();
        v["result"]["pending"] = json!(pending);
        v["result"]["errors"] = json!(errors);
        let s = app.node.cluster_state().settings.mail;
        v["result"]["example"] = json!(s.address_for("anna"));
        v["result"]["settings"] = json!(s);
        // where a new mailbox goes, by OU (Round 18): the OU's path → its instance
        let p = app.node.cluster_state().placement;
        let by_ou: BTreeMap<String, Option<String>> = p.ous.keys().map(|o| (p.ou_path(*o), crate::node::mailadmin::instance_for(&p, *o))).collect();
        v["result"]["instance_by_ou"] = json!(by_ou);
    }
    ok(v)
}

async fn mail_action(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(op): Json<Value>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.mail(&a, op).await {
        Ok(v) => ok(v),
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

async fn web_sites(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap) -> Response {
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.web_overview(&a).await {
        Ok(v) => ok(v),
        Err(e) => fail(StatusCode::FORBIDDEN, &e),
    }
}

#[derive(Deserialize)]
struct WebAction {
    site: String,
    action: String,
}

async fn web_action(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<WebAction>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.web_action(&a, &b.site, &b.action).await {
        Ok(v) => ok(v),
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

async fn nodes_info(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap) -> Response {
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.nodes_info(&a).await {
        Ok(v) => ok(v),
        Err(e) => fail(StatusCode::FORBIDDEN, &e),
    }
}

async fn dns_check(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap) -> Response {
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.dns_check(&a).await {
        Ok(v) => ok(v),
        Err(e) => fail(StatusCode::FORBIDDEN, &e),
    }
}

#[derive(Deserialize)]
struct DiskFormat {
    node: String,
    device: String,
    label: String,
    confirm: String,
}

async fn disk_format(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<DiskFormat>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.disk_format(&a, &b.node, &b.device, &b.label, &b.confirm).await {
        Ok(v) => ok(v),
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

#[derive(Deserialize)]
struct WebFiles {
    site: String,
    op: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    all: bool,
}

async fn web_files(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<WebFiles>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.web_files(&a, &b.site, &b.op, &b.path, b.to, b.data, b.all).await {
        Ok(v) => ok(v),
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

#[derive(Deserialize)]
struct WebFile {
    site: String,
    path: String,
}

/// A site's file, to download (admin → web, T13).
async fn web_file(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, axum::extract::Query(q): axum::extract::Query<WebFile>) -> Response {
    use base64::Engine;
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.web_files(&a, &q.site, "read", &q.path, None, None, false).await {
        Ok(v) => match v["data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok()) {
            Some(b) => {
                let name = q.path.rsplit('/').next().unwrap_or("file").replace(['"', '\\', '\r', '\n'], "_");
                ([(header::CONTENT_TYPE, "application/octet-stream".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))], b).into_response()
            }
            None => fail(StatusCode::BAD_GATEWAY, "no data"),
        },
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

async fn set_settings(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(s): Json<Settings>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_settings(&a, s).await)
}

async fn put_storage(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Path(name): Path<String>, Json(c): Json<crate::storage::Change>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_storage(&a, &name, Some(c)).await)
}

async fn delete_storage(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Path(name): Path<String>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_storage(&a, &name, None).await)
}

async fn test_storage(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Path(name): Path<String>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.test_storage(&a, &name).await)
}

#[derive(Deserialize)]
struct HandoverBody {
    collection: String,
    to: String,
}

async fn handover(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<HandoverBody>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    let Some(id) = CollectionId::parse(&b.collection) else { return fail(StatusCode::BAD_REQUEST, "no such collection") };
    done(app.node.handover_as(&a, id, NodeId(b.to)).await)
}

#[derive(Deserialize)]
struct NewUser {
    name: String,
    display: String,
    ou: u64,
    password: String,
}

async fn create_user(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<NewUser>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    let name = b.name.trim().to_lowercase();
    done(app.node.create_user(&a, &name, b.display.trim(), OuId(b.ou), &b.password).await)
}

#[derive(Deserialize)]
struct UserChange {
    id: String,
    display: Option<String>,
    disabled: Option<bool>,
    password: Option<String>,
    outside: Option<String>,
    #[serde(default)]
    drop_old_admin_flag: bool,
}

async fn update_user(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<UserChange>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    if b.drop_old_admin_flag {
        return done(app.node.drop_legacy_admin(&a, &b.id).await);
    }
    let pw = b.password.clone();
    let r = app.node.update_user(&a, &b.id, b.display, b.disabled, b.password, b.outside).await;
    if let (Ok(_), Some(pw)) = (&r, pw) {
        app.password_set(&b.id, &pw);
    }
    done(r)
}

async fn user_card(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.user_card(&a.user, &id).await {
        Ok(v) => ok(v),
        Err(e) => fail(StatusCode::FORBIDDEN, e),
    }
}

#[derive(Deserialize)]
struct UserSession {
    user: String,
    sid: String,
}

async fn end_user_session(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<UserSession>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.end_session(&a, &b.user, &b.sid))
}

#[derive(Deserialize)]
struct NewGroup {
    name: String,
    display: String,
    ou: u64,
}

async fn create_group(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<NewGroup>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    let name = b.name.trim().to_lowercase();
    done(app.node.create_group(&a, &name, b.display.trim(), OuId(b.ou)).await)
}

#[derive(Deserialize)]
struct Membership {
    group: String,
    user: String,
    member: bool,
}

async fn set_member(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<Membership>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_member(&a, &b.group, &b.user, b.member).await)
}

#[derive(Deserialize)]
struct NewGrant {
    scope: u64,
    role: String,
    subject: Subject,
}

async fn grant(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<NewGrant>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.grant(&a, OuId(b.scope), &b.role, b.subject).await)
}

#[derive(Deserialize)]
struct Revocation {
    scope: u64,
    id: String,
}

async fn revoke(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<Revocation>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.revoke(&a, OuId(b.scope), &b.id).await)
}

async fn query_logs(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(q): Json<LogQuery>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.query_logs(&a.user, q).await {
        Ok(page) => ok(json!(page)),
        Err(e) => fail(refusal(&e), e),
    }
}

#[derive(serde::Deserialize)]
struct QlReq {
    q: String,
    from: i64,
    to: i64,
    #[serde(default)]
    limit: usize,
}

/// A query over the cluster's logs (D-U31).
async fn logs_ql(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(r): Json<QlReq>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.logs_ql(&a.user, r.q, r.from, r.to, if r.limit == 0 { 1000 } else { r.limit }).await {
        Ok(v) => ok(v),
        Err(e) => fail(refusal(&e), e),
    }
}

async fn logs_info(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap) -> Response {
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.logs_info(&a.user).await {
        Ok(v) => ok(v),
        Err(e) => fail(refusal(&e), e),
    }
}

async fn rules_view(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap) -> Response {
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.rules_view(&a.user).await {
        Ok(v) => ok(v),
        Err(e) => fail(refusal(&e), e),
    }
}

async fn put_rule(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Path(name): Path<String>, Json(r): Json<crate::rules::Rule>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_rule(&a, &name, Some(r)).await)
}

async fn delete_rule(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Path(name): Path<String>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_rule(&a, &name, None).await)
}

async fn test_rule(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(r): Json<crate::rules::Rule>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.test_rule(&a, r).await {
        Ok(v) => ok(v),
        Err(e) => fail(refusal(&e), e),
    }
}

#[derive(serde::Deserialize)]
struct BanReq {
    addr: String,
    /// 0: lift the ban
    #[serde(default)]
    secs: u64,
}

async fn set_ban(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(r): Json<BanReq>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_ban(&a, &r.addr, r.secs).await)
}

#[derive(serde::Deserialize)]
struct NeverBanReq {
    list: Vec<String>,
}

async fn set_never_ban(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(r): Json<NeverBanReq>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_never_ban(&a, r.list).await)
}

/// One page of a table (node/tables.rs).
async fn table(
    State(app): State<Arc<App>>,
    Peer(peer, _): Peer,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(q): Json<crate::table::Query>,
) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.table(&a.user, &name, &q) {
        Ok(page) => ok(json!(page)),
        Err(e) => fail(refusal(&e), e),
    }
}

#[derive(Deserialize)]
struct NewAccess {
    module: String,
    subject: Subject,
}

async fn grant_access(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<NewAccess>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.grant_access(&a, &b.module, b.subject).await)
}

#[derive(Deserialize)]
struct AccessId {
    id: String,
}

async fn revoke_access(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<AccessId>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.revoke_access(&a, &b.id).await)
}

#[derive(Deserialize)]
struct Bulk {
    selection: Selection,
    action: BulkAction,
}

/// A mass update of users, as a job (node/bulk.rs).
async fn bulk(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<Bulk>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.start_bulk(a, b.selection, b.action).map(|id| json!({ "job": id })))
}

async fn job(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.job(&id) {
        Some(j) if j.by == a.user.name => ok(json!(j)),
        _ => fail(StatusCode::NOT_FOUND, "no such job"),
    }
}

// ---- chat (§12.1, D-U42) ----------------------------------------------------

fn room_id(id: &str) -> Result<CollectionId, Response> {
    CollectionId::parse(id).ok_or_else(|| fail(StatusCode::NOT_FOUND, "no such room"))
}

fn chat_refusal(e: String) -> Response {
    let status = if e.contains("not in this room") || e.starts_with("only ") {
        StatusCode::FORBIDDEN
    } else if e == "no such room" {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    fail(status, e)
}

async fn chat_rooms(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let (_, _, u) = try_resp!(signed_in(&app, &headers));
    ok(app.node.chat_rooms(&u))
}

#[derive(Deserialize)]
struct NewRoom {
    name: String,
    #[serde(default)]
    members: Vec<String>,
}

async fn chat_create(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<NewRoom>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    match app.node.chat_create(&a, &b.name, &b.members).await {
        Ok(id) => ok(json!({ "id": id })),
        Err(e) => chat_refusal(e),
    }
}

#[derive(Deserialize)]
struct Dm {
    user: String,
}

async fn chat_dm(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<Dm>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    match app.node.chat_dm(&a, &b.user).await {
        Ok(id) => ok(json!({ "id": id })),
        Err(e) => chat_refusal(e),
    }
}

#[derive(Deserialize)]
struct PeopleQuery {
    #[serde(default)]
    q: String,
}

async fn chat_people(State(app): State<Arc<App>>, headers: HeaderMap, axum::extract::Query(q): axum::extract::Query<PeopleQuery>) -> Response {
    try_resp!(signed_in(&app, &headers));
    ok(json!({ "people": app.node.chat_people(&q.q) }))
}

#[derive(Deserialize)]
struct WaitQuery {
    #[serde(default)]
    v: u64,
}

/// A long poll: answers when anything in chat changed after version `v`, or after 25 s.
async fn chat_wait(State(app): State<Arc<App>>, headers: HeaderMap, axum::extract::Query(q): axum::extract::Query<WaitQuery>) -> Response {
    try_resp!(signed_in(&app, &headers));
    let v = app.node.chat_wait(q.v).await;
    ok(json!({ "v": v }))
}

#[derive(Deserialize)]
struct HistoryQuery {
    before: Option<u64>,
    after: Option<u64>,
    limit: Option<usize>,
}

async fn chat_room(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, axum::extract::Query(q): axum::extract::Query<HistoryQuery>) -> Response {
    let (_, _, u) = try_resp!(signed_in(&app, &headers));
    let id = try_resp!(room_id(&id));
    match app.node.chat_room(&u, id, q.before, q.after, q.limit.unwrap_or(60)) {
        Ok(v) => ok(v),
        Err(e) => chat_refusal(e),
    }
}

/// say, edit, delete, rename, topic, add (user names), remove (a user name), leave.
async fn chat_do(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Path(id): Path<String>, Json(mut b): Json<Value>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    let id = try_resp!(room_id(&id));
    // people come by name from the page
    match b["op"].as_str() {
        Some("add") => {
            let names: Vec<String> = b["users"].as_array().map(|l| l.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
            b["users"] = json!(try_resp!(app.node.chat_user_ids(&names).map_err(chat_refusal)));
        }
        Some("remove") => {
            let name = b["user"].as_str().unwrap_or("").to_string();
            b["user"] = json!(try_resp!(app.node.chat_user_ids(&[name]).map_err(chat_refusal))[0]);
        }
        _ => {}
    }
    let op: crate::chat::ChatOp = match serde_json::from_value(b) {
        Ok(op) => op,
        Err(e) => return fail(StatusCode::BAD_REQUEST, e.to_string()),
    };
    match app.node.chat_do(&a, id, op).await {
        Ok(seq) => ok(json!({ "seq": seq })),
        Err(e) => chat_refusal(e),
    }
}

#[derive(Deserialize)]
struct ReadBody {
    seq: u64,
}

async fn chat_read(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<ReadBody>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(actor(&app, &headers, peer));
    let id = try_resp!(room_id(&id));
    match app.node.chat_read(&a, id, b.seq).await {
        Ok(()) => ok(json!({})),
        Err(e) => chat_refusal(e),
    }
}

async fn chat_typing(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    try_resp!(csrf(&headers));
    let (_, _, u) = try_resp!(signed_in(&app, &headers));
    let id = try_resp!(room_id(&id));
    match app.node.chat_typing(&u, id) {
        Ok(()) => ok(json!({})),
        Err(e) => chat_refusal(e),
    }
}

#[derive(Deserialize)]
struct ExposureBody {
    node: String,
}

/// admin → nodes, a node's card: what it exposes, checked now by a peer (D-U35).
async fn exposure_now(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<ExposureBody>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.exposure_now(&a, &b.node).await {
        Ok(v) => ok(v),
        Err(e) => fail(refusal(&e), e),
    }
}

/// admin → nodes, a node's card: its signing key withdrawn (D-U45).
async fn withdraw_key(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(b): Json<ExposureBody>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    match app.node.withdraw_key(&a, &b.node).await {
        Ok(v) => ok(v),
        Err(e) => fail(refusal(&e), e),
    }
}

// ---- sign-in for other programs at this node (D-U45, crate::oidc) ---------------------------

/// The issuer as the request finds this node: its scheme and the name asked for.
fn oidc_issuer(app: &App, headers: &HeaderMap) -> Option<String> {
    let scheme = app.opts.oidc?;
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok())?.trim().to_ascii_lowercase();
    let ok = !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.:[]".contains(&b));
    ok.then(|| format!("{scheme}://{host}"))
}

fn oidc_public(v: Value) -> Response {
    let mut r = Json(v).into_response();
    r.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn oidc_discovery(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    match oidc_issuer(&app, &headers) {
        Some(iss) => oidc_public(crate::oidc::discovery(&iss)),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn oidc_jwks(State(app): State<Arc<App>>) -> Response {
    oidc_public(json!({ "keys": app.node.idp_jwks() }))
}

async fn oidc_preflight() -> Response {
    let mut r = StatusCode::NO_CONTENT.into_response();
    let h = r.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, OPTIONS"));
    h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("authorization, content-type"));
    r
}

fn oidc_session(app: &App, headers: &HeaderMap) -> Option<(User, u64)> {
    app.current(headers).map(|(_, s, u)| (u, s.started))
}

fn oidc_outcome(o: crate::oidc::Outcome) -> Response {
    use crate::oidc::Outcome;
    let to = |u: &str| match HeaderValue::from_str(u) {
        Ok(v) => (StatusCode::SEE_OTHER, [(header::LOCATION, v), (header::CACHE_CONTROL, HeaderValue::from_static("no-store"))]).into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    };
    match o {
        Outcome::Redirect(u) => to(&u),
        Outcome::Login { id, client, hint } => {
            let enc = |s: &str| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string();
            let mut u = format!("/signin?next={}&to={}", enc(&format!("/oidc/resume/{id}")), enc(&client));
            if let Some(h) = hint.filter(|h| crate::directory::valid_user_name(h.trim())) {
                u.push_str(&format!("&hint={}", enc(h.trim())));
            }
            to(&u)
        }
        Outcome::Error(m) => (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("sign-in: {m}\n")).into_response(),
    }
}

async fn oidc_authorize_get(State(app): State<Arc<App>>, headers: HeaderMap, axum::extract::Query(req): axum::extract::Query<crate::oidc::AuthReq>) -> Response {
    let Some(iss) = oidc_issuer(&app, &headers) else { return StatusCode::NOT_FOUND.into_response() };
    oidc_outcome(app.oidc.authorize(&app.node, &iss, req, oidc_session(&app, &headers)))
}

async fn oidc_authorize_post(State(app): State<Arc<App>>, headers: HeaderMap, axum::extract::Form(req): axum::extract::Form<crate::oidc::AuthReq>) -> Response {
    let Some(iss) = oidc_issuer(&app, &headers) else { return StatusCode::NOT_FOUND.into_response() };
    oidc_outcome(app.oidc.authorize(&app.node, &iss, req, oidc_session(&app, &headers)))
}

async fn oidc_resume(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let Some(iss) = oidc_issuer(&app, &headers) else { return StatusCode::NOT_FOUND.into_response() };
    oidc_outcome(app.oidc.resume(&app.node, &iss, &id, oidc_session(&app, &headers)))
}

async fn oidc_token(State(app): State<Arc<App>>, headers: HeaderMap, axum::extract::Form(req): axum::extract::Form<crate::oidc::TokenReq>) -> Response {
    let Some(iss) = oidc_issuer(&app, &headers) else { return StatusCode::NOT_FOUND.into_response() };
    let basic = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Basic "));
    let mut r = match app.oidc.token(&app.node, &iss, basic, &req) {
        Ok(v) => (StatusCode::OK, [(header::CACHE_CONTROL, "no-store"), (header::PRAGMA, "no-cache")], Json(v)).into_response(),
        Err((status, code, desc)) => {
            let mut r = (StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST), [(header::CACHE_CONTROL, "no-store")], Json(json!({ "error": code, "error_description": desc }))).into_response();
            if status == 401 {
                r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"osada\""));
            }
            r
        }
    };
    r.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    r
}

async fn oidc_userinfo(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let token = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    match token.and_then(|t| app.oidc.userinfo(&app.node, t)) {
        Some(v) => oidc_public(v),
        None => {
            let mut r = (StatusCode::UNAUTHORIZED, Json(json!({ "error": "invalid_token" }))).into_response();
            r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer error=\"invalid_token\""));
            r.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
            r
        }
    }
}

/// admin → files → backups: the live backups (Round 14).
async fn set_backups(State(app): State<Arc<App>>, Peer(peer, _): Peer, headers: HeaderMap, Json(list): Json<Vec<crate::audit::LiveBackup>>) -> Response {
    try_resp!(csrf(&headers));
    let a = try_resp!(staff(&app, &headers, peer));
    done(app.node.set_backups(&a, list).await)
}

// ---- contacts and calendar (Round 15): the person's, from Stalwart, joined with osada's people ----

/// /.well-known/caldav and carddav: to the mail instance's host, where Stalwart answers them.
async fn dav_discovery(State(app): State<Arc<App>>, uri: axum::http::Uri) -> Response {
    let p = app.node.cluster_state().placement;
    let host = p.modules.values().find(|m| m.kind == crate::placement::ModuleKind::Data(crate::placement::Domain::Mail)).and_then(|m| m.host.clone());
    match host {
        Some(h) => (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, format!("https://{h}{}", uri.path()))]).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn pim_issuer(app: &App) -> Result<String, Response> {
    app.opts.hooks.as_ref().and_then(|h| h.issuer()).ok_or_else(|| fail(StatusCode::SERVICE_UNAVAILABLE, "osada has no public address (OSADA_PUBLIC_URL) to sign for the mail server"))
}

fn responses(v: &Value) -> BTreeMap<String, Value> {
    v.as_array().map(|l| l.iter().filter_map(|r| Some((r[2].as_str()?.to_string(), r[1].clone()))).collect()).unwrap_or_default()
}

/// A site at `/web/<site>/` on osada's own names (Round 18): served here when this node serves
/// it, else asked of the node it runs on over the link. Osada's cookies stay here, cookies it
/// sets are dropped, and a redirect to one of its own paths stays under
/// /web/<site>/.
async fn web_site(State(app): State<Arc<App>>, Peer(ip, https): Peer, req: axum::extract::Request) -> Response {
    let path = req.uri().path().to_string();
    let rest = path.strip_prefix("/web/").unwrap_or("");
    let (name, sub) = rest.split_once('/').map_or((rest, None), |(n, s)| (n, Some(s)));
    let p = app.node.cluster_state().placement;
    let Some((name, m, site)) = crate::sites::under_web(&p, name) else {
        return (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "no site here\n").into_response();
    };
    let q = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let Some(sub) = sub else {
        return (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, format!("/web/{name}/{q}"))]).into_response();
    };
    // signed in to osada (Round 18; Round 20: every site at /web/ unless open to anyone): a page
    // goes to sign in and comes back, anything else is refused
    let guarded = !site.open;
    if guarded && app.current(req.headers()).is_none() {
        let page = req.method() == axum::http::Method::GET && req.headers().get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/html"));
        if page {
            let back = req.uri().path_and_query().map_or(path.clone(), |p| p.as_str().to_string());
            let to = format!("/signin?next={}", percent_encoding::utf8_percent_encode(&back, percent_encoding::NON_ALPHANUMERIC));
            return (StatusCode::FOUND, [(header::LOCATION, to), (header::CACHE_CONTROL, "no-store".to_string())]).into_response();
        }
        return (StatusCode::UNAUTHORIZED, [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], "sign in to osada first\n").into_response();
    }
    // no service worker (Round 20): it would answer the site's pages without asking osada
    if guarded && req.headers().get("service-worker").is_some() {
        return (StatusCode::FORBIDDEN, [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], "no service workers at /web/\n").into_response();
    }
    let (mut parts, body) = req.into_parts();
    parts.uri = match format!("/{sub}{q}").parse() {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "not a path\n").into_response(),
    };
    // osada's session is a cookie, which the browser sends to every path of osada's names: kept
    // here (an Authorization header the browser does not add by itself: the site's own sign-in)
    parts.headers.remove(header::COOKIE);
    let host = parts.headers.get(header::HOST).and_then(|v| v.to_str().ok()).map(|h| h.rsplit_once(':').filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit())).map_or(h, |(n, _)| n).to_ascii_lowercase()).unwrap_or_default();
    let v = crate::websrv::Visitor { ip, proto: if https { "https" } else { "http" }, host };
    let req = axum::extract::Request::from_parts(parts, body);
    let mut res = if crate::sites::served_by(&p, m, site, app.node.me()) {
        match app.node.site_web() {
            Some(w) => w.serve(name, site, req, &v).await,
            None => (StatusCode::SERVICE_UNAVAILABLE, "this node has no data folder\n").into_response(),
        }
    } else {
        crate::relay::forward_site(&app.node.link_handle(), &m.node, name, req, &v).await
    };
    res.headers_mut().remove(header::SET_COOKIE);
    if guarded {
        guard_cache(res.headers_mut());
    }
    let to = res.headers().get(header::LOCATION).and_then(|l| l.to_str().ok()).filter(|l| l.starts_with('/') && !l.starts_with("//")).map(|l| format!("/web/{name}{l}"));
    if let Some(to) = to.and_then(|t| HeaderValue::from_str(&t).ok()) {
        res.headers_mut().insert(header::LOCATION, to);
    }
    res
}

/// A signed-in site's answer is the browser's to keep only as long as osada says each time
/// (Round 20): private (no shared cache), and revalidated on every use (no-cache: a conditional
/// request through osada, which checks the session; 304 when it has not changed); no-store kept.
pub fn guard_cache(h: &mut HeaderMap) {
    let store = h.get_all(header::CACHE_CONTROL).iter().filter_map(|v| v.to_str().ok()).any(|v| v.to_ascii_lowercase().contains("no-store"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(if store { "private, no-store" } else { "private, no-cache" }));
    h.remove(header::EXPIRES);
    h.remove(header::AGE);
}

/// Contacts in groups, one search over them on the page: the people here (osada's directory),
/// the person's address books (Stalwart's contact cards), and whom they mailed with (the from,
/// to and cc of their newest 500 messages).
async fn pim_contacts(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let (_, _, u) = try_resp!(signed_in(&app, &headers));
    let people: Vec<Value> = app.node.users().into_iter().filter(|x| !x.disabled).map(|x| json!({
        "id": x.id, "name": x.name, "display": if x.display.trim().is_empty() { x.name.clone() } else { x.display.clone() },
        "email": x.mailbox.as_ref().map(|m| m.address.clone()),
    })).collect();
    let mut out = json!({ "people": people, "book": [], "mailed": [], "books": [] });
    if u.mailbox.is_none() {
        out["note"] = json!("you have no mailbox: only the people here");
        return ok(out);
    }
    let iss = try_resp!(pim_issuer(&app));
    let calls = json!([
        ["AddressBook/get", {}, "b"],
        ["ContactCard/query", { "limit": 2000 }, "q"],
        ["ContactCard/get", { "#ids": { "resultOf": "q", "name": "ContactCard/query", "path": "/ids" } }, "c"],
        ["Email/query", { "sort": [{ "property": "receivedAt", "isAscending": false }], "limit": 500 }, "eq"],
        ["Email/get", { "#ids": { "resultOf": "eq", "name": "Email/query", "path": "/ids" }, "properties": ["from", "to", "cc", "receivedAt"] }, "e"],
    ]);
    let r = match app.node.mail_as(&u, &iss, calls).await {
        Ok(v) => responses(&v),
        Err(e) => {
            out["error"] = json!(e);
            return ok(out);
        }
    };
    let books: BTreeMap<String, String> = r.get("b").and_then(|b| b["list"].as_array()).map(|l| l.iter().filter_map(|b| Some((b["id"].as_str()?.to_string(), b["name"].as_str().unwrap_or("").to_string()))).collect()).unwrap_or_default();
    out["books"] = json!(books.values().collect::<Vec<_>>());
    let book: Vec<Value> = r.get("c").and_then(|c| c["list"].as_array()).cloned().unwrap_or_default().into_iter().map(|c| {
        let name = c["name"]["full"].as_str().map(str::to_string).unwrap_or_else(|| {
            c["name"]["components"].as_array().map(|l| l.iter().filter_map(|x| x["value"].as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default()
        });
        let vals = |k: &str, f: &str| -> Vec<String> { c[k].as_object().map(|m| m.values().filter_map(|x| x[f].as_str().map(str::to_string)).collect()).unwrap_or_default() };
        let org = c["organizations"].as_object().and_then(|m| m.values().next()).and_then(|o| o["name"].as_str()).unwrap_or("").to_string();
        let in_books: Vec<&String> = c["addressBookIds"].as_object().map(|m| m.keys().filter_map(|k| books.get(k)).collect()).unwrap_or_default();
        json!({ "id": c["id"], "name": name, "emails": vals("emails", "address"), "phones": vals("phones", "number"), "org": org, "books": in_books })
    }).collect();
    out["book"] = json!(book);
    // whom they mailed with: every address but their own, with a name, how often and when last
    let own: Vec<String> = u.mailbox.as_ref().map(|m| m.claims().into_iter().map(str::to_lowercase).collect()).unwrap_or_default();
    let mut seen: BTreeMap<String, (String, u32, String)> = BTreeMap::new();
    for e in r.get("e").and_then(|e| e["list"].as_array()).cloned().unwrap_or_default() {
        let when = e["receivedAt"].as_str().unwrap_or("").to_string();
        for k in ["from", "to", "cc"] {
            for a in e[k].as_array().cloned().unwrap_or_default() {
                let Some(addr) = a["email"].as_str().map(str::to_lowercase) else { continue };
                if own.contains(&addr) || own.iter().any(|o| o.starts_with("*@") && addr.ends_with(&o[1..])) {
                    continue;
                }
                let name = a["name"].as_str().unwrap_or("").to_string();
                let x = seen.entry(addr).or_insert((String::new(), 0, String::new()));
                if x.0.is_empty() {
                    x.0 = name;
                }
                x.1 += 1;
                if when > x.2 {
                    x.2 = when.clone();
                }
            }
        }
    }
    let mut mailed: Vec<Value> = seen.into_iter().map(|(a, (n, c, w))| json!({ "email": a, "name": n, "count": c, "last": w })).collect();
    mailed.sort_by(|a, b| b["last"].as_str().cmp(&a["last"].as_str()));
    out["mailed"] = json!(mailed);
    ok(out)
}

#[derive(Deserialize)]
struct Range {
    from: String,
    to: String,
}

/// The person's calendars and their events between `from` and `to` (UTC, RFC 3339), repeats
/// expanded.
async fn pim_calendar(State(app): State<Arc<App>>, headers: HeaderMap, axum::extract::Query(q): axum::extract::Query<Range>) -> Response {
    let (_, _, u) = try_resp!(signed_in(&app, &headers));
    if u.mailbox.is_none() {
        return fail(StatusCode::NOT_FOUND, "you have no mailbox, so no calendar");
    }
    let iss = try_resp!(pim_issuer(&app));
    let calls = json!([
        ["Calendar/get", {}, "cals"],
        ["CalendarEvent/query", { "filter": { "after": q.from, "before": q.to }, "expandRecurrences": true, "sort": [{ "property": "start", "isAscending": true }], "limit": 2000 }, "q"],
        ["CalendarEvent/get", { "#ids": { "resultOf": "q", "name": "CalendarEvent/query", "path": "/ids" },
            "properties": ["title", "start", "duration", "timeZone", "showWithoutTime", "calendarIds", "locations", "description", "recurrenceId", "baseEventId"] }, "ev"],
    ]);
    match app.node.mail_as(&u, &iss, calls).await {
        Ok(v) => {
            let r = responses(&v);
            ok(json!({ "calendars": r.get("cals").map(|c| c["list"].clone()), "events": r.get("ev").map(|e| e["list"].clone()), "errors": v.as_array().map(|l| l.iter().filter(|x| x[0] == "error").cloned().collect::<Vec<_>>()) }))
        }
        Err(e) => fail(StatusCode::BAD_GATEWAY, e),
    }
}

#[derive(Deserialize)]
struct NewEvent {
    calendar: String,
    title: String,
    /// local date-time, `2026-11-18T10:00:00`
    start: String,
    /// ISO 8601, `PT1H`, `P1D`
    duration: String,
    #[serde(default)]
    time_zone: Option<String>,
    #[serde(default)]
    all_day: bool,
    #[serde(default)]
    location: String,
    #[serde(default)]
    description: String,
}

async fn pim_event(State(app): State<Arc<App>>, headers: HeaderMap, Json(b): Json<NewEvent>) -> Response {
    try_resp!(csrf(&headers));
    let (_, _, u) = try_resp!(signed_in(&app, &headers));
    let iss = try_resp!(pim_issuer(&app));
    let mut ev = json!({ "calendarIds": { b.calendar.clone(): true }, "title": b.title.trim(), "start": b.start, "duration": b.duration, "showWithoutTime": b.all_day });
    if let Some(tz) = b.time_zone.filter(|t| !t.is_empty() && !b.all_day) {
        ev["timeZone"] = json!(tz);
    }
    if !b.location.trim().is_empty() {
        ev["locations"] = json!({ "l1": { "name": b.location.trim() } });
    }
    if !b.description.trim().is_empty() {
        ev["description"] = json!(b.description.trim());
    }
    match app.node.mail_as(&u, &iss, json!([["CalendarEvent/set", { "create": { "n": ev } }, "s"]])).await {
        Ok(v) => {
            let r = responses(&v);
            match r.get("s") {
                Some(s) if s["created"]["n"].is_object() => ok(json!({ "id": s["created"]["n"]["id"] })),
                Some(s) => fail(StatusCode::BAD_REQUEST, format!("the mail server: {}", s["notCreated"]["n"])),
                None => fail(StatusCode::BAD_GATEWAY, format!("the mail server: {v}")),
            }
        }
        Err(e) => fail(StatusCode::BAD_GATEWAY, e),
    }
}
