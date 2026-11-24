//! Serving a site (D-U21) on the node it runs on: a route's or the site's
//! upstream proxied (nginx's proxy_pass: an upstream with a path takes the
//! place of the matched prefix, one without gets the whole path), a
//! container at the port its manager published on 127.0.0.1, static files
//! from the site's folder `<data>/web/<site>/` (D-U22).
//!
//! Also what an edge needs at its plain http port for sites behind
//! Cloudflare: Cloudflare's addresses, and the visitor's address from
//! CF-Connecting-IP.

use std::{
    io::SeekFrom,
    net::IpAddr,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::TryStreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{
    osada1::edge::Cidr,
    sites::{Content, Site},
};

/// Headers that do not pass a proxy, either way.
const HOP: &[&str] = &[
    "connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding",
    "upgrade", "host", "x-real-ip", "x-forwarded-for", "x-forwarded-proto", "x-forwarded-host", "forwarded",
];

fn hop(name: &str) -> bool {
    HOP.contains(&name) || name.starts_with("x-shelf-") || name.starts_with("cf-")
}

/// Where a site's folder linked to a osada folder is (D-U22): on the node
/// that has osada's files (home); None elsewhere, or when it is not one.
pub type Links = Arc<dyn Fn(&crate::sites::Link) -> Option<PathBuf> + Send + Sync>;

/// The sites' server on a node.
pub struct Web {
    data: PathBuf,
    http: reqwest::Client,
    links: Option<Links>,
}

/// Who asked, as the site should see it.
#[derive(Clone, Debug)]
pub struct Visitor {
    pub ip: IpAddr,
    /// `https` or `http`, as the visitor's browser spoke.
    pub proto: &'static str,
    /// The host name asked for.
    pub host: String,
}

fn plain(status: StatusCode, text: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{text}\n")).into_response()
}

impl Web {
    pub fn new(data: &Path) -> Arc<Web> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60))
            .http1_only()
            .build()
            .expect("an http client");
        Arc::new(Web { data: data.to_path_buf(), http, links: None })
    }

    /// The same, where osada's files are: sites linked to a osada folder are served from it.
    pub fn with_links(data: &Path, links: Links) -> Arc<Web> {
        let mut w = Arc::try_unwrap(Web::new(data)).ok().expect("a new Web");
        w.links = Some(links);
        Arc::new(w)
    }

    /// A site's webroot: its folder, or the osada folder it links to.
    pub fn root(&self, site: &str, s: &Site) -> Option<PathBuf> {
        match &s.content {
            Content::Static { link: Some(l), .. } => self.links.as_ref().and_then(|f| f(l)),
            _ => Some(self.folder(site)),
        }
    }

    /// The site's folder: its webroot, always (D-U22).
    pub fn folder(&self, site: &str) -> PathBuf {
        self.data.join("web").join(site)
    }

    /// The port podman published the site's container on (127.0.0.1), as its
    /// manager wrote it down.
    pub fn container_port(&self, site: &str) -> Option<u16> {
        let ports: std::collections::BTreeMap<String, u16> = serde_json::from_slice(&std::fs::read(self.data.join("web/ports.json")).ok()?).ok()?;
        ports.get(site).copied()
    }

    /// The answer of the site `name` to `req`.
    pub async fn serve(&self, name: &str, site: &Site, req: Request, v: &Visitor) -> Response {
        let mut res = self.serve_content(name, site, req, v).await;
        // the site's own headers (Round 18), where its answer has none of that name
        for (k, val) in &site.headers {
            if let (Ok(k), Ok(val)) = (HeaderName::from_bytes(k.to_ascii_lowercase().as_bytes()), HeaderValue::from_str(val)) {
                if !res.headers().contains_key(&k) {
                    res.headers_mut().insert(k, val);
                }
            }
        }
        res
    }

    async fn serve_content(&self, name: &str, site: &Site, req: Request, v: &Visitor) -> Response {
        let path = req.uri().path().to_string();
        // a route keeps the visitor's Host (nginx's proxy_set_header Host $host)
        if let Some(r) = site.route(&path) {
            return self.proxy(&r.upstream, Some(&r.prefix), req, v, true).await;
        }
        match &site.content {
            Content::Upstream { url } => self.proxy(url, None, req, v, false).await,
            Content::Container(_) => match self.container_port(name) {
                Some(p) => self.proxy(&format!("http://127.0.0.1:{p}"), None, req, v, false).await,
                None => plain(StatusCode::SERVICE_UNAVAILABLE, &format!("{}: its container is not running", v.host)),
            },
            Content::Job(_) => plain(StatusCode::NOT_FOUND, &format!("{}: a job, not a site", v.host)),
            Content::Static { spa, not_found, .. } => match self.root(name, site) {
                Some(root) => self.files_in(&root, *spa, not_found.as_deref(), req).await,
                None => plain(StatusCode::SERVICE_UNAVAILABLE, &format!("{}: its folder is a osada folder, served by the node with osada's files", v.host)),
            },
        }
    }

    /// `req` to `upstream`; `prefix`: the route's, which an upstream with a
    /// path takes the place of. `keep_host`: the visitor's Host header goes
    /// on (a server that tells its sites by it, as nginx behind the edge),
    /// else the upstream's.
    pub async fn proxy(&self, upstream: &str, prefix: Option<&str>, req: Request, v: &Visitor, keep_host: bool) -> Response {
        let pq = req.uri().path_and_query().map_or("/", |p| p.as_str()).to_string();
        let target = target(upstream, prefix, &pq);
        if req.headers().get(header::UPGRADE).is_some() {
            return upgrade(&target, req, v, keep_host).await;
        }
        let (parts, body) = req.into_parts();
        let mut h = HeaderMap::new();
        for (k, val) in &parts.headers {
            if !hop(k.as_str()) {
                h.append(k, val.clone());
            }
        }
        let ip = v.ip.to_canonical().to_string();
        for (k, val) in [("x-forwarded-for", ip.as_str()), ("x-real-ip", ip.as_str()), ("x-forwarded-proto", v.proto), ("x-forwarded-host", v.host.as_str())] {
            if let Ok(val) = HeaderValue::from_str(val) {
                h.insert(HeaderName::from_static(k), val);
            }
        }
        if keep_host {
            if let Some(host) = parts.headers.get(header::HOST) {
                h.insert(header::HOST, host.clone());
            }
        }
        let has_body = parts.headers.contains_key(header::CONTENT_LENGTH) || parts.headers.contains_key(header::TRANSFER_ENCODING);
        let mut rb = self.http.request(parts.method.clone(), &target).headers(h);
        if has_body {
            rb = rb.body(reqwest::Body::wrap_stream(body.into_data_stream()));
        }
        let r = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("{}: {} {target}: {e}", v.host, parts.method);
                return plain(StatusCode::BAD_GATEWAY, &format!("{}: the server behind it does not answer", v.host));
            }
        };
        let mut b = Response::builder().status(r.status());
        let out = b.headers_mut().expect("a fresh builder");
        for (k, val) in r.headers() {
            if !hop(k.as_str()) {
                out.append(k, val.clone());
            }
        }
        b.body(Body::from_stream(r.bytes_stream().map_err(std::io::Error::other))).expect("a response")
    }

    /// A file of the site's folder: `index.html` for a folder; with `spa`,
    /// `index.html` for a path with no file; else the site's `404.html` or a
    /// plain 404. ETag, 304, byte ranges.
    #[cfg_attr(not(test), allow(dead_code))]
    async fn files(&self, name: &str, spa: bool, not_found: Option<&str>, req: Request) -> Response {
        let root = self.folder(name);
        self.files_in(&root, spa, not_found, req).await
    }

    async fn files_in(&self, root: &Path, spa: bool, not_found: Option<&str>, req: Request) -> Response {
        if req.method() != Method::GET && req.method() != Method::HEAD {
            return plain(StatusCode::METHOD_NOT_ALLOWED, "GET or HEAD");
        }
        let root = root.to_path_buf();
        let Some(rel) = safe_path(req.uri().path()) else { return plain(StatusCode::BAD_REQUEST, "not a path") };
        let mut file = root.join(&rel);
        // clean URLs: /popis is popis.html when there is no /popis (nginx's try_files $uri $uri.html)
        if tokio::fs::metadata(&file).await.is_err() && !rel.as_os_str().is_empty() {
            let html = root.join(format!("{}.html", rel.display()));
            if tokio::fs::metadata(&html).await.is_ok_and(|m| m.is_file()) {
                file = html;
            }
        }
        if tokio::fs::metadata(&file).await.is_ok_and(|m| m.is_dir()) {
            if !req.uri().path().ends_with('/') {
                let to = format!("{}/{}", req.uri().path(), req.uri().query().map(|q| format!("?{q}")).unwrap_or_default());
                return (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, to)]).into_response();
            }
            file = file.join("index.html");
        }
        let mut status = StatusCode::OK;
        if !tokio::fs::metadata(&file).await.is_ok_and(|m| m.is_file()) {
            if spa {
                file = root.join("index.html");
            } else {
                file = root.join(not_found.unwrap_or("404.html"));
                status = StatusCode::NOT_FOUND;
            }
            if !tokio::fs::metadata(&file).await.is_ok_and(|m| m.is_file()) {
                return plain(StatusCode::NOT_FOUND, "not found");
            }
        }
        // what it really is stays in the root (a symlink in a linked osada folder must not lead out)
        let inside = match (tokio::fs::canonicalize(&file).await, tokio::fs::canonicalize(&root).await) {
            (Ok(f), Ok(r)) => f.starts_with(&r),
            _ => false,
        };
        if !inside {
            return plain(StatusCode::NOT_FOUND, "not found");
        }
        // a compressed copy next to it (nginx's brotli_static/gzip_static): file.br, file.gz
        if status == StatusCode::OK {
            for (enc, ext) in [("br", "br"), ("gzip", "gz")] {
                if !accepts(req.headers(), enc) {
                    continue;
                }
                let mut c = file.clone().into_os_string();
                c.push(format!(".{ext}"));
                let c = PathBuf::from(c);
                let ok = match (tokio::fs::symlink_metadata(&c).await, tokio::fs::canonicalize(&root).await) {
                    (Ok(m), Ok(r)) if m.is_file() => tokio::fs::canonicalize(&c).await.is_ok_and(|f| f.starts_with(&r)),
                    _ => false,
                };
                if ok {
                    return send_file_as(&c, &file, Some(enc), status, req.headers(), req.method() == Method::HEAD).await;
                }
            }
        }
        send_file(&file, status, req.headers(), req.method() == Method::HEAD).await
    }
}

