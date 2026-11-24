//! Egress (Round 19): where the outgoing connections of a site's container or a job leave the
//! cluster. Every node has it built in (nothing to deploy): a container or a job says
//! `egress: <node>` ("outgoing connections exit by"), and
//!
//! - the root agent on the node it runs on opens a listener at 127.0.0.1:[`PORT`] inside the
//!   container's own network namespace (only that container reaches it: no port on the host, no
//!   firewall rule), sets HTTP_PROXY and HTTPS_PROXY (and the lower-case ones; NO_PROXY for local
//!   names) to it, and hands each connection to its node on a local socket (abstract, root's
//!   only: SO_PEERCRED), saying whose it is;
//! - the node reads the proxy request (`CONNECT host:port`, or a plain http request with its
//!   absolute URI) and opens a link stream of kind [`KIND`] to the exit node;
//! - the exit node checks that a site or job of the asking node goes out by it, resolves the name
//!   and connects (not to its own loopback, link-local, multicast addresses), and pumps.
//!
//! What honours HTTP(S)_PROXY goes out there (curl, Python, Go, .NET, Java with its properties);
//! what ignores it goes out as before. Everything through it (routing, nftables or WireGuard) is
//! a later option.

use std::{
    collections::BTreeMap,
    io::Write,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::FutureExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{
    link::{self, Link},
    placement::{NodeId, Placement},
    sites::Content,
};

/// The stream kind: head `{host, port, from}`, answer `{ok}` or `{error}`, then the bytes.
pub const KIND: &str = "egress";
/// Where the proxy is inside each container that has an exit node.
pub const PORT: u16 = 17128;

/// What a node knows of placement now.
pub type PlacementFn = Arc<dyn Fn() -> Placement + Send + Sync>;

/// The node that `from` (`site:<name>`, a site's container, or `job:<name>`) of node `node` goes
/// out by, if it says one.
pub fn exit_of(p: &Placement, node: &NodeId, from: &str) -> Option<NodeId> {
    let (kind, name) = from.split_once(':')?;
    let m = p.modules.get(name)?;
    if &m.node != node {
        return None;
    }
    match (&m.site.as_ref()?.content, kind) {
        (Content::Container(c), "site") => c.egress.clone(),
        (Content::Job(j), "job") => j.egress.clone(),
        _ => None,
    }
}

/// The proxy's environment for a container or job (`direct`: names it reaches itself).
pub fn proxy_env(direct: &[String]) -> Vec<(String, String)> {
    let url = format!("http://127.0.0.1:{PORT}");
    let mut no = vec!["localhost".to_string(), "127.0.0.1".into(), "::1".into()];
    no.extend(direct.iter().cloned());
    let no = no.join(",");
    ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"]
        .iter()
        .map(|k| (k.to_string(), url.clone()))
        .chain(["NO_PROXY", "no_proxy"].iter().map(|k| (k.to_string(), no.clone())))
        .collect()
}

/// The local socket between a node and its agent: abstract, named after the node's folder.
pub fn socket_name(data: &Path) -> String {
    let p = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf());
    format!("osada-egress-{}", &hex::encode(Sha256::digest(p.to_string_lossy().as_bytes()))[..16])
}

/// Whether an exit node may connect to `ip`: not its own loopback, nothing unspecified,
/// link-local (a cloud's metadata service) or multicast.
pub fn allowed(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(a) => !(a.is_loopback() || a.is_unspecified() || a.is_link_local() || a.is_multicast() || a.is_broadcast()),
        IpAddr::V6(a) => !(a.is_loopback() || a.is_unspecified() || a.is_multicast() || (a.segments()[0] & 0xffc0) == 0xfe80),
    }
}

// ---- the proxy request ------------------------------------------------------------------------

/// What a container asked for: where to, whether a tunnel (CONNECT), and the bytes that go there
/// first (a plain request, rewritten: its path only, no proxy headers, one request).
#[derive(Debug, PartialEq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub connect: bool,
    pub first: Vec<u8>,
}

