//! The edge's HTTP relay (docs/PLAN-0.2.0.md, D-U6). An edge node takes the
//! public names (https, its certificate in `<data>/edge-tls/fullchain.pem`
//! and `privkey.pem`, read again when they change: certbot can renew them)
//! and passes every request to the home node over their node link: a stream
//! of kind `http`, the request's head and then its body, the answer's head and
//! then its body, both streamed. The home node dials the edge (no port open at
//! home, no tunnel); at home the request goes into osada 0.1's edge router as
//! from osada-edge: the visitor's address decides what an account may do from
//! outside, apptab and the other proxies answer at their names.
//!
//! - Only nodes with the role edge-http may relay into a node.
//! - The visitor's `x-shelf-*` headers and hop-by-hop headers stay at the edge.
//! - While home cannot be reached the edge answers 503 with `x-shelf-offline`
//!   (osada 0.1's pages show that they are offline); unlike osada-edge it
//!   keeps no copies of public links yet.
//! - A plain http listener (optional) answers certbot's http-01 challenges
//!   from `<data>/acme/.well-known/acme-challenge/` (`certbot certonly
//!   --webroot -w <data>/acme`) and sends everything else to https.

use std::{
    io::SeekFrom,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use futures_util::{FutureExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;
use tower::ServiceExt;

use crate::{
    edgecache::{
        cache::{Cache, Fill},
        dropped,
        keep::{KEEP_FOR, KEEP_MAX, Keep, Kept, is_gzip, token_of},
    },
    link::{self, Link, StreamHandler},
    placement::NodeId,
    osada1::{edge::PeerAddr, edge::Remote, edge::TlsListener},
};

/// The stream kind.
pub const KIND: &str = "http";

/// A relayed request's head. Header values are bytes, as a string of
/// chars 0 to 255.
#[derive(Debug, Serialize, Deserialize)]
pub struct Head {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(String, String)>,
    /// The visitor's address, as the edge saw it.
    pub client: IpAddr,
}

/// The answer's head.
#[derive(Debug, Serialize, Deserialize)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

const HOP: &[&str] = &["connection", "keep-alive", "proxy-connection", "transfer-encoding", "te", "trailer", "upgrade", "proxy-authorization", "proxy-authenticate"];

fn bytes_str(v: &[u8]) -> String {
    v.iter().map(|&b| b as char).collect()
}

fn str_bytes(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u32 as u8).collect()
}

/// The headers to pass on: no hop-by-hop ones; from a visitor, no `x-shelf-*` either.
fn pass_on(h: &HeaderMap, visitor: bool) -> Vec<(String, String)> {
    h.iter()
        .filter(|(k, _)| !HOP.contains(&k.as_str()) && !(visitor && k.as_str().starts_with("x-shelf-")))
        .map(|(k, v)| (k.as_str().to_string(), bytes_str(v.as_bytes())))
        .collect()
}

fn put(h: &mut HeaderMap, list: Vec<(String, String)>) {
    for (k, v) in list {
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_bytes(&str_bytes(&v))) {
            h.append(k, v);
        }
    }
}

// ---- at the edge ----------------------------------------------------------

/// a probe has no body: home answers it at once
/// How long a file's probe may take (home answers it at once: headers, no body); while home is
/// known to be away, AWAY_WAIT, so the copies answer without a wait (load test: 24 s before).
const PROBE_WAIT: Duration = Duration::from_secs(5);
const AWAY_WAIT: Duration = Duration::from_secs(1);
/// A body above this goes after the small answers on a link (stream priority, D-U39).
pub const BULK: u64 = 1 << 20;
/// a page, a /static/ file, a link's JSON: small
const KEEP_WAIT: Duration = Duration::from_secs(60);

/// What the edge does, by its roles in node control (D-U11): copies of files (cache), public links
/// for when home is away (public-cache), and how many GB it keeps.
pub type Roles = Arc<dyn Fn() -> (bool, bool, u32) + Send + Sync>;

/// Which node home is, looked up when asked (D-U19: the node named by id or
/// name, else the authority for files); None while it is not known yet.
pub type Home = Arc<dyn Fn() -> Option<NodeId> + Send + Sync>;

/// The site a host name is among those exposed on this edge: its instance's
/// name and module (D-U21).
pub type SiteOf = Arc<dyn Fn(&str) -> Option<(String, crate::placement::Module)> + Send + Sync>;

/// What an edge needs for the sites exposed on it (D-U21, D-U25).
pub struct WebEdge {
    pub me: NodeId,
    pub site_of: SiteOf,
    pub web: Arc<crate::websrv::Web>,
    /// Cloudflare's addresses (sites whose front is Cloudflare).
    pub cf: Vec<crate::osada1::edge::Cidr>,
    /// Plain http for names this edge has no certificate for and no site:
    /// the server that answered them before (nginx), Host kept.
    pub fallback_http: Option<String>,
    /// Whether this edge's certificate is for a name.
    pub ours: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// An http-01 token of a name whose certificate another node issues: its
    /// key authorization, asked of that node (D-U26).
    pub acme: Arc<dyn Fn(String, String) -> futures_util::future::BoxFuture<'static, Option<String>> + Send + Sync>,
    /// The node link: a site that runs elsewhere is asked there (D-U27).
    pub link: Arc<Link>,
    /// The authoritative web node (Round 16): it serves a static site of its own folder when the
    /// node the site runs on cannot be reached.
    pub web_authority: Arc<dyn Fn() -> Option<NodeId> + Send + Sync>,
}

/// The stream kind of a site's request passed to the node it runs on (D-U27).
pub const WEB_KIND: &str = "web";

/// What an edge tells the node a site runs on.
#[derive(Serialize, Deserialize)]
struct WebHead {
    site: String,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    /// The visitor, as the edge saw it (Cloudflare's header read there).
    client: IpAddr,
    proto: String,
    host: String,
}

/// The body of `body` onto `send`, then its end (or a reset on an error).
fn pump(body: Body, mut send: quinn::SendStream) {
    tokio::spawn(async move {
        let mut s = body.into_data_stream();
        while let Some(chunk) = s.next().await {
            match chunk {
                Ok(b) => {
                    if send.write_all(&b).await.is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = send.reset(1u32.into());
                    return;
                }
            }
        }
        let _ = send.finish();
    });
}

impl WebEdge {
    /// A site's request to the node it runs on, its answer back (D-U27).
    async fn forward(&self, node: &NodeId, site: &str, req: Request, v: &crate::websrv::Visitor) -> Response {
        forward_site(&self.link, node, site, req, v).await
    }
}

/// A site's request to the node it runs on over the link, its answer back (D-U27; also home's
/// /web/<site>/, Round 18).
pub async fn forward_site(link: &Link, node: &NodeId, site: &str, req: Request, v: &crate::websrv::Visitor) -> Response {
    if req.headers().contains_key(header::UPGRADE) {
        return (StatusCode::NOT_IMPLEMENTED, "a WebSocket goes only to a site that runs on the edge that answers it\n").into_response();
    }
    let (parts, body) = req.into_parts();
    let head = WebHead {
        site: site.into(),
        method: parts.method.as_str().into(),
        uri: parts.uri.path_and_query().map_or("/", |p| p.as_str()).into(),
        headers: pass_on(&parts.headers, true),
        client: v.ip,
        proto: v.proto.into(),
        host: v.host.clone(),
    };
    let Ok(head) = serde_json::to_value(&head) else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    let (send, mut recv) = match link.open_stream(node, WEB_KIND, head).await {
        Ok(s) => s,
        Err(e) => return offline(&format!("{}: its node: {}", v.host, e.0)),
    };
    pump(body, send);
    let a: Answer = match link::read_head(&mut recv).await.map_err(|e| e.0).and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string())) {
        Ok(a) => a,
        Err(e) => return offline(&format!("{}: its node: {e}", v.host)),
    };
    let mut b = Response::builder().status(StatusCode::from_u16(a.status).unwrap_or(StatusCode::BAD_GATEWAY));
    if let Some(h) = b.headers_mut() {
        put(h, a.headers);
    }
    b.body(Body::from_stream(ReaderStream::new(recv))).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// The sites that run on this node, as a node asks for them (D-U27): `site_of`
