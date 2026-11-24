//! One `http` line per request a node answers (D-U31): the edges' https and plain http
//! listeners and home's own (LAN, https). A request an edge relays home is the edge's line only;
//! TLS passed through to a mail server is one line per connection (passthrough). Secrets in query
//! strings (token, code, …) are not written.
use crate::logstore;
use crate::osada1::edge::PeerAddr;
use crate::osada1::edge::Cidr;
use axum::extract::{ConnectInfo, Request};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use serde_json::{Value, json};
use std::net::IpAddr;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

static CF: OnceLock<Vec<Cidr>> = OnceLock::new();

/// Cloudflare's addresses as this node knows them (websrv::cloudflare).
pub fn init(data: &Path) {
    let _ = CF.set(crate::websrv::cloudflare(data));
}

fn cf() -> &'static [Cidr] {
    CF.get_or_init(|| crate::websrv::cloudflare(Path::new("/nonexistent")))
}

/// Whether `ip` is Cloudflare's.
pub fn is_cloudflare(ip: IpAddr) -> bool {
    cf().iter().any(|n| n.contains(ip))
}

fn cut(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// A query string with the values of secret-looking keys left out.
pub fn clean_query(q: &str) -> String {
    const SECRET: [&str; 14] = ["token", "code", "secret", "password", "pass", "key", "sig", "signature", "state", "ticket", "auth", "session", "jwt", "otp"];
    q.split('&')
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let lk = k.to_ascii_lowercase();
            if !v.is_empty() && SECRET.iter().any(|s| lk == *s || lk.ends_with(&format!("_{s}")) || lk.starts_with(&format!("{s}_"))) {
                format!("{k}=…")
            } else {
                kv.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// The visitor: Cloudflare's CF-Connecting-IP when the connection comes from Cloudflare.
pub fn visitor(peer: IpAddr, h: &axum::http::HeaderMap) -> (IpAddr, bool) {
    let peer = peer.to_canonical();
    if is_cloudflare(peer) {
        let ip = h.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<IpAddr>().ok()).unwrap_or(peer);
        return (ip.to_canonical(), true);
    }
    (peer, false)
}

async fn log(front: &'static str, req: Request, next: Next) -> Response {
    if !logstore::on("http", 1) {
        return next.run(req).await;
    }
    let start = Instant::now();
    let peer = req.extensions().get::<ConnectInfo<PeerAddr>>().map(|c| (c.0).0.ip());
    let h = req.headers();
    let (client, front) = match peer {
        Some(p) => {
            let (ip, cfl) = visitor(p, h);
            (Value::from(ip.to_string()), if cfl { "cloudflare" } else { front })
        }
        None => (Value::Null, front),
    };
    let s = |n: header::HeaderName, max: usize| h.get(n).and_then(|v| v.to_str().ok()).map(|v| cut(v, max));
    let host = s(header::HOST, 255).or_else(|| req.uri().host().map(str::to_string)).map(|x| {
        let x = x.trim_end_matches('.').to_ascii_lowercase();
        // without the port (an IPv6 address keeps its brackets)
        match x.rfind(':') {
            Some(i) if !x.ends_with(']') && x[i + 1..].chars().all(|c| c.is_ascii_digit()) => x[..i].to_string(),
            _ => x,
        }
    });
    let referer = s(header::REFERER, 300).map(|r| r.split('?').next().unwrap_or("").to_string());
    let mut ev = json!({
        "front": front,
        "host": host,
        "client": client,
        "method": req.method().as_str(),
        "path": cut(req.uri().path(), 1000),
        "query": req.uri().query().map(|q| cut(&clean_query(q), 500)),
        "ua": s(header::USER_AGENT, 300),
        "referer": referer,
    });
    let resp = next.run(req).await;
    ev["status"] = Value::from(resp.status().as_u16());
    ev["bytes"] = resp.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).map(Value::from).unwrap_or(Value::Null);
    ev["ms"] = Value::from((start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0);
    logstore::record("http", 1, ev);
    resp
}

/// For a node's https listener (an edge's, home's own).
pub async fn tls(req: Request, next: Next) -> Response {
    log("tls", req, next).await
}

/// For an edge's plain http listener.
pub async fn plain(req: Request, next: Next) -> Response {
    log("plain", req, next).await
}

/// For home's LAN listener.
pub async fn lan(req: Request, next: Next) -> Response {
    log("lan", req, next).await
}

/// A TLS connection passed on to a mail server untouched: one line when it ends.
pub fn passed(host: &str, client: IpAddr, bytes_in: u64, bytes_out: u64, start: Instant) {
    logstore::record(
        "http",
        1,
        json!({
            "front": "passthrough", "host": host, "client": client.to_canonical().to_string(),
            "bytes": bytes_out, "bytes_in": bytes_in, "ms": (start.elapsed().as_secs_f64() * 1000.0).round(),
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_left_out_of_queries() {
        assert_eq!(clean_query("a=1&code=abc&state=x&next=/files"), "a=1&code=…&state=…&next=/files");
        assert_eq!(clean_query("access_token=t&id_token=u&tokens=v"), "access_token=…&id_token=…&tokens=v");
        assert_eq!(clean_query("q=wp-login.php"), "q=wp-login.php");
        assert_eq!(clean_query("key"), "key");
    }

    #[test]
    fn cloudflare_visitors() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("cf-connecting-ip", "203.0.113.9".parse().unwrap());
        assert_eq!(visitor("104.16.0.1".parse().unwrap(), &h), ("203.0.113.9".parse().unwrap(), true));
        // not from Cloudflare: the header is not believed
        assert_eq!(visitor("198.51.100.1".parse().unwrap(), &h), ("198.51.100.1".parse().unwrap(), false));
        assert_eq!(cut("abcdef", 3), "abc…");
    }
}