fn host_port(s: &str, default: u16) -> Result<(String, u16), String> {
    let bad = || format!("{s:?} is not host:port");
    let (h, p) = if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest.split_once(']').ok_or_else(bad)?;
        (h, after.strip_prefix(':'))
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (s, None),
        }
    };
    let port = match p {
        Some(p) => p.parse::<u16>().map_err(|_| bad())?,
        None => default,
    };
    if h.is_empty() || h.len() > 253 || port == 0 || h.chars().any(|c| c.is_whitespace() || c.is_control() || c == '/' || c == '@') {
        return Err(bad());
    }
    Ok((h.to_ascii_lowercase(), port))
}

/// The request head (up to the empty line) a client sent its proxy.
pub fn parse_request(head: &[u8]) -> Result<Target, String> {
    let text = std::str::from_utf8(head).map_err(|_| "not a request".to_string())?;
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(format!("{first:?} is not a request line"));
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(format!("{first:?} is not a request line"));
    }
    if method == "CONNECT" {
        let (host, port) = host_port(target, 443)?;
        return Ok(Target { host, port, connect: true, first: Vec::new() });
    }
    let rest = target.strip_prefix("http://").ok_or_else(|| format!("{target:?}: a proxy takes http://… (https: CONNECT)"))?;
    let (auth, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = if path.starts_with('?') { format!("/{path}") } else { path.to_string() };
    let (host, port) = host_port(auth, 80)?;
    let mut out = format!("{method} {path} {version}\r\n");
    let mut has_host = false;
    for l in lines.filter(|l| !l.is_empty()) {
        let name = l.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
        if ["proxy-connection", "proxy-authorization", "connection", "keep-alive"].contains(&name.as_str()) {
            continue;
        }
        has_host |= name == "host";
        out.push_str(l);
        out.push_str("\r\n");
    }
    if !has_host {
        out.push_str(&format!("Host: {auth}\r\n"));
    }
    // one request a connection: the next one may be for another host
    out.push_str("Connection: close\r\n\r\n");
    Ok(Target { host, port, connect: false, first: out.into_bytes() })
}

// ---- what each container's connections did (the web tab) ---------------------------------------

#[derive(Default, Clone, serde::Serialize)]
struct Stat {
    connections: u64,
    failed: u64,
    bytes_out: u64,
    bytes_in: u64,
    last_at: u64,
    last: String,
    last_error: Option<String>,
    exit: String,
}

fn stats() -> &'static Mutex<BTreeMap<String, Stat>> {
    static S: std::sync::OnceLock<Mutex<BTreeMap<String, Stat>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

fn note(from: &str, exit: &NodeId, to: &str, r: Result<(u64, u64), String>) {
    let mut s = stats().lock().unwrap_or_else(|e| e.into_inner());
    let e = s.entry(from.to_string()).or_default();
    e.connections += 1;
    e.last_at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    e.last = to.to_string();
    e.exit = exit.0.clone();
    match r {
        Ok((o, i)) => {
            e.bytes_out += o;
            e.bytes_in += i;
        }
        Err(why) => {
            e.failed += 1;
            e.last_error = Some(format!("{to}: {why}"));
        }
    }
}

/// By `site:<name>` / `job:<name>`: connections, bytes, the last one and the last failure.
pub fn report() -> Value {
    json!(*stats().lock().unwrap_or_else(|e| e.into_inner()))
}

// ---- the node: the agent's connections, each to its exit --------------------------------------