/// An HTTP upgrade (WebSocket) to an http:// upstream: the request as it
/// came (its Upgrade and Connection too), then both ways once the upstream
/// says 101.
async fn upgrade(target: &str, mut req: Request, v: &Visitor, keep_host: bool) -> Response {
    use tokio::io::AsyncWriteExt;
    let bad = |why: String| {
        tracing::warn!("{}: upgrade to {target}: {why}", v.host);
        plain(StatusCode::BAD_GATEWAY, &format!("{}: the server behind it does not answer", v.host))
    };
    let Ok(url) = reqwest::Url::parse(target) else { return bad("not a url".into()) };
    if url.scheme() != "http" {
        return bad("an upgrade goes to http:// only".into());
    }
    let (Some(host), port) = (url.host_str().map(str::to_string), url.port_or_known_default().unwrap_or(80)) else { return bad("no host".into()) };
    let pq = &target[target.find("://").map_or(0, |i| i + 3)..];
    let pq = pq.find('/').map_or("/", |i| &pq[i..]);
    let mut head = format!("{} {pq} HTTP/1.1\r\n", req.method());
    let authority = url.port().map_or_else(|| host.clone(), |p| format!("{host}:{p}"));
    let host_h = if keep_host { req.headers().get(header::HOST).and_then(|x| x.to_str().ok()).unwrap_or(&authority).to_string() } else { authority };
    head.push_str(&format!("Host: {host_h}\r\n"));
    for (k, val) in req.headers() {
        let n = k.as_str();
        if n == "host" || n.starts_with("x-forwarded-") || n == "x-real-ip" || n.starts_with("x-shelf-") || n == "forwarded" {
            continue;
        }
        if let Ok(s) = val.to_str() {
            head.push_str(&format!("{k}: {s}\r\n"));
        }
    }
    let ip = v.ip.to_canonical();
    head.push_str(&format!("X-Forwarded-For: {ip}\r\nX-Real-IP: {ip}\r\nX-Forwarded-Proto: {}\r\nX-Forwarded-Host: {}\r\n\r\n", v.proto, v.host));
    let mut up = match tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect((host.as_str(), port))).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return bad(e.to_string()),
        Err(_) => return bad("no answer in 5 s".into()),
    };
    if let Err(e) = up.write_all(head.as_bytes()).await {
        return bad(e.to_string());
    }
    // the upstream's answer, up to its blank line
    let mut buf = Vec::new();
    let end = loop {
        let mut chunk = [0u8; 4096];
        let n = match tokio::time::timeout(Duration::from_secs(30), up.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) => return bad("closed before answering".into()),
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return bad(e.to_string()),
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 64 * 1024 {
            return bad("an answer head over 64 KiB".into());
        }
    };
    let text = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = text.split("\r\n");
    let status = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|c| c.parse::<u16>().ok()).unwrap_or(502);
    let mut b = Response::builder().status(status);
    for l in lines.filter(|l| !l.is_empty()) {
        if let Some((k, val)) = l.split_once(':') {
            if let (Ok(k), Ok(val)) = (HeaderName::from_bytes(k.trim().as_bytes()), HeaderValue::from_str(val.trim())) {
                b = b.header(k, val);
            }
        }
    }
    let rest = buf[end..].to_vec();
    if status != 101 {
        // no upgrade after all: what came, and the rest of the body as it comes
        let tail = ReaderStream::new(up);
        let first = futures_util::stream::once(async move { Ok::<_, std::io::Error>(bytes::Bytes::from(rest)) });
        return b.body(Body::from_stream(futures_util::StreamExt::chain(first, tail))).unwrap_or_else(|_| bad("a bad answer".into()));
    }
    let on = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        if let Ok(client) = on.await {
            let mut client = hyper_util::rt::TokioIo::new(client);
            if !rest.is_empty() && client.write_all(&rest).await.is_err() {
                return;
            }
            let _ = tokio::io::copy_bidirectional(&mut client, &mut up).await;
        }
    });
    b.body(Body::empty()).unwrap_or_else(|_| bad("a bad answer".into()))
}

