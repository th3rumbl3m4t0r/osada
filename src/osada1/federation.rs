//! Federation with other osada instances (Round 22).
//!
//! Every instance publishes `/.well-known/osada-federation`: its base URL, the domain of its UPNs,
//! its API and the key it signs with (its OpenID provider's RS256 key). Unauthenticated, the API
//! answers one question: `GET {api}/who/{upn}`: is that exact UPN someone here, yes or no; nothing
//! else about them, never a list, and only a few questions a minute from one address (a federated
//! instance's signed questions are not counted).
//!
//! Everything else is between instances whose administrators federated them (each lists the
//! other: admin → federation). Each request is signed: a JWT by the asking instance's key, for the
//! other's base URL, with the method, the path and a hash of the body, good for two minutes, once.
//!
//!   GET  {api}/hello                  a signed ping: do you federate with me?
//!   POST {api}/shares                 the owner's instance offers someone here a share
//!   POST {api}/shares/{id}/revoke     the owner stopped sharing it
//!   POST {api}/shares/{id}/decline    the recipient dropped it
//!   GET  {api}/shares/{id}/ls?path=   what is in it (with the share's secret)
//!   GET  {api}/shares/{id}/raw?path=  a file of it (with the share's secret)
//!
//! A federated share is view-only (for now): the recipient's instance lists and fetches it for them.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, Extension, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::osada1::{
    auth::{Admin, User},
    crypto,
    db::{self, now},
    edge::{self, Remote},
    error::{AppError, Res},
    files::{self, Target},
    http_util, users,
    vfs::RelPath,
    App,
};

/// Where an instance says how to federate with it.
pub const WELL_KNOWN: &str = "/.well-known/osada-federation";
/// The API, under it.
const API: &str = "/.well-known/osada-federation/v1";
const PROTOCOL: u32 = 1;
/// Unsigned `who` questions one address (an IPv6 /64) may ask a minute.
const WHO_PER_MINUTE: u32 = 20;
/// How long a signed request is good for.
const SIGNED_SECS: i64 = 120;
const AUTH: &str = "Osada-Federation ";
const SECRET_HEADER: &str = "x-shelf-share-secret";
/// Everything but the unreserved characters (RFC 3986 §2.3).
const ENC: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route(WELL_KNOWN, get(discovery))
        .route(&format!("{API}/who/{{upn}}"), get(who))
        .route(&format!("{API}/hello"), get(hello))
        .route(&format!("{API}/shares"), post(s_offer))
        .route(&format!("{API}/shares/{{id}}/revoke"), post(s_revoke))
        .route(&format!("{API}/shares/{{id}}/decline"), post(s_decline))
        .route(&format!("{API}/shares/{{id}}/ls"), get(s_ls))
        .route(&format!("{API}/shares/{{id}}/raw"), get(s_raw))
        // this instance's people
        .route("/api/federated", get(mine))
        .route("/api/federated/{id}", delete(drop_incoming))
        .route("/api/federated/{id}/ls", get(in_ls))
        .route("/api/federated/{id}/raw", get(in_raw))
        .route("/api/fedshares/{id}", delete(revoke_outgoing))
        // its administrators
        .route("/api/federation", get(admin_view))
        .route("/api/federation/peers", post(add_peer))
        .route("/api/federation/peers/{host}", delete(remove_peer))
}

/* ---- this instance, the peers ---- */

/// This instance's base URL (`https://osada.example.org`): its public URL. None: no federation.
pub fn base(app: &App) -> Option<String> {
    let u = app.cfg.public_url.as_deref()?.trim_end_matches('/');
    let rest = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://"))?;
    (!rest.is_empty() && !rest.contains('/')).then(|| u.to_string())
}

/// An instance this one federates with (admin → federation).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Peer {
    /// its base URL, as it gives it: what it is known by
    pub base: String,
    /// the domain of its UPNs
    pub upn_domain: String,
    /// its API's URL
    pub api: String,
    pub added: i64,
    pub by: String,
}

impl Peer {
    pub fn host(&self) -> &str {
        self.base.split("://").nth(1).unwrap_or(&self.base)
    }
}

pub fn peers(app: &App) -> Vec<Peer> {
    db::get_setting(&app.db, "fed_peers").ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_peers(app: &App, list: &[Peer]) -> Res<()> {
    db::set_setting(&app.db, "fed_peers", &serde_json::to_string(list).map_err(AppError::internal)?)
}

/// The federated instance whose UPNs `upn` is one of.
pub fn peer_for(app: &App, upn: &str) -> Option<Peer> {
    let dom = upn.rsplit_once('@')?.1.to_ascii_lowercase();
    peers(app).into_iter().find(|p| p.upn_domain == dom)
}

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("osada-federation/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client")
    })
}