async fn refuse(s: &mut tokio::net::UnixStream, code: &str, why: &str) {
    let body = format!("osada egress: {why}\n");
    let _ = s.write_all(format!("HTTP/1.1 {code}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
    let _ = s.shutdown().await;
}

/// Up to the end of the request head (`\r\n\r\n`, at most 64 KiB): the head and what came after.
async fn read_head<R: tokio::io::AsyncRead + Unpin>(s: &mut R, first_line: bool) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        let end = if first_line { buf.iter().position(|b| *b == b'\n').map(|i| i + 1) } else { buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4) };
        if let Some(i) = end {
            let rest = buf.split_off(i);
            return Ok((buf, rest));
        }
        if buf.len() > 64 << 10 {
            return Err("a request head over 64 KiB".into());
        }
        let n = tokio::time::timeout(Duration::from_secs(30), s.read(&mut chunk)).await.map_err(|_| "no request in 30 s".to_string())?.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("closed before its request".into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// A connection the agent handed over: its preamble (`{"from"}`), the request, the exit.
async fn proxy(mut s: tokio::net::UnixStream, link: Arc<Link>, placement: PlacementFn) {
    let (pre, mut rest) = match read_head(&mut s, true).await {
        Ok(x) => x,
        Err(_) => return,
    };
    let from = serde_json::from_slice::<Value>(&pre).ok().and_then(|v| v["from"].as_str().map(str::to_string)).unwrap_or_default();
    let me = link.me().clone();
    let Some(exit) = exit_of(&placement(), &me, &from) else {
        return refuse(&mut s, "403 Forbidden", &format!("{from} has no exit node")).await;
    };
    // the request head: the rest of what came with the preamble, then more
    let (head, after) = match rest.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => {
            let after = rest.split_off(i + 4);
            (rest, after)
        }
        None => {
            let mut chained = std::io::Cursor::new(rest).chain(&mut s);
            match read_head(&mut chained, false).await {
                Ok(x) => x,
                Err(why) => return refuse(&mut s, "400 Bad Request", &why).await,
            }
        }
    };
    let t = match parse_request(&head) {
        Ok(t) => t,
        Err(why) => return refuse(&mut s, "400 Bad Request", &why).await,
    };
    let to = format!("{}:{}", t.host, t.port);
    let mut first = t.first.clone();
    first.extend_from_slice(&after);
    let r = if exit == me {
        match connect_out(&t.host, t.port).await {
            Ok(mut up) => pump(&mut s, &mut up, t.connect, &first).await,
            Err(why) => Err(why),
        }
    } else {
        match open_exit(&link, &exit, &t, &from).await {
            Ok(mut up) => pump(&mut s, &mut up, t.connect, &first).await,
            Err(why) => Err(why),
        }
    };
    if let Err(why) = &r {
        refuse(&mut s, "502 Bad Gateway", &format!("{to} by {}: {why}", exit)).await;
    }
    note(&from, &exit, &to, r);
}

/// The stream to the exit node, once it has connected.
async fn open_exit(link: &Link, exit: &NodeId, t: &Target, from: &str) -> Result<tokio::io::Join<quinn::RecvStream, quinn::SendStream>, String> {
    let (send, mut recv) = link.open_stream(exit, KIND, json!({ "host": t.host, "port": t.port, "from": from })).await.map_err(|e| e.0)?;
    let h = tokio::time::timeout(Duration::from_secs(25), link::read_head(&mut recv)).await.map_err(|_| "the exit node did not answer".to_string())?.map_err(|e| e.0)?;
    if let Some(e) = h["error"].as_str() {
        return Err(e.to_string());
    }
    Ok(tokio::io::join(recv, send))
}

/// Connected: a tunnel says so, then the first bytes go, then both ways until both end.
async fn pump<U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(s: &mut tokio::net::UnixStream, up: &mut U, connect: bool, first: &[u8]) -> Result<(u64, u64), String> {
    if connect {
        s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.map_err(|e| e.to_string())?;
    }
    if !first.is_empty() {
        up.write_all(first).await.map_err(|e| e.to_string())?;
    }
    let (o, i) = tokio::io::copy_bidirectional(s, up).await.unwrap_or((0, 0));
    Ok((o + first.len() as u64, i))
}

/// The node's side: the agent's local socket (root only), each connection to its exit.
pub fn spawn_local(data: &Path, link: Arc<Link>, placement: PlacementFn) {
    use std::os::linux::net::SocketAddrExt;
    let name = socket_name(data);
    let l = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())
        .and_then(|a| std::os::unix::net::UnixListener::bind_addr(&a))
        .and_then(|l| l.set_nonblocking(true).map(|()| l))
        .and_then(tokio::net::UnixListener::from_std);
    let l = match l {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{}: egress: the agent's socket @{name}: {e}", link.me());
            return;
        }
    };
    tokio::spawn(async move {
        loop {
            let s = match l.accept().await {
                Ok((s, _)) => s,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            };
            // the agent's (root) only
            if !s.peer_cred().is_ok_and(|c| c.uid() == 0) {
                continue;
            }
            tokio::spawn(proxy(s, link.clone(), placement.clone()));
        }
    });
}

// ---- the exit node ------------------------------------------------------------------------------