/// gives a site that runs here by its name; `may` says which nodes may ask.
pub fn web_runner(
    site_of: Arc<dyn Fn(&str) -> Option<crate::placement::Module> + Send + Sync>,
    web: Arc<crate::websrv::Web>,
    may: Arc<dyn Fn(&NodeId) -> bool + Send + Sync>,
) -> StreamHandler {
    Arc::new(move |peer: NodeId, kind: String, head: serde_json::Value, mut send: quinn::SendStream, recv: quinn::RecvStream| {
        let (site_of, web, may) = (site_of.clone(), web.clone(), may.clone());
        async move {
            let h = match serde_json::from_value::<WebHead>(head) {
                Ok(h) if kind == WEB_KIND && may(&peer) => h,
                _ => {
                    let _ = send.reset(1u32.into());
                    return;
                }
            };
            let res = match site_of(&h.site) {
                Some(m) if m.site.is_some() => {
                    let mut req = Request::new(Body::from_stream(ReaderStream::new(recv)));
                    *req.method_mut() = Method::from_bytes(h.method.as_bytes()).unwrap_or(Method::GET);
                    *req.uri_mut() = h.uri.parse().unwrap_or_else(|_| Uri::from_static("/"));
                    put(req.headers_mut(), h.headers);
                    let proto = if h.proto == "https" { "https" } else { "http" };
                    let v = crate::websrv::Visitor { ip: h.client, proto, host: h.host };
                    web.serve(&h.site, m.site.as_ref().expect("checked"), req, &v).await
                }
                _ => (StatusCode::NOT_FOUND, format!("{} does not run here\n", h.site)).into_response(),
            };
            let (parts, body) = res.into_parts();
            let answer = Answer { status: parts.status.as_u16(), headers: pass_on(&parts.headers, false) };
            // a big body (a file) goes after the small answers on the same link (D-U39)
            if parts.headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).is_none_or(|n| n > BULK) {
                let _ = send.set_priority(-1);
            }
            let Ok(answer) = serde_json::to_value(&answer) else { return };
            if link::write_head(&mut send, answer).await.is_err() {
                return;
            }
            pump(body, send);
        }
        .boxed()
    })
}

/// One handler for the streams of every kind: each to the handler of its kind.
pub fn dispatch(handlers: Vec<(&'static str, StreamHandler)>) -> StreamHandler {
    Arc::new(move |peer: NodeId, kind: String, head: serde_json::Value, mut send: quinn::SendStream, recv: quinn::RecvStream| {
        match handlers.iter().find(|(k, _)| *k == kind) {
            Some((_, h)) => h(peer, kind, head, send, recv),
            None => async move {
                let _ = send.reset(1u32.into());
            }
            .boxed(),
        }
    })
}

impl WebEdge {
    /// The answer for a site's request, if the host is a site here.
    async fn answer(&self, host: &str, req: Request, peer: IpAddr, https: bool) -> Option<Response> {
        let (name, m) = (self.site_of)(host)?;
        let site = m.site.as_ref()?;
        let (ip, proto) = match site.front {
            crate::sites::Front::Cloudflare => match crate::websrv::behind_cloudflare(&self.cf, peer, req.headers()) {
                Some(ip) => {
                    let p = req.headers().get(header::HeaderName::from_static("x-forwarded-proto")).and_then(|v| v.to_str().ok());
                    (ip, if https || p == Some("https") { "https" } else { "http" })
                }
                None => return Some((StatusCode::FORBIDDEN, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{host}: through Cloudflare only\n")).into_response()),
            },
            crate::sites::Front::Tls if !https => {
                let to = format!("https://{host}{}", req.uri().path_and_query().map_or("/", |p| p.as_str()));
                return Some((StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, to)]).into_response());
            }
            crate::sites::Front::Tls => (peer, "https"),
        };
        let v = crate::websrv::Visitor { ip, proto, host: host.to_string() };
        if m.node != self.me {
            // the decoupler: the node it runs on answers (D-U27); away, the authoritative web node
            // has the master copy of a static site of its own folder (Round 16)
            let peers = self.link.peers();
            let to = match (self.web_authority)() {
                Some(a) if !peers.contains(&m.node) && crate::sites::own_static(site) && (a == self.me || peers.contains(&a)) => a,
                _ => m.node.clone(),
            };
            if to == self.me {
                return Some(self.web.serve(&name, site, req, &v).await);
            }
            return Some(self.forward(&to, &name, req, &v).await);
        }
        Some(self.web.serve(&name, site, req, &v).await)
    }
}

/// The host a request is for, lower case, without a port.
fn host_of(req: &Request) -> Option<String> {
    let h = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).map(str::to_string).or_else(|| req.uri().authority().map(|a| a.as_str().to_string()))?;
    let name = if h.starts_with('[') { h.split(']').next().map(|x| format!("{x}]"))? } else { h.rsplit_once(':').map_or(h.clone(), |(n, _)| n.to_string()) };
    Some(name.to_ascii_lowercase())
}

/// Whether the certificate in `dir` (fullchain.pem) is for a name; read
/// again once a minute.
pub fn cert_covers(dir: PathBuf) -> Arc<dyn Fn(&str) -> bool + Send + Sync> {
    let seen = Mutex::new((std::time::Instant::now() - Duration::from_secs(3600), None::<Vec<u8>>));
    Arc::new(move |name: &str| {
        let mut s = seen.lock().unwrap_or_else(|e| e.into_inner());
        if s.0.elapsed() > Duration::from_secs(60) {
            s.1 = std::fs::read(dir.join("fullchain.pem")).ok().and_then(|pem| {
                use rustls::pki_types::pem::PemObject;
                rustls::pki_types::CertificateDer::pem_slice_iter(&pem).next()?.ok().map(|c| c.to_vec())
            });
            s.0 = std::time::Instant::now();
        }
        let (Some(der), Ok(sn)) = (s.1.as_ref(), rustls::pki_types::ServerName::try_from(name.to_string())) else { return false };
        let der = rustls::pki_types::CertificateDer::from(der.as_slice());
        webpki::EndEntityCert::try_from(&der).is_ok_and(|ee| ee.verify_is_valid_for_subject_name(&sn).is_ok())
    })
}

/// What an edge answers itself while home is away (D-U45): this node's own pages API and its
/// OIDC provider, for sign-in, the session and other programs' sign-in, when `active` says it
/// keeps some account's secrets.
pub struct Local {
    pub router: axum::Router,
    pub active: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// The paths a local answer is for.
fn local_path(path: &str) -> bool {
    path.starts_with("/oidc/") || path == "/.well-known/openid-configuration" || matches!(path, "/api/signin" | "/api/signin/totp" | "/api/signin/fido2" | "/api/signout" | "/api/account")
}

struct Edge {
    link: Arc<Link>,
    home: Home,
    local: Option<Local>,
    roles: Roles,
    cache: Arc<Cache>,
    keep: Keep,
    /// home answered the last time it was asked
    up: AtomicBool,
    /// The public links last listed (None: not yet since the start): a new one is prefetched.
    links_seen: Mutex<Option<std::collections::HashSet<String>>>,
    /// When osada 0.2's page was last asked of home (its copy is refreshed at most once a minute).
    page2_at: Mutex<Option<std::time::Instant>>,
    /// the last asset list a page named, being fetched or fetched
    assets: Mutex<String>,
    /// the sites exposed here (D-U21)
    sites: Option<Arc<WebEdge>>,
}

/// An answer from home: its head, and its body still on the stream.
struct Got {
    status: StatusCode,
    headers: HeaderMap,
    body: quinn::RecvStream,
    /// The node it comes from (home: None).
    from: Option<NodeId>,
}

/// What a GET is, for what the edge keeps of it (osada-edge's classes).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Class {
    /// /static/<file>?v=<hash>: the same for ever
    Static,
    /// a page of osada 0.1's app (all are the same page)
    App,
    /// a public link's page (all are the same page)
    LinkPage,
    /// a public link's listing, info, markdown
    LinkJson,
    /// a file of a public link
    LinkFile,
    /// the rest; signed-in answers and osada 0.2's pages among it
    Other,
}

