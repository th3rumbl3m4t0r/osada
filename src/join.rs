//! Joining a cluster (§6.2): one-time tokens, made on the node that holds
//! the cluster CA, and what that node does when a new node comes with one.
//!
//! A token is `osada-join.<secret>.<CA fingerprint>`: the secret (32 random
//! bytes, hex) lets the new node in once, before it runs out; the
//! fingerprint lets the new node make sure it talks to this cluster. The CA's
//! node keeps only the secret's sha256, with the node name and roles it is
//! for, in its own store (keys `n/join/…`, which are not replicated).

use std::collections::BTreeSet;

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    cluster::{Cluster, Command},
    kv::{Batch, Kv},
    link::{JoinGrant, JoinRequest},
    pki::{self, Ca, P256PublicKey},
    placement::{Node, NodeId, Role},
};

const PREFIX: &str = "osada-join";
/// Tokens made before the rename.
const LEGACY_PREFIX: &str = "shelf-join";

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Ticket {
    /// The id the node gets (D-U19: a GUID).
    node: String,
    /// Its name, which the joining node may give to say which token it uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    roles: BTreeSet<Role>,
    /// Milliseconds since 1970.
    expires: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub secret: String,
    pub ca_fingerprint: String,
}

impl Token {
    pub fn parse(s: &str) -> Option<Token> {
        let mut parts = s.trim().split('.');
        let (prefix, secret, fp) = (parts.next()?, parts.next()?, parts.next()?);
        let hex64 = |x: &str| x.len() == 64 && x.bytes().all(|b| b.is_ascii_hexdigit());
        ((prefix == PREFIX || prefix == LEGACY_PREFIX) && hex64(secret) && hex64(fp) && parts.next().is_none())
            .then(|| Token { secret: secret.to_ascii_lowercase(), ca_fingerprint: fp.to_ascii_lowercase() })
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{PREFIX}.{}.{}", self.secret, self.ca_fingerprint)
    }
}

fn ticket_key(secret_hex: &str) -> Vec<u8> {
    let mut k = b"n/join/".to_vec();
    k.extend_from_slice(&Sha256::digest(secret_hex.as_bytes()));
    k
}

/// A token for a node people will call `name` to join with `roles`, good
/// until `expires`; the id it will have (a new GUID, D-U19).
pub fn make_token(kv: &dyn Kv, ca: &Ca, name: &str, roles: BTreeSet<Role>, expires: u64) -> Result<(Token, NodeId), String> {
    let id = NodeId::fresh();
    make_token_for(kv, ca, &id, name, roles, expires).map(|t| (t, id))
}

/// The same for the id `id`.
pub fn make_token_for(kv: &dyn Kv, ca: &Ca, id: &NodeId, name: &str, roles: BTreeSet<Role>, expires: u64) -> Result<Token, String> {
    if !crate::placement::valid_node_label(name) {
        return Err(format!("{name:?} is not a node name (letters, digits, '-', '_' and '.')"));
    }
    if !pki::valid_node_name(&id.0) {
        return Err(format!("{id:?} is not a node id"));
    }
    let node = id.0.as_str();
    let mut secret = [0u8; 32];
    SystemRandom::new().fill(&mut secret).map_err(|_| "no randomness".to_string())?;
    let token = Token { secret: hex::encode(secret), ca_fingerprint: ca.fingerprint().map_err(|e| e.to_string())? };
    let ticket = Ticket { node: node.into(), name: Some(name.into()), roles, expires };
    let mut b = Batch::new();
    b.put(ticket_key(&token.secret), serde_json::to_vec(&ticket).map_err(|e| e.to_string())?);
    kv.write(b).map_err(|e| e.to_string())?;
    Ok(token)
}

