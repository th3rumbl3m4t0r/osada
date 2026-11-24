//! The cluster log (§6.1): what every node must agree on, kept as a
//! collection log of its own whose authority is the voter. With one voter
//! (§7.5) that is all it takes; with three or five, consensus among the
//! voters will decide its entries (decision D3), and the state machine here
//! stays as it is.
//!
//! The state ([`ClusterState`]) is what the entries leave when applied in
//! order: node control ([`Placement`]: nodes, roles, rules, the OU tree and
//! its links, secrets, sign-in rules) and the collections, each with the
//! term it is in (authority, epoch, members). Every node keeps a replica of
//! the log and the state in memory, so routing and lookups stay local.
//!
//! A command is checked against the state before it is appended, so the log
//! holds only commands that applied. Entries carry the command as JSON.

use std::{collections::BTreeMap, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{
    kv::Kv,
    log::{CollectionId, Draft, Entry, Hash, Log, LogError},
    audit::Settings,
    placement::{Domain, Node, NodeId, Ou, OuId, Placement, Problem, Role, Rule},
    repl::Term,
};

/// The cluster log's own collection id.
pub const CLUSTER: CollectionId = CollectionId([0; 16]);
const MODULE: &str = "cluster";

/// The root OU of a new cluster.
pub const ROOT_OU: OuId = OuId(1);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    /// Node control as a whole, as the administrator left it.
    Placement(Placement),
    /// A new collection of a domain in an OU; its authority is where node
    /// control places it, in epoch 1.
    Create { id: CollectionId, domain: Domain, ou: OuId },
    /// The next term of a collection: a handover (§7.3) or a promotion (§7.4).
    Term { id: CollectionId, term: Term },
    /// The settings the admin UI changes: log switches, session lifetime.
    Settings(Settings),
    /// Addresses banned (replacing a ban of the same address) and unbanned (D-U33).
    Bans { add: Vec<crate::bans::Ban>, remove: Vec<String> },
    /// An idp node's signing key published by the node (D-U45), or (None) its key withdrawn by
    /// an administrator: never taken again; the node makes a new one.
    Key { node: NodeId, key: Option<crate::idpkey::PublicKey> },
    /// A program that signs people in with osada (OIDC client), or (None) removed (D-U45).
    OidcClient { name: String, client: Option<crate::oidc::Client> },
}