fn class(path: &str, query: Option<&str>) -> Class {
    if let Some(f) = path.strip_prefix("/static/") {
        let v = query.is_some_and(|q| q.split('&').any(|kv| kv.starts_with("v=")));
        return if v && !f.is_empty() && !f.contains('/') { Class::Static } else { Class::Other };
    }
    if let Some(rest) = path.strip_prefix("/s/") {
        let Some((_, sub)) = token_of(path).map(|t| (t, &rest[t.len()..])) else { return Class::Other };
        return match sub.trim_start_matches('/').split('/').next().unwrap_or("") {
            "" if !sub.starts_with("//") => Class::LinkPage,
            "p" => Class::LinkPage,
            "ls" | "info" | "md" => Class::LinkJson,
            "raw" => Class::LinkFile,
            _ => Class::Other,
        };
    }
    let zero2 = path == "/" || ["/signin", "/account", "/admin", "/chat", "/contacts", "/calendar", "/static2/", "/apptab", "/proxy/"].iter().any(|p| path.starts_with(p));
    let other = ["/api/", "/oidc/", "/.well-known/", "/_edge/"].iter().any(|p| path.starts_with(p)) || path == "/sw.js" || path == "/favicon.ico";
    if other || zero2 { Class::Other } else { Class::App }
}

/// The stream kind of an edge's copies for another edge (D-U40).
pub const CACHE_KIND: &str = "cache";
/// This process's edge cache, for the other edges (cache_server).
static CACHE: std::sync::OnceLock<Arc<Cache>> = std::sync::OnceLock::new();
/// Under this, a file comes from home without asking anyone (asking costs more than it saves).
const SMALL: u64 = 1 << 20;
/// From this on, the holders and home are weighed by their links and how busy they are.
const BIG: u64 = 250 << 20;

/// The `cache` stream kind: another edge, for a file home said yes to there (its key, etag and
/// size from home's probe), asks `{"op":"has"}`: is a whole copy here, or a fill and how far, how
/// busy is this node; `{"op":"get"}`: the bytes (from the copy, or following the fill).
pub fn cache_server() -> StreamHandler {
    Arc::new(move |_peer: NodeId, _kind: String, head: serde_json::Value, mut send: quinn::SendStream, _recv: quinn::RecvStream| {
        async move {
            let Some(c) = CACHE.get() else {
                let _ = send.reset(1u32.into());
                return;
            };
            let (key, etag, size) = (head["key"].as_str().unwrap_or("").to_string(), head["etag"].as_str().unwrap_or("").to_string(), head["size"].as_u64().unwrap_or(0));
            let have = c.has(&key, &etag, size);
            let filling = if have { None } else { c.filling(&key, &etag, size) };
            let busy = crate::linkstats::stats().busy().2;
            if head["op"].as_str() != Some("get") {
                if link::write_head(&mut send, serde_json::json!({ "have": have, "filling": filling, "busy": busy })).await.is_ok() {
                    let _ = send.finish();
                }
                return;
            }
            let body: Option<crate::edgecache::cache::Bytes> = match c.get(&key, &etag, size).await {
                Some(f) => Some(Box::pin(ReaderStream::with_capacity(f.take(size), 256 << 10))),
                None => c.follow(&key, &etag, size, 0, size),
            };
            let Some(mut body) = body else {
                let _ = link::write_head(&mut send, serde_json::json!({ "ok": false })).await;
                let _ = send.finish();
                return;
            };
            let _ = send.set_priority(-1);
            if link::write_head(&mut send, serde_json::json!({ "ok": true })).await.is_err() {
                return;
            }
            while let Some(b) = body.next().await {
                match b {
                    Ok(b) => {
                        if send.write_all(&b).await.is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = send.reset(1u32.into());
                        return;
                    }
                }
            }
            let _ = send.finish();
        }
        .boxed()
    })
}

/// osada 0.2's page: the same document at each of these paths (the data come by its API).
fn page2(path: &str) -> bool {
    matches!(path, "/" | "/signin" | "/account" | "/admin" | "/chat" | "/contacts" | "/calendar") || path.starts_with("/admin/")
}

/// osada 0.2's static files: `/static2/<hash>/<name>`, never changed under a hash.
fn static2(path: &str) -> bool {
    path.strip_prefix("/static2/").and_then(|r| r.split_once('/')).is_some_and(|(h, n)| !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()) && !n.is_empty() && !n.contains('/'))
}

/// The host a request is for (lower case, no port): what the edge keeps is per host, so another
/// name relayed to home (a proxy's, apptab) is never answered with osada's copies.
fn host_key(h: &HeaderMap) -> String {
    let v = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let host = if v.starts_with('[') { v.split(']').next().map(|x| format!("{x}]")).unwrap_or_default() } else { v.split(':').next().unwrap_or("").to_string() };
    host.to_ascii_lowercase()
}

fn accepts_gzip(h: &HeaderMap) -> bool {
    h.get_all(header::ACCEPT_ENCODING).iter().filter_map(|v| v.to_str().ok()).any(|v| v.contains("gzip"))
}

/// osada's cache keys are sha256 hex.
fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Answers that are a file body: home answers these to a probe.
fn is_file(path: &str) -> bool {
    path.starts_with("/api/raw/") || path.strip_prefix("/s/").and_then(|r| r.split_once('/')).is_some_and(|(_, rest)| rest == "raw" || rest.starts_with("raw/"))
}

fn what(cls: Class, pq: &str) -> String {
    match cls {
        Class::Static => format!("static:{pq}"),
        Class::App => "page:app".into(),
        Class::LinkPage => "page:link".into(),
        Class::LinkFile => format!("file:{}", pq.split('?').next().unwrap_or(pq)),
        _ => format!("link:{pq}"),
    }
}

/// the asset list in a page of osada 0.1's: <meta name="osada-assets" content="name=hash …">
fn meta_of(html: &[u8]) -> Option<String> {
    const M: &str = "<meta name=\"osada-assets\" content=\"";
    let s = std::str::from_utf8(html).ok()?;
    let rest = &s[s.find(M)? + M.len()..];
    Some(rest[..rest.find('"')?].to_string())
}

fn marked(status: StatusCode, json: bool, body: String) -> Response {
    let ctype = if json { "application/json" } else { "text/plain; charset=utf-8" };
    (status, [(header::CONTENT_TYPE, ctype), (header::CACHE_CONTROL, "no-store"), (HeaderName::from_static("x-shelf-offline"), "1")], body).into_response()
}

fn offline(why: &str) -> Response {
    marked(StatusCode::SERVICE_UNAVAILABLE, false, format!("osada at home cannot be reached right now ({why})\n"))
}

fn away_answer(json: bool) -> Response {
    let body = if json { "{\"error\":\"osada at home is not reachable\"}" } else { "osada at home is not reachable right now, try again in a minute\n" };
    marked(StatusCode::SERVICE_UNAVAILABLE, json, body.into())
}

fn from_kept(k: Kept, status: Option<StatusCode>, away: bool) -> Response {
    let mut r = Response::new(Body::from(k.body.clone()));
    *r.status_mut() = status.unwrap_or(k.status);
    let out = r.headers_mut();
    for (n, v) in &k.headers {
        out.append(n, v.clone());
    }
    if away {
        out.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        out.insert(HeaderName::from_static("x-shelf-offline"), HeaderValue::from_static("1"));
    } else {
        out.insert(HeaderName::from_static("x-edge-cache"), HeaderValue::from_static("hit"));
    }
    out.insert(header::CONTENT_LENGTH, HeaderValue::from(k.body.len()));
    r
}

/// "bytes=a-b", "bytes=a-", "bytes=-n" of `size` bytes -> (first, last); Err: not satisfiable; None: not one range
fn range(h: &str, size: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = h.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        return Some(if n == 0 { Err(()) } else { Ok((size.saturating_sub(n), size - 1)) });
    }
    let s: u64 = a.parse().ok()?;
    let e = if b.is_empty() { size - 1 } else { b.parse::<u64>().ok()?.min(size - 1) };
    Some(if s >= size || e < s { Err(()) } else { Ok((s, e)) })
}

/// "bytes 5-9/10" -> (5, 5)
fn content_range(v: &str) -> Option<(u64, u64)> {
    let (span, _) = v.strip_prefix("bytes ")?.split_once('/')?;
    let (a, b) = span.split_once('-')?;
    let (a, b) = (a.parse::<u64>().ok()?, b.parse::<u64>().ok()?);
    (b >= a).then_some((a, b - a + 1))
}

