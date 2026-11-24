//! apptab: apps1's apps (`OSADA_APPTAB_UPSTREAM`, http://192.168.1.11) in a tab of osada, for the users
//! it is on for (`users.apptab`, admin → users). They are shown at a name of their own
//! (`OSADA_APPTAB_HOST`, apptab.example.org): apps1's pages are then not on osada's origin (they
//! could not act as the user in osada) and their paths from the root (/x11/…, /tracker/…) stay as they are.
//!
//! Nothing goes to apps1 without a pass. The tab asks for a ticket (`POST /api/apptab/ticket`: one minute,
//! once), the frame opens `https://<host>/_apptab/enter?t=<ticket>`, and that sets the pass, a cookie of
//! that name for eight hours. Both are signed with a key made from the edge's token, so the edge (osada-edge
//! on edge1, which implements the same) checks them itself and passes nothing else on; here every request
//! is checked again: the pass, the osada session it was made for (logged out: over), the account (still on,
//! not disabled; through the edge, from its networks). At home the name points here (the https listener).
//!
//! apps1 signs its users in at osada (the `apptab` program, oidc.rs) at addresses only the LAN reaches: in
//! its answers `http://192.168.1.11` becomes `https://<host>` and osada's LAN address its public one, and
//! osada, signing someone in over https, sends them back to the upstream's callback at `<host>` (oidc.rs).
//!
//! Inside osada 0.2 this is the module kind `proxy` (docs/PLAN-0.2.0.md, D-U7): any number of instances,
//! each a name of its own in front of an upstream, from the node control (`Proxies::set`); who may use one
//! is access to the instance (auth::Bridge::may_use). The instance `apptab` keeps the tickets and passes
//! osada-edge checks (kinds `ticket` and `pass`); the others' are `ticket:<name>` and `pass:<name>`, so a
//! pass of one is nothing at another. On its own (no bridge) it is OSADA_APPTAB_* and `users.apptab`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use futures_util::TryStreamExt;
use ring::hmac;
use serde_json::json;

use crate::osada1::{
    auth::{self, User},
    crypto,
    db::now,
    edge::{self, Remote},
    error::{AppError, Res},
    tls::Https,
    App,
};

pub const COOKIE: &str = "shelf_apptab";
pub const TICKET_SECS: i64 = 60;
pub const PASS_SECS: i64 = 8 * 3600;
pub const ENTER: &str = "/_apptab/enter";

pub struct Apptab {
    /// the instance's name (`apptab` for OSADA_APPTAB_*), and what the tab says
    pub name: String,
    pub title: String,
    /// the name apps1's apps are shown at, e.g. apptab.example.org
    pub host: String,
    /// where they are, e.g. http://192.168.1.11 (no path)
    pub upstream: String,
    key: hmac::Key,
    /// tickets taken, until they expire
    used: Mutex<HashMap<String, i64>>,
    http: reqwest::Client,
}

/// A ticket or a pass, as the edge reads it too: `<expires>.<session>.<user id>.<signature>`, the signature
/// HMAC-SHA256 over `<kind>:<expires>.<session>.<user id>` with HMAC-SHA256(edge token, "shelf apptab v1")
/// as the key; `<session>` is the session's token_hash.
#[derive(Debug, PartialEq)]
pub struct Claims {
    pub expires: i64,
    pub session: String,
    pub user: i64,
}

pub fn key(edge_token: &str) -> hmac::Key {
    let k = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, edge_token.as_bytes()), b"shelf apptab v1");
    hmac::Key::new(hmac::HMAC_SHA256, k.as_ref())
}

pub fn sign(key: &hmac::Key, kind: &str, c: &Claims) -> String {
    let body = format!("{}.{}.{}", c.expires, c.session, c.user);
    format!("{body}.{}", hex::encode(hmac::sign(key, format!("{kind}:{body}").as_bytes())))
}

/// The claims of a token of `kind` signed with `key`, if it is one and has not expired.
pub fn verify(key: &hmac::Key, kind: &str, s: &str, t: i64) -> Option<Claims> {
    let mut p = s.splitn(4, '.');
    let (exp, session, user, sig) = (p.next()?, p.next()?, p.next()?, p.next()?);
    let hexed = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hexed(session, 64) || !hexed(sig, 64) || exp.len() > 12 || user.len() > 18 {
        return None;
    }
    let (expires, user) = (exp.parse::<i64>().ok()?, user.parse::<i64>().ok()?);
    hmac::verify(key, format!("{kind}:{exp}.{session}.{user}").as_bytes(), &hex::decode(sig).ok()?).ok()?;
    (expires > t).then(|| Claims { expires, session: session.to_string(), user })
}