/// The url `pq` goes to at `upstream`, nginx's way.
pub fn target(upstream: &str, prefix: Option<&str>, pq: &str) -> String {
    let after_scheme = upstream.find("://").map_or(0, |i| i + 3);
    let (base, upath) = match upstream[after_scheme..].find('/') {
        Some(i) => upstream.split_at(after_scheme + i),
        None => (upstream, ""),
    };
    if upath.is_empty() {
        return format!("{base}{pq}");
    }
    match prefix {
        Some(p) => {
            let rest = pq.strip_prefix(p).or_else(|| pq.strip_prefix(p.trim_end_matches('/'))).unwrap_or(pq);
            let sep = if upath.ends_with('/') || rest.is_empty() || rest.starts_with(['/', '?']) { "" } else { "/" };
            format!("{base}{upath}{sep}{rest}")
        }
        None => format!("{base}{}{pq}", upath.trim_end_matches('/')),
    }
}

/// The path of a request, as a relative path with no `..`; None when it is not one.
fn safe_path(p: &str) -> Option<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(p).decode_utf8().ok()?;
    if decoded.contains('\0') {
        return None;
    }
    let mut out = PathBuf::new();
    for c in Path::new(decoded.trim_start_matches('/')).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// Whether the request takes this content coding (Accept-Encoding, not with q=0).