impl Command {
    fn op(&self) -> &'static str {
        match self {
            Command::Placement(_) => "placement",
            Command::Create { .. } => "create",
            Command::Term { .. } => "term",
            Command::Settings(_) => "settings",
            Command::Bans { .. } => "bans",
            Command::Key { .. } => "key",
            Command::OidcClient { .. } => "oidc_client",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    pub domain: Domain,
    pub ou: OuId,
    pub term: Term,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClusterState {
    pub placement: Placement,
    pub collections: BTreeMap<CollectionId, Collection>,
    pub settings: Settings,
    /// Banned addresses (D-U33), by address.
    pub bans: BTreeMap<String, crate::bans::Ban>,
    /// The idp nodes' signing keys (D-U45), by node, and the kids withdrawn.
    pub keys: BTreeMap<NodeId, crate::idpkey::PublicKey>,
    pub withdrawn: std::collections::BTreeSet<String>,
    /// The programs that sign people in with osada, by name (D-U45).
    pub oidc_clients: BTreeMap<String, crate::oidc::Client>,
}

/// Why a command was not committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// The new node control setting does not pass its check.
    Placement(Vec<Problem>),
    /// The new setting drops a node that a collection still has as a member.
    NodeInUse(NodeId),
    Exists(CollectionId),
    UnknownCollection(CollectionId),
    /// Node control places the domain nowhere for that OU.
    Unplaced(Problem),
    /// A term must be the next epoch.
    Epoch { have: u64, got: u64 },
    /// Not among the members, not a node, or not a store node.
    Member(NodeId),
    /// The node's rule for the collection's domain is `never`.
    Barred(NodeId),
    Settings(String),
}

impl ClusterState {
    /// Applies `cmd`, or says why not and changes nothing.
    pub fn apply(&mut self, cmd: &Command) -> Result<(), Rejected> {
        match cmd {
            Command::Placement(p) => {
                p.check().map_err(Rejected::Placement)?;
                for c in self.collections.values() {
                    if let Some(m) = c.term.members.iter().find(|m| !p.nodes.contains_key(m)) {
                        return Err(Rejected::NodeInUse(m.clone()));
                    }
                }
                self.placement = p.clone();
            }
            Command::Create { id, domain, ou } => {
                if self.collections.contains_key(id) {
                    return Err(Rejected::Exists(*id));
                }
                let a = self.placement.authority(*domain, *ou).map_err(Rejected::Unplaced)?;
                let term = Term { epoch: 1, authority: a.node.clone(), members: vec![a.node] };
                self.collections.insert(*id, Collection { domain: *domain, ou: *ou, term });
            }
            Command::Term { id, term } => {
                let c = self.collections.get(id).ok_or(Rejected::UnknownCollection(*id))?;
                if term.epoch != c.term.epoch + 1 {
                    return Err(Rejected::Epoch { have: c.term.epoch, got: term.epoch });
                }
                if !term.members.contains(&term.authority) {
                    return Err(Rejected::Member(term.authority.clone()));
                }
                for m in &term.members {
                    if !self.placement.nodes.get(m).is_some_and(|n| n.has(Role::Store)) {
                        return Err(Rejected::Member(m.clone()));
                    }
                }
                // An emergency promotion may go against where node control
                // places the domain (`misplaced` then shows it), but never
                // onto a node barred from it.
                if self.placement.nodes[&term.authority].rule(c.domain) == Rule::Never {
                    return Err(Rejected::Barred(term.authority.clone()));
                }
                self.collections.get_mut(id).expect("checked above").term = term.clone();
            }
            Command::Settings(s) => {
                s.check().map_err(Rejected::Settings)?;
                self.settings = s.clone();
            }
            Command::OidcClient { name, client } => match client {
                Some(c) => {
                    c.check(name).map_err(Rejected::Settings)?;
                    self.oidc_clients.insert(name.clone(), c.clone());
                }
                None => drop(self.oidc_clients.remove(name)),
            },
            Command::Key { node, key } => {
                if !self.placement.nodes.contains_key(node) {
                    return Err(Rejected::Settings(format!("no node {node}")));
                }
                match key {
                    Some(k) => {
                        k.check().map_err(Rejected::Settings)?;
                        if self.withdrawn.contains(&k.kid) {
                            return Err(Rejected::Settings(format!("the key {} was withdrawn", k.kid)));
                        }
                        if self.keys.iter().any(|(n, x)| n != node && x.kid == k.kid) {
                            return Err(Rejected::Settings(format!("the key {} is another node's", k.kid)));
                        }
                        self.keys.insert(node.clone(), k.clone());
                    }
                    None => {
                        if let Some(old) = self.keys.remove(node) {
                            self.withdrawn.insert(old.kid);
                        }
                    }
                }
            }
            Command::Bans { add, remove } => {
                for b in add {
                    if b.addr.parse::<std::net::IpAddr>().is_err() || b.until <= b.since {
                        return Err(Rejected::Settings(format!("not a ban: {:?}", b.addr)));
                    }
                }
                for a in remove {
                    self.bans.remove(a);
                }
                for b in add {
                    self.bans.insert(b.addr.clone(), b.clone());
                }
            }
        }
        Ok(())
    }

    /// Collections whose authority is not where node control now places
    /// them, with where it is and where it should be: the handovers to make.
    pub fn misplaced(&self) -> Vec<(CollectionId, NodeId, NodeId)> {
        self.collections
            .iter()
            .filter_map(|(id, c)| {
                let want = self.placement.authority(c.domain, c.ou).ok()?.node;
                (want != c.term.authority).then(|| (*id, c.term.authority.clone(), want))
            })
            .collect()
    }
}

#[derive(Debug)]
pub enum ClusterError {
    Rejected(Rejected),
    Log(LogError),
    /// An entry that cannot be read or no longer applies.
    Entry(String),
    AlreadyMade,
}

impl fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClusterError::Rejected(r) => write!(f, "rejected: {r:?}"),
            ClusterError::Log(e) => write!(f, "{e}"),
            ClusterError::Entry(m) => write!(f, "cluster log: {m}"),
            ClusterError::AlreadyMade => write!(f, "this store already has a cluster log"),
        }
    }
}

impl std::error::Error for ClusterError {}

impl From<Rejected> for ClusterError {
    fn from(r: Rejected) -> Self {
        ClusterError::Rejected(r)
    }
}

impl From<LogError> for ClusterError {
    fn from(e: LogError) -> Self {
        ClusterError::Log(e)
    }
}

pub struct Cluster {
    log: Arc<Log>,
    state: ClusterState,
    /// The last entry applied to `state`.
    applied: (u64, Hash),
}

impl Cluster {
    /// The cluster log in `kv`, and its state rebuilt from it.
    pub fn open(kv: Arc<dyn Kv>) -> Result<Cluster, ClusterError> {
        let log = Arc::new(Log::open(kv, CLUSTER)?);
        let mut c = Cluster { log, state: ClusterState::default(), applied: (0, CLUSTER.genesis()) };
        c.catch_up()?;
        Ok(c)
    }