/// Where the frame goes after the ticket: a path here, nothing else.
pub fn safe_to(to: &str) -> &str {
    if to.starts_with('/') && !to.starts_with("//") && !to.contains('\\') && !to.chars().any(|c| c.is_control()) {
        to
    } else {
        "/"
    }
}

impl Apptab {
    /// `secret`: the edge's token (osada-edge checks passes with it), or one of this server's own.
    pub fn new(name: &str, title: &str, host: &str, upstream: &str, secret: &str) -> anyhow::Result<Self> {
        let upstream = upstream.trim().trim_end_matches('/').to_string();
        anyhow::ensure!(upstream.starts_with("http://") || upstream.starts_with("https://"), "OSADA_APPTAB_UPSTREAM: http(s)://host[:port]");
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60))
            .http1_only()
            .build()?;
        Ok(Apptab {
            name: name.into(),
            title: title.into(),
            host: host.trim().to_ascii_lowercase(),
            upstream,
            key: key(secret),
            used: Mutex::new(HashMap::new()),
            http,
        })
    }

    /// The kind of a ticket or pass of this instance (apptab's as osada-edge knows them).
    fn kind(&self, k: &str) -> String {
        if self.name == "apptab" { k.to_string() } else { format!("{k}:{}", self.name) }
    }

    fn same(&self, d: &ProxyDef) -> bool {
        self.name == d.name && self.title == d.title && self.host == d.host && self.upstream == d.upstream.trim_end_matches('/')
    }

    pub fn origin(&self) -> String {
        format!("https://{}", self.host)
    }

    /// An address on the upstream as a browser away from the LAN reaches it: at the apptab name.
    pub fn outside(&self, uri: &str) -> Option<String> {
        let rest = uri.strip_prefix(&self.upstream)?;
        (rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')).then(|| format!("{}{rest}", self.origin()))
    }

    fn take(&self, ticket: &str, t: i64) -> Option<Claims> {
        let c = verify(&self.key, &self.kind("ticket"), ticket, t).filter(|c| c.expires <= t + TICKET_SECS + 5)?;
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        used.retain(|_, exp| *exp > t);
        used.insert(ticket.to_string(), c.expires).is_none().then_some(c)
    }
}

/// A proxy instance as the node control has it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProxyDef {
    pub name: String,
    pub title: String,
    pub host: String,
    pub upstream: String,
}

/// The proxy instances in use.
pub struct Proxies {
    list: std::sync::RwLock<Vec<Arc<Apptab>>>,
    /// what their passes are signed with
    secret: String,
}

impl Proxies {
    pub fn new(list: Vec<Apptab>, secret: String) -> Self {
        Proxies { list: std::sync::RwLock::new(list.into_iter().map(Arc::new).collect()), secret }
    }

    pub fn all(&self) -> Vec<Arc<Apptab>> {
        self.list.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn by_host(&self, host: &str) -> Option<Arc<Apptab>> {
        self.all().into_iter().find(|m| m.host == host)
    }

    pub fn by_name(&self, name: &str) -> Option<Arc<Apptab>> {
        self.all().into_iter().find(|m| m.name == name)
    }

    /// The instances from the node control; those that did not change are kept as they are (their
    /// tickets taken). True when something changed.
    pub fn set(&self, defs: &[ProxyDef]) -> bool {
        let old = self.all();
        if old.len() == defs.len() && defs.iter().all(|d| old.iter().any(|m| m.same(d))) {
            return false;
        }
        let mut list = Vec::new();
        for d in defs {
            match old.iter().find(|m| m.same(d)) {
                Some(m) => list.push(m.clone()),
                None => match Apptab::new(&d.name, &d.title, &d.host, &d.upstream, &self.secret) {
                    Ok(m) => list.push(Arc::new(m)),
                    Err(e) => tracing::warn!("proxy {}: {e}", d.name),
                },
            }
        }
        *self.list.write().unwrap_or_else(|e| e.into_inner()) = list;
        true
    }

    /// What pages may show in a frame: the instances' names and upstreams.
    pub fn frames(&self) -> Vec<String> {
        self.all().iter().flat_map(|m| [m.origin(), m.upstream.clone()]).collect()
    }
}

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/apptab/ticket", post(ticket))
}

