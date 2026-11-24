//! What a node exposes to the internet (D-U35; crate::exposure): every hour (first 90 s after a
//! start) this node asks a peer, one after another in turn, to connect to its TCP ports that
//! listen beyond loopback, at its public addresses and at the address the peer sees it come from.
//! The peer checks only addresses node control gives the asker, and the one it comes from. The answer is kept in memory and shown on admin → nodes.

use std::{net::IpAddr, sync::Arc, time::Duration};

use serde_json::{Value, json};

use super::{Actor, Answer, Call, Node};
use crate::{exposure, placement::NodeId, rbac::Perm};

const EVERY: Duration = Duration::from_secs(3600);

impl Node {
    /// The public addresses node control and the interfaces give this node.
    fn public_addrs(&self) -> Vec<IpAddr> {
        let p = self.cluster_state().placement;
        let mut v: Vec<IpAddr> = p.nodes.get(&self.me).map(|n| n.addrs.iter().filter_map(|a| a.trim().trim_matches(['[', ']']).parse().ok()).collect()).unwrap_or_default();
        for a in exposure::public_ifaddrs() {
            if !v.contains(&a) {
                v.push(a);
            }
        }
        v.retain(exposure::is_public);
        v
    }

    /// Asks a peer (the `turn`-th, round) to check this node's TCP ports; keeps what it found.
    pub(super) async fn check_exposure(self: &Arc<Self>, turn: usize) -> exposure::Probe {
        let mut peers = self.link.peers();
        peers.sort();
        let now = super::now_ms();
        let Some(peer) = (!peers.is_empty()).then(|| peers[turn % peers.len()].clone()) else {
            let p = exposure::Probe { at: now, error: Some("no other node to check from".into()), ..Default::default() };
            exposure::set_last(p.clone());
            return p;
        };
        let mut ports: Vec<u16> = exposure::listening().into_iter().filter(|s| s.proto == "tcp").map(|s| s.port).collect();
        ports.sort();
        ports.dedup();
        let addrs: Vec<String> = self.public_addrs().iter().map(|a| a.to_string()).collect();
        let call = Call::Probe { addrs, ports };
        let p = match tokio::time::timeout(Duration::from_secs(20), self.ask(&peer, call)).await {
            Ok(Ok(Answer::Web { value })) => exposure::Probe { at: now, by: peer.0.clone(), results: serde_json::from_value(value["results"].clone()).unwrap_or_default(), error: None },
            Ok(Ok(Answer::Refused { why })) => exposure::Probe { at: now, by: peer.0.clone(), error: Some(why), ..Default::default() },
            Ok(Ok(other)) => exposure::Probe { at: now, by: peer.0.clone(), error: Some(format!("{other:?}")), ..Default::default() },
            Ok(Err(e)) => exposure::Probe { at: now, by: peer.0.clone(), error: Some(e), ..Default::default() },
            Err(_) => exposure::Probe { at: now, by: peer.0.clone(), error: Some("no answer in 20 s".into()), ..Default::default() },
        };
        exposure::set_last(p.clone());
        p
    }

    pub(super) fn spawn_exposure(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(90)).await;
            let mut turn = 0;
            loop {
                let Some(n) = weak.upgrade() else { break };
                n.check_exposure(turn).await;
                drop(n);
                turn += 1;
                tokio::time::sleep(EVERY).await;
            }
        });
    }

    /// As the prober: connects to `ports` at `addrs` and at the address `from` comes from.
    pub(super) fn answer_probe(&self, from: &NodeId, addrs: Vec<String>, ports: Vec<u16>) -> Answer {
        if ports.len() > 200 || addrs.len() > 16 {
            return Answer::Refused { why: "too many to check".into() };
        }
        // only the asker's own addresses: those node control gives it, and where its link comes from
        // (never a probe of anyone else's)
        let declared: Vec<IpAddr> = self
            .cluster_state()
            .placement
            .nodes
            .get(from)
            .map(|n| n.addrs.iter().filter_map(|a| a.trim().trim_matches(['[', ']']).parse().ok()).collect())
            .unwrap_or_default();
        let mut targets: Vec<IpAddr> = addrs.iter().filter_map(|a| a.parse().ok()).filter(|a| exposure::is_public(a) && declared.contains(a)).collect();
        let seen = self.link.remote_of(from).map(|a| a.ip().to_canonical());
        if let Some(s) = seen.filter(exposure::is_public) {
            if !targets.contains(&s) {
                targets.push(s);
            }
        }
        let results = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(exposure::probe(&targets, &ports)));
        Answer::Web { value: json!({ "seen": seen.map(|s| s.to_string()), "results": results }) }
    }

    /// This node's exposure for node info.
    pub(super) fn exposure_report(&self) -> Value {
        let mut l = exposure::listening();
        // the mail instance's Stalwart on this node is osada's to run (D-U38)
        let mail_here = self.mail_here();
        for s in &mut l {
            if mail_here && !s.osada && s.proto == "tcp" && exposure::MAIL_PORTS.contains(&s.port) && s.process.as_deref().is_none_or(|p| p == "stalwart") {
                s.by = Some("mail (Stalwart)".into());
            }
        }
        exposure::report(&l, &self.public_addrs())
    }

    /// admin → nodes → a node's card: check now.
    pub async fn exposure_now(self: &Arc<Self>, actor: &Actor, node: &str) -> Result<Value, String> {
        if !self.may(&actor.user, Perm::ClusterManage, crate::cluster::ROOT_OU) {
            return Err("you cannot manage the nodes".into());
        }
        let id = self.cluster_state().placement.resolve_node(node).ok_or_else(|| format!("no node {node}"))?;
        match tokio::time::timeout(Duration::from_secs(30), self.ask(&id, Call::ExposureNow)).await {
            Ok(Ok(Answer::Web { value })) => Ok(value),
            Ok(Ok(Answer::Refused { why })) => Err(why),
            Ok(Ok(other)) => Err(format!("{other:?}")),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("no answer in 30 s".into()),
        }
    }
}
