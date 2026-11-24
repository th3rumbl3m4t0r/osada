//! The cluster's bans (D-U33), enforced on every node that answers the internet: a direct
//! connection from a banned address is dropped at accept (crate::osada1::edge's TLS listener),
//! a request through Cloudflare by its visitor (CF-Connecting-IP) answered 403 (`guard`). The
//! list is the cluster log's (Command::Bans); the node copies it here every few seconds.
use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

/// One banned address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ban {
    pub addr: String,
    /// ms since 1970
    pub until: u64,
    pub since: u64,
    /// The rule that banned it, or `admin:<name>`.
    pub rule: String,
    /// How many times it has been banned, this one included.
    pub times: u32,
}

/// Connections and requests refused because of a ban, since the start.
pub static REFUSED: AtomicU64 = AtomicU64::new(0);

fn set() -> &'static RwLock<HashMap<IpAddr, u64>> {
    static S: OnceLock<RwLock<HashMap<IpAddr, u64>>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(HashMap::new()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The node's copy of the cluster's bans.
pub fn sync(bans: &BTreeMap<String, Ban>) {
    let m: HashMap<IpAddr, u64> = bans.values().filter_map(|b| b.addr.parse::<IpAddr>().ok().map(|ip| (ip.to_canonical(), b.until))).collect();
    let mut w = set().write().unwrap_or_else(|e| e.into_inner());
    if *w != m {
        *w = m;
    }
}

/// Whether `ip` is banned now.
pub fn banned(ip: IpAddr) -> bool {
    let r = set().read().unwrap_or_else(|e| e.into_inner());
    if r.is_empty() {
        return false;
    }
    r.get(&ip.to_canonical()).is_some_and(|&until| until > now_ms())
}

/// For a direct connection: drop it when banned.
pub fn refuse(ip: IpAddr) -> bool {
    let b = banned(ip);
    if b {
        REFUSED.fetch_add(1, Ordering::Relaxed);
    }
    b
}

/// Middleware on the edges' listeners: a banned visitor (through Cloudflare too) gets 403.
pub async fn guard(req: Request, next: Next) -> Response {
    if let Some(ConnectInfo(crate::osada1::edge::PeerAddr(peer))) = req.extensions().get::<ConnectInfo<crate::osada1::edge::PeerAddr>>().copied() {
        let (ip, _) = crate::accesslog::visitor(peer.ip(), req.headers());
        if refuse(ip) {
            return (axum::http::StatusCode::FORBIDDEN, [(axum::http::header::CONTENT_TYPE, "text/plain")], "banned for a while\n").into_response();
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banned_until_it_ends() {
        let mut m = BTreeMap::new();
        let now = now_ms();
        m.insert("203.0.113.5".into(), Ban { addr: "203.0.113.5".into(), until: now + 60_000, since: now, rule: "t".into(), times: 1 });
        m.insert("203.0.113.6".into(), Ban { addr: "203.0.113.6".into(), until: now - 1, since: now - 10, rule: "t".into(), times: 1 });
        sync(&m);
        assert!(banned("203.0.113.5".parse().unwrap()));
        assert!(banned("::ffff:203.0.113.5".parse().unwrap()));
        assert!(!banned("203.0.113.6".parse().unwrap()));
        assert!(!banned("203.0.113.7".parse().unwrap()));
        sync(&BTreeMap::new());
        assert!(!banned("203.0.113.5".parse().unwrap()));
    }
}
