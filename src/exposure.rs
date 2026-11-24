//! What a node exposes to the internet (D-U35): the sockets listening on addresses that are not
//! loopback (TCP and UDP, from /proc/net; the process when this node's account may see it, osada's
//! own marked), and what another node found it could reach (node/expose.rs: every hour a peer
//! connects to the TCP ports at this node's public addresses and at the address it sees this
//! node's link come from: a home node behind NAT is checked at its router's address, so a port
//! forward shows; what a firewall drops is not reachable).

use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Mutex,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Socket {
    pub proto: String,
    pub addr: String,
    pub port: u16,
    #[serde(skip)]
    pub inode: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// This node's own process (osada).
    pub osada: bool,
    /// Another program osada runs on this node (its mail instance's Stalwart), when not its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// The ports Stalwart listens on beyond loopback (SMTP, submission, IMAP, POP3, ManageSieve).
pub const MAIL_PORTS: [u16; 8] = [25, 465, 587, 143, 993, 110, 995, 4190];

/// One line of /proc/net/{tcp,udp}{,6}: the local address, port, state and inode.
fn parse_line(line: &str) -> Option<(IpAddr, u16, u8, u64)> {
    let f: Vec<&str> = line.split_whitespace().collect();
    let (addr, port) = f.get(1)?.split_once(':')?;
    let state = u8::from_str_radix(f.get(3)?, 16).ok()?;
    let inode = f.get(9)?.parse().ok()?;
    let port = u16::from_str_radix(port, 16).ok()?;
    // each 32-bit word as the kernel holds it (network order bytes, printed as a native number)
    let words: Vec<[u8; 4]> = (0..addr.len() / 8).map(|i| u32::from_str_radix(&addr[i * 8..i * 8 + 8], 16).map(u32::to_le_bytes)).collect::<Result<_, _>>().ok()?;
    let ip = match words.len() {
        1 => IpAddr::V4(Ipv4Addr::from(words[0])),
        4 => {
            let mut b = [0u8; 16];
            for (i, w) in words.iter().enumerate() {
                b[i * 4..i * 4 + 4].copy_from_slice(w);
            }
            IpAddr::V6(Ipv6Addr::from(b)).to_canonical()
        }
        _ => return None,
    };
    Some((ip, port, state, inode))
}

/// The sockets of `text` (one /proc/net file) that listen (TCP: LISTEN; UDP: bound, unconnected)
/// on an address that is not loopback.
fn listening_in(text: &str, proto: &str) -> Vec<Socket> {
    let want = if proto == "tcp" { 0x0A } else { 0x07 };
    text.lines()
        .skip(1)
        .filter_map(parse_line)
        .filter(|(ip, port, state, _)| *state == want && *port != 0 && !ip.is_loopback())
        .map(|(ip, port, _, inode)| Socket { proto: proto.into(), addr: ip.to_string(), port, inode, process: None, osada: false, by: None })
        .collect()
}

/// This node's listening sockets, with the process where it can be seen (its own always).
pub fn listening() -> Vec<Socket> {
    let mut v = Vec::new();
    for (file, proto) in [("tcp", "tcp"), ("tcp6", "tcp"), ("udp", "udp"), ("udp6", "udp")] {
        if let Ok(t) = std::fs::read_to_string(format!("/proc/net/{file}")) {
            v.extend(listening_in(&t, proto));
        }
    }
    let owners = owners();
    let me = std::process::id();
    for s in &mut v {
        if let Some((pid, comm)) = owners.get(&s.inode) {
            s.osada = *pid == me;
            s.process = Some(comm.clone());
        }
    }
    v.sort_by(|a, b| (&a.proto, a.port, &a.addr).cmp(&(&b.proto, b.port, &b.addr)));
    v.dedup_by(|a, b| a.proto == b.proto && a.port == b.port && a.addr == b.addr);
    v
}

/// Socket inodes to (pid, process name), for the processes this account may look into.
fn owners() -> BTreeMap<u64, (u32, String)> {
    let mut m = BTreeMap::new();
    let Ok(dir) = std::fs::read_dir("/proc") else { return m };
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let Ok(fds) = std::fs::read_dir(e.path().join("fd")) else { continue };
        let comm = std::fs::read_to_string(e.path().join("comm")).map(|c| c.trim().to_string()).unwrap_or_default();
        for fd in fds.flatten() {
            if let Ok(t) = std::fs::read_link(fd.path()) {
                if let Some(i) = t.to_str().and_then(|s| s.strip_prefix("socket:[")).and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse().ok()) {
                    m.insert(i, (pid, comm.clone()));
                }
            }
        }
    }
    m
}

/// Whether the internet may reach `ip` directly (not private, loopback, link-local, CGNAT, ULA,
/// documentation).
pub fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_private() || a.is_loopback() || a.is_link_local() || a.is_unspecified() || a.is_broadcast() || a.is_documentation()
                || (o[0] == 100 && (64..128).contains(&o[1])))
        }
        IpAddr::V6(a) => {
            let s = a.segments();
            !(a.is_loopback() || a.is_unspecified() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || s[0] == 0x2001 && s[1] == 0x0db8)
        }
    }
}