/// May this account use the instance `name` (and is it there at all)?
pub fn on_for(app: &App, user: i64, name: &str) -> Res<bool> {
    if app.proxies.by_name(name).is_none() {
        return Ok(false);
    }
    if let Some(b) = &app.bridge {
        let sub: Option<String> = app.db.opt(|c| c.query_row("SELECT sub FROM users WHERE id = ?1 AND disabled = 0", [user], |r| r.get(0)))?;
        return Ok(sub.is_some_and(|s| b.may_use(&s, name)));
    }
    Ok(name == "apptab"
        && app.db.opt(|c| c.query_row("SELECT apptab FROM users WHERE id = ?1 AND disabled = 0", [user], |r| r.get::<_, bool>(0)))?.unwrap_or(false))
}

/// The instances this account may use: `[{name, title}]`.
pub fn usable(app: &App, user: i64) -> Res<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for m in app.proxies.all() {
        if on_for(app, user, &m.name)? {
            out.push(json!({ "name": m.name, "title": if m.title.is_empty() { &m.name } else { &m.title } }));
        }
    }
    Ok(out)
}

#[derive(serde::Deserialize)]
struct Which {
    name: Option<String>,
}

/// A ticket for the frame, for the session this request came with (`?name=`: the instance, apptab when none).
async fn ticket(State(app): State<Arc<App>>, user: User, headers: HeaderMap, axum::extract::Query(w): axum::extract::Query<Which>) -> Res<Json<serde_json::Value>> {
    let name = w.name.unwrap_or_else(|| "apptab".into());
    let Some(mv) = app.proxies.by_name(&name) else { return Err(AppError::not_found()) };
    if !on_for(&app, user.id, &name)? {
        return Err(AppError::forbidden());
    }
    let token = auth::cookie_token(&headers).ok_or_else(AppError::unauthorized)?;
    let c = Claims { expires: now() + TICKET_SECS, session: crypto::sha256_hex(&token), user: user.id };
    let t = sign(&mv.key, &mv.kind("ticket"), &c);
    Ok(Json(json!({ "url": format!("{}{ENTER}?t={t}&to=/", mv.origin()), "origin": mv.origin(), "lan": format!("{}/", mv.upstream) })))
}

/// Requests for the apptab name: the edge says so (`x-shelf-host`, after its own check), or at home the
/// name itself (the https listener). They never reach osada's own routes.
pub async fn route(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let edge = req.extensions().get::<Remote>().is_some();
    let host = if edge {
        req.headers().get("x-shelf-host").and_then(|v| v.to_str().ok()).map(str::to_string)
    } else {
        req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).map(|h| h.rsplit_once(':').map_or(h, |(n, _)| n).to_ascii_lowercase())
    };
    let Some(mv) = host.as_deref().and_then(|h| app.proxies.by_host(h)) else {
        if edge && host.is_some() {
            return refused("not this name");
        }
        return next.run(req).await;
    };
    if req.extensions().get::<Https>().is_none() && !edge {
        return refused("https only");
    }
    if req.uri().path() == ENTER {
        return enter(&mv, &req);
    }
    match gate(&app, &mv, &req) {
        Ok(()) => proxy(&app, &mv, req).await,
        Err(why) => refused(why),
    }
}

/// A ticket becomes the pass (here only at home: the edge does this itself).
fn enter(mv: &Apptab, req: &Request) -> Response {
    let q: HashMap<String, String> = url_query(req.uri().query().unwrap_or(""));
    let t = now();
    let Some(c) = q.get("t").and_then(|tk| mv.take(tk, t)) else { return refused("the ticket is not valid (any more): open apptab again in osada") };
    let pass = sign(&mv.key, &mv.kind("pass"), &Claims { expires: t + PASS_SECS, session: c.session, user: c.user });
    let to = safe_to(q.get("to").map_or("/", String::as_str)).to_string();
    let cookie = format!("{COOKIE}={pass}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={PASS_SECS}");
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, to), (header::SET_COOKIE, cookie), (header::CACHE_CONTROL, "no-store".into()), (header::REFERRER_POLICY, "no-referrer".into())],
    )
        .into_response()
}

fn url_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), percent_encoding::percent_decode_str(v).decode_utf8_lossy().into_owned()))
        .collect()
}

fn pass_of(h: &HeaderMap) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
}

