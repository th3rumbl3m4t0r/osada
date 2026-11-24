//! Nodes this node dials, kept in its own store (`n/dial/<host:port>`, not replicated): the
//! addresses `osada token --dial` (install.sh token --at) and `osada dial --add` put there, so a
//! node that cannot be reached (home behind NAT) keeps the link to one that can (an edge in the
//! cloud), for the join and after it. What OSADA_PEERS and --peer do, without editing a file or
//! restarting: the running node looks at the list every few seconds and keeps dialing every
//! address on it; an address taken off the list (`osada dial --remove`) is let go.
//!
//! While no node of the cluster has answered at an address yet, a node holding the CA also
//! invites whoever is there ([`crate::link::Link::invite`]): a node waiting to join
//! (`osada join --listen`) gets its certificate that way, home never needing to be reachable.
//!
//! The address a node was reached at goes into node control (its `link`; for an edge without
//! public addresses, the host too when it is a public IP), and every node keeps a link to every
//! node with a `link` (the mesh): edges reach each other (an issuer's ACME challenges and the
//! copies of its certificates, sites that run on another node) without going through home.

use std::{
    collections::BTreeSet,
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::Node;
use crate::{
    cluster::Command,
    kv::{self, Batch, Kv},
    placement::NodeId,
};

const PREFIX: &[u8] = b"n/dial/";
/// How long one attempt to reach an address may take.
const ATTEMPT: Duration = Duration::from_secs(10);
/// The longest pause between attempts.
const MAX_WAIT: Duration = Duration::from_secs(15);
/// How often a link that is up is looked at.
const UP: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Dial {
    /// The name it was added under (the token's node), for people.
    #[serde(default)]
    pub name: String,
    /// The node that answered there, once one has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// When it was added, milliseconds since 1970.
    #[serde(default)]
    pub since: u64,
}

fn key(addr: &str) -> Vec<u8> {
    let mut k = PREFIX.to_vec();
    k.extend_from_slice(addr.as_bytes());
    k
}

/// `host:port` with a port: an address the list takes.
pub fn valid_addr(addr: &str) -> bool {
    let Some((host, port)) = addr.rsplit_once(':') else { return false };
    !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p > 0) && !addr.chars().any(char::is_whitespace)
}

/// Puts `addr` on the list (again: under the new name, the node that answered forgotten).
pub fn add(kv: &dyn Kv, addr: &str, name: &str, now: u64) -> Result<(), String> {
    if !valid_addr(addr) {
        return Err(format!("{addr:?}: not host:port"));
    }
    let d = Dial { name: name.into(), node: None, since: now };
    let mut b = Batch::new();
    b.put(key(addr), serde_json::to_vec(&d).map_err(|e| e.to_string())?);
    kv.write(b).map_err(|e| e.to_string())
}

/// Takes `addr` off the list; whether it was on it.
pub fn remove(kv: &dyn Kv, addr: &str) -> Result<bool, String> {
    let there = kv.get(&key(addr)).map_err(|e| e.to_string())?.is_some();
    let mut b = Batch::new();
    b.delete(key(addr));
    kv.write(b).map_err(|e| e.to_string())?;
    Ok(there)
}

/// The list: address and what is known of it.
pub fn list(kv: &dyn Kv) -> Vec<(String, Dial)> {
    kv::scan_prefix(kv, PREFIX, 10_000)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| Some((String::from_utf8(k[PREFIX.len()..].to_vec()).ok()?, serde_json::from_slice(&v).unwrap_or_default())))
        .collect()
}

fn get(kv: &dyn Kv, addr: &str) -> Option<Dial> {
    kv.get(&key(addr)).ok().flatten().map(|v| serde_json::from_slice(&v).unwrap_or_default())
}

fn answered(kv: &dyn Kv, addr: &str, node: &NodeId) {
    if let Some(mut d) = get(kv, addr) {
        d.node = Some(node.0.clone());
        let mut b = Batch::new();
        if let Ok(v) = serde_json::to_vec(&d) {
            b.put(key(addr), v);
            let _ = kv.write(b);
        }
    }
}

fn resolve(addr: &str) -> Result<SocketAddr, String> {
    addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or_else(|| "no address".into())
}