    /// `init` (§6.2): the first node's cluster log. The node has every role
    /// and is the authority for every domain; the organisation is one OU.
    pub fn init(kv: Arc<dyn Kv>, node: NodeId, time: u64) -> Result<Cluster, ClusterError> {
        Cluster::init_named(kv, node, None, time)
    }

    /// A new cluster of this one node, which people call `name` (D-U19).
    pub fn init_named(kv: Arc<dyn Kv>, node: NodeId, name: Option<String>, time: u64) -> Result<Cluster, ClusterError> {
        let mut c = Cluster::open(kv)?;
        if c.log.head().seq > 0 {
            return Err(ClusterError::AlreadyMade);
        }
        let mut p = Placement::default();
        let roles = [Role::Voter, Role::Idp, Role::Store, Role::Blob, Role::EdgeHttp, Role::EdgeMail, Role::Worker];
        let mut me = Node::new(roles, []);
        me.name = name;
        p.nodes.insert(node.clone(), me);
        p.ous.insert(ROOT_OU, Ou::new("organisation", None));
        // a new cluster's OUs besides the root (Round 24): people, sites, programs; admin → OUs makes more
        for (id, name) in [(2, "users"), (3, "web"), (4, "services")] {
            p.ous.insert(OuId(id), Ou::new(name, Some(ROOT_OU)));
        }
        p.web_ou = Some(OuId(3));
        for d in Domain::ALL.into_iter().filter(|&d| d != Domain::Ledger) {
            p.defaults.insert(d, node.clone());
        }
        p.default_secrets.insert(node);
        c.commit(1, time, "init", Command::Placement(p))?;
        Ok(c)
    }