fn accepts(req: &HeaderMap, enc: &str) -> bool {
    req.get_all(header::ACCEPT_ENCODING).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(',')).any(|t| {
        let mut it = t.split(';');
        let name = it.next().unwrap_or("").trim();
        let zero = it.any(|q| q.trim().strip_prefix("q=").is_some_and(|x| x.trim().parse::<f32>().is_ok_and(|q| q == 0.0)));
        name.eq_ignore_ascii_case(enc) && !zero
    })
}

async fn send_file(file: &Path, status: StatusCode, req: &HeaderMap, head: bool) -> Response {
    send_file_as(file, file, None, status, req, head).await
}

/// `file` sent as `typed` (its type from that name); `enc`: the content coding `file` has.
async fn send_file_as(file: &Path, typed: &Path, enc: Option<&str>, status: StatusCode, req: &HeaderMap, head: bool) -> Response {
    let Ok(mut f) = tokio::fs::File::open(file).await else { return plain(StatusCode::NOT_FOUND, "not found") };
    let Ok(meta) = f.metadata().await else { return plain(StatusCode::NOT_FOUND, "not found") };
    let size = meta.len();
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let etag = match enc {
        Some(e) => format!("\"{mtime:x}-{size:x}-{e}\""),
        None => format!("\"{mtime:x}-{size:x}\""),
    };
    let ctype = mime_guess::from_path(typed).first_or_octet_stream();
    let ctype = if ctype.type_() == "text" || ctype.essence_str() == "application/javascript" { format!("{}; charset=utf-8", ctype.essence_str()) } else { ctype.essence_str().to_string() };
    let mut b = Response::builder()
        .header(header::ETAG, &etag)
        .header(header::LAST_MODIFIED, httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(mtime)))
        .header(header::CACHE_CONTROL, "public, no-cache")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, ctype)
        .header(HeaderName::from_static("x-content-type-options"), "nosniff");
    if let Some(e) = enc {
        b = b.header(header::CONTENT_ENCODING, e).header(header::VARY, "accept-encoding");
    }
    if status == StatusCode::OK && req.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*")) {
        return b.status(StatusCode::NOT_MODIFIED).body(Body::empty()).expect("a response");
    }
    // a range of a compressed copy is not one of the file: the whole of it
    let range = (status == StatusCode::OK && enc.is_none())
        .then(|| req.get(header::RANGE).and_then(|v| v.to_str().ok()))
        .flatten()
        .filter(|_| req.get(header::IF_RANGE).and_then(|v| v.to_str().ok()).is_none_or(|v| v == etag))
        .and_then(|r| crate::osada1::http_util::parse_range(r, size));
    let (start, len, status) = match range {
        Some(Ok((s, e))) => {
            b = b.header(header::CONTENT_RANGE, format!("bytes {s}-{e}/{size}"));
            (s, e - s + 1, StatusCode::PARTIAL_CONTENT)
        }
        Some(Err(())) => {
            return Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE).header(header::CONTENT_RANGE, format!("bytes */{size}")).body(Body::empty()).expect("a response");
        }
        None => (0, size, status),
    };
    b = b.status(status).header(header::CONTENT_LENGTH, len);
    if head {
        return b.body(Body::empty()).expect("a response");
    }
    if start > 0 && f.seek(SeekFrom::Start(start)).await.is_err() {
        return plain(StatusCode::INTERNAL_SERVER_ERROR, "cannot read the file");
    }
    b.body(Body::from_stream(ReaderStream::new(f.take(len)))).expect("a response")
}