/// A response with home's headers (not the hop-by-hop ones, nor its length: the body decides).
fn head(status: StatusCode, h: &HeaderMap, body: Body) -> Response {
    let mut r = Response::new(body);
    *r.status_mut() = status;
    for (k, v) in h {
        if !dropped(k.as_str()) && k != header::CONTENT_LENGTH {
            r.headers_mut().append(k, v.clone());
        }
    }
    r
}

fn pass(g: Got) -> Response {
    let len = g.headers.get(header::CONTENT_LENGTH).cloned();
    let mut r = head(g.status, &g.headers, Body::from_stream(ReaderStream::new(g.body)));
    if let Some(n) = len {
        r.headers_mut().insert(header::CONTENT_LENGTH, n);
    }
    r
}

/// The body goes to the visitor and into the cache; if the visitor leaves half way, the copy is
/// still finished.
fn tee(g: Got, mut fill: Fill, etag: &str) -> Response {
    let len = g.headers.get(header::CONTENT_LENGTH).cloned();
    let etag = etag.to_string();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(8);
    let mut s = ReaderStream::new(g.body);
    tokio::spawn(async move {
        let mut tx = Some(tx);
        while let Some(c) = s.next().await {
            match c {
                Ok(b) => {
                    fill.write(&b).await;
                    if let Some(t) = &tx {
                        if t.send(Ok(b)).await.is_err() {
                            tx = None;
                        }
                    }
                    if tx.is_none() && fill.broken() {
                        return;
                    }
                }
                Err(e) => {
                    if let Some(t) = &tx {
                        let _ = t.send(Err(e)).await;
                    }
                    return;
                }
            }
        }
        fill.commit(&etag).await;
    });
    let mut r = head(g.status, &g.headers, Body::from_stream(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))));
    if let Some(n) = len {
        r.headers_mut().insert(header::CONTENT_LENGTH, n);
    }
    r
}