/// The host of `host:port` when it is an address the internet reaches (not loopback, private,
/// link-local, CGNAT or unique-local).
fn public_ip(addr: &str) -> Option<std::net::IpAddr> {
    use std::net::IpAddr;
    let host = addr.rsplit_once(':')?.0.trim_start_matches('[').trim_end_matches(']');
    let ip = host.parse::<IpAddr>().ok()?.to_canonical();
    let public = match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_loopback() || v.is_private() || v.is_link_local() || v.is_unspecified() || v.is_broadcast() || (o[0] == 100 && (o[1] & 0xc0) == 64))
        }
        IpAddr::V6(v) => !(v.is_loopback() || v.is_unspecified() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80),
    };
    public.then_some(ip)
}

impl Node {
    /// Keeps dialing the addresses on the list, as long as the node runs.
    pub fn spawn_dials(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut started = BTreeSet::new();
            loop {
                let Some(n) = weak.upgrade() else { break };
                for (addr, d) in list(&*n.kv) {
                    if !started.contains(&addr) {
                        started.insert(addr.clone());
                        n.dial_on(addr, d);
                    }
                }
                // an address taken off and put on again starts anew
                started.retain(|a| get(&*n.kv, a).is_some());
                drop(n);
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }

    fn dial_on(self: &Arc<Self>, addr: String, d: Dial) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let who = if d.name.is_empty() { "a node".to_string() } else { d.name.clone() };
            let mut known = d.node.map(NodeId);
            let mut wait = Duration::from_millis(500);
            let mut said = String::new();
            loop {
                let Some(n) = weak.upgrade() else { break };
                if get(&*n.kv, &addr).is_none() {
                    println!("{}: no longer dialing {who} at {addr}", n.me);
                    break;
                }
                let was_up = known.as_ref().is_some_and(|p| n.link.peers().contains(p));
                let r = match resolve(&addr) {
                    Err(e) => Err(e),
                    Ok(sa) => match &known {
                        Some(p) => match tokio::time::timeout(ATTEMPT, n.link.dial(p, sa)).await {
                            Ok(Ok(())) => {
                                if !was_up {
                                    n.learn_link(p, &addr).await;
                                }
                                Ok(())
                            }
                            Ok(Err(e)) => Err(e.to_string()),
                            Err(_) => Err("no answer".into()),
                        },
                        None => match tokio::time::timeout(ATTEMPT, n.link.dial_any(sa)).await {
                            Ok(Ok(p)) => {
                                answered(&*n.kv, &addr, &p);
                                println!("{}: linked to {who} ({p}) at {addr}", n.me);
                                n.learn_link(&p, &addr).await;
                                known = Some(p);
                                Ok(())
                            }
                            // nobody of the cluster there: perhaps a node waiting to join
                            failed if n.link.takes_joins() => {
                                let first = match failed {
                                    Ok(Err(e)) => e.to_string(),
                                    _ => "no answer".to_string(),
                                };
                                match n.link.invite(sa).await {
                                    Ok(id) => {
                                        println!("{}: {who} ({id}) at {addr} joined (invited)", n.me);
                                        Ok(())
                                    }
                                    Err(e) => Err(format!("{first}; no node waiting to join either ({e})")),
                                }
                            }
                            Ok(Err(e)) => Err(e.to_string()),
                            Err(_) => Err("no answer".into()),
                        },
                    },
                };
                match r {
                    Ok(()) => {
                        wait = if known.is_some() { UP } else { Duration::from_millis(500) };
                        said.clear();
                    }
                    Err(e) => {
                        // a line when it changes, not every attempt
                        if e != said {
                            eprintln!("{}: dialing {who} at {addr}: {e}", n.me);
                            said = e;
                        }
                        wait = (wait * 2).min(MAX_WAIT);
                    }
                }
                drop(n);
                tokio::time::sleep(wait).await;
            }
        });
    }
}