/// An instance's `/.well-known/osada-federation`, checked: a osada, at the base it was asked at.
async fn discover(base: &str) -> Result<Value, String> {
    let r = http().get(format!("{base}{WELL_KNOWN}")).timeout(Duration::from_secs(15)).send().await.map_err(|e| format!("{base}: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("{base}: {} (is it a osada of Round 22 or later?)", r.status()));
    }
    let doc: Value = serde_json::from_slice(&r.bytes().await.map_err(|e| e.to_string())?).map_err(|_| format!("{base}: not a osada's answer"))?;
    if doc["software"] != "osada" || doc["protocol"].as_u64() != Some(PROTOCOL as u64) {
        return Err(format!("{base}: not a osada speaking federation {PROTOCOL}"));
    }
    if doc["base"].as_str() != Some(base) {
        return Err(format!("{base}: it calls itself {}", doc["base"]));
    }
    Ok(doc)
}

/// A peer's signing keys: kept ten minutes; asked again (at most once a minute) for a key not known.
async fn peer_keys(peer: &Peer, kid: &str) -> Result<Vec<Value>, String> {
    static KEYS: OnceLock<Mutex<HashMap<String, (Instant, Vec<Value>)>>> = OnceLock::new();
    let keys = KEYS.get_or_init(Default::default);
    if let Some((at, list)) = keys.lock().unwrap_or_else(|e| e.into_inner()).get(&peer.base) {
        let has = list.iter().any(|k| k["kid"] == kid);
        if (has && at.elapsed() < Duration::from_secs(600)) || (!has && at.elapsed() < Duration::from_secs(60)) {
            return Ok(list.clone());
        }
    }
    let doc = discover(&peer.base).await?;
    let list = doc["keys"].as_array().cloned().unwrap_or_default();
    keys.lock().unwrap_or_else(|e| e.into_inner()).insert(peer.base.clone(), (Instant::now(), list.clone()));
    Ok(list)
}

fn rs256_ok(jwk: &Value, input: &[u8], sig: &[u8]) -> bool {
    let part = |k: &str| jwk[k].as_str().and_then(|s| B64URL.decode(s).ok());
    let (Some(n), Some(e)) = (part("n"), part("e")) else { return false };
    ring::signature::RsaPublicKeyComponents { n, e }.verify(&ring::signature::RSA_PKCS1_2048_8192_SHA256, input, sig).is_ok()
}

/* ---- signed requests ---- */

type Refused = (StatusCode, String);

fn refused(s: StatusCode, m: impl Into<String>) -> Refused {
    (s, m.into())
}

fn answer(r: Refused) -> Response {
    (r.0, Json(json!({ "error": r.1 }))).into_response()
}

/// The asking instance of a signed request, after every check.
async fn verify(app: &App, method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> Result<Peer, Refused> {
    let unauth = |m: &str| refused(StatusCode::UNAUTHORIZED, m);
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix(AUTH))
        .ok_or_else(|| unauth("a signed request of a federated instance is needed"))?;
    let parts: Vec<&str> = token.split('.').collect();
    let [h, c, s] = parts[..] else { return Err(unauth("not a signed request")) };
    let dec = |x: &str| B64URL.decode(x).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let (Some(head), Some(claims), Ok(sig)) = (dec(h), dec(c), B64URL.decode(s)) else { return Err(unauth("not a signed request")) };
    if head["alg"] != "RS256" || head["typ"] != "osada-fed+jwt" {
        return Err(unauth("not a signed request"));
    }
    let iss = claims["iss"].as_str().unwrap_or("");
    let peer = peers(app)
        .into_iter()
        .find(|p| p.base == iss)
        .ok_or_else(|| refused(StatusCode::FORBIDDEN, format!("this instance does not federate with {iss}")))?;
    let me = base(app).ok_or_else(|| refused(StatusCode::NOT_FOUND, "no federation here"))?;
    let t = now();
    let (iat, exp) = (claims["iat"].as_i64().unwrap_or(0), claims["exp"].as_i64().unwrap_or(0));
    if claims["aud"].as_str() != Some(me.as_str()) {
        return Err(unauth("signed for another instance"));
    }
    if (t - iat).abs() > 300 || exp < t || exp - iat > 300 {
        return Err(unauth("signed too long ago (or the clocks differ)"));
    }
    let htu = uri.path_and_query().map(|p| p.as_str()).unwrap_or("");
    if claims["htm"].as_str() != Some(method.as_str()) || claims["htu"].as_str() != Some(htu) {
        return Err(unauth("signed for another request"));
    }
    if claims["bh"].as_str() != Some(B64URL.encode(Sha256::digest(body)).as_str()) {
        return Err(unauth("the body is not what was signed"));
    }
    let kid = head["kid"].as_str().unwrap_or("");
    let keys = peer_keys(&peer, kid).await.map_err(|e| refused(StatusCode::BAD_GATEWAY, e))?;
    let input = format!("{h}.{c}");
    if !keys.iter().filter(|k| k["kid"] == kid).any(|k| rs256_ok(k, input.as_bytes(), &sig)) {
        return Err(unauth("the signature does not check out"));
    }
    // once only
    static SEEN: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    let jti = format!("{iss} {}", claims["jti"].as_str().unwrap_or(""));
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    seen.retain(|_, e| *e >= t);
    if seen.insert(jti, exp).is_some() {
        return Err(unauth("that request was made already"));
    }
    Ok(peer)
}

/// A signed request to a peer: `rel` under its API (with its query, already encoded).
async fn call(app: &App, peer: &Peer, method: Method, rel: &str, body: Option<&Value>, secret: Option<&str>, headers: &[(header::HeaderName, HeaderValue)]) -> Result<reqwest::Response, String> {
    let me = base(app).ok_or("federation needs this instance's public URL (OSADA_PUBLIC_URL)")?;
    let url = reqwest::Url::parse(&format!("{}{rel}", peer.api)).map_err(|e| e.to_string())?;
    let htu = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    };
    let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    let t = now();
    let claims = json!({
        "iss": me, "aud": peer.base, "iat": t, "exp": t + SIGNED_SECS, "jti": crypto::random_token(16),
        "htm": method.as_str(), "htu": htu, "bh": B64URL.encode(Sha256::digest(&bytes)),
    });
    let token = crate::osada1::oidc::sign_fed(app, &claims).map_err(|e| e.msg)?;
    let mut rq = http().request(method, url).header(header::AUTHORIZATION, format!("{AUTH}{token}"));
    if body.is_some() {
        rq = rq.header(header::CONTENT_TYPE, "application/json").body(bytes);
    }
    if let Some(s) = secret {
        rq = rq.header(SECRET_HEADER, s);
    }
    for (k, v) in headers {
        rq = rq.header(k, v);
    }
    rq.send().await.map_err(|e| format!("{}: {e}", peer.host()))
}