impl Edge {
    /// A request to home over the link: `headers` a visitor's (their x-shelf-* dropped), `extra` the
    /// edge's own words. `wait`: for answers that are quick or small; home not answering within it
    /// counts as away.
    async fn ask(&self, method: &Method, pq: &str, headers: &HeaderMap, extra: &[(&str, &str)], client: IpAddr, body: Option<Body>, wait: Option<Duration>) -> Result<Got, String> {
        let mut list = pass_on(headers, true);
        list.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let head = Head { method: method.as_str().into(), uri: pq.into(), headers: list, client };
        let head = serde_json::to_value(&head).map_err(|e| e.to_string())?;
        let go = async {
            let home = (self.home)().ok_or_else(|| "home is not known yet".to_string())?;
            let (mut send, mut recv) = self.link.open_stream(&home, KIND, head).await.map_err(|e| e.0)?;
            if body.is_some() && headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).is_none_or(|n| n > BULK) {
                let _ = send.set_priority(-1);
            }
            match body {
                // the body goes while the answer comes (an answer may come before the whole body)
                Some(body) => {
                    tokio::spawn(async move {
                        let mut s = body.into_data_stream();
                        while let Some(chunk) = s.next().await {
                            match chunk {
                                Ok(b) => {
                                    if send.write_all(&b).await.is_err() {
                                        return;
                                    }
                                }
                                Err(_) => {
                                    let _ = send.reset(1u32.into());
                                    return;
                                }
                            }
                        }
                        let _ = send.finish();
                    });
                }
                None => {
                    let _ = send.finish();
                }
            }
            let a: Answer = serde_json::from_value(link::read_head(&mut recv).await.map_err(|e| e.0)?).map_err(|e| e.to_string())?;
            let mut headers = HeaderMap::new();
            put(&mut headers, a.headers);
            Ok::<Got, String>(Got { status: StatusCode::from_u16(a.status).unwrap_or(StatusCode::BAD_GATEWAY), headers, body: recv, from: None })
        };
        let r = match wait {
            Some(w) => tokio::time::timeout(w, go).await.unwrap_or_else(|_| Err("no answer in time".into())),
            None => go.await,
        };
        self.up.store(r.is_ok(), Relaxed);
        r
    }

    /// A file: probe, then the copy or the body from home (kept for next time).
    async fn file(self: &Arc<Self>, pq: &str, h: &HeaderMap, client: IpAddr, public: bool) -> Result<Response, String> {
        self.file_for(pq, h, client, public, false).await
    }

    /// The same; `prefetch`: only to have the copy here (nothing when it is here or under way, or
    /// too big to keep).
    async fn file_for(self: &Arc<Self>, pq: &str, h: &HeaderMap, client: IpAddr, public: bool, prefetch: bool) -> Result<Response, String> {
        let wait = if self.up.load(Relaxed) { PROBE_WAIT } else { AWAY_WAIT };
        let probe = self.ask(&Method::GET, pq, h, &[("x-shelf-probe", "1")], client, None, Some(wait)).await?;
        let hv = |n: &str| probe.headers.get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
        // not a yes (an error, 304, 416): that is the answer
        let (Some(key), Some(size)) = (hv("x-shelf-cache"), hv("x-shelf-size").and_then(|s| s.parse::<u64>().ok())) else {
            return Ok(pass(probe));
        };
        let etag = hv("etag").unwrap_or_default();
        let span = match probe.status {
            StatusCode::OK => Some((0, size)),
            StatusCode::PARTIAL_CONTENT => hv("content-range").as_deref().and_then(content_range),
            _ => None,
        };
        // a file of a public link: which copy it is, for when home is away
        let path = pq.split('?').next().unwrap_or(pq);
        if public && span.is_some() && path.starts_with("/s/") && !self.keep.knows(&format!("file:{path}"), &etag) {
            self.keep.put(&format!("file:{path}"), StatusCode::OK, &probe.headers, b"", Some((&key, &etag, size))).await;
        }
        if prefetch && (self.cache.has(&key, &etag, size) || self.cache.filling(&key, &etag, size).is_some()) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        if let Some((start, len)) = span {
            if let Some(mut f) = self.cache.get(&key, &etag, size).await {
                if f.seek(SeekFrom::Start(start)).await.is_ok() {
                    let mut r = head(probe.status, &probe.headers, Body::from_stream(ReaderStream::with_capacity(f.take(len), 256 << 10)));
                    r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(len));
                    r.headers_mut().insert(HeaderName::from_static("x-edge-cache"), HeaderValue::from_static("hit"));
                    return Ok(r);
                }
            }
            // being fetched right now (another visitor, a prefetch): follow that, fetch nothing (D-U39)
            if let Some(stream) = self.cache.follow(&key, &etag, size, start, len) {
                let mut r = head(probe.status, &probe.headers, Body::from_stream(stream));
                r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(len));
                r.headers_mut().insert(HeaderName::from_static("x-edge-cache"), HeaderValue::from_static("follow"));
                return Ok(r);
            }
        }
        let fetch = [("x-shelf-probe", "fetch")];
        if probe.status != StatusCode::OK {
            // a range (video seeking): pass it on, fetch the whole file aside
            if let Some(fill) = self.cache.claim(&key, &etag, size) {
                let mut bh = h.clone();
                for k in [header::RANGE, header::IF_RANGE, header::IF_NONE_MATCH, header::IF_MODIFIED_SINCE] {
                    bh.remove(k);
                }
                let (edge, pq, etag) = (self.clone(), pq.to_string(), etag.clone());
                tokio::spawn(async move {
                    let Ok(g) = edge.fetch_body(&pq, &bh, client, &key, &etag, size).await else { return };
                    if g.status != StatusCode::OK || g.headers.get(header::ETAG).and_then(|v| v.to_str().ok()) != Some(etag.as_str()) {
                        return;
                    }
                    edge.fill_aside(g, fill, &etag).await;
                });
            }
            return self.ask(&Method::GET, pq, h, &fetch, client, None, None).await.map(pass);
        }
        // one fill per copy: claimed before the fetch, so a second request follows it (above)
        let fill = self.cache.claim(&key, &etag, size);
        if fill.is_none() && !prefetch {
            // refused: the copy was finished a moment ago, or a fill began meanwhile
            if let Some(f) = self.cache.get(&key, &etag, size).await {
                let mut r = head(probe.status, &probe.headers, Body::from_stream(ReaderStream::with_capacity(f.take(size), 256 << 10)));
                r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(size));
                r.headers_mut().insert(HeaderName::from_static("x-edge-cache"), HeaderValue::from_static("hit"));
                return Ok(r);
            }
            if let Some(stream) = self.cache.follow(&key, &etag, size, 0, size) {
                let mut r = head(probe.status, &probe.headers, Body::from_stream(stream));
                r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(size));
                r.headers_mut().insert(HeaderName::from_static("x-edge-cache"), HeaderValue::from_static("follow"));
                return Ok(r);
            }
        }
        if prefetch {
            if let Some(fill) = fill {
                let g = self.fetch_body(pq, h, client, &key, &etag, size).await?;
                if g.status == StatusCode::OK && g.headers.get(header::ETAG).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
                    self.fill_aside(g, fill, &etag).await;
                }
            }
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        let g = if fill.is_some() { self.fetch_body(pq, h, client, &key, &etag, size).await? } else { self.ask(&Method::GET, pq, h, &fetch, client, None, None).await? };
        // kept only as the version home said yes to (the file may have changed since the probe)
        let same = g.headers.get(header::ETAG).and_then(|v| v.to_str().ok()) == Some(etag.as_str());
        let whole = same && g.status == StatusCode::OK && g.headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()) == Some(size);
        let source = if g.from.is_some() { "peer" } else { "home" };
        let mut r = match (whole, fill) {
            (true, Some(fill)) => tee(g, fill, &etag),
            _ => pass(g),
        };
        r.headers_mut().insert(HeaderName::from_static("x-edge-source"), HeaderValue::from_static(source));
        Ok(r)
    }

    /// A copy filled in the background (nobody waiting on it reads it): its source's line is
    /// measured on the way (linkstats).
    async fn fill_aside(&self, g: Got, mut fill: Fill, etag: &str) {
        let start = std::time::Instant::now();
        let (from, mut n) = (g.from.clone().or_else(|| (self.home)()), 0u64);
        let mut s = ReaderStream::with_capacity(g.body, 256 << 10);
        while let Some(c) = s.next().await {
            let Ok(b) = c else { return };
            n += b.len() as u64;
            fill.write(&b).await;
            if fill.broken() {
                return;
            }
        }
        if let Some(p) = from {
            crate::linkstats::stats().record(&p, n, start.elapsed(), false);
        }
        fill.commit(etag).await;
    }

    /// The body of a file home said yes to (`key`, `etag`, `size` from its probe), for a copy here,
    /// from where it comes fastest (D-U40): under a megabyte from home without asking (asking costs
    /// more than it saves); up to 250 MB from another edge that has it (a whole copy, else one
    /// being filled), asked all at once, else home; above that, every holder and home are weighed:
    /// the link's rate from it (weekly tests, background fills) times how idle its uplink is now.
    async fn fetch_body(self: &Arc<Self>, pq: &str, h: &HeaderMap, client: IpAddr, key: &str, etag: &str, size: u64) -> Result<Got, String> {
        let from_home = || self.ask(&Method::GET, pq, h, &[("x-shelf-probe", "fetch")], client, None, None);
        let home = (self.home)();
        let peers: Vec<NodeId> = self.link.peers().into_iter().filter(|p| Some(p) != home.as_ref()).collect();
        if size < SMALL || peers.is_empty() || !valid_key(key) {
            return from_home().await;
        }
        let head = serde_json::json!({ "op": "has", "key": key, "etag": etag, "size": size });
        let wait = Duration::from_millis(if size < BIG { 400 } else { 800 });
        let asks = peers.iter().map(|p| {
            let (l, head, p) = (self.link.clone(), head.clone(), p.clone());
            async move {
                let go = async {
                    let (mut send, mut recv) = l.open_stream(&p, CACHE_KIND, head).await.ok()?;
                    let _ = send.finish();
                    link::read_head(&mut recv).await.ok()
                };
                let a = tokio::time::timeout(wait, go).await.ok().flatten();
                (p, a)
            }
        });
        let holders: Vec<(NodeId, bool, f64)> = futures_util::future::join_all(asks)
            .await
            .into_iter()
            .filter_map(|(p, a)| {
                let a = a?;
                let whole = a["have"].as_bool() == Some(true);
                (whole || a["filling"].is_u64()).then(|| (p, whole, a["busy"].as_f64().unwrap_or(0.0)))
            })
            .collect();
        let pick = if size < BIG {
            holders.iter().find(|(_, whole, _)| *whole).or(holders.first()).map(|(p, _, _)| p.clone())
        } else {
            let stats = crate::linkstats::stats();
            // unknown lines: a peer in the cloud is taken to be fast, home's uplink slow
            let score = |p: &NodeId, busy: f64, whole: bool| stats.mbit_in(p).unwrap_or(100.0) * (1.0 - busy).max(0.05) * if whole { 1.0 } else { 0.5 };
            let best = holders.iter().map(|(p, whole, busy)| (p.clone(), score(p, *busy, *whole))).max_by(|a, b| a.1.total_cmp(&b.1));
            match (best, home.as_ref()) {
                (Some((p, s)), Some(hm)) => {
                    let hb = crate::linkstats::ask_busy(&self.link, hm, wait).await.map_or(0.0, |x| x.0);
                    let hs = stats.mbit_in(hm).unwrap_or(10.0) * (1.0 - hb).max(0.05);
                    (s > hs).then_some(p)
                }
                (best, _) => best.map(|(p, _)| p),
            }
        };
        if let Some(p) = pick {
            match self.get_from(&p, key, etag, size).await {
                Ok(g) => return Ok(g),
                Err(e) => eprintln!("edge: {key} from {p}: {e}; from home"),
            }
        }
        from_home().await
    }

    /// A file's bytes from another edge's copy (or its fill).
    async fn get_from(&self, peer: &NodeId, key: &str, etag: &str, size: u64) -> Result<Got, String> {
        let head = serde_json::json!({ "op": "get", "key": key, "etag": etag, "size": size });
        let (mut send, mut recv) = self.link.open_stream(peer, CACHE_KIND, head).await.map_err(|e| e.0)?;
        let _ = send.finish();
        let a = tokio::time::timeout(PROBE_WAIT, link::read_head(&mut recv)).await.map_err(|_| "no answer".to_string())?.map_err(|e| e.0)?;
        if a["ok"].as_bool() != Some(true) {
            return Err("its copy is gone".into());
        }
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
        if let Ok(v) = HeaderValue::from_str(etag) {
            headers.insert(header::ETAG, v);
        }
        Ok(Got { status: StatusCode::OK, headers, body: recv, from: Some(peer.clone()) })
    }

    /// A page, a /static/ file or a link's JSON from home: kept when it is a good one.
    async fn keep_answer(self: &Arc<Self>, cls: Class, pq: &str, g: Got) -> Response {
        let what = what(cls, pq);
        let ctype = g.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let len = g.headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        let good = g.status == StatusCode::OK
            && match cls {
                Class::Static => g.headers.get(header::CACHE_CONTROL).is_some_and(|v| v.to_str().is_ok_and(|v| v.contains("immutable"))),
                Class::App | Class::LinkPage => ctype.starts_with("text/html"),
                _ => ctype.contains("json"),
            };
        if !good || len.is_none_or(|n| n > KEEP_MAX) {
            if cls == Class::LinkJson && g.status == StatusCode::NOT_FOUND {
                self.keep.forget(&what).await;
            }
            return pass(g);
        }
        let (status, h, mut body) = (g.status, g.headers, g.body);
        let bytes = match body.read_to_end(KEEP_MAX as usize).await {
            Ok(b) => b,
            Err(e) => return offline(&e.to_string()),
        };
        // a page: only osada 0.1's, which names its static files
        let meta = matches!(cls, Class::App | Class::LinkPage).then(|| meta_of(&bytes)).flatten();
        if !matches!(cls, Class::App | Class::LinkPage) || meta.is_some() {
            self.keep.put(&what, status, &h, &bytes, None).await;
        }
        if let Some(m) = meta {
            self.fetch_assets(m);
        }
        let mut r = head(status, &h, Body::from(bytes.clone()));
        r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        if cls == Class::Static {
            r.headers_mut().insert(HeaderName::from_static("x-edge-cache"), HeaderValue::from_static("miss"));
        }
        r
    }

    /// every /static/ file a page names, in the background; then versions no kept page names go
    fn fetch_assets(self: &Arc<Self>, list: String) {
        {
            let mut last = self.assets.lock().unwrap_or_else(|e| e.into_inner());
            if *last == list {
                return;
            }
            *last = list.clone();
        }
        let edge = self.clone();
        tokio::spawn(async move {
            let local = IpAddr::from([127, 0, 0, 1]);
            for kv in list.split(' ') {
                let Some((n, v)) = kv.split_once('=') else { continue };
                let pq = format!("/static/{n}?v={v}");
                if edge.keep.has(&format!("static:{pq}")) {
                    continue;
                }
                let mut h = HeaderMap::new();
                h.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
                let got = match edge.ask(&Method::GET, &pq, &h, &[], local, None, Some(KEEP_WAIT)).await {
                    Ok(g) if g.status == StatusCode::OK => {
                        let (h, mut b) = (g.headers, g.body);
                        b.read_to_end(64 << 20).await.ok().map(|b| (h, b))
                    }
                    _ => None,
                };
                let Some((h, b)) = got else {
                    // home went away: the next page tries again
                    edge.assets.lock().unwrap_or_else(|e| e.into_inner()).clear();
                    return;
                };
                edge.keep.put(&format!("static:{pq}"), StatusCode::OK, &h, &b, None).await;
            }
            let mut want = std::collections::HashSet::new();
            for p in ["page:app", "page:link"] {
                if let Some(m) = edge.keep.get(p).await.and_then(|k| meta_of(&k.body)) {
                    want.extend(m.split(' ').filter_map(|kv| kv.split_once('=')).map(|(n, v)| format!("static:/static/{n}?v={v}")));
                }
            }
            edge.keep.forget_where(|w| w.starts_with("static:") && !want.contains(w)).await;
        });
    }

    async fn kept_static(&self, pq: &str, h: &HeaderMap) -> Option<Response> {
        let k = self.keep.get(&format!("static:{pq}")).await?;
        // a browser without gzip (hardly any) gets it from home, when home is there
        let gzip_ok = h.get(header::ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).is_some_and(|v| v.contains("gzip"));
        if is_gzip(&k.headers) && !gzip_ok && self.up.load(Relaxed) {
            return None;
        }
        Some(from_kept(k, None, false))
    }

    /// The answer while home is away: what was kept, else a 503; both marked.
    async fn offline(&self, cls: Class, method: &Method, uri: &Uri, h: &HeaderMap) -> Response {
        let (path, pq) = (uri.path(), uri.path_and_query().map_or("/", |p| p.as_str()));
        let json = path.starts_with("/api/") || cls == Class::LinkJson;
        if method != Method::GET {
            return away_answer(json);
        }
        let link = token_of(path).and_then(|t| self.keep.link(t));
        let fresh = |k: &Kept| k.at.elapsed().is_ok_and(|d| d < KEEP_FOR);
        match cls {
            Class::Static => {
                if let Some(k) = self.keep.get(&what(cls, pq)).await {
                    return from_kept(k, None, false);
                }
            }
            Class::App => {
                if let Some(k) = self.keep.get("page:app").await {
                    return from_kept(k, None, true);
                }
            }
            Class::LinkPage => {
                if let (Some(ok), Some(k)) = (link, self.keep.get("page:link").await) {
                    return from_kept(k, (!ok).then_some(StatusCode::NOT_FOUND), true);
                }
            }
            Class::LinkJson | Class::LinkFile if link == Some(false) => {
                return marked(StatusCode::NOT_FOUND, json, if json { "{\"error\":\"not found\"}".into() } else { "not found\n".into() });
            }
            Class::LinkJson if link == Some(true) => {
                if let Some(k) = self.keep.get(&what(cls, pq)).await.filter(fresh) {
                    return from_kept(k, None, true);
                }
            }
            Class::LinkFile if link == Some(true) => {
                if let Some(r) = self.kept_file(path, uri.query(), h).await {
                    return r;
                }
            }
            _ => {}
        }
        away_answer(json)
    }

    /// a file of a link, from the file cache, whole or a range
    async fn kept_file(&self, path: &str, query: Option<&str>, h: &HeaderMap) -> Option<Response> {
        let k = self.keep.get(&format!("file:{path}")).await.filter(|k| k.at.elapsed().is_ok_and(|d| d < KEEP_FOR))?;
        let (key, etag, size) = k.object.clone()?;
        let mut f = self.cache.get(&key, &etag, size).await?;
        let range = h.get(header::RANGE).and_then(|v| v.to_str().ok()).filter(|_| size > 0).and_then(|r| range(r, size));
        let (status, start, len) = match range {
            None => (StatusCode::OK, 0, size),
            Some(Err(())) => {
                let cr = format!("bytes */{size}");
                return Some((StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, cr.as_str()), (HeaderName::from_static("x-shelf-offline"), "1")]).into_response());
            }
            Some(Ok((s, e))) => (StatusCode::PARTIAL_CONTENT, s, e - s + 1),
        };
        f.seek(SeekFrom::Start(start)).await.ok()?;
        let mut r = Response::new(Body::from_stream(ReaderStream::with_capacity(f.take(len), 256 << 10)));
        *r.status_mut() = status;
        let out = r.headers_mut();
        for (n, v) in &k.headers {
            out.append(n, v.clone());
        }
        if status == StatusCode::PARTIAL_CONTENT {
            out.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {start}-{}/{size}", start + len - 1)).ok()?);
        }
        // ?dl=1: a download
        let dl = query.is_some_and(|q| q.split('&').any(|kv| kv == "dl" || kv.starts_with("dl=")));
        let inline = out.get(header::CONTENT_DISPOSITION).and_then(|v| v.to_str().ok()).and_then(|d| d.strip_prefix("inline")).map(str::to_string);
        if let (true, Some(rest)) = (dl, inline) {
            out.insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&format!("attachment{rest}")).ok()?);
        }
        out.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        out.insert(HeaderName::from_static("x-shelf-offline"), HeaderValue::from_static("1"));
        out.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
        Some(r)
    }

    /// osada 0.2's static file: from the copy (kept as home sends it: gzip), else from home, kept.
    async fn static2(self: &Arc<Self>, path: &str, h: &HeaderMap, client: IpAddr) -> Response {
        let what = format!("static2:{}{path}", host_key(h));
        let gzip_ok = accepts_gzip(h);
        if let Some(k) = self.keep.get(&what).await {
            if gzip_ok || !is_gzip(&k.headers) {
                return from_kept(k, None, false);
            }
        }
        let mut hh = h.clone();
        hh.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        let g = match self.ask(&Method::GET, path, &hh, &[], client, None, Some(KEEP_WAIT)).await {
            Ok(g) => g,
            Err(e) => return offline(&e),
        };
        let immutable = g.headers.get(header::CACHE_CONTROL).is_some_and(|v| v.to_str().is_ok_and(|v| v.contains("immutable")));
        let len = g.headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        if g.status != StatusCode::OK || !immutable || len.is_none_or(|n| n > KEEP_MAX) {
            return pass(g);
        }
        let (status, headers, mut body) = (g.status, g.headers, g.body);
        let Ok(bytes) = body.read_to_end(KEEP_MAX as usize).await else { return offline("the file did not come whole") };
        self.keep.put(&what, status, &headers, &bytes, None).await;
        let mut r = head(status, &headers, Body::from(bytes.clone()));
        if is_gzip(&headers) && !gzip_ok {
            // a client without gzip (rare): the plain bytes
            let mut plain = Vec::new();
            if std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut plain).is_ok() {
                r = head(status, &headers, Body::from(plain.clone()));
                r.headers_mut().remove(header::CONTENT_ENCODING);
                r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(plain.len()));
                return r;
            }
        }
        r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        r.headers_mut().insert(HeaderName::from_static("x-edge-cache"), HeaderValue::from_static("miss"));
        r
    }

    /// osada 0.2's page: from the copy at once, and asked of home again in the background (at most
    /// once a minute), so the site shows at the edge's speed whatever home's line does; what needs
    /// home comes by the API, and the page says when home is slow or away (D-U39).
    async fn page2(self: &Arc<Self>, pq: &str, h: &HeaderMap, client: IpAddr) -> Response {
        let host = host_key(h);
        if let Some(k) = self.keep.get(&format!("page2:{host}")).await {
            let due = {
                let mut at = self.page2_at.lock().unwrap_or_else(|e| e.into_inner());
                let due = at.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
                if due {
                    *at = Some(std::time::Instant::now());
                }
                due
            };
            if due {
                let (edge, pq, h) = (self.clone(), pq.to_string(), h.clone());
                tokio::spawn(async move {
                    if let Ok(g) = edge.ask(&Method::GET, &pq, &h, &[], client, None, Some(KEEP_WAIT)).await {
                        let _ = edge.keep_page2(g, &host_key(&h)).await;
                    }
                });
            }
            let mut r = from_kept(k, None, false);
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            return r;
        }
        match self.ask(&Method::GET, pq, h, &[], client, None, Some(KEEP_WAIT)).await {
            Ok(g) => self.keep_page2(g, &host).await,
            Err(e) => offline(&e),
        }
    }

    /// Keeps osada 0.2's page when it is one, and drops the static files of hashes it no longer names.
    /// Only osada 0.2's own page is kept (it names /static2/ files): another host's answer at these
    /// paths (a proxy's page) goes on as it came, every time.
    async fn keep_page2(self: &Arc<Self>, g: Got, host: &str) -> Response {
        let html = g.headers.get(header::CONTENT_TYPE).is_some_and(|v| v.as_bytes().starts_with(b"text/html"));
        if g.status != StatusCode::OK || !html {
            return pass(g);
        }
        let (status, headers, mut body) = (g.status, g.headers, g.body);
        let Ok(bytes) = body.read_to_end(KEEP_MAX as usize).await else { return offline("the page did not come whole") };
        let text = String::from_utf8_lossy(&bytes);
        let hash = text.split("/static2/").nth(1).and_then(|r| r.split('/').next()).filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()));
        if let Some(hash) = hash {
            let (mine, now) = (format!("static2:{host}/static2/"), format!("static2:{host}/static2/{hash}/"));
            self.keep.forget_where(|w| w.starts_with(&mine) && !w.starts_with(&now)).await;
            self.keep.put(&format!("page2:{host}"), status, &headers, &bytes, None).await;
        }
        let mut r = head(status, &headers, Body::from(bytes.clone()));
        r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        r
    }

    /// Public links made since the last look: their page and file are fetched now (D-U40), so even
    /// the first visitor gets them at the edge's speed. After 0..20 s, so the edges do not all ask
    /// home at once: the later ones find the copy (being filled) at the first.
    fn prefetch(self: &Arc<Self>, tokens: Vec<String>) {
        let edge = self.clone();
        tokio::spawn(async move {
            use rand::Rng;
            let wait = rand::thread_rng().gen_range(0..20_000u64);
            tokio::time::sleep(Duration::from_millis(wait)).await;
            let local = IpAddr::from([127, 0, 0, 1]);
            for t in tokens {
                let page = format!("/s/{t}");
                if let Ok(g) = edge.ask(&Method::GET, &page, &HeaderMap::new(), &[], local, None, Some(KEEP_WAIT)).await {
                    drop(edge.keep_answer(Class::LinkPage, &page, g).await);
                }
                match edge.file_for(&format!("/s/{t}/raw"), &HeaderMap::new(), local, true, true).await {
                    Ok(_) => println!("edge: public link {}…: prefetched", &t[..t.len().min(6)]),
                    Err(e) => eprintln!("edge: public link {}…: {e}", &t[..t.len().min(6)]),
                }
            }
        });
    }

    /// Every 10 s: is home there; with public-cache, which public links work (for when it is not).
    async fn watch(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            let up = match (self.home)() {
                Some(h) => self.link.ping(&h).await.is_ok(),
                None => false,
            };
            self.up.store(up, Relaxed);
            if !up || !(self.roles)().1 {
                continue;
            }
            let local = IpAddr::from([127, 0, 0, 1]);
            if let Ok(g) = self.ask(&Method::GET, "/_edge/links", &HeaderMap::new(), &[], local, None, Some(Duration::from_secs(10))).await {
                let plain = g.headers.get(header::CONTENT_TYPE).is_some_and(|v| v.as_bytes().starts_with(b"text/plain"));
                if g.status.is_success() && plain {
                    let mut b = g.body;
                    if let Ok(t) = b.read_to_end(16 << 20).await {
                        let text = String::from_utf8_lossy(&t).into_owned();
                        self.keep.set_links(&text).await;
                        let now: std::collections::HashSet<String> = text.lines().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect();
                        let new: Vec<String> = {
                            let mut seen = self.links_seen.lock().unwrap_or_else(|e| e.into_inner());
                            let new = seen.as_ref().map(|s| now.difference(s).cloned().collect()).unwrap_or_default();
                            *seen = Some(now);
                            new
                        };
                        if !new.is_empty() {
                            self.prefetch(new);
                        }
                    }
                }
            }
        }
    }
}