    pub fn state(&self) -> &ClusterState {
        &self.state
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    /// The same log handle, for the replica that carries it (one per
    /// collection on a node).
    pub fn shared_log(&self) -> Arc<Log> {
        self.log.clone()
    }

    /// The seq of the last entry the state reflects.
    pub fn applied(&self) -> u64 {
        self.applied.0
    }

    /// As the voter in `epoch`: checks `cmd` against the state, then
    /// appends and applies it.
    pub fn commit(&mut self, epoch: u64, time: u64, actor: &str, cmd: Command) -> Result<Entry, ClusterError> {
        self.catch_up()?;
        let mut next = self.state.clone();
        next.apply(&cmd)?;
        let payload = serde_json::to_vec(&cmd).map_err(|e| ClusterError::Entry(e.to_string()))?;
        let draft = Draft { actor: actor.into(), module: MODULE.into(), op: cmd.op().into(), payload };
        let e = self.log.append(epoch, time, draft)?;
        self.state = next;
        self.applied = (e.seq, e.hash());
        Ok(e)
    }

    /// Applies what the log holds past what was applied. When the log was
    /// cut below that (§7.4), the state is rebuilt from the start.
    pub fn catch_up(&mut self) -> Result<(), ClusterError> {
        if self.log.hash_at(self.applied.0)? != Some(self.applied.1) {
            self.state = ClusterState::default();
            self.applied = (0, CLUSTER.genesis());
        }
        loop {
            let entries = self.log.read(self.applied.0, 1024)?;
            if entries.is_empty() {
                return Ok(());
            }
            for e in entries {
                let cmd: Command = serde_json::from_slice(&e.payload)
                    .map_err(|err| ClusterError::Entry(format!("entry {}: {err}", e.seq)))?;
                self.state
                    .apply(&cmd)
                    .map_err(|r| ClusterError::Entry(format!("entry {} no longer applies: {r:?}", e.seq)))?;
                self.applied = (e.seq, e.hash());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::mem::MemKv;

    fn n(s: &str) -> NodeId {
        NodeId(s.into())
    }

    fn store() -> Arc<dyn Kv> {
        Arc::new(MemKv::new())
    }

    const FILES: CollectionId = CollectionId([1; 16]);
    const WIKI: CollectionId = CollectionId([2; 16]);

    /// home alone, then edge joins: an idp and store node, always the
    /// authority for mail and links, never for the ledger or files.
    fn with_edge(c: &mut Cluster) {
        let mut p = c.state().placement.clone();
        p.nodes.insert(
            n("edge"),
            Node::new(
                [Role::Idp, Role::Store, Role::Blob, Role::EdgeHttp, Role::EdgeMail],
                [(Domain::Mail, Rule::Always), (Domain::Links, Rule::Always), (Domain::Ledger, Rule::Never), (Domain::Files, Rule::Never)],
            ),
        );
        let home = p.nodes.get_mut(&n("home")).unwrap();
        home.roles.remove(&Role::EdgeHttp);
        home.roles.remove(&Role::EdgeMail);
        c.commit(1, 2, "admin", Command::Placement(p)).unwrap();
    }

    #[test]
    fn init_once_and_reopen() {
        let kv = store();
        let c = Cluster::init(kv.clone(), n("home"), 1).unwrap();
        let p = &c.state().placement;
        assert_eq!(p.voters(), [&n("home")]);
        assert_eq!(p.authority(Domain::Files, ROOT_OU).unwrap().node, n("home"));
        assert!(matches!(Cluster::init(kv.clone(), n("other"), 2), Err(ClusterError::AlreadyMade)));
        let again = Cluster::open(kv).unwrap();
        assert_eq!(again.state(), c.state());
        assert_eq!(again.log().head().seq, 1);
    }

    #[test]
    fn rejected_commands_leave_no_trace() {
        let mut c = Cluster::init(store(), n("home"), 1).unwrap();
        c.commit(1, 2, "admin", Command::Create { id: FILES, domain: Domain::Files, ou: ROOT_OU }).unwrap();
        let before = (c.state().clone(), c.log().head());
        let bad = [
            (Command::Create { id: FILES, domain: Domain::Files, ou: ROOT_OU }, Rejected::Exists(FILES)),
            (
                Command::Term { id: FILES, term: Term { epoch: 3, authority: n("home"), members: vec![n("home")] } },
                Rejected::Epoch { have: 1, got: 3 },
            ),
            (
                Command::Term { id: FILES, term: Term { epoch: 2, authority: n("nowhere"), members: vec![n("nowhere")] } },
                Rejected::Member(n("nowhere")),
            ),
            (
                Command::Create { id: WIKI, domain: Domain::Wiki, ou: OuId(99) },
                Rejected::Unplaced(Problem::UnknownOu(OuId(99))),
            ),
            (Command::Placement(Placement::default()), Rejected::Placement(Placement::default().check().unwrap_err())),
        ];
        for (cmd, why) in bad {
            match c.commit(1, 3, "admin", cmd) {
                Err(ClusterError::Rejected(r)) => assert_eq!(r, why),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!((c.state().clone(), c.log().head()), before);
    }

    #[test]
    fn placement_changes_show_handovers_to_make() {
        let mut c = Cluster::init(store(), n("home"), 1).unwrap();
        with_edge(&mut c);
        for (id, domain) in [(FILES, Domain::Files), (WIKI, Domain::Wiki)] {
            c.commit(1, 3, "admin", Command::Create { id, domain, ou: ROOT_OU }).unwrap();
        }
        assert!(c.state().misplaced().is_empty());

        // The administrator moves the wiki to edge.
        let mut p = c.state().placement.clone();
        p.defaults.insert(Domain::Wiki, n("edge"));
        c.commit(1, 4, "admin", Command::Placement(p)).unwrap();
        assert_eq!(c.state().misplaced(), [(WIKI, n("home"), n("edge"))]);
        let term = Term { epoch: 2, authority: n("edge"), members: vec![n("home"), n("edge")] };
        c.commit(1, 5, "admin", Command::Term { id: WIKI, term }).unwrap();
        assert!(c.state().misplaced().is_empty());

        // Files never go to the edge, not even by promotion.
        let term = Term { epoch: 2, authority: n("edge"), members: vec![n("home"), n("edge")] };
        assert!(matches!(
            c.commit(1, 6, "admin", Command::Term { id: FILES, term }),
            Err(ClusterError::Rejected(Rejected::Barred(_)))
        ));
        // A node that is still a member cannot be dropped.
        let mut p = c.state().placement.clone();
        p.nodes.remove(&n("edge"));
        p.defaults.insert(Domain::Wiki, n("home"));
        assert!(matches!(
            c.commit(1, 7, "admin", Command::Placement(p)),
            Err(ClusterError::Rejected(Rejected::NodeInUse(_)))
        ));
    }

    #[test]
    fn a_replica_rebuilds_the_same_state() {
        let mut voter = Cluster::init(store(), n("home"), 1).unwrap();
        with_edge(&mut voter);
        voter.commit(1, 3, "admin", Command::Create { id: FILES, domain: Domain::Files, ou: ROOT_OU }).unwrap();
        let mut replica = Cluster::open(store()).unwrap();
        replica.log().accept_all(1, voter.log().read(0, 100).unwrap()).unwrap();
        replica.catch_up().unwrap();
        assert_eq!(replica.state(), voter.state());

        // Its tail cut after a promotion (§7.4): the state is rebuilt.
        replica.log().fence(2).unwrap();
        replica.log().cut_after(1).unwrap();
        replica.catch_up().unwrap();
        assert_eq!(replica.state().collections.len(), 0);
        assert_eq!(replica.state().placement.nodes.len(), 1);
    }
}