/// As the CA's node, at `now`: checks the request against its token, adds
/// the node to node control in the cluster log (with the rules proposed for
/// its roles), uses the token up and signs the node's key. The answer's
/// error is what the joining node is told.
pub fn grant(kv: &dyn Kv, ca: &Ca, cluster: &mut Cluster, req: &JoinRequest, now: u64) -> Result<JoinGrant, String> {
    const UNKNOWN: &str = "unknown or used token";
    let key = ticket_key(&req.token.to_ascii_lowercase());
    let raw = kv.get(&key).map_err(|e| e.to_string())?.ok_or(UNKNOWN)?;
    let ticket: Ticket = serde_json::from_slice(&raw).map_err(|_| UNKNOWN)?;
    if ticket.expires <= now {
        let mut b = Batch::new();
        b.delete(key);
        kv.write(b).map_err(|e| e.to_string())?;
        return Err("the token has run out".into());
    }
    // the joining node may say which node it is: the token's name or id
    let said = req.node.trim();
    let named = ticket.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(said));
    if !(said.is_empty() && ticket.name.is_some()) && !named && said != ticket.node {
        return Err(format!("the token is for node {:?}", ticket.name.as_deref().unwrap_or(&ticket.node)));
    }
    let public_key = P256PublicKey::from_hex(&req.public_key).map_err(|e| e.to_string())?;
    let cert_pem = ca.issue_for(&ticket.node, &public_key).map_err(|e| e.to_string())?;

    let node = NodeId(ticket.node.clone());
    let mut p = cluster.state().placement.clone();
    if p.nodes.contains_key(&node) {
        return Err(format!("the cluster has a node {node} already"));
    }
    if let Some(n) = ticket.name.as_deref().filter(|n| p.resolve_node(n).is_some()) {
        return Err(format!("the cluster has a node {n} already"));
    }
    let rules = Node::proposed_rules(&ticket.roles);
    p.nodes.insert(node.clone(), Node { name: ticket.name, roles: ticket.roles, rules, cache_gb: None, addrs: Vec::new(), link: None, files_dir: None, log_batch: None });
    let epoch = cluster.log().head().epoch.max(1);
    cluster.commit(epoch, now, &format!("join:{node}"), Command::Placement(p)).map_err(|e| e.to_string())?;

    let mut b = Batch::new();
    b.delete(key);
    kv.write(b).map_err(|e| e.to_string())?;
    Ok(JoinGrant { cert_pem, ca_pem: ca.cert_pem.clone(), node: ticket.node, by: String::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        kv::mem::MemKv,
        placement::{Domain, Rule},
    };
    use std::sync::Arc;

    fn request(token: &Token, node: &str) -> JoinRequest {
        let key = rcgen::KeyPair::generate().unwrap();
        JoinRequest { node: node.into(), token: token.secret.clone(), public_key: hex::encode(key.public_key_raw()) }
    }

    #[test]
    fn tokens_read_back() {
        let t = Token { secret: "ab".repeat(32), ca_fingerprint: "cd".repeat(32) };
        assert_eq!(Token::parse(&format!(" {t}\n")), Some(t.clone()));
        assert_eq!(Token::parse(&t.to_string().to_uppercase().replace("OSADA-JOIN", PREFIX)), Some(t.clone()));
        for bad in ["", "osada-join", "osada-join.ab.cd", &format!("{t}.x"), &t.to_string().replace("osada", "other")] {
            assert_eq!(Token::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_token_lets_one_node_in_once() {
        let kv: Arc<dyn Kv> = Arc::new(MemKv::new());
        let ca = Ca::new().unwrap();
        let mut cluster = Cluster::init(kv.clone(), NodeId("home".into()), 1).unwrap();
        let roles: BTreeSet<Role> = [Role::Idp, Role::Store, Role::Blob, Role::EdgeHttp, Role::EdgeMail].into();
        let token = make_token_for(&*kv, &ca, &NodeId("edge".into()), "edge", roles.clone(), 1000).unwrap();
        assert_eq!(token.ca_fingerprint, ca.fingerprint().unwrap());

        assert_eq!(grant(&*kv, &ca, &mut cluster, &request(&token, "eve"), 10).unwrap_err(), "the token is for node \"edge\"");
        let g = grant(&*kv, &ca, &mut cluster, &request(&token, "edge"), 10).unwrap();
        assert_eq!(pki::fingerprint(&g.ca_pem).unwrap(), token.ca_fingerprint);
        let edge = &cluster.state().placement.nodes[&NodeId("edge".into())];
        assert_eq!(edge.roles, roles);
        assert_eq!((edge.rule(Domain::Files), edge.rule(Domain::Ledger), edge.rule(Domain::Wiki)), (Rule::Never, Rule::Never, Rule::May));
        assert_eq!(grant(&*kv, &ca, &mut cluster, &request(&token, "edge"), 11).unwrap_err(), "unknown or used token");

        let late = make_token(&*kv, &ca, "late", [Role::Store].into(), 100).unwrap().0;
        assert_eq!(grant(&*kv, &ca, &mut cluster, &request(&late, "late"), 100).unwrap_err(), "the token has run out");
        assert_eq!(grant(&*kv, &ca, &mut cluster, &request(&late, "late"), 50).unwrap_err(), "unknown or used token");

        let twice = make_token_for(&*kv, &ca, &NodeId("edge".into()), "edge2", [Role::Store].into(), 1000).unwrap();
        assert!(grant(&*kv, &ca, &mut cluster, &request(&twice, "edge2"), 10).unwrap_err().contains("has a node edge"));
        // a new id, a name taken
        let (same, _) = make_token(&*kv, &ca, "EDGE", [Role::Store].into(), 1000).unwrap();
        assert!(grant(&*kv, &ca, &mut cluster, &request(&same, ""), 10).unwrap_err().contains("has a node EDGE"));
        // a GUID for a new node, its name in node control; the joining node need not say its name
        let (t, id) = make_token(&*kv, &ca, "mail-1", [Role::Store, Role::EdgeMail].into(), 1000).unwrap();
        assert_eq!(id.0.len(), 36);
        let g = grant(&*kv, &ca, &mut cluster, &request(&t, ""), 10).unwrap();
        assert_eq!(g.node, id.0);
        let p = &cluster.state().placement;
        assert_eq!(p.resolve_node("Mail-1"), Some(id.clone()));
        assert_eq!(p.node_label(&id), "mail-1");
        assert_eq!(p.resolve_node(&id.0), Some(id));
        assert!(make_token(&*kv, &ca, "not a name", BTreeSet::new(), 1000).is_err());
    }
}