/// The pass, the session it was made for, the account (on, not disabled; through the edge, from its networks).
fn gate(app: &App, mv: &Apptab, req: &Request) -> Result<(), &'static str> {
    let t = now();
    let c = pass_of(req.headers()).and_then(|p| verify(&mv.key, &mv.kind("pass"), &p, t)).ok_or("no pass: open it in osada")?;
    let found = match &app.bridge {
        // Inside 0.2: the session is 0.2's; the account's row says the rest.
        Some(b) => match b.person_by_hash(&c.session).filter(|p| !p.disabled && b.may_use(&p.sub, &mv.name)) {
            None => None,
            Some(p) => app
                .db
                .opt(|db| {
                    db.query_row(
                        "SELECT remote_cidrs FROM users WHERE id = ?1 AND sub = ?2 AND disabled = 0",
                        rusqlite::params![c.user, p.sub],
                        |r| r.get::<_, String>(0),
                    )
                })
                .map_err(|_| "the server could not check the pass")?,
        },
        None => app
            .db
            .opt(|db| {
                db.query_row(
                    "SELECT u.remote_cidrs FROM sessions s JOIN users u ON u.id = s.user_id
                     WHERE s.token_hash = ?1 AND s.expires > ?2 AND u.id = ?3 AND u.disabled = 0 AND u.apptab = 1",
                    rusqlite::params![c.session, t, c.user],
                    |r| r.get::<_, String>(0),
                )
            })
            .map_err(|_| "the server could not check the pass")?,
    };
    let cidrs = found.ok_or("the osada session of this pass has ended, or the access: open it in osada again")?;
    if let Some(Remote(ip)) = req.extensions().get::<Remote>() {
        if !edge::allowed(app, &cidrs, *ip) {
            return Err("this account cannot be used from here");
        }
    }
    Ok(())
}

fn refused(why: &str) -> Response {
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>apptab</title>\
         <body style=\"font:14px monospace;background:#111;color:#ccc;padding:2em\"><p>apptab: {}</p>",
        html_escape(why)
    );
    (StatusCode::UNAUTHORIZED, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], page).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Not passed on, either way.
const HOP: &[&str] = &[
    "connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding",
    "upgrade", "host", "x-real-ip", "x-forwarded-for", "x-forwarded-proto", "x-forwarded-host", "forwarded",
];

fn hop(name: &str) -> bool {
    HOP.contains(&name) || name.starts_with("x-shelf-")
}

async fn proxy(app: &App, mv: &Apptab, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let pq = parts.uri.path_and_query().map_or("/", |p| p.as_str());
    let mut h = HeaderMap::new();
    for (k, v) in &parts.headers {
        if hop(k.as_str()) || k == header::COOKIE {
            continue;
        }
        // what apps1 checks a form against is its own address
        if k == header::ORIGIN || k == header::REFERER {
            if let Some(rest) = v.to_str().ok().and_then(|s| s.strip_prefix(mv.origin().as_str())) {
                if let Ok(nv) = HeaderValue::from_str(&format!("{}{rest}", mv.upstream)) {
                    h.append(k, nv);
                }
                continue;
            }
        }
        h.append(k, v.clone());
    }
    // apps1's cookies, without the pass
    let cookies: Vec<&str> = parts
        .headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|kv| !kv.is_empty() && kv.split_once('=').is_none_or(|(k, _)| k != COOKIE))
        .collect();
    if !cookies.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&cookies.join("; ")) {
            h.insert(header::COOKIE, v);
        }
    }
    let has_body = parts.headers.contains_key(header::CONTENT_LENGTH) || parts.headers.contains_key(header::TRANSFER_ENCODING);
    let mut rb = mv.http.request(parts.method.clone(), format!("{}{pq}", mv.upstream)).headers(h);
    if has_body {
        rb = rb.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }
    let r = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("{}: {} {pq}: {e}", mv.name, parts.method);
            return (StatusCode::BAD_GATEWAY, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{}: {} does not answer\n", mv.name, mv.upstream)).into_response();
        }
    };
    let mut b = Response::builder().status(r.status());
    let out = b.headers_mut().expect("fresh builder");
    for (k, v) in r.headers() {
        let name = k.as_str();
        if hop(name) || k == header::X_FRAME_OPTIONS {
            continue;
        }
        if k == header::LOCATION {
            if let Some(nv) = v.to_str().ok().map(|l| relocate(app, mv, l)).and_then(|l| HeaderValue::from_str(&l).ok()) {
                out.append(k, nv);
            }
            continue;
        }
        if k == header::SET_COOKIE {
            if let Some(nv) = v.to_str().ok().map(cookie_here).and_then(|c| HeaderValue::from_str(&c).ok()) {
                out.append(k, nv);
            }
            continue;
        }
        out.append(k, v.clone());
    }
    // shown in osada's frame, nowhere else
    let ancestors = app.cfg.public_url.as_deref().map_or_else(|| "'self'".to_string(), |u| format!("'self' {u}"));
    if let Ok(v) = HeaderValue::from_str(&format!("frame-ancestors {ancestors}")) {
        out.append(header::CONTENT_SECURITY_POLICY, v);
    }
    out.insert(HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff"));
    b.body(Body::from_stream(r.bytes_stream().map_err(std::io::Error::other))).expect("response")
}