impl Node {
    /// Node control learns where `id` was reached: its link, and for an edge without public
    /// addresses, the host when it is a public IP (the dns tab shows what to point at it).
    async fn learn_link(self: &Arc<Self>, id: &NodeId, addr: &str) {
        let mut p = self.cluster_state().placement;
        let Some(n) = p.nodes.get_mut(id) else { return };
        let mut what = Vec::new();
        let new_edge = n.link.is_none() && n.has(crate::placement::Role::EdgeHttp);
        if n.link.as_deref() != Some(addr) {
            n.link = Some(addr.to_string());
            what.push(format!("link {addr}"));
        }
        if n.addrs.is_empty() && n.roles.iter().any(|r| r.is_edge()) {
            if let Some(ip) = public_ip(addr) {
                n.addrs = vec![ip.to_string()];
                what.push(format!("public address {ip}"));
            }
        }
        // a new public edge answers the cluster's public names: a copy of every certificate
        // (admin → nodes → certificates can take it off again)
        if new_edge {
            let mut got = Vec::new();
            for (name, c) in p.certs.iter_mut() {
                if c.issuer != *id && !c.copies.contains(id) {
                    c.copies.push(id.clone());
                    got.push(name.clone());
                }
            }
            if !got.is_empty() {
                what.push(format!("copies of {}", got.join(", ")));
            }
        }
        if what.is_empty() {
            return;
        }
        let label = p.node_label(id);
        match self.commit_anywhere(&format!("node {}", self.me), Command::Placement(p)).await {
            Ok(()) => println!("{}: node control: {label}: {}", self.me, what.join(", ")),
            Err(e) => eprintln!("{}: node control: {label}'s link {addr}: {e}", self.me),
        }
    }

    /// Keeps a link to every node that node control knows a `link` for.
    pub fn spawn_mesh(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut started: BTreeSet<NodeId> = BTreeSet::new();
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(n) = weak.upgrade() else { break };
                let p = n.cluster_state().placement;
                // a task ends when its node's link goes; it starts again when there is one
                started.retain(|id| p.nodes.get(id).is_some_and(|x| x.link.is_some()));
                for (id, x) in &p.nodes {
                    if *id != n.me && x.link.is_some() && !started.contains(id) {
                        started.insert(id.clone());
                        n.mesh_to(id.clone());
                    }
                }
            }
        });
    }

    fn mesh_to(self: &Arc<Self>, id: NodeId) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut wait = Duration::from_millis(500);
            let mut said = String::new();
            loop {
                let Some(n) = weak.upgrade() else { break };
                let p = n.cluster_state().placement;
                let Some(addr) = p.nodes.get(&id).and_then(|x| x.link.clone()) else { break };
                if n.link.peers().contains(&id) {
                    wait = Duration::from_secs(5);
                } else {
                    let r = match resolve(&addr) {
                        Err(e) => Err(e),
                        Ok(sa) => match tokio::time::timeout(ATTEMPT, n.link.dial(&id, sa)).await {
                            Ok(r) => r.map_err(|e| e.to_string()),
                            Err(_) => Err("no answer".into()),
                        },
                    };
                    match r {
                        Ok(()) => {
                            if !said.is_empty() {
                                println!("{}: linked to {} at {addr}", n.me, p.node_label(&id));
                            }
                            said.clear();
                            wait = Duration::from_secs(5);
                        }
                        Err(e) => {
                            if e != said {
                                eprintln!("{}: dialing {} at {addr}: {e}", n.me, p.node_label(&id));
                                said = e;
                            }
                            wait = (wait * 2).min(Duration::from_secs(30));
                        }
                    }
                }
                drop(n);
                tokio::time::sleep(wait).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list() {
        let kv = crate::kv::mem::MemKv::new();
        assert!(add(&kv, "edge.example.org", "edge", 1).is_err());
        add(&kv, "203.0.113.7:7441", "edge", 1).unwrap();
        add(&kv, "[2001:db8::7]:7441", "edge6", 2).unwrap();
        let l = list(&kv);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].0, "203.0.113.7:7441");
        answered(&kv, "203.0.113.7:7441", &NodeId("4480f59b-10b0-4a96-a0b5-2065a76f6dc9".into()));
        assert_eq!(get(&kv, "203.0.113.7:7441").unwrap().node.as_deref(), Some("4480f59b-10b0-4a96-a0b5-2065a76f6dc9"));
        assert!(remove(&kv, "203.0.113.7:7441").unwrap());
        assert!(!remove(&kv, "203.0.113.7:7441").unwrap());
        assert_eq!(list(&kv).len(), 1);
    }

    #[test]
    fn public_addresses() {
        assert_eq!(public_ip("198.18.1.228:7441").map(|i| i.to_string()).as_deref(), Some("198.18.1.228"));
        assert_eq!(public_ip("[2001:2:0:f85e::1]:7441").map(|i| i.to_string()).as_deref(), Some("2001:2:0:f85e::1"));
        for a in ["192.168.1.13:7441", "10.0.0.3:7441", "100.64.1.2:7441", "127.0.0.1:7441", "[fd00::1]:7441", "[fe80::1]:7441", "edge.example.org:7441"] {
            assert!(public_ip(a).is_none(), "{a}");
        }
    }
}