async fn relay(State(e): State<Arc<Edge>>, ConnectInfo(PeerAddr(addr)): ConnectInfo<PeerAddr>, req: Request) -> Response {
    let client = addr.ip().to_canonical();
    // a site exposed here (D-U21)
    let req = match (&e.sites, host_of(&req)) {
        (Some(w), Some(host)) if (w.site_of)(&host).is_some() => match w.answer(&host, req, client, true).await {
            Some(r) => return r,
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        _ => req,
    };
    let (parts, body) = req.into_parts();
    let mut headers = parts.headers.clone();
    // HTTP/2 or an absolute URI: the name is in the URI, not a Host header
    if !headers.contains_key(header::HOST) {
        if let Some(v) = parts.uri.authority().and_then(|a| HeaderValue::from_str(a.as_str()).ok()) {
            headers.insert(header::HOST, v);
        }
    }
    let path = parts.uri.path();
    let pq = parts.uri.path_and_query().map_or("/", |p| p.as_str()).to_string();
    // the edge's own words to home are not a visitor's
    if path.starts_with("/_edge/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    // home away: sign-in, the session and other programs' sign-in here, when this node keeps
    // secrets (D-U45)
    if let Some(local) = &e.local {
        // away: no live link to home (a stopping home says so at once; a lost one shows within
        // the link's idle time); the link back, and home answers again
        let away = (e.home)().is_none_or(|h| !e.link.peers().contains(&h));
        if local_path(path) && away && (local.active)() {
            let mut req = Request::from_parts(parts, body);
            req.extensions_mut().insert(Remote(client));
            req.extensions_mut().insert(crate::osada1::tls::Https);
            return match local.router.clone().oneshot(req).await {
                Ok(r) => r,
                Err(never) => match never {},
            };
        }
    }
    let (cache_on, public_on, _) = (e.roles)();
    let cls = class(path, parts.uri.query());
    let get = parts.method == Method::GET;
    // osada 0.2's page and static files: every edge answers them itself (D-U39)
    if get && static2(path) {
        return e.static2(path, &headers, client).await;
    }
    if get && page2(path) {
        return e.page2(&pq, &headers, client).await;
    }
    if get && is_file(path) && (cache_on || (public_on && path.starts_with("/s/"))) {
        return match e.file(&pq, &headers, client, public_on).await {
            Ok(r) => r,
            Err(err) if public_on => {
                let _ = err;
                e.offline(cls, &parts.method, &parts.uri, &headers).await
            }
            Err(err) => offline(&err),
        };
    }
    if get && public_on {
        if cls == Class::Static {
            if let Some(r) = e.kept_static(&pq, &headers).await {
                return r;
            }
        }
        if matches!(cls, Class::Static | Class::App | Class::LinkPage | Class::LinkJson) {
            return match e.ask(&Method::GET, &pq, &headers, &[], client, None, Some(KEEP_WAIT)).await {
                Ok(g) => e.keep_answer(cls, &pq, g).await,
                Err(_) => e.offline(cls, &parts.method, &parts.uri, &headers).await,
            };
        }
    }
    match e.ask(&parts.method, &pq, &headers, &[], client, Some(body), None).await {
        Ok(g) => pass(g),
        Err(_) if public_on => e.offline(cls, &parts.method, &parts.uri, &headers).await,
        Err(err) => offline(&err),
    }
}

/// The edge's listeners until the process ends: https on `bind`, relayed to `home`; with `plain`,
/// http for certbot and a redirect to https. What it keeps is in `<data>/cache` and `<data>/keep`.
/// With `pass`, TLS for the host names of this node's mail instances goes on
/// to their servers untouched (crate::passthrough, D-U16).
pub async fn serve_edge(
    link: Arc<Link>,
    home: Home,
    data: PathBuf,
    // where the copies are kept (cache/, keep/): the data folder, or the node's files folder (D-U41)
    files: PathBuf,
    bind: SocketAddr,
    plain: Option<SocketAddr>,
    roles: Roles,
    pass: Option<crate::passthrough::Pass>,
    sites: Option<Arc<WebEdge>>,
    certs: Arc<crate::acme::Store>,
    local: Option<Local>,
) -> Result<(), String> {
    // a certificate per name: <data>/certs/*/ and edge-tls/ (D-U26)
    let tls = certs.config();
    let gb = (roles)().2.max(1) as u64;
    let cache = Cache::open(files.join("cache"), gb << 30).map_err(|e| e.to_string())?;
    let _ = CACHE.set(cache.clone());
    let keep = Keep::open(files.join("keep")).map_err(|e| e.to_string())?;
    let edge = Arc::new(Edge { link, home, local, roles, cache, keep, up: AtomicBool::new(false), links_seen: Mutex::new(None), page2_at: Mutex::new(None), assets: Mutex::new(String::new()), sites: sites.clone() });
    tokio::spawn(edge.clone().watch());
    crate::accesslog::init(&data);
    let router = Router::new()
        .fallback(relay)
        .with_state(edge)
        .layer(axum::middleware::from_fn(crate::bans::guard))
        .layer(axum::middleware::from_fn(crate::accesslog::tls));
    let listener = TlsListener::bind_passing(&bind.to_string(), tls, pass).await.map_err(|e| format!("{bind}: {e}"))?;
    if let Some(plain) = plain {
        let acme = data.join("acme");
        let r = Router::new()
            .fallback(move |ConnectInfo(PeerAddr(addr)): ConnectInfo<PeerAddr>, req: Request| plain_http(acme.clone(), sites.clone(), addr.ip().to_canonical(), req))
            .layer(axum::middleware::from_fn(crate::bans::guard))
            .layer(axum::middleware::from_fn(crate::accesslog::plain));
        let l = tokio::net::TcpListener::bind(plain).await.map_err(|e| format!("{plain}: {e}"))?;
        tokio::spawn(async move {
            if let Err(e) = axum::serve(l, r.into_make_service_with_connect_info::<PeerAddr>()).await {
                eprintln!("edge http: {e}");
            }
        });
    }
    axum::serve(listener, router.into_make_service_with_connect_info::<PeerAddr>()).await.map_err(|e| e.to_string())
}

/// Plain http at the edge: certbot's challenges; a site exposed here (those
/// behind Cloudflare are served, the others go to https); a name this edge
/// has no certificate for, to the fallback (nginx) when there is one; else
/// the same address over https.
async fn plain_http(acme: PathBuf, sites: Option<Arc<WebEdge>>, peer: IpAddr, req: Request) -> Response {
    let acme_path = req.uri().path().starts_with("/.well-known/acme-challenge/");
    // a token another node published (its certificate's issuer, D-U26)
    if let (true, Some(w), Some(host)) = (acme_path, &sites, host_of(&req)) {
        let token = req.uri().path().trim_start_matches("/.well-known/acme-challenge/").to_string();
        let here = acme.join(".well-known/acme-challenge").join(&token);
        if !token.is_empty() && !token.contains('/') && !here.exists() {
            if let Some(k) = (w.acme)(host, token).await {
                return ([(header::CONTENT_TYPE, "text/plain")], k).into_response();
            }
        }
    }
    if let (Some(w), Some(host), false) = (&sites, host_of(&req), acme_path) {
        if (w.site_of)(&host).is_some() {
            return w.answer(&host, req, peer, false).await.unwrap_or_else(|| StatusCode::NOT_FOUND.into_response());
        }
        if let Some(fb) = w.fallback_http.as_deref().filter(|_| !(w.ours)(&host)) {
            let v = crate::websrv::Visitor { ip: peer, proto: "http", host: host.clone() };
            return w.web.proxy(fb, None, req, &v, true).await;
        }
    }
    challenge(acme, req.uri().clone(), req.headers().clone(), req.method().clone()).await
}

/// certbot's challenges, else the same address over https.
async fn challenge(acme: PathBuf, uri: Uri, h: HeaderMap, m: Method) -> Response {
    if let Some(token) = uri.path().strip_prefix("/.well-known/acme-challenge/") {
        if !token.is_empty() && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return match tokio::fs::read(acme.join(".well-known/acme-challenge").join(token)).await {
                Ok(b) => ([(header::CONTENT_TYPE, "text/plain")], b).into_response(),
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            };
        }
    }
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).map(|h| h.rsplit_once(':').map_or(h, |(n, _)| n));
    match host {
        Some(host) if m == Method::GET || m == Method::HEAD => {
            let to = format!("https://{host}{}", uri.path_and_query().map_or("/", |p| p.as_str()));
            (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, to)]).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, "https only\n").into_response(),
    }
}