/// A redirect of apps1's as the browser can follow it: the upstream at the apptab name, osada's LAN
/// address (its sign-in) at the public one.
fn relocate(app: &App, mv: &Apptab, l: &str) -> String {
    if let Some(o) = mv.outside(l) {
        return o;
    }
    if let (Some(iss), Some(public)) = (app.cfg.oidc_issuer.as_deref(), app.cfg.public_url.as_deref()) {
        if let Some(rest) = l.strip_prefix(iss).filter(|r| r.is_empty() || r.starts_with('/') || r.starts_with('?')) {
            return format!("{public}{rest}");
        }
    }
    l.to_string()
}

/// apps1's cookie, for the apptab name only (no Domain), https only.
fn cookie_here(c: &str) -> String {
    let mut parts: Vec<&str> = c.split(';').map(str::trim).filter(|p| !p.to_ascii_lowercase().starts_with("domain=")).collect();
    if !parts.iter().any(|p| p.eq_ignore_ascii_case("secure")) {
        parts.push("Secure");
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn tokens() {
        let k = key("test-token");
        let c = Claims { expires: 2_000_000_000, session: SESSION.into(), user: 7 };
        let t = sign(&k, "ticket", &c);
        // the edge (edge/src/apptab.rs) has the same vector
        assert_eq!(t, format!("2000000000.{SESSION}.7.{}", "cb96f6603d6a831a16ceb523eabc10303afc19aadbf2d03c775dc8a6c4aa307a"));
        assert_eq!(verify(&k, "ticket", &t, 1_999_999_999), Some(Claims { expires: 2_000_000_000, session: SESSION.into(), user: 7 }));
        assert_eq!(verify(&k, "pass", &t, 1_999_999_999), None, "a ticket is not a pass");
        assert_eq!(verify(&k, "ticket", &t, 2_000_000_000), None, "expired");
        assert_eq!(verify(&key("other"), "ticket", &t, 0), None);
        assert_eq!(verify(&k, "ticket", &t.replace(".7.", ".8."), 0), None);
        assert_eq!(verify(&k, "ticket", "1.2.3.4", 0), None);
    }

    #[test]
    fn addresses() {
        assert_eq!(safe_to("/tracker/?a=1"), "/tracker/?a=1");
        for bad in ["//evil.example/", "https://evil.example/", "/\\evil", "x", "", "/a\nb"] {
            assert_eq!(safe_to(bad), "/", "{bad:?}");
        }
        let mv = Apptab::new("apptab", "", "Apptab.example", "http://192.168.1.11/", "t").unwrap();
        assert_eq!(mv.outside("http://192.168.1.11/oauth2/callback?code=1").as_deref(), Some("https://apptab.example/oauth2/callback?code=1"));
        assert_eq!(mv.outside("http://192.168.1.11").as_deref(), Some("https://apptab.example"));
        assert_eq!(mv.outside("http://192.168.1.110/x"), None);
        assert_eq!(cookie_here("a=1; Path=/x; Domain=192.168.1.11; HttpOnly"), "a=1; Path=/x; HttpOnly; Secure");
        assert_eq!(cookie_here("a=1; secure"), "a=1; secure");
    }

    #[test]
    fn single_use() {
        let mv = Apptab::new("apptab", "", "m.example", "http://u", "tok").unwrap();
        let t = 1_000;
        let tk = sign(&mv.key, "ticket", &Claims { expires: t + 60, session: SESSION.into(), user: 1 });
        assert!(mv.take(&tk, t).is_some());
        assert!(mv.take(&tk, t).is_none(), "once");
        let long = sign(&mv.key, "ticket", &Claims { expires: t + 3600, session: SESSION.into(), user: 1 });
        assert!(mv.take(&long, t).is_none(), "a ticket is short");
    }
}