/// The public addresses on this node's interfaces.
pub fn public_ifaddrs() -> Vec<IpAddr> {
    let mut out = Vec::new();
    // SAFETY: getifaddrs fills a list we only read and then free.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return out;
        }
        let mut p = list;
        while !p.is_null() {
            let a = (*p).ifa_addr;
            if !a.is_null() {
                let ip = match (*a).sa_family as i32 {
                    libc::AF_INET => Some(IpAddr::V4(Ipv4Addr::from(u32::from_be((*(a as *const libc::sockaddr_in)).sin_addr.s_addr)))),
                    libc::AF_INET6 => Some(IpAddr::V6(Ipv6Addr::from((*(a as *const libc::sockaddr_in6)).sin6_addr.s6_addr))),
                    _ => None,
                };
                if let Some(ip) = ip.filter(is_public) {
                    if !out.contains(&ip) {
                        out.push(ip);
                    }
                }
            }
            p = (*p).ifa_next;
        }
        libc::freeifaddrs(list);
    }
    out
}

/// What a peer found: when, who checked, from where it saw this node, and each address and port.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Probe {
    pub at: u64,
    pub by: String,
    pub results: Vec<Reach>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reach {
    pub addr: String,
    pub port: u16,
    pub open: bool,
}

static LAST: Mutex<Option<Probe>> = Mutex::new(None);

pub fn set_last(p: Probe) {
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
}

/// As the prober: a TCP connection to each address and port, 3 s each, all at once.
pub async fn probe(addrs: &[IpAddr], ports: &[u16]) -> Vec<Reach> {
    let mut tasks = Vec::new();
    for a in addrs {
        for p in ports {
            let sa = SocketAddr::new(*a, *p);
            tasks.push(tokio::spawn(async move {
                let open = matches!(tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(sa)).await, Ok(Ok(_)));
                Reach { addr: a_str(&sa.ip()), port: sa.port(), open }
            }));
        }
    }
    let mut out = Vec::new();
    for t in tasks {
        if let Ok(r) = t.await {
            out.push(r);
        }
    }
    out
}

fn a_str(ip: &IpAddr) -> String {
    ip.to_canonical().to_string()
}

/// For the admin pages (nodes, the node card): listening, public addresses, the last check.
pub fn report(listening: &[Socket], public: &[IpAddr]) -> Value {
    let last = LAST.lock().unwrap_or_else(|e| e.into_inner()).clone();
    json!({ "listening": listening, "public": public.iter().map(a_str).collect::<Vec<_>>(), "probe": last })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000   990        0 1111 1 0000000000000000 100 0 0 10 0
   1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2222 1 0000000000000000 100 0 0 10 0
   2: 6001A8C0:01BB 6301A8C0:C350 01 00000000:00000000 00:00000000 00000000   990        0 3333 1 0000000000000000 20 4 30 10 -1
   3: 780012C6:0019 00000000:0000 0A 00000000:00000000 00:00000000 00000000   990        0 4444 1 0000000000000000 100 0 0 10 0
";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:01BB 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 5555 1 0000000000000000 100 0 0 10 0
   1: 00000000000000000000000001000000:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 6666 1 0000000000000000 100 0 0 10 0
   2: 0000000000000000FFFF0000780012C6:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 7777 1 0000000000000000 100 0 0 10 0
";
    const UDP: &str = "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
  100: 00000000:1D11 00000000:0000 07 00000000:00000000 00:00000000 00000000   990        0 8888 2 0000000000000000 0
  101: 0100007F:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000   990        0 9999 2 0000000000000000 0
";

    #[test]
    fn listening_sockets_from_proc() {
        let t = listening_in(TCP, "tcp");
        assert_eq!(t.iter().map(|s| (s.addr.as_str(), s.port, s.inode)).collect::<Vec<_>>(), [("0.0.0.0", 22, 2222), ("198.18.0.120", 25, 4444)]);
        let t6 = listening_in(TCP6, "tcp");
        assert_eq!(t6.iter().map(|s| (s.addr.as_str(), s.port)).collect::<Vec<_>>(), [("::", 443), ("198.18.0.120", 80)]);
        let u = listening_in(UDP, "udp");
        assert_eq!(u.iter().map(|s| (s.addr.as_str(), s.port)).collect::<Vec<_>>(), [("0.0.0.0", 7441)]);
    }

    #[test]
    fn public_addresses() {
        for a in ["198.18.0.120", "198.18.0.42", "2001:2:0:992::1"] {
            assert!(is_public(&a.parse().unwrap()), "{a}");
        }
        for a in ["192.168.1.10", "10.0.0.3", "127.0.0.1", "100.64.1.1", "fd00::1", "fe80::1", "::1", "169.254.1.1"] {
            assert!(!is_public(&a.parse().unwrap()), "{a}");
        }
    }

    #[tokio::test]
    async fn a_probe_tells_open_from_closed() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = l.local_addr().unwrap().port();
        let closed = {
            let x = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            x.local_addr().unwrap().port()
        };
        let r = probe(&["127.0.0.1".parse().unwrap()], &[open, closed]).await;
        assert_eq!(r, [Reach { addr: "127.0.0.1".into(), port: open, open: true }, Reach { addr: "127.0.0.1".into(), port: closed, open: false }]);
    }
}