/// A connection out from here to `host:port` (resolved here; [`allowed`] addresses only).
pub async fn connect_out(host: &str, port: u16) -> Result<tokio::net::TcpStream, String> {
    if host.is_empty() || host.len() > 253 || port == 0 {
        return Err("no such address".into());
    }
    let addrs: Vec<SocketAddr> = tokio::time::timeout(Duration::from_secs(10), tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| "its name did not resolve in 10 s".to_string())?
        .map_err(|e| format!("its name: {e}"))?
        .collect();
    let ok: Vec<SocketAddr> = addrs.iter().copied().filter(|a| allowed(a.ip())).collect();
    if ok.is_empty() {
        return Err(if addrs.is_empty() { "its name has no address".into() } else { "not an address to go to from here (loopback, link-local)".into() });
    }
    let mut last = String::new();
    for a in ok {
        match tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(a)).await {
            Ok(Ok(s)) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Ok(Err(e)) => last = format!("{a}: {e}"),
            Err(_) => last = format!("{a}: no answer in 10 s"),
        }
    }
    Err(last)
}

/// The `egress` stream kind: a node's container or job going out here (when it says so).
pub fn server(placement: PlacementFn, me: NodeId) -> link::StreamHandler {
    Arc::new(move |peer: NodeId, _kind: String, head: Value, mut send: quinn::SendStream, recv: quinn::RecvStream| {
        let (placement, me) = (placement.clone(), me.clone());
        async move {
            let from = head["from"].as_str().unwrap_or_default().to_string();
            let host = head["host"].as_str().unwrap_or_default().to_string();
            let port = head["port"].as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(0);
            if exit_of(&placement(), &peer, &from).as_ref() != Some(&me) {
                let _ = link::write_head(&mut send, json!({ "error": format!("{from} of {peer} does not go out by {me}") })).await;
                let _ = send.finish();
                return;
            }
            match connect_out(&host, port).await {
                Err(why) => {
                    let _ = link::write_head(&mut send, json!({ "error": why })).await;
                    let _ = send.finish();
                }
                Ok(mut tcp) => {
                    if link::write_head(&mut send, json!({ "ok": true })).await.is_err() {
                        return;
                    }
                    let mut s = tokio::io::join(recv, send);
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut s).await;
                }
            }
        }
        .boxed()
    })
}

// ---- the agent (root): a listener in each container's network ----------------------------------