/// Cloudflare's addresses (https://www.cloudflare.com/ips/, 2026-11), or the
/// list in `<data>/cloudflare-ips` (one network a line) when there is one.
pub fn cloudflare(data: &Path) -> Vec<Cidr> {
    const BUILT_IN: &str = "173.245.48.0/20 103.21.244.0/22 103.22.200.0/22 103.31.4.0/22 141.101.64.0/18 108.162.192.0/18 \
        190.93.240.0/20 188.114.96.0/20 197.234.240.0/22 198.41.128.0/17 162.158.0.0/15 104.16.0.0/13 104.24.0.0/14 172.64.0.0/13 \
        131.0.72.0/22 2400:cb00::/32 2606:4700::/32 2803:f800::/32 2405:b500::/32 2405:8100::/32 2a06:98c0::/29 2c0f:f248::/32";
    let own = std::fs::read_to_string(data.join("cloudflare-ips")).ok();
    own.as_deref().unwrap_or(BUILT_IN).split_whitespace().filter_map(Cidr::parse).collect()
}

/// The visitor behind Cloudflare: CF-Connecting-IP, when the connection is
/// Cloudflare's; None when it is not (refused: the site is Cloudflare's only).
pub fn behind_cloudflare(nets: &[Cidr], peer: IpAddr, h: &HeaderMap) -> Option<IpAddr> {
    if !nets.iter().any(|n| n.contains(peer)) {
        return None;
    }
    Some(h.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok()).unwrap_or(peer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_as_nginx() {
        // proxy_pass http://10.0.0.2:8080/; in location /api/pi/
        assert_eq!(target("http://10.0.0.2:8080/", Some("/api/pi/"), "/api/pi/temp?x=1"), "http://10.0.0.2:8080/temp?x=1");
        // proxy_pass http://127.0.0.1:8765; in location /api/scan
        assert_eq!(target("http://127.0.0.1:8765", Some("/api/scan"), "/api/scan?q=2"), "http://127.0.0.1:8765/api/scan?q=2");
        assert_eq!(target("http://127.0.0.1:81", None, "/search?q=a"), "http://127.0.0.1:81/search?q=a");
        assert_eq!(target("https://h.example/base/", None, "/x"), "https://h.example/base/x");
        assert_eq!(target("http://u:1/v", Some("/a/"), "/a/b"), "http://u:1/v/b");
    }

    #[test]
    fn paths() {
        assert_eq!(safe_path("/a/b%20c.html"), Some(PathBuf::from("a/b c.html")));
        assert_eq!(safe_path("/"), Some(PathBuf::new()));
        assert_eq!(safe_path("/a/../../etc/passwd"), None);
        assert_eq!(safe_path("/%2e%2e/x"), None);
        assert_eq!(safe_path("/a%00"), None);
    }

    #[test]
    fn cloudflare_only() {
        let nets = cloudflare(Path::new("/nonexistent"));
        let mut h = HeaderMap::new();
        h.insert("cf-connecting-ip", HeaderValue::from_static("192.0.2.9"));
        assert_eq!(behind_cloudflare(&nets, "172.64.1.1".parse().unwrap(), &h), Some("192.0.2.9".parse().unwrap()));
        assert_eq!(behind_cloudflare(&nets, "::ffff:104.16.0.1".parse().unwrap(), &h), Some("192.0.2.9".parse().unwrap()));
        assert_eq!(behind_cloudflare(&nets, "198.51.100.1".parse().unwrap(), &h), None);
    }

    #[tokio::test]
    async fn static_files() {
        let d = std::env::temp_dir().join(format!("osada-websrv-{}", std::process::id()));
        let root = d.join("web/site");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("index.html"), "<h1>home</h1>").unwrap();
        std::fs::write(root.join("sub/a.css"), "body{}").unwrap();
        std::fs::write(root.join("404.html"), "gone").unwrap();
        std::fs::write(root.join("sub/page.html"), "clean").unwrap();
        let web = Web::new(&d);
        let get = |p: &str| Request::builder().uri(p).body(Body::empty()).unwrap();
        let r = web.files("site", false, None, get("/")).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert!(r.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
        assert_eq!(web.files("site", false, None, get("/sub")).await.status(), StatusCode::MOVED_PERMANENTLY);
        let r = web.files("site", false, None, get("/sub/a.css")).await;
        let etag = r.headers()[header::ETAG].clone();
        let again = Request::builder().uri("/sub/a.css").header(header::IF_NONE_MATCH, etag).body(Body::empty()).unwrap();
        assert_eq!(web.files("site", false, None, again).await.status(), StatusCode::NOT_MODIFIED);
        let part = Request::builder().uri("/sub/a.css").header(header::RANGE, "bytes=1-3").body(Body::empty()).unwrap();
        let r = web.files("site", false, None, part).await;
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(axum::body::to_bytes(r.into_body(), 100).await.unwrap(), "ody");
        assert_eq!(web.files("site", false, None, get("/nope")).await.status(), StatusCode::NOT_FOUND);
        let r = web.files("site", false, None, get("/sub/page")).await;
        assert_eq!(r.status(), StatusCode::OK, "clean URLs");
        assert_eq!(axum::body::to_bytes(r.into_body(), 100).await.unwrap(), "clean");
        assert_eq!(web.files("site", true, None, get("/app/route")).await.status(), StatusCode::OK);
        assert_eq!(web.files("site", false, None, get("/../../etc/passwd")).await.status(), StatusCode::BAD_REQUEST);
        // a site linked to a osada folder (D-U22): served from it; a symlink in it does not lead out
        let osada = d.join("users/alice/www");
        std::fs::create_dir_all(&osada).unwrap();
        std::fs::write(osada.join("index.html"), "FROM OSADA").unwrap();
        std::os::unix::fs::symlink("/etc/hostname", osada.join("out.txt")).unwrap();
        let base = d.join("users");
        let linked = Web::with_links(&d, Arc::new(move |l: &crate::sites::Link| Some(base.join(&l.user).join(l.path.trim_start_matches('/')))));
        let site = Site { names: vec!["x.example.org".into()], exposed_on: vec![], content: Content::Static { spa: false, link: Some(crate::sites::Link { user: "alice".into(), group: None, share: None, path: "/www".into() }), not_found: None }, routes: vec![], front: crate::sites::Front::Tls, ou: None, ..Default::default() };
        let v = Visitor { ip: "127.0.0.1".parse().unwrap(), proto: "https", host: "x.example.org".into() };
        let r = linked.serve("x", &site, get("/"), &v).await;
        assert_eq!(axum::body::to_bytes(r.into_body(), 100).await.unwrap(), "FROM OSADA");
        assert_eq!(linked.serve("x", &site, get("/out.txt"), &v).await.status(), StatusCode::NOT_FOUND, "a symlink out of the folder");
        assert_eq!(web.serve("x", &site, get("/"), &v).await.status(), StatusCode::SERVICE_UNAVAILABLE, "not where osada's files are");
        // Round 18: a compressed copy next to a file, for a browser that takes it; the site's headers
        std::fs::write(root.join("big.wasm"), "PLAIN").unwrap();
        std::fs::write(root.join("big.wasm.br"), "BROTLI").unwrap();
        std::fs::write(root.join("big.wasm.gz"), "GZIP").unwrap();
        let with = |ae: &str| Request::builder().uri("/big.wasm").header(header::ACCEPT_ENCODING, ae).body(Body::empty()).unwrap();
        let r = web.files("site", false, None, with("gzip, deflate, br, zstd")).await;
        assert_eq!(r.headers()[header::CONTENT_ENCODING], "br");
        assert_eq!(r.headers()[header::CONTENT_TYPE], "application/wasm");
        assert_eq!(axum::body::to_bytes(r.into_body(), 100).await.unwrap(), "BROTLI");
        let r = web.files("site", false, None, with("gzip, deflate")).await;
        assert_eq!(r.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(axum::body::to_bytes(r.into_body(), 100).await.unwrap(), "GZIP");
        let r = web.files("site", false, None, with("br;q=0, identity")).await;
        assert!(r.headers().get(header::CONTENT_ENCODING).is_none());
        assert_eq!(axum::body::to_bytes(r.into_body(), 100).await.unwrap(), "PLAIN");
        assert!(web.files("site", false, None, get("/big.wasm")).await.headers().get(header::CONTENT_ENCODING).is_none());
        let mut own = Site { names: vec![], under_web: true, ..Default::default() };
        own.headers.insert("Cross-Origin-Embedder-Policy".into(), "require-corp".into());
        let r = web.serve("site", &own, get("/"), &v).await;
        assert_eq!(r.headers()["cross-origin-embedder-policy"], "require-corp");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