// ---- at home --------------------------------------------------------------

/// Set on a request an edge relayed: that edge, the node the browser is connected to (the pages
/// show it; a client cannot set an extension, unlike a header).
#[derive(Clone, Debug)]
pub struct Via(pub NodeId);

/// What takes relayed requests at home: `router` (osada 0.1's, as for its
/// edge listener, once it is there), for nodes `may` says may relay;
/// `proxy` tells a proxy's name (those get `x-shelf-host`, as from osada-edge).
pub fn home(
    router: Arc<std::sync::OnceLock<Router>>,
    may: Arc<dyn Fn(&NodeId) -> bool + Send + Sync>,
    proxy: Arc<dyn Fn(&str) -> bool + Send + Sync>,
) -> StreamHandler {
    Arc::new(move |peer: NodeId, kind: String, head: serde_json::Value, mut send: quinn::SendStream, recv: quinn::RecvStream| {
        let router = router.clone();
        let may = may.clone();
        let proxy = proxy.clone();
        async move {
            let ready = router.get().cloned();
            let (Some(router), true, true) = (ready, kind == KIND, may(&peer)) else {
                let _ = send.reset(1u32.into());
                return;
            };
            let Ok(h) = serde_json::from_value::<Head>(head) else {
                let _ = send.reset(1u32.into());
                return;
            };
            let mut req = Request::new(Body::from_stream(ReaderStream::new(recv)));
            *req.method_mut() = Method::from_bytes(h.method.as_bytes()).unwrap_or(Method::GET);
            *req.uri_mut() = h.uri.parse().unwrap_or_else(|_| Uri::from_static("/"));
            put(req.headers_mut(), h.headers);
            // a proxy's name, as osada-edge says it (apptab.rs)
            let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).map(|h| h.rsplit_once(':').map_or(h, |(n, _)| n).to_ascii_lowercase());
            if let Some(v) = host.filter(|h| proxy(h)).and_then(|h| HeaderValue::from_str(&h).ok()) {
                req.headers_mut().insert(HeaderName::from_static("x-shelf-host"), v);
            }
            req.extensions_mut().insert(Remote(h.client));
            req.extensions_mut().insert(Via(peer.clone()));
            let Ok(res) = router.oneshot(req).await;
            let (parts, body) = res.into_parts();
            let answer = Answer { status: parts.status.as_u16(), headers: pass_on(&parts.headers, false) };
            // a big body (a file) goes after the small answers on the same link (D-U39)
            if parts.headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).is_none_or(|n| n > BULK) {
                let _ = send.set_priority(-1);
            }
            let Ok(answer) = serde_json::to_value(&answer) else { return };
            if link::write_head(&mut send, answer).await.is_err() {
                return;
            }
            let mut s = body.into_data_stream();
            while let Some(chunk) = s.next().await {
                match chunk {
                    Ok(b) => {
                        if send.write_all(&b).await.is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = send.reset(1u32.into());
                        return;
                    }
                }
            }
            let _ = send.finish();
        }
        .boxed()
    })
}