/// A listener at 127.0.0.1:PORT in the network namespace of process `pid` (a thread of its own
/// enters it; the agent's other threads stay where they are).
pub fn listen_in(pid: i64) -> std::io::Result<std::net::TcpListener> {
    use std::os::fd::AsRawFd;
    let ns = std::fs::File::open(format!("/proc/{pid}/ns/net"))?;
    std::thread::spawn(move || -> std::io::Result<std::net::TcpListener> {
        // SAFETY: setns on this thread only, with an open namespace file; the thread ends after
        if unsafe { libc::setns(ns.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let l = std::net::TcpListener::bind(("127.0.0.1", PORT))?;
        l.set_nonblocking(true)?;
        Ok(l)
    })
    .join()
    .map_err(|_| std::io::Error::other("its thread panicked"))?
}

/// The agent's listeners, by `site:<name>` / `job:<name>`: the network namespace each is in.
#[derive(Default)]
pub struct Agent {
    open: BTreeMap<String, (u64, Arc<AtomicBool>)>,
}

impl Agent {
    /// `from` (running as process `pid`) has its listener; true when made now.
    pub fn ensure(&mut self, data: &Path, from: &str, pid: i64) -> Result<bool, String> {
        use std::os::unix::fs::MetadataExt;
        if pid <= 0 {
            return Err("not running".into());
        }
        let ino = std::fs::metadata(format!("/proc/{pid}/ns/net")).map_err(|e| format!("its network: {e}"))?.ino();
        if self.open.get(from).is_some_and(|(i, _)| *i == ino) {
            return Ok(false);
        }
        if let Some((_, stop)) = self.open.remove(from) {
            stop.store(true, Ordering::Relaxed);
        }
        let l = listen_in(pid).map_err(|e| format!("the proxy in its network: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let (name, f, s) = (socket_name(data), from.to_string(), stop.clone());
        std::thread::spawn(move || accept_loop(l, &name, &f, &s));
        self.open.insert(from.to_string(), (ino, stop));
        Ok(true)
    }

    /// The listeners of anything not in `keep` closed (gone, stopped, no exit node any more).
    pub fn keep_only(&mut self, prefix: &str, keep: &[String]) {
        self.open.retain(|k, (_, stop)| {
            let kept = !k.starts_with(prefix) || keep.contains(k);
            if !kept {
                stop.store(true, Ordering::Relaxed);
            }
            kept
        });
    }
}

fn accept_loop(l: std::net::TcpListener, name: &str, from: &str, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match l.accept() {
            Ok((c, _)) => {
                let (n, f) = (name.to_string(), from.to_string());
                std::thread::spawn(move || relay(c, &n, &f));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// One connection from a container to the node, both ways.
fn relay(c: std::net::TcpStream, name: &str, from: &str) {
    use std::os::linux::net::SocketAddrExt;
    let _ = c.set_nonblocking(false);
    let u = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).and_then(|a| std::os::unix::net::UnixStream::connect_addr(&a));
    let mut u = match u {
        Ok(u) => u,
        Err(_) => {
            let _ = (&c).write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-type: text/plain\r\ncontent-length: 31\r\nconnection: close\r\n\r\nosada egress: the node is away\n");
            return;
        }
    };
    if u.write_all(format!("{}\n", json!({ "from": from })).as_bytes()).is_err() {
        return;
    }
    let (Ok(mut c2), Ok(mut u2)) = (c.try_clone(), u.try_clone()) else { return };
    let t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut c2, &mut u2);
        let _ = u2.shutdown(std::net::Shutdown::Write);
    });
    let mut c = c;
    let _ = std::io::copy(&mut u, &mut c);
    let _ = c.shutdown(std::net::Shutdown::Write);
    let _ = t.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests() {
        let t = parse_request(b"CONNECT example.org:443 HTTP/1.1\r\nHost: example.org:443\r\n\r\n").unwrap();
        assert_eq!((t.host.as_str(), t.port, t.connect, t.first.len()), ("example.org", 443, true, 0));
        let t = parse_request(b"CONNECT [2001:db8::1]:8443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("2001:db8::1", 8443));
        let t = parse_request(b"GET http://Example.org:8080/a/b?c=1 HTTP/1.1\r\nHost: example.org:8080\r\nProxy-Connection: keep-alive\r\nProxy-Authorization: Basic eA==\r\nUser-Agent: x\r\nConnection: keep-alive\r\n\r\n").unwrap();
        assert_eq!((t.host.as_str(), t.port, t.connect), ("example.org", 8080, false));
        let s = String::from_utf8(t.first).unwrap();
        assert_eq!(s, "GET /a/b?c=1 HTTP/1.1\r\nHost: example.org:8080\r\nUser-Agent: x\r\nConnection: close\r\n\r\n");
        let t = parse_request(b"GET http://example.org HTTP/1.0\r\n\r\n").unwrap();
        assert_eq!(String::from_utf8(t.first).unwrap(), "GET / HTTP/1.0\r\nHost: example.org\r\nConnection: close\r\n\r\n");
        assert_eq!(parse_request(b"GET http://example.org?x HTTP/1.1\r\nHost: example.org\r\n\r\n").unwrap().first.starts_with(b"GET /?x HTTP/1.1"), true);
        assert!(parse_request(b"GET /relative HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_request(b"GET https://example.org/ HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_request(b"CONNECT example.org:0 HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_request(b"CONNECT user@example.org:443 HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_request(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn where_it_may_go() {
        for a in ["127.0.0.1", "::1", "0.0.0.0", "169.254.169.254", "fe80::1", "224.0.0.1", "::ffff:127.0.0.1", "255.255.255.255"] {
            assert!(!allowed(a.parse().unwrap()), "{a}");
        }
        for a in ["1.1.1.1", "192.168.1.12", "10.0.0.3", "2001:2::1"] {
            assert!(allowed(a.parse().unwrap()), "{a}");
        }
    }

    #[test]
    fn its_environment() {
        let e: BTreeMap<String, String> = proxy_env(&["ds.example.org".into()]).into_iter().collect();
        assert_eq!(e["HTTPS_PROXY"], format!("http://127.0.0.1:{PORT}"));
        assert_eq!(e["http_proxy"], e["HTTP_PROXY"]);
        assert_eq!(e["no_proxy"], "localhost,127.0.0.1,::1,ds.example.org");
    }
}