/// What a peer's refusal says.
async fn why(peer: &Peer, r: reqwest::Response) -> String {
    let status = r.status();
    let said = r.bytes().await.ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["error"].as_str().map(String::from));
    format!("{}: {}", peer.host(), said.unwrap_or_else(|| status.to_string()))
}

/* ---- the public part ---- */

async fn discovery(State(app): State<Arc<App>>) -> Res<Response> {
    let me = base(&app).ok_or_else(AppError::not_found)?;
    let jwk = crate::osada1::oidc::public_jwk(&app)?;
    let doc = json!({
        "software": "osada", "protocol": PROTOCOL, "version": env!("CARGO_PKG_VERSION"),
        "base": me, "upn_domain": users::upn_domain(&app), "api": format!("{me}{API}"), "keys": [jwk],
    });
    let mut r = Json(doc).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    r.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    Ok(r)
}

/// The person of this instance whose UPN is exactly `upn`.
fn by_upn(app: &App, upn: &str) -> Option<users::Local> {
    let upn = upn.trim().to_lowercase();
    users::people_index(app).into_iter().find(|p| p.upn == upn)
}

/// The address a question is counted against: an IPv4 address, an IPv6 /64.
fn bucket(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

fn may_ask(ip: IpAddr) -> bool {
    static ASKED: OnceLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> = OnceLock::new();
    let mut m = ASKED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let t = Instant::now();
    if m.len() > 50_000 {
        m.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
    }
    let e = m.entry(bucket(ip)).or_insert((t, 0));
    if e.0.elapsed() >= Duration::from_secs(60) {
        *e = (t, 0);
    }
    e.1 += 1;
    e.1 <= WHO_PER_MINUTE
}

/// Does this UPN exist here? Yes or no, nothing more.
async fn who(
    State(app): State<Arc<App>>,
    Path(upn): Path<String>,
    remote: Option<Extension<Remote>>,
    peer: Option<Extension<ConnectInfo<edge::PeerAddr>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let signed = headers.contains_key(header::AUTHORIZATION) && verify(&app, &method, &uri, &headers, &[]).await.is_ok();
    let ip = match (remote, peer) {
        (Some(Extension(Remote(ip))), _) => ip,
        (None, Some(Extension(ConnectInfo(edge::PeerAddr(a))))) => a.ip(),
        _ => IpAddr::from([0, 0, 0, 0]),
    };
    if !signed && !may_ask(ip) {
        let mut r = answer(refused(StatusCode::TOO_MANY_REQUESTS, "too many questions; ask again in a minute"));
        r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        return r;
    }
    let upn = upn.trim().to_lowercase();
    let exists = by_upn(&app, &upn).is_some();
    // as long for a yes as for a no
    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut r = Json(json!({ "upn": upn, "exists": exists })).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn hello(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    match verify(&app, &method, &uri, &headers, &[]).await {
        Ok(p) => Json(json!({ "ok": true, "you": p.base, "me": base(&app) })).into_response(),
        Err(e) => answer(e),
    }
}

/* ---- shares: the instances' side ---- */

#[derive(Deserialize)]
struct Offer {
    id: i64,
    owner: String,
    #[serde(default)]
    owner_display: String,
    to: String,
    name: String,
    dir: bool,
    #[serde(default)]
    size: Option<i64>,
    secret: String,
}

/// The users row of someone of this instance (made, as sharing does, for one of 0.2's directory not synced yet).
fn local_id(app: &App, username: &str) -> Res<i64> {
    if let Some(id) = app.db.opt(|c| c.query_row("SELECT id FROM users WHERE username = ?1 AND disabled = 0", [username], |r| r.get::<_, i64>(0)))? {
        return Ok(id);
    }
    if let Some(p) = app.bridge.as_ref().and_then(|b| b.people().into_iter().find(|p| p.name == username && !p.disabled)) {
        return Ok(crate::osada1::auth::ensure_user(app, &p)?.id);
    }
    Err(AppError::not_found())
}

/// A peer offers someone here a share.
async fn s_offer(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let peer = match verify(&app, &method, &uri, &headers, &body).await {
        Ok(p) => p,
        Err(e) => return answer(e),
    };
    let Ok(o) = serde_json::from_slice::<Offer>(&body) else { return answer(refused(StatusCode::BAD_REQUEST, "not an offer")) };
    let owner = o.owner.trim().to_lowercase();
    if owner.rsplit_once('@').map(|(_, d)| d) != Some(peer.upn_domain.as_str()) {
        return answer(refused(StatusCode::FORBIDDEN, format!("{owner} is not of {}", peer.host())));
    }
    if o.name.is_empty() || o.name.len() > 255 || o.name.contains(['/', '\\', '\0']) || o.secret.len() < 20 || o.secret.len() > 200 {
        return answer(refused(StatusCode::BAD_REQUEST, "not an offer"));
    }
    let Some(p) = by_upn(&app, &o.to) else { return answer(refused(StatusCode::NOT_FOUND, format!("nobody here is {}", o.to))) };
    let uid = match local_id(&app, &p.username) {
        Ok(id) => id,
        Err(_) => return answer(refused(StatusCode::NOT_FOUND, format!("nobody here is {}", o.to))),
    };
    let display: String = o.owner_display.chars().filter(|c| !c.is_control()).take(100).collect();
    let r = app.db.with(|c| {
        c.execute(
            "INSERT INTO fed_in (user_id, peer, remote_id, owner_upn, owner_display, name, is_dir, size, secret, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (peer, remote_id) DO UPDATE SET user_id = excluded.user_id, owner_upn = excluded.owner_upn,
               owner_display = excluded.owner_display, name = excluded.name, is_dir = excluded.is_dir, size = excluded.size, secret = excluded.secret",
            params![uid, peer.base, o.id, owner, display, o.name, o.dir, o.size, o.secret, now()],
        )
    });
    match r {
        Ok(_) => {
            tracing::info!(peer = %peer.base, from = %owner, to = %p.upn, name = %o.name, "federation: a share offered");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => answer(refused(StatusCode::INTERNAL_SERVER_ERROR, e.msg)),
    }
}

/// The owner (at the peer) stopped sharing.
async fn s_revoke(State(app): State<Arc<App>>, Path(id): Path<i64>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let peer = match verify(&app, &method, &uri, &headers, &body).await {
        Ok(p) => p,
        Err(e) => return answer(e),
    };
    match app.db.with(|c| c.execute("DELETE FROM fed_in WHERE peer = ?1 AND remote_id = ?2", params![peer.base, id])) {
        Ok(n) => Json(json!({ "ok": true, "removed": n })).into_response(),
        Err(e) => answer(refused(StatusCode::INTERNAL_SERVER_ERROR, e.msg)),
    }
}

/// The recipient (at the peer) dropped it.
async fn s_decline(State(app): State<Arc<App>>, Path(id): Path<i64>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let peer = match verify(&app, &method, &uri, &headers, &body).await {
        Ok(p) => p,
        Err(e) => return answer(e),
    };
    match app.db.with(|c| c.execute("DELETE FROM fed_out WHERE id = ?1 AND peer = ?2", params![id, peer.base])) {
        Ok(n) => Json(json!({ "ok": true, "removed": n })).into_response(),
        Err(e) => answer(refused(StatusCode::INTERNAL_SERVER_ERROR, e.msg)),
    }
}

fn secret_hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// What a peer's share `id` points at here, read-only, after its signature and the share's secret.
async fn shared_target(app: &App, peer: &Peer, id: i64, headers: &HeaderMap, sub: &str) -> Result<Target, Refused> {
    let not = || refused(StatusCode::NOT_FOUND, "no such share");
    let row: Option<(i64, String, Option<i64>, String, bool, String)> = app
        .db
        .opt(|c| {
            c.query_row(
                "SELECT f.owner_id, u.username, f.mount_id, f.path, f.is_dir, f.secret_hash FROM fed_out f JOIN users u ON u.id = f.owner_id
                 WHERE f.id = ?1 AND f.peer = ?2 AND u.disabled = 0",
                params![id, peer.base],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
        })
        .map_err(|e| refused(StatusCode::INTERNAL_SERVER_ERROR, e.msg))?;
    let (owner, owner_name, mount, path, is_dir, hash) = row.ok_or_else(not)?;
    let given = headers.get(SECRET_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    let mine = secret_hash(given);
    let ok = mine.len() == hash.len() && mine.bytes().zip(hash.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0;
    if !ok {
        return Err(not());
    }
    let root = RelPath::parse(&path).map_err(|_| not())?;
    let sub = RelPath::parse(sub).map_err(|_| refused(StatusCode::BAD_REQUEST, "bad path"))?;
    if !is_dir && !sub.is_root() {
        return Err(not());
    }
    let backend = files::owner_backend(app, &owner_name, mount).await.map_err(|e| refused(e.status, e.msg))?;
    Ok(Target { backend, path: root.join(&sub), root_name: root.name().unwrap_or("shared").to_string(), root, write: false, owner, mount, group: None })
}

#[derive(Deserialize, Default)]
struct PathQ {
    #[serde(default)]
    path: String,
    dl: Option<String>,
}

async fn s_ls(State(app): State<Arc<App>>, Path(id): Path<i64>, Query(q): Query<PathQ>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let peer = match verify(&app, &method, &uri, &headers, &[]).await {
        Ok(p) => p,
        Err(e) => return answer(e),
    };
    let t = match shared_target(&app, &peer, id, &headers, &q.path).await {
        Ok(t) => t,
        Err(e) => return answer(e),
    };
    let listed = async {
        let me = t.backend.stat(&t.path).await?;
        if !me.is_dir {
            let mut e = me.json();
            e["name"] = json!(t.name(&t.path));
            return Ok::<Value, AppError>(json!({ "file": e, "path": t.rel().to_string(), "root_name": t.root_name }));
        }
        let mut list: Vec<Value> = t.backend.list(&t.path).await?.iter().map(|e| e.json()).collect();
        list.sort_by(|a, b| (!a["dir"].as_bool().unwrap_or(false), a["name"].as_str()).cmp(&(!b["dir"].as_bool().unwrap_or(false), b["name"].as_str())));
        Ok(json!({ "entries": list, "path": t.rel().to_string(), "root_name": t.root_name }))
    };
    match listed.await {
        Ok(v) => Json(v).into_response(),
        Err(e) => answer(refused(e.status, e.msg)),
    }
}

async fn s_raw(State(app): State<Arc<App>>, Path(id): Path<i64>, Query(q): Query<PathQ>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let peer = match verify(&app, &method, &uri, &headers, &[]).await {
        Ok(p) => p,
        Err(e) => return answer(e),
    };
    let t = match shared_target(&app, &peer, id, &headers, &q.path).await {
        Ok(t) => t,
        Err(e) => return answer(e),
    };
    match t.backend.open(&t.path).await {
        Ok((e, r)) => {
            files::note(3, "read", &format!("federation {}", peer.host()), t.owner, t.mount, &t.path, None, Some(e.size), "federation");
            http_util::serve(e, r, &headers, &t.name(&t.path), q.dl.is_some(), false, None).await
        }
        Err(e) => {
            let e = AppError::from(e);
            answer(refused(e.status, e.msg))
        }
    }
}

/* ---- sharing out (the owner's side) ---- */

/// Shares the user's item with `to`, someone of a federated instance: view-only.
pub async fn offer(app: &App, user: &User, t: &Target, is_dir: bool, to: &str) -> Res<Value> {
    let to = to.trim().trim_start_matches('<').trim_end_matches('>').to_lowercase();
    let peer = peer_for(app, &to).ok_or_else(|| AppError::bad(format!("{to}: no federated instance has that domain")))?;
    if t.group.is_some() {
        return Err(AppError::bad("a group's folder is not shared with other instances (yet): share it from your own files"));
    }
    let path = t.path.to_string();
    let name = t.name(&t.path);
    let size = if is_dir { None } else { t.backend.stat(&t.path).await.ok().map(|e| e.size as i64) };
    let have: Option<i64> = app.db.opt(|c| {
        c.query_row(
            "SELECT id FROM fed_out WHERE owner_id = ?1 AND IFNULL(mount_id, 0) = IFNULL(?2, 0) AND path = ?3 AND to_upn = ?4",
            params![user.id, t.mount, path, to],
            |r| r.get(0),
        )
    })?;
    if let Some(id) = have {
        return Ok(json!({ "id": id, "federated": true, "instance": peer.host(), "already": true }));
    }
    let secret = crypto::random_token(32);
    let id = app.db.with(|c| {
        c.execute(
            "INSERT INTO fed_out (owner_id, mount_id, path, is_dir, peer, to_upn, secret_hash, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![user.id, t.mount, path, is_dir, peer.base, to, secret_hash(&secret), now()],
        )?;
        Ok(c.last_insert_rowid())
    })?;
    let me = users::people_index(app).into_iter().find(|p| p.username == user.username);
    let body = json!({
        "id": id, "owner": users::upn_of(app, &user.username), "owner_display": me.map(|p| p.display).unwrap_or_else(|| user.username.clone()),
        "to": to, "name": name, "dir": is_dir, "size": size, "secret": secret,
    });
    let failed = match call(app, &peer, Method::POST, "/shares", Some(&body), None, &[]).await {
        Ok(r) if r.status().is_success() => None,
        Ok(r) => Some(why(&peer, r).await),
        Err(e) => Some(e),
    };
    if let Some(e) = failed {
        let _ = app.db.with(|c| c.execute("DELETE FROM fed_out WHERE id = ?1", [id]));
        return Err(AppError::bad(format!("not shared: {e}")));
    }
    tracing::info!(peer = %peer.base, to = %to, path = %path, "federation: shared");
    Ok(json!({ "id": id, "federated": true, "instance": peer.host() }))
}

/// The item's federated shares, for the share window.
pub fn out_for_item(app: &App, owner: i64, mount: Option<i64>, path: &str) -> Res<Vec<Value>> {
    app.db.with(|c| {
        let mut st = c.prepare(
            "SELECT id, to_upn, peer, created FROM fed_out WHERE owner_id = ?1 AND IFNULL(mount_id, 0) = IFNULL(?2, 0) AND path = ?3 ORDER BY to_upn",
        )?;
        let v = st
            .query_map(params![owner, mount, path], |r| {
                let peer: String = r.get(2)?;
                Ok(json!({ "id": r.get::<_, i64>(0)?, "upn": r.get::<_, String>(1)?, "instance": peer.split("://").nth(1).unwrap_or(&peer), "created": r.get::<_, i64>(3)? }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>();
        v
    })
}

/// The share window's second step: is `q`, an address at a federated instance's domain, someone there?
pub async fn search(app: &App, q: &str) -> (Vec<Value>, &'static str) {
    let list = peers(app);
    if list.is_empty() {
        return (Vec::new(), "none");
    }
    let Some(peer) = peer_for(app, q).filter(|_| crate::directory::valid_address(q)) else { return (Vec::new(), "on") };
    static ASKED: OnceLock<Mutex<HashMap<String, (Instant, bool)>>> = OnceLock::new();
    let cache = ASKED.get_or_init(Default::default);
    let known = cache.lock().unwrap_or_else(|e| e.into_inner()).get(q).filter(|(at, _)| at.elapsed() < Duration::from_secs(120)).map(|x| x.1);
    let exists = match known {
        Some(x) => x,
        None => {
            let rel = format!("/who/{}", utf8_percent_encode(q, ENC));
            let x = match call(app, &peer, Method::GET, &rel, None, None, &[]).await {
                Ok(r) if r.status().is_success() => r.bytes().await.ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).is_some_and(|v| v["exists"] == true),
                _ => return (Vec::new(), "unreachable"),
            };
            let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
            if c.len() > 5000 {
                c.clear();
            }
            c.insert(q.to_string(), (Instant::now(), x));
            x
        }
    };
    if exists {
        (vec![json!({ "upn": q, "display": q, "instance": peer.host() })], "on")
    } else {
        (Vec::new(), "on")
    }
}

/* ---- this instance's people ---- */

/// What others' instances share with the user, and what the user shares with people there.
async fn mine(State(app): State<Arc<App>>, user: User) -> Res<Json<Value>> {
    let (incoming, outgoing) = app.db.with(|c| {
        let mut st = c.prepare("SELECT id, peer, owner_upn, owner_display, name, is_dir, size, created FROM fed_in WHERE user_id = ?1 ORDER BY created DESC")?;
        let incoming = st
            .query_map([user.id], |r| {
                let peer: String = r.get(1)?;
                Ok(json!({ "id": r.get::<_, i64>(0)?, "instance": peer.split("://").nth(1).unwrap_or(&peer), "from": r.get::<_, String>(2)?,
                    "from_display": r.get::<_, String>(3)?, "name": r.get::<_, String>(4)?, "dir": r.get::<_, bool>(5)?,
                    "size": r.get::<_, Option<i64>>(6)?, "created": r.get::<_, i64>(7)? }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut st = c.prepare(
            "SELECT f.id, f.peer, f.to_upn, f.path, f.is_dir, f.mount_id, m.name, f.created FROM fed_out f LEFT JOIN mounts m ON m.id = f.mount_id
             WHERE f.owner_id = ?1 ORDER BY f.created DESC",
        )?;
        let outgoing = st
            .query_map([user.id], |r| {
                let peer: String = r.get(1)?;
                let path: String = r.get(3)?;
                let mount: Option<i64> = r.get(5)?;
                Ok(json!({ "id": r.get::<_, i64>(0)?, "instance": peer.split("://").nth(1).unwrap_or(&peer), "upn": r.get::<_, String>(2)?,
                    "name": path.rsplit('/').next().unwrap_or(&path), "path": path, "dir": r.get::<_, bool>(4)?,
                    "loc": mount.map_or("home".to_string(), |m| format!("m{m}")), "loc_name": r.get::<_, Option<String>>(6)?.unwrap_or("home".into()),
                    "created": r.get::<_, i64>(7)? }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((incoming, outgoing))
    })?;
    Ok(Json(json!({ "incoming": incoming, "outgoing": outgoing, "peers": peers(&app).iter().map(|p| p.host().to_string()).collect::<Vec<_>>() })))
}

/// The user's incoming share `id`: its peer, the owner's id for it, its secret.
fn incoming(app: &App, user: &User, id: i64) -> Res<(Peer, i64, String, String)> {
    let row: Option<(String, i64, String, String)> = app.db.opt(|c| {
        c.query_row("SELECT peer, remote_id, secret, name FROM fed_in WHERE id = ?1 AND user_id = ?2", params![id, user.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
    })?;
    let (peer, remote, secret, name) = row.ok_or_else(AppError::not_found)?;
    let p = peers(app).into_iter().find(|p| p.base == peer).ok_or_else(|| AppError::bad("this instance no longer federates with the one it came from"))?;
    Ok((p, remote, secret, name))
}

async fn drop_incoming(State(app): State<Arc<App>>, user: User, Path(id): Path<i64>) -> Res<Json<Value>> {
    let told = match incoming(&app, &user, id) {
        Ok((p, remote, _, _)) => match call(&app, &p, Method::POST, &format!("/shares/{remote}/decline"), Some(&json!({})), None, &[]).await {
            Ok(r) if r.status().is_success() => true,
            _ => false,
        },
        Err(_) => false,
    };
    app.db.with(|c| c.execute("DELETE FROM fed_in WHERE id = ?1 AND user_id = ?2", params![id, user.id]))?;
    Ok(Json(json!({ "ok": true, "owner_told": told })))
}

/// A peer's refusal, as this instance answers it: a bad request or nothing there as such, else a bad gateway.
fn passed(status: u16) -> StatusCode {
    match status {
        400 => StatusCode::BAD_REQUEST,
        404 | 410 => StatusCode::NOT_FOUND,
        416 => StatusCode::RANGE_NOT_SATISFIABLE,
        _ => StatusCode::BAD_GATEWAY,
    }
}

async fn in_ls(State(app): State<Arc<App>>, user: User, Path(id): Path<i64>, Query(q): Query<PathQ>) -> Res<Response> {
    let (p, remote, secret, _) = incoming(&app, &user, id)?;
    let rel = format!("/shares/{remote}/ls?path={}", utf8_percent_encode(&q.path, ENC));
    let r = call(&app, &p, Method::GET, &rel, None, Some(&secret), &[]).await.map_err(AppError::bad)?;
    if !r.status().is_success() {
        let status = r.status();
        let e = why(&p, r).await;
        return Err(AppError::new(passed(status.as_u16()), e));
    }
    let v: Value = serde_json::from_slice(&r.bytes().await.map_err(|e| AppError::bad(e.to_string()))?).map_err(|_| AppError::bad("a strange answer"))?;
    Ok(Json(v).into_response())
}

async fn in_raw(State(app): State<Arc<App>>, user: User, Path(id): Path<i64>, Query(q): Query<PathQ>, headers: HeaderMap) -> Res<Response> {
    let (p, remote, secret, _) = incoming(&app, &user, id)?;
    let mut rel = format!("/shares/{remote}/raw?path={}", utf8_percent_encode(&q.path, ENC));
    if q.dl.is_some() {
        rel.push_str("&dl=1");
    }
    let pass: Vec<(header::HeaderName, HeaderValue)> =
        [header::RANGE, header::IF_NONE_MATCH, header::IF_RANGE].into_iter().filter_map(|h| headers.get(&h).map(|v| (h, v.clone()))).collect();
    let r = call(&app, &p, Method::GET, &rel, None, Some(&secret), &pass).await.map_err(AppError::bad)?;
    let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if !(status.is_success() || status == StatusCode::NOT_MODIFIED) {
        let e = why(&p, r).await;
        return Err(AppError::new(passed(status.as_u16()), e));
    }
    let mut out = Response::builder().status(status);
    for h in [header::CONTENT_TYPE, header::CONTENT_LENGTH, header::CONTENT_RANGE, header::ACCEPT_RANGES, header::ETAG, header::LAST_MODIFIED, header::CONTENT_DISPOSITION, header::CONTENT_SECURITY_POLICY] {
        if let Some(v) = r.headers().get(h.as_str()).and_then(|v| HeaderValue::from_bytes(v.as_bytes()).ok()) {
            out = out.header(h, v);
        }
    }
    // what another instance serves is never a page of this one
    out = out.header(header::X_CONTENT_TYPE_OPTIONS, "nosniff").header(header::CACHE_CONTROL, "private, no-store");
    if r.headers().get(header::CONTENT_SECURITY_POLICY.as_str()).is_none() {
        out = out.header(header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'");
    }
    out.body(Body::from_stream(r.bytes_stream())).map_err(AppError::internal)
}

async fn revoke_outgoing(State(app): State<Arc<App>>, user: User, Path(id): Path<i64>) -> Res<Json<Value>> {
    let peer: String = app
        .db
        .opt(|c| c.query_row("SELECT peer FROM fed_out WHERE id = ?1 AND owner_id = ?2", params![id, user.id], |r| r.get(0)))?
        .ok_or_else(AppError::not_found)?;
    app.db.with(|c| c.execute("DELETE FROM fed_out WHERE id = ?1 AND owner_id = ?2", params![id, user.id]))?;
    let told = match peers(&app).into_iter().find(|p| p.base == peer) {
        Some(p) => matches!(call(&app, &p, Method::POST, &format!("/shares/{id}/revoke"), Some(&json!({})), None, &[]).await, Ok(r) if r.status().is_success()),
        None => false,
    };
    Ok(Json(json!({ "ok": true, "recipient_told": told })))
}

/* ---- administration ---- */

async fn admin_view(State(app): State<Arc<App>>, _a: Admin) -> Res<Json<Value>> {
    let me = base(&app);
    let list = peers(&app);
    let checks = futures_util::future::join_all(list.iter().map(|p| {
        let app = app.clone();
        async move {
            match call(&app, p, Method::GET, "/hello", None, None, &[]).await {
                Ok(r) if r.status().is_success() => json!({ "ok": true, "state": "federated both ways" }),
                Ok(r) if r.status() == reqwest::StatusCode::FORBIDDEN => json!({ "ok": false, "state": "it does not list this instance yet" }),
                Ok(r) => json!({ "ok": false, "state": why(p, r).await }),
                Err(e) => json!({ "ok": false, "state": e }),
            }
        }
    }))
    .await;
    let out: Vec<Value> = list
        .iter()
        .zip(checks)
        .map(|(p, c)| json!({ "host": p.host(), "base": p.base, "upn_domain": p.upn_domain, "added": p.added, "by": p.by, "ok": c["ok"], "state": c["state"] }))
        .collect();
    let counts: (i64, i64) = app.db.with(|c| Ok((c.query_row("SELECT count(*) FROM fed_in", [], |r| r.get(0))?, c.query_row("SELECT count(*) FROM fed_out", [], |r| r.get(0))?)))?;
    Ok(Json(json!({
        "base": me, "upn_domain": users::upn_domain(&app), "discovery": me.as_ref().map(|b| format!("{b}{WELL_KNOWN}")),
        "who": me.as_ref().map(|b| format!("{b}{API}/who/<upn>")), "peers": out, "incoming": counts.0, "outgoing": counts.1,
    })))
}

#[derive(Deserialize)]
struct AddPeer {
    url: String,
}

async fn add_peer(State(app): State<Arc<App>>, Admin(a): Admin, Json(req): Json<AddPeer>) -> Res<Json<Value>> {
    let me = base(&app).ok_or_else(|| AppError::bad("federation needs this instance's public URL (OSADA_PUBLIC_URL)"))?;
    let mut u = req.url.trim().trim_end_matches('/').to_string();
    if let Some(i) = u.find(WELL_KNOWN) {
        u.truncate(i);
    }
    if !u.contains("://") {
        u = format!("https://{u}");
    }
    let rest = u.split("://").nth(1).unwrap_or("");
    if rest.is_empty() || rest.contains(['/', '?', '#', '@']) || !(u.starts_with("https://") || u.starts_with("http://")) {
        return Err(AppError::bad("give the other osada's address: osada.example.org or https://osada.example.org"));
    }
    if u == me {
        return Err(AppError::bad("that is this instance"));
    }
    let doc = discover(&u).await.map_err(AppError::bad)?;
    let upn_domain = doc["upn_domain"].as_str().unwrap_or("").to_ascii_lowercase();
    let api = doc["api"].as_str().unwrap_or("").to_string();
    if upn_domain.is_empty() || !api.starts_with(&format!("{u}/")) {
        return Err(AppError::bad(format!("{u}: its answer lacks the UPN domain or the API")));
    }
    if upn_domain == users::upn_domain(&app) {
        return Err(AppError::bad(format!("{u} has this instance's UPN domain ({upn_domain})")));
    }
    let mut list = peers(&app);
    if list.iter().any(|p| p.upn_domain == upn_domain && p.base != u) {
        return Err(AppError::bad(format!("another federated instance has the UPN domain {upn_domain}")));
    }
    list.retain(|p| p.base != u);
    list.push(Peer { base: u.clone(), upn_domain: upn_domain.clone(), api, added: now(), by: a.username.clone() });
    save_peers(&app, &list)?;
    tracing::info!(peer = %u, by = %a.username, "federation: an instance added");
    Ok(Json(json!({ "base": u, "upn_domain": upn_domain })))
}

async fn remove_peer(State(app): State<Arc<App>>, Admin(a): Admin, Path(host): Path<String>) -> Res<Json<Value>> {
    let mut list = peers(&app);
    let Some(p) = list.iter().find(|p| p.host() == host).cloned() else { return Err(AppError::not_found()) };
    list.retain(|x| x.base != p.base);
    save_peers(&app, &list)?;
    // its shares go with it
    let (i, o) = app.db.with(|c| Ok((c.execute("DELETE FROM fed_in WHERE peer = ?1", [&p.base])?, c.execute("DELETE FROM fed_out WHERE peer = ?1", [&p.base])?)))?;
    tracing::info!(peer = %p.base, by = %a.username, "federation: an instance removed");
    Ok(Json(json!({ "ok": true, "incoming_removed": i, "outgoing_removed": o })))
}
