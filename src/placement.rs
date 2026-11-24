//! Node control (§6.4): which node is the authority for what, and what each
//! node may be.
//!
//! The whole setting is one value, [`Placement`], kept in the cluster log. A
//! change makes a new value ([`Placement::changed`]), checked as a whole
//! before it may be committed. [`Placement::authority`] answers, for a domain
//! and an OU, which node is the authority and why; [`moves`] lists the
//! handovers a change sets off.
//!
//! How a domain's authority is found for an OU:
//!
//! 1. the node whose rule for the domain is `always`;
//! 2. else the highest enforced link on the way from the root OU down;
//! 3. else the nearest link, from the OU itself upwards;
//! 4. else the cluster's default for the domain.
//!
//! The ledger (the cluster log) is not placed: its authorities are the voters.
//!
//! Secrets (§10.7) are placed beside the domains: per OU, the `idp` nodes
//! that keep its credentials and alone sign its tokens
//! ([`Placement::secret_holders`]); a sign-in anywhere else is proxied to
//! them ([`Placement::sign_in`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::signin::SignInRule;

/// What authority is placed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Domain {
    /// The cluster log; its authorities are the voters.
    Ledger,
    /// The OU objects and the credentials: sign-in data.
    Directory,
    Files,
    Wiki,
    /// Shared links.
    Links,
    Mail,
    Chat,
    Calendar,
    Contacts,
}

impl Domain {
    pub const ALL: [Domain; 9] = [
        Domain::Ledger,
        Domain::Directory,
        Domain::Files,
        Domain::Wiki,
        Domain::Links,
        Domain::Mail,
        Domain::Chat,
        Domain::Calendar,
        Domain::Contacts,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Domain::Ledger => "ledger",
            Domain::Directory => "directory",
            Domain::Files => "files",
            Domain::Wiki => "wiki",
            Domain::Links => "links",
            Domain::Mail => "mail",
            Domain::Chat => "chat",
            Domain::Calendar => "calendar",
            Domain::Contacts => "contacts",
        }
    }
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a node does (§4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Voter,
    Idp,
    Store,
    Blob,
    EdgeHttp,
    EdgeMail,
    Worker,
    Modhost,
    Media,
    /// Keeps copies of files fetched through it (an edge), sent only after
    /// home said yes to the very request (D-U11).
    Cache,
    /// Keeps public links (and the pages) for when home is away (D-U11).
    PublicCache,
}

impl Role {
    pub fn is_edge(self) -> bool {
        matches!(self, Role::EdgeHttp | Role::EdgeMail)
    }
}

/// A node's rule for a domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// The authority for the domain everywhere; OU placements do not apply.
    Always,
    /// May be placed there (the default).
    May,
    /// Never the authority for the domain.
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl NodeId {
    /// A new node's id: a random GUID (the UUID v4 form), which its
    /// certificate names. People see and type its name instead (D-U19).
    pub fn fresh() -> NodeId {
        let mut b: [u8; 16] = rand::random();
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h = hex::encode(b);
        NodeId(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
    }
}

/// A node's name (D-U19): letters, digits, '-', '_' and '.', 1 to 63, not
/// starting or ending with '-' or '.'.
/// A node's files folder (D-U41): an absolute path, no `..`, nothing odd in it.
pub fn valid_files_dir(d: &str) -> bool {
    d.starts_with('/') && d.len() > 1 && d.len() <= 255 && !d.split('/').any(|p| p == ".." || p == ".") && d.chars().all(|c| c.is_ascii_alphanumeric() || "/_-.@+".contains(c))
}

pub fn valid_node_label(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !s.starts_with(['-', '.'])
        && !s.ends_with(['-', '.'])
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OuId(pub u64);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// What people see and type for the node (D-U19): its id is a GUID made
    /// at its first start, this is editable. Unique among the nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub roles: BTreeSet<Role>,
    /// Rules set in node control; a domain not here is `may`.
    pub rules: BTreeMap<Domain, Rule>,
    /// How much a cache or public-cache node keeps, in GB (20 when not set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_gb: Option<u32>,
    /// Its public addresses (what DNS should point at for the names it is exposed on; T10, T11).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addrs: Vec<String>,
    /// Where other nodes dial its link (`host:port`): every node keeps a link to every node that
    /// has one (node/dial.rs). Learnt when a node that dials it reached it there (a dial list), or
    /// set on its card; none for a node nobody can reach (home behind NAT: it dials out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Where it keeps copies of files (cache, public-cache): a folder on a volume of their own
    /// (the node card's drives), as `<files_dir>/osada-<id>`; none: its data folder (D-U41).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_dir: Option<String>,
    /// Lines of its logs sent to the logs authority at once (none: 500; D-U41).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_batch: Option<u32>,
}

impl Node {
    pub fn new(roles: impl IntoIterator<Item = Role>, rules: impl IntoIterator<Item = (Domain, Rule)>) -> Node {
        Node { name: None, roles: roles.into_iter().collect(), rules: rules.into_iter().collect(), cache_gb: None, addrs: Vec::new(), link: None, files_dir: None, log_batch: None }
    }

    pub fn has(&self, role: Role) -> bool {
        self.roles.contains(&role)
    }

    pub fn rule(&self, domain: Domain) -> Rule {
        self.rules.get(&domain).copied().unwrap_or(Rule::May)
    }

    /// The rules node control proposes for a node with these roles: an edge
    /// is never the ledger's or the files' authority. Only proposed; a
    /// single node that is everything keeps `may`.
    pub fn proposed_rules(roles: &BTreeSet<Role>) -> BTreeMap<Domain, Rule> {
        if roles.iter().any(|r| r.is_edge()) {
            [(Domain::Ledger, Rule::Never), (Domain::Files, Rule::Never)].into()
        } else {
            BTreeMap::new()
        }
    }
}

/// An OU's placement of a domain on a node. An enforced link decides for
/// every OU below it, whatever they link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub node: NodeId,
    pub enforced: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ou {
    pub name: String,
    /// None for the root, the organisation.
    pub parent: Option<OuId>,
    pub links: BTreeMap<Domain, Link>,
    /// Where the credentials of its accounts are kept (§10.7); none: as
    /// the OU above, or the cluster default.
    pub secrets: Option<SecretLink>,
    /// Tier 0 (§10.7), turned on here by an administrator: FIDO2 for this
    /// OU and every OU below it. Off unless turned on.
    pub tier0: bool,
    /// A second factor required here and below (§10.6). Off unless turned on.
    pub second_factor: bool,
    /// The storage limit (bytes) of accounts here and below that have none of their own
    /// (D-U30): the nearest OU's; below, only a lower one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_limit: Option<u64>,
    /// The longest a sign-in lasts here and below, in days (D-U30): the shortest on the way
    /// down, and never longer than the cluster's setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_days: Option<u32>,
}

impl Ou {
    pub fn new(name: &str, parent: Option<OuId>) -> Ou {
        Ou { name: name.into(), parent, links: BTreeMap::new(), secrets: None, tier0: false, second_factor: false, storage_limit: None, session_days: None }
    }
}

/// The `idp` nodes that keep an OU's credentials and sign its tokens,
/// linked to the OU like a domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretLink {
    pub nodes: BTreeSet<NodeId>,
    pub enforced: bool,
}

/// Where an OU's secrets are, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secrets {
    pub nodes: BTreeSet<NodeId>,
    pub source: Source,
}

/// What a node does with a sign-in for an account (§10.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignIn {
    /// It keeps the account's secrets: it checks them and signs.
    Here,
    /// It passes the exchange on to one of these and keeps nothing.
    Proxy(Vec<NodeId>),
}

/// A module instance (§12, A24): one kind of module under a name of its
/// own, on a node. A deployment may run several of a kind (two mail
/// modules, a wiki per department), meant to sit on different nodes; the
/// node's rule for the kind applies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub kind: ModuleKind,
    pub title: String,
    pub node: NodeId,
    /// A proxy's own public name (its pages are not on osada's origin); for a
    /// mail instance run by a server outside osada (Stalwart), that server's
    /// name, whose TLS the instance's node passes on (D-U16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Where a proxy passes requests to: `http(s)://host[:port]`; a mail
    /// instance's server: `tls://host:port` (sent a PROXY v2 header first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// A web instance's site (D-U21); its node is where the site runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<crate::sites::Site>,
}

impl Module {
    pub fn new(kind: ModuleKind, title: &str, node: NodeId) -> Module {
        Module { kind, title: title.into(), node, host: None, upstream: None, site: None }
    }
}

/// What a module instance is: data of a domain, or a proxy (another
/// program's pages at a name of their own, for those with access; D-U7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModuleKind {
    Data(Domain),
    Other(OtherKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OtherKind {
    Proxy,
    /// A website (D-U21).
    Web,
}

impl ModuleKind {
    pub const PROXY: ModuleKind = ModuleKind::Other(OtherKind::Proxy);
    pub const WEB: ModuleKind = ModuleKind::Other(OtherKind::Web);

    pub fn name(self) -> &'static str {
        match self {
            ModuleKind::Data(d) => d.name(),
            ModuleKind::Other(OtherKind::Proxy) => "proxy",
            ModuleKind::Other(OtherKind::Web) => "web",
        }
    }
}

impl From<Domain> for ModuleKind {
    fn from(d: Domain) -> Self {
        ModuleKind::Data(d)
    }
}

impl fmt::Display for ModuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModuleKind::Data(d) => d.fmt(f),
            ModuleKind::Other(OtherKind::Proxy) => f.write_str("proxy"),
            ModuleKind::Other(OtherKind::Web) => f.write_str("web"),
        }
    }
}

/// A host name for a proxy or a site: a DNS name with a dot.
pub fn valid_host(s: &str) -> bool {
    s.contains('.')
        && s.len() <= 253
        && s.split('.').all(|l| {
            (1..=63).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !l.starts_with('-') && !l.ends_with('-')
        })
}

/// `http(s)://host[:port]`, nothing after.
fn valid_upstream(s: &str) -> bool {
    let rest = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://"));
    rest.is_some_and(|r| !r.is_empty() && !r.contains(['/', '?', '#', '@']) && !r.chars().any(|c| c.is_whitespace() || c.is_control()))
}

/// `tls://host:port`: a server outside osada that a node passes TLS
/// connections for a mail instance's host to, as they come (D-U16).
pub fn valid_passthrough(s: &str) -> bool {
    s.strip_prefix("tls://").and_then(|r| r.rsplit_once(':')).is_some_and(|(h, p)| {
        !h.is_empty() && !h.contains(['/', '?', '#', '@']) && !h.chars().any(|c| c.is_whitespace() || c.is_control()) && p.parse::<u16>().is_ok_and(|p| p > 0)
    })
}

/// Lower-case letters, digits and '-', 1 to 40, starting with a letter.
pub fn valid_module_name(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub nodes: BTreeMap<NodeId, Node>,
    pub ous: BTreeMap<OuId, Ou>,
    pub defaults: BTreeMap<Domain, NodeId>,
    /// Where the secrets of OUs that link none are kept.
    pub default_secrets: BTreeSet<NodeId>,
    /// The module instances, by name.
    #[serde(default)]
    pub modules: BTreeMap<String, Module>,
    /// Certificates with their issuer and copies (D-U26), by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub certs: BTreeMap<String, crate::acme::CertSpec>,
    /// The OU new sites go into (D-U30; the root when not set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_ou: Option<OuId>,
    /// What the web nodes do, and which holds the sites' configuration (Round 14).
    #[serde(default, skip_serializing_if = "WebSetup::is_empty")]
    pub web: WebSetup,
}

/// The web module's nodes (Round 14): each web node an edge (it exposes sites), a host of static
/// files, of containers, or more of these; one node is authoritative: it holds the sites'
/// configuration, the single source of truth, and is not exposed. A web node not listed does all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSetup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<NodeId>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub nodes: BTreeMap<NodeId, WebNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebNode {
    #[serde(default)]
    pub edge: bool,
    #[serde(default)]
    pub static_files: bool,
    #[serde(default)]
    pub containers: bool,
}

impl WebSetup {
    pub fn is_empty(&self) -> bool {
        self.authority.is_none() && self.nodes.is_empty()
    }
}

/// Where a placement comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A node's `always` rule.
    Always,
    /// A link on this OU.
    Link(OuId),
    /// The cluster's default for the domain.
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authority {
    pub node: NodeId,
    pub source: Source,
}

/// What [`Placement::check`] finds wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A node's name is not one, or another node's (D-U19).
    NodeName(NodeId, String),
    /// Not exactly one root OU: the number found.
    Roots(usize),
    /// The parent of this OU is not there.
    NoParent(OuId),
    /// This OU is its own ancestor.
    Cycle(OuId),
    UnknownOu(OuId),
    UnknownNode(NodeId, Source),
    /// The ledger is placed by its voters, not by a default or a link.
    LedgerPlaced(Source),
    /// Two nodes are `always` for one domain.
    TwoAlways(Domain, NodeId, NodeId),
    /// Placed on a node whose rule for the domain is `never`.
    Barred(Domain, NodeId, Source),
    /// Placed on a node without the store role.
    NotStore(Domain, NodeId, Source),
    /// A link that changes nothing: an `always` rule or an enforced link
    /// above it decides.
    Overridden { domain: Domain, ou: OuId, by: Source },
    NoVoter,
    /// A voter whose rule for the ledger is `never`.
    VoterBarred(NodeId),
    NoIdp,
    /// Nothing places the domain for this OU.
    Unplaced(Domain, OuId),
    /// An OU name that is empty, has a '/', or is taken by a sibling.
    OuName(OuId, String),
    /// An OU policy that loosens what an OU above sets (D-U30).
    OuPolicy(OuId, String),
    /// A module instance that cannot be so: its name, and why.
    Module(String, String),
    /// A node named to keep secrets without the idp role.
    NotIdp(NodeId, Source),
    /// No node keeps the OU's secrets.
    NoSecretHolder(OuId),
    /// The OU's directory authority, which takes its credential changes,
    /// does not keep its secrets.
    AuthorityHoldsNoSecrets { ou: OuId, node: NodeId },
    /// A secrets link that an enforced one above it overrides.
    SecretsOverridden { ou: OuId, by: OuId },
}

impl Placement {
    /// The node that is the authority for `domain` in `ou`, and why.
    /// The node a person means: its id, else its name (any case).
    pub fn resolve_node(&self, s: &str) -> Option<NodeId> {
        let s = s.trim();
        if let Some((id, _)) = self.nodes.get_key_value(&NodeId(s.to_string())) {
            return Some(id.clone());
        }
        self.nodes.iter().find(|(_, n)| n.name.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(s))).map(|(id, _)| id.clone())
    }

    /// What people see for a node: its name, else its id.
    pub fn node_label(&self, id: &NodeId) -> String {
        self.nodes.get(id).and_then(|n| n.name.clone()).unwrap_or_else(|| id.0.clone())
    }

    /// Home, for an edge that relays to it: the authority for files at the
    /// root, else the first voter.
    pub fn home_node(&self) -> Option<NodeId> {
        self.authority(Domain::Files, crate::cluster::ROOT_OU).ok().map(|a| a.node).or_else(|| self.voters().into_iter().next().cloned())
    }

    pub fn authority(&self, domain: Domain, ou: OuId) -> Result<Authority, Problem> {
        let path = self.path(ou)?;
        if domain == Domain::Ledger {
            return Err(Problem::Unplaced(domain, ou));
        }
        if let Some(node) = self.always(domain) {
            return Ok(Authority { node: node.clone(), source: Source::Always });
        }
        let link = |o: &OuId| self.ous[o].links.get(&domain);
        if let Some(o) = path.iter().find(|o| link(o).is_some_and(|l| l.enforced)) {
            return Ok(Authority { node: link(o).unwrap().node.clone(), source: Source::Link(*o) });
        }
        if let Some(o) = path.iter().rev().find(|o| link(o).is_some()) {
            return Ok(Authority { node: link(o).unwrap().node.clone(), source: Source::Link(*o) });
        }
        match self.defaults.get(&domain) {
            Some(node) => Ok(Authority { node: node.clone(), source: Source::Default }),
            // a module set nowhere follows the IdP master of the OU (Round 14)
            None if domain != Domain::Directory => self.authority(Domain::Directory, ou),
            None => Err(Problem::Unplaced(domain, ou)),
        }
    }

    pub fn root(&self) -> Option<OuId> {
        self.ous.iter().find(|(_, o)| o.parent.is_none()).map(|(id, _)| *id)
    }

    /// The OU's path: `/` for the root, `/EMEA/DE` below it.
    pub fn ou_path(&self, ou: OuId) -> String {
        match self.path(ou) {
            Ok(ids) if ids.len() > 1 => ids[1..].iter().map(|o| format!("/{}", self.ous[o].name)).collect(),
            Ok(_) => "/".into(),
            Err(_) => format!("(OU {})", ou.0),
        }
    }

    /// The OU at a path such as `/EMEA/DE` (`/` is the root).
    pub fn ou_by_path(&self, path: &str) -> Option<OuId> {
        let mut at = self.root()?;
        for name in path.split('/').filter(|s| !s.is_empty()) {
            at = *self.ous.iter().find(|(_, o)| o.parent == Some(at) && o.name == name)?.0;
        }
        Some(at)
    }

    /// The OUs from the root down to `ou`.
    pub fn path(&self, ou: OuId) -> Result<Vec<OuId>, Problem> {
        let mut path: Vec<OuId> = Vec::new();
        let mut id = ou;
        loop {
            let Some(o) = self.ous.get(&id) else {
                return Err(match path.last() {
                    None => Problem::UnknownOu(ou),
                    Some(child) => Problem::NoParent(*child),
                });
            };
            path.push(id);
            if path.len() > self.ous.len() {
                return Err(Problem::Cycle(ou));
            }
            match o.parent {
                Some(p) => id = p,
                None => break,
            }
        }
        path.reverse();
        Ok(path)
    }

    /// The storage limit of accounts in `ou` without their own, and the OU that sets it (D-U30).
    pub fn storage_limit(&self, ou: OuId) -> Option<(u64, OuId)> {
        let path = self.path(ou).ok()?;
        path.iter().rev().find_map(|o| self.ous.get(o).and_then(|x| x.storage_limit).map(|l| (l, *o)))
    }

    /// The longest sign-in (days) of accounts in `ou`, and the OU that sets it (D-U30).
    pub fn session_cap(&self, ou: OuId) -> Option<(u32, OuId)> {
        let path = self.path(ou).ok()?;
        path.iter().filter_map(|o| self.ous.get(o).and_then(|x| x.session_days).map(|d| (d, *o))).min_by_key(|x| x.0)
    }

    pub fn voters(&self) -> Vec<&NodeId> {
        self.with_role(Role::Voter)
    }

    pub fn idps(&self) -> Vec<&NodeId> {
        self.with_role(Role::Idp)
    }

    fn with_role(&self, role: Role) -> Vec<&NodeId> {
        self.nodes.iter().filter(|(_, n)| n.has(role)).map(|(id, _)| id).collect()
    }

    fn always(&self, domain: Domain) -> Option<&NodeId> {
        self.nodes.iter().find(|(_, n)| n.rule(domain) == Rule::Always).map(|(id, _)| id)
    }

    /// The nodes that keep the credentials of `ou`'s accounts and alone
    /// sign their tokens, and why.
    pub fn secret_holders(&self, ou: OuId) -> Result<Secrets, Problem> {
        let path = self.path(ou)?;
        let link = |o: &OuId| self.ous[o].secrets.as_ref();
        let found = path
            .iter()
            .find(|o| link(o).is_some_and(|l| l.enforced))
            .or_else(|| path.iter().rev().find(|o| link(o).is_some()));
        let (nodes, source) = match found {
            Some(o) => (link(o).unwrap().nodes.clone(), Source::Link(*o)),
            None => (self.default_secrets.clone(), Source::Default),
        };
        if nodes.is_empty() {
            return Err(Problem::NoSecretHolder(ou));
        }
        Ok(Secrets { nodes, source })
    }

    /// Whether a token for an account in `ou`, signed with `node`'s key, is
    /// to be believed.
    pub fn may_sign(&self, node: &NodeId, ou: OuId) -> bool {
        self.secret_holders(ou).is_ok_and(|s| s.nodes.contains(node))
    }

    /// What node `at` does with a sign-in for an account in `ou`.
    pub fn sign_in(&self, at: &NodeId, ou: OuId) -> Result<SignIn, Problem> {
        let s = self.secret_holders(ou)?;
        Ok(if s.nodes.contains(at) { SignIn::Here } else { SignIn::Proxy(s.nodes.into_iter().collect()) })
    }

    /// The sign-in rule for accounts in `ou`: tier 0 and a required second
    /// factor, each from the highest OU that turned it on.
    pub fn sign_in_rule(&self, ou: OuId) -> Result<SignInRule, Problem> {
        let path = self.path(ou)?;
        Ok(SignInRule {
            tier0: path.iter().copied().find(|o| self.ous[o].tier0),
            second_factor: path.iter().copied().find(|o| self.ous[o].second_factor),
        })
    }

    /// A copy with `change` made, if the result passes [`Placement::check`].
    pub fn changed(&self, change: impl FnOnce(&mut Placement)) -> Result<Placement, Vec<Problem>> {
        let mut p = self.clone();
        change(&mut p);
        p.check()?;
        Ok(p)
    }

    /// Everything wrong with this setting; nothing may be committed that
    /// does not pass.
    pub fn check(&self) -> Result<(), Vec<Problem>> {
        let mut out = Vec::new();

        // OU policies only tighten downwards (D-U30)
        for (&id, o) in &self.ous {
            let above = |f: &dyn Fn(OuId) -> Option<(u64, OuId)>| o.parent.and_then(|p| f(p));
            if let Some(l) = o.storage_limit {
                if l == 0 {
                    out.push(Problem::OuPolicy(id, "a storage limit of nothing".into()));
                } else if let Some((pl, from)) = above(&|p| self.storage_limit(p)) {
                    if l > pl {
                        out.push(Problem::OuPolicy(id, format!("a storage limit above the one from {}", self.ou_path(from))));
                    }
                }
            }
            if let Some(d) = o.session_days {
                if !(1..=3650).contains(&d) {
                    out.push(Problem::OuPolicy(id, "a sign-in lasts 1 to 3650 days".into()));
                } else if let Some((pd, from)) = above(&|p| self.session_cap(p).map(|(d, o)| (u64::from(d), o))) {
                    if u64::from(d) > pd {
                        out.push(Problem::OuPolicy(id, format!("sign-ins longer than {}'s", self.ou_path(from))));
                    }
                }
            }
        }
        let roots = self.ous.values().filter(|o| o.parent.is_none()).count();
        if roots != 1 {
            out.push(Problem::Roots(roots));
        }
        for &id in self.ous.keys() {
            if let Err(p) = self.path(id) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }

        for (&id, ou) in &self.ous {
            let why = if ou.name.trim().is_empty() || ou.name.len() > 64 {
                Some("a name of 1 to 64 characters")
            } else if ou.name.contains('/') {
                Some("no '/' in a name")
            } else if ou.parent.is_some() && self.ous.iter().any(|(o, x)| *o != id && x.parent == ou.parent && x.name == ou.name) {
                Some("a sibling has that name")
            } else {
                None
            };
            if let Some(why) = why {
                out.push(Problem::OuName(id, why.into()));
            }
        }
        for (&d, node) in &self.defaults {
            self.check_target(d, node, Source::Default, &mut out);
        }
        for (&id, ou) in &self.ous {
            for (&d, link) in &ou.links {
                self.check_target(d, &link.node, Source::Link(id), &mut out);
            }
        }

        for d in Domain::ALL {
            let always: Vec<&NodeId> =
                self.nodes.iter().filter(|(_, n)| n.rule(d) == Rule::Always).map(|(id, _)| id).collect();
            if always.len() > 1 {
                out.push(Problem::TwoAlways(d, always[0].clone(), always[1].clone()));
            }
            if let Some(&id) = always.first() {
                if d != Domain::Ledger && !self.nodes[id].has(Role::Store) {
                    out.push(Problem::NotStore(d, id.clone(), Source::Always));
                }
            }
        }

        for (&id, ou) in &self.ous {
            for &d in ou.links.keys().filter(|&&d| d != Domain::Ledger) {
                if self.always(d).is_some() {
                    out.push(Problem::Overridden { domain: d, ou: id, by: Source::Always });
                } else if let Ok(path) = self.path(id) {
                    let above = &path[..path.len() - 1];
                    if let Some(&o) = above.iter().find(|o| self.ous[o].links.get(&d).is_some_and(|l| l.enforced)) {
                        out.push(Problem::Overridden { domain: d, ou: id, by: Source::Link(o) });
                    }
                }
            }
        }

        self.check_secret_nodes(&self.default_secrets, Source::Default, &mut out);
        for (&id, ou) in &self.ous {
            if let Some(l) = &ou.secrets {
                self.check_secret_nodes(&l.nodes, Source::Link(id), &mut out);
                if let Ok(path) = self.path(id) {
                    let above = &path[..path.len() - 1];
                    if let Some(&o) = above.iter().find(|o| self.ous[o].secrets.as_ref().is_some_and(|l| l.enforced)) {
                        out.push(Problem::SecretsOverridden { ou: id, by: o });
                    }
                }
            }
            match self.secret_holders(id) {
                Err(Problem::NoSecretHolder(o)) => out.push(Problem::NoSecretHolder(o)),
                Err(_) => {} // the tree's problem, already listed
                Ok(s) => {
                    if let Ok(a) = self.authority(Domain::Directory, id) {
                        if !s.nodes.contains(&a.node) {
                            out.push(Problem::AuthorityHoldsNoSecrets { ou: id, node: a.node });
                        }
                    }
                }
            }
        }

        if let Some(o) = self.web_ou.filter(|o| !self.ous.contains_key(o)) {
            out.push(Problem::Module("web".into(), format!("the default OU for sites, {}, is not there", o.0)));
        }
        // the web nodes (Round 14)
        for n in self.web.nodes.keys().chain(self.web.authority.iter()) {
            if !self.nodes.contains_key(n) {
                out.push(Problem::Module("web".into(), format!("no node {n}")));
            }
        }
        for (name, c) in &self.certs {
            if let Some(why) = crate::acme::check(name, c, self) {
                out.push(Problem::Module(format!("cert:{name}"), why));
            }
        }
        for (id, n) in &self.nodes {
            if let Some(a) = n.addrs.iter().find(|a| a.parse::<std::net::IpAddr>().is_err()) {
                out.push(Problem::NodeName(id.clone(), format!("{a:?} is not an address")));
            }
            if let Some(l) = n.link.as_deref().filter(|l| !crate::node::dial::valid_addr(l)) {
                out.push(Problem::NodeName(id.clone(), format!("link {l:?}: host:port")));
            }
            if let Some(d) = n.files_dir.as_deref().filter(|d| !valid_files_dir(d)) {
                out.push(Problem::NodeName(id.clone(), format!("files folder {d:?}: an absolute path of a mounted volume")));
            }
            if let Some(b) = n.log_batch.filter(|b| !(50..=20_000).contains(b)) {
                out.push(Problem::NodeName(id.clone(), format!("log batch {b}: 50 to 20000 lines")));
            }
            let Some(name) = n.name.as_deref() else { continue };
            let why = if !valid_node_label(name) {
                Some("a node's name is letters, digits, '-', '_' and '.', up to 63".to_string())
            } else if self.nodes.iter().any(|(o, x)| o != id && (o.0.eq_ignore_ascii_case(name) || x.name.as_deref().is_some_and(|y| y.eq_ignore_ascii_case(name)))) {
                Some(format!("another node is {name}"))
            } else {
                None
            };
            if let Some(why) = why {
                out.push(Problem::NodeName(id.clone(), why));
            }
        }

        for (name, m) in &self.modules {
            let why = if !valid_module_name(name) {
                Some("a name is lower-case letters, digits and '-', starting with a letter".to_string())
            } else if m.kind == ModuleKind::WEB {
                crate::sites::check(name, m, self)
            } else if m.site.is_some() {
                Some("only a web instance has a site".to_string())
            } else if let ModuleKind::Other(OtherKind::Proxy) = m.kind {
                let host = m.host.as_deref().unwrap_or("");
                if !valid_host(host) {
                    Some("a proxy needs a host name of its own (lower case, with a dot)".to_string())
                } else if !m.upstream.as_deref().is_some_and(valid_upstream) {
                    Some("a proxy needs an upstream: http(s)://host[:port], nothing after".to_string())
                } else if self.modules.iter().any(|(n, o)| n != name && (o.host.as_deref() == Some(host) || o.site.as_ref().is_some_and(|s| s.names.iter().any(|x| x == host)))) {
                    Some(format!("another module has the name {host}"))
                } else if !self.nodes.contains_key(&m.node) {
                    Some(format!("no node {}", m.node))
                } else {
                    None
                }
            } else if (m.host.is_some() || m.upstream.is_some()) && m.kind != ModuleKind::Data(Domain::Mail) {
                Some("only a proxy or a mail instance has a host and an upstream".to_string())
            } else if m.kind == ModuleKind::Data(Domain::Mail) && (m.host.is_some() || m.upstream.is_some()) && !m.host.as_deref().is_some_and(valid_host) {
                Some("a mail instance's host is its mail server's name (lower case, with a dot)".to_string())
            } else if m.kind == ModuleKind::Data(Domain::Mail) && (m.host.is_some() || m.upstream.is_some()) && !m.upstream.as_deref().is_some_and(valid_passthrough) {
                Some("a mail instance's upstream is its mail server's TLS port: tls://host:port".to_string())
            } else if let Some((o, _)) = self.modules.iter().find(|(n, o)| m.kind == ModuleKind::Data(Domain::Mail) && *n != name && o.kind == m.kind && o.node == m.node) {
                // Round 18: osada talks to the node's one Stalwart (127.0.0.1:8080)
                Some(format!("the mail instance {o} runs on {} already: one Stalwart a node", self.node_label(&m.node)))
            } else if let Some(h) = m.host.as_deref().filter(|h| self.modules.iter().any(|(n, o)| n != name && o.host.as_deref() == Some(*h))) {
                Some(format!("another module has the name {h}"))
            } else if let ModuleKind::Data(Domain::Ledger | Domain::Directory) = m.kind {
                Some(format!("{} is not a kind of module", m.kind))
            } else {
                let ModuleKind::Data(kind) = m.kind else { unreachable!("proxies are checked above") };
                match self.nodes.get(&m.node) {
                    None => Some(format!("no node {}", m.node)),
                    Some(n) if !n.has(Role::Store) => Some(format!("{} has no store role", m.node)),
                    Some(n) if n.rule(kind) == Rule::Never => Some(format!("{} is never the authority for {}", m.node, m.kind)),
                    _ => self.always(kind).filter(|a| **a != m.node).map(|a| format!("{a} is always the authority for {}", m.kind)),
                }
            };
            if let Some(why) = why {
                out.push(Problem::Module(name.clone(), why));
            }
        }

        let voters = self.voters();
        if voters.is_empty() {
            out.push(Problem::NoVoter);
        }
        for id in voters {
            if self.nodes[id].rule(Domain::Ledger) == Rule::Never {
                out.push(Problem::VoterBarred(id.clone()));
            }
        }
        if self.idps().is_empty() {
            out.push(Problem::NoIdp);
        }

        if out.is_empty() { Ok(()) } else { Err(out) }
    }

    fn check_secret_nodes(&self, set: &BTreeSet<NodeId>, at: Source, out: &mut Vec<Problem>) {
        for node in set {
            match self.nodes.get(node) {
                None => out.push(Problem::UnknownNode(node.clone(), at)),
                Some(n) if !n.has(Role::Idp) => out.push(Problem::NotIdp(node.clone(), at)),
                Some(_) => {}
            }
        }
    }

    fn check_target(&self, domain: Domain, node: &NodeId, at: Source, out: &mut Vec<Problem>) {
        if domain == Domain::Ledger {
            out.push(Problem::LedgerPlaced(at));
            return;
        }
        match self.nodes.get(node) {
            None => out.push(Problem::UnknownNode(node.clone(), at)),
            Some(n) => {
                if n.rule(domain) == Rule::Never {
                    out.push(Problem::Barred(domain, node.clone(), at));
                }
                if !n.has(Role::Store) {
                    out.push(Problem::NotStore(domain, node.clone(), at));
                }
            }
        }
    }
}

/// A change of authority a new setting sets off: a planned handover (§7.3),
/// or with no `from` the first placement of the domain in that OU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    pub domain: Domain,
    pub ou: OuId,
    pub from: Option<NodeId>,
    pub to: Option<NodeId>,
}

/// For every OU in both settings and every placed domain, where the
/// authority changes from `old` to `new`.
pub fn moves(old: &Placement, new: &Placement) -> Vec<Move> {
    let mut out = Vec::new();
    for &ou in old.ous.keys().filter(|id| new.ous.contains_key(id)) {
        for domain in Domain::ALL.into_iter().filter(|&d| d != Domain::Ledger) {
            let from = old.authority(domain, ou).ok().map(|a| a.node);
            let to = new.authority(domain, ou).ok().map(|a| a.node);
            if from != to {
                out.push(Move { domain, ou, from, to });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signin::{Factor, Missing};
    use Domain::*;
    use Role::*;

    const ORG: OuId = OuId(1);
    const HOME: OuId = OuId(2);
    const FAMILY: OuId = OuId(3);
    const T0: OuId = OuId(4);

    fn n(s: &str) -> NodeId {
        NodeId(s.into())
    }

    /// home at home and the edge node, as decided on 2026-11-15.
    fn this_deployment() -> Placement {
        let mut p = Placement::default();
        p.nodes.insert(n("home"), Node::new([Voter, Idp, Store, Blob], []));
        p.nodes.insert(
            n("edge"),
            Node::new(
                [Idp, Store, Blob, EdgeHttp, EdgeMail],
                [(Mail, Rule::Always), (Links, Rule::Always), (Ledger, Rule::Never), (Files, Rule::Never)],
            ),
        );
        p.ous.insert(ORG, Ou::new("org", None));
        p.ous.insert(HOME, Ou::new("home", Some(ORG)));
        p.ous.insert(FAMILY, Ou::new("family", Some(HOME)));
        for d in [Directory, Files, Wiki] {
            p.defaults.insert(d, n("home"));
        }
        // Ordinary accounts keep their secrets on both nodes; tier 0 on home only.
        p.default_secrets = [n("home"), n("edge")].into();
        p.ous.insert(T0, Ou::new("tier0", Some(ORG)));
        secrets(&mut p, T0, &["home"], false);
        p
    }

    #[test]
    fn ou_policies_tighten_downwards() {
        let mut p = this_deployment();
        assert!(p.check().is_ok(), "{:?}", p.check());
        p.ous.get_mut(&ORG).unwrap().storage_limit = Some(100 << 30);
        p.ous.get_mut(&HOME).unwrap().session_days = Some(30);
        assert!(p.check().is_ok(), "{:?}", p.check());
        // below: the nearest limit, the shortest sign-in
        assert_eq!(p.storage_limit(FAMILY), Some((100 << 30, ORG)));
        assert_eq!(p.session_cap(FAMILY), Some((30, HOME)));
        p.ous.get_mut(&FAMILY).unwrap().storage_limit = Some(10 << 30);
        p.ous.get_mut(&FAMILY).unwrap().session_days = Some(7);
        assert!(p.check().is_ok());
        assert_eq!(p.storage_limit(FAMILY), Some((10 << 30, FAMILY)));
        assert_eq!(p.session_cap(FAMILY), Some((7, FAMILY)));
        // looser than above: refused
        p.ous.get_mut(&FAMILY).unwrap().storage_limit = Some(200 << 30);
        p.ous.get_mut(&FAMILY).unwrap().session_days = Some(60);
        let probs = p.check().unwrap_err();
        assert_eq!(probs.iter().filter(|x| matches!(x, Problem::OuPolicy(FAMILY, _))).count(), 2, "{probs:?}");
        // and a stored placement without them reads as before
        let v: Placement = serde_json::from_value(serde_json::to_value(this_deployment()).unwrap()).unwrap();
        assert_eq!(v.storage_limit(FAMILY), None);
    }

    fn secrets(p: &mut Placement, ou: OuId, nodes: &[&str], enforced: bool) {
        p.ous.get_mut(&ou).unwrap().secrets = Some(SecretLink { nodes: nodes.iter().map(|s| n(s)).collect(), enforced });
    }

    fn auth(p: &Placement, d: Domain, ou: OuId) -> (String, Source) {
        let a = p.authority(d, ou).unwrap();
        (a.node.0, a.source)
    }

    fn link(p: &mut Placement, ou: OuId, d: Domain, node: &str, enforced: bool) {
        p.ous.get_mut(&ou).unwrap().links.insert(d, Link { node: n(node), enforced });
    }

    #[test]
    fn this_deployment_places_as_decided() {
        let p = this_deployment();
        p.check().unwrap();
        for ou in [ORG, HOME, FAMILY] {
            assert_eq!(auth(&p, Mail, ou), ("edge".into(), Source::Always));
            assert_eq!(auth(&p, Links, ou), ("edge".into(), Source::Always));
            for d in [Directory, Files, Wiki] {
                assert_eq!(auth(&p, d, ou), ("home".into(), Source::Default));
            }
        }
        // a module set nowhere follows the IdP master (Round 14)
        assert_eq!(auth(&p, Chat, FAMILY), ("home".into(), Source::Default));
        assert_eq!(p.authority(Ledger, ORG), Err(Problem::Unplaced(Ledger, ORG)));
        assert_eq!(p.authority(Files, OuId(77)), Err(Problem::UnknownOu(OuId(77))));
        assert_eq!(p.voters(), [&n("home")]);
        assert_eq!(p.idps(), [&n("edge"), &n("home")]);
        let edge = &p.nodes[&n("edge")];
        assert_eq!(Node::proposed_rules(&edge.roles), [(Ledger, Rule::Never), (Files, Rule::Never)].into());
        assert!(Node::proposed_rules(&p.nodes[&n("home")].roles).is_empty());
    }

    #[test]
    fn the_edge_never_holds_files_or_the_ledger() {
        let p = this_deployment();
        let e = p.changed(|p| drop(p.defaults.insert(Files, n("edge")))).unwrap_err();
        assert_eq!(e, [Problem::Barred(Files, n("edge"), Source::Default)]);
        let e = p.changed(|p| link(p, HOME, Files, "edge", false)).unwrap_err();
        assert_eq!(e, [Problem::Barred(Files, n("edge"), Source::Link(HOME))]);
        let e = p.changed(|p| { p.nodes.get_mut(&n("edge")).unwrap().roles.insert(Voter); }).unwrap_err();
        assert_eq!(e, [Problem::VoterBarred(n("edge"))]);
    }

    #[test]
    fn ous_switch_authority() {
        let p = this_deployment().changed(|p| link(p, HOME, Wiki, "edge", false)).unwrap();
        assert_eq!(auth(&p, Wiki, FAMILY), ("edge".into(), Source::Link(HOME)));
        assert_eq!(auth(&p, Wiki, HOME), ("edge".into(), Source::Link(HOME)));
        assert_eq!(auth(&p, Wiki, ORG), ("home".into(), Source::Default));
        let p = p.changed(|p| link(p, FAMILY, Wiki, "home", false)).unwrap();
        assert_eq!(auth(&p, Wiki, FAMILY), ("home".into(), Source::Link(FAMILY)));

        // An enforced link at the root decides below it; the links under it
        // are pointed out.
        let mut q = p.clone();
        link(&mut q, ORG, Wiki, "home", true);
        assert_eq!(auth(&q, Wiki, HOME), ("home".into(), Source::Link(ORG)));
        assert_eq!(
            q.check().unwrap_err(),
            [
                Problem::Overridden { domain: Wiki, ou: HOME, by: Source::Link(ORG) },
                Problem::Overridden { domain: Wiki, ou: FAMILY, by: Source::Link(ORG) },
            ]
        );
    }

    #[test]
    fn always_decides_and_is_one_per_domain() {
        let p = this_deployment();
        let e = p.changed(|p| link(p, HOME, Mail, "home", false)).unwrap_err();
        assert_eq!(e, [Problem::Overridden { domain: Mail, ou: HOME, by: Source::Always }]);
        let e = p
            .changed(|p| drop(p.nodes.insert(n("mailvm"), Node::new([Store, EdgeMail], [(Mail, Rule::Always)]))))
            .unwrap_err();
        assert_eq!(e, [Problem::TwoAlways(Mail, n("edge"), n("mailvm"))]);
    }

    #[test]
    fn moves_list_the_handovers() {
        let old = this_deployment();
        let new = old.changed(|p| link(p, HOME, Wiki, "edge", false)).unwrap();
        let m = |ou| Move { domain: Wiki, ou, from: Some(n("home")), to: Some(n("edge")) };
        assert_eq!(moves(&old, &new), [m(HOME), m(FAMILY)]);
        assert!(moves(&old, &old).is_empty());
    }

    #[test]
    fn broken_settings_are_refused() {
        let p = this_deployment();
        let e = p.changed(|p| { p.nodes.get_mut(&n("home")).unwrap().roles.remove(&Voter); }).unwrap_err();
        assert_eq!(e, [Problem::NoVoter]);
        let e = p
            .changed(|p| {
                for node in p.nodes.values_mut() {
                    node.roles.remove(&Idp);
                }
            })
            .unwrap_err();
        assert!(e.contains(&Problem::NoIdp));
        let e = p.changed(|p| drop(p.defaults.insert(Chat, n("nowhere")))).unwrap_err();
        assert_eq!(e, [Problem::UnknownNode(n("nowhere"), Source::Default)]);
        let e = p.changed(|p| link(p, HOME, Ledger, "home", false)).unwrap_err();
        assert_eq!(e, [Problem::LedgerPlaced(Source::Link(HOME))]);
        let e = p
            .changed(|p| {
                p.nodes.insert(n("witness"), Node::new([Voter], []));
                p.defaults.insert(Chat, n("witness"));
            })
            .unwrap_err();
        assert_eq!(e, [Problem::NotStore(Chat, n("witness"), Source::Default)]);
        let e = p.changed(|p| p.ous.get_mut(&HOME).unwrap().parent = Some(FAMILY)).unwrap_err();
        assert_eq!(e, [Problem::Cycle(HOME), Problem::Cycle(FAMILY)]);
        let e = p.changed(|p| drop(p.ous.insert(OuId(9), Ou::new("lost", Some(OuId(8)))))).unwrap_err();
        assert_eq!(e, [Problem::NoParent(OuId(9))]);
        let e = p.changed(|p| drop(p.ous.insert(OuId(9), Ou::new("second root", None)))).unwrap_err();
        assert_eq!(e, [Problem::Roots(2)]);
    }

    #[test]
    fn secrets_stay_where_the_ou_says() {
        let p = this_deployment();
        let s = p.secret_holders(FAMILY).unwrap();
        assert_eq!((s.nodes, s.source), ([n("edge"), n("home")].into(), Source::Default));
        let s = p.secret_holders(T0).unwrap();
        assert_eq!((s.nodes, s.source), ([n("home")].into(), Source::Link(T0)));
        assert_eq!(p.sign_in(&n("edge"), FAMILY), Ok(SignIn::Here));
        assert_eq!(p.sign_in(&n("edge"), T0), Ok(SignIn::Proxy(vec![n("home")])));
        assert_eq!(p.sign_in(&n("home"), T0), Ok(SignIn::Here));
        assert!(p.may_sign(&n("edge"), HOME));
        assert!(!p.may_sign(&n("edge"), T0));
        assert!(p.may_sign(&n("home"), T0));
    }

    #[test]
    fn secrets_settings_are_checked() {
        let p = this_deployment();
        assert_eq!(p.changed(|p| secrets(p, T0, &[], false)).unwrap_err(), [Problem::NoSecretHolder(T0)]);
        let e = p
            .changed(|p| {
                p.nodes.insert(n("witness"), Node::new([Voter], []));
                secrets(p, T0, &["witness"], false);
            })
            .unwrap_err();
        assert_eq!(
            e,
            [Problem::NotIdp(n("witness"), Source::Link(T0)), Problem::AuthorityHoldsNoSecrets { ou: T0, node: n("home") }]
        );
        // Tier 0 on a node of its own: its directory has to go there too.
        let t0_node = |p: &mut Placement| {
            p.nodes.insert(n("t0"), Node::new([Idp, Store], []));
            secrets(p, T0, &["t0"], false);
        };
        let e = p.changed(t0_node).unwrap_err();
        assert_eq!(e, [Problem::AuthorityHoldsNoSecrets { ou: T0, node: n("home") }]);
        let q = p
            .changed(|p| {
                t0_node(p);
                link(p, T0, Directory, "t0", false);
            })
            .unwrap();
        assert_eq!(q.sign_in(&n("edge"), T0), Ok(SignIn::Proxy(vec![n("t0")])));
        assert!(!q.may_sign(&n("home"), T0) && q.may_sign(&n("t0"), T0));
        // An enforced set at the root decides for every OU.
        let e = p.changed(|p| secrets(p, ORG, &["home"], true)).unwrap_err();
        assert_eq!(e, [Problem::SecretsOverridden { ou: T0, by: ORG }]);
    }

    #[test]
    fn tier0_only_when_turned_on_and_only_tightening() {
        let mut p = this_deployment();
        const ADMINS: OuId = OuId(5);
        p.ous.insert(ADMINS, Ou::new("admins", Some(T0)));
        p.check().unwrap();
        // Named tier0, but nobody turned it on: nothing beyond a password.
        for ou in [ORG, HOME, FAMILY, T0, ADMINS] {
            assert_eq!(p.sign_in_rule(ou), Ok(SignInRule::default()));
        }
        p.ous.get_mut(&T0).unwrap().tier0 = true;
        p.ous.get_mut(&HOME).unwrap().second_factor = true;
        assert_eq!(p.sign_in_rule(ADMINS).unwrap(), SignInRule { tier0: Some(T0), second_factor: None });
        assert_eq!(p.sign_in_rule(FAMILY).unwrap(), SignInRule { tier0: None, second_factor: Some(HOME) });
        assert_eq!(p.sign_in_rule(ORG).unwrap(), SignInRule::default());
        // Below, nothing loosens it again: the flags only turn things on.
        assert!(!p.ous[&ADMINS].tier0 && !p.ous[&FAMILY].second_factor);
        assert_eq!(p.sign_in_rule(ADMINS).unwrap().check(&[Factor::Password, Factor::Totp]), Err(Missing::Fido2));
    }

    #[test]
    fn ou_paths_and_names() {
        let mut p = this_deployment();
        assert_eq!(p.ou_path(ORG), "/");
        assert_eq!(p.ou_path(FAMILY), "/home/family");
        assert_eq!(p.ou_by_path("/home/family"), Some(FAMILY));
        assert_eq!(p.ou_by_path("/"), Some(ORG));
        assert_eq!(p.ou_by_path("/home/nobody"), None);
        p.ous.insert(OuId(9), Ou::new("family", Some(HOME)));
        assert_eq!(p.check().unwrap_err(), [Problem::OuName(FAMILY, "a sibling has that name".into()), Problem::OuName(OuId(9), "a sibling has that name".into())]);
        p.ous.get_mut(&OuId(9)).unwrap().name = "a/b".into();
        assert_eq!(p.check().unwrap_err(), [Problem::OuName(OuId(9), "no '/' in a name".into())]);
    }

    #[test]
    fn one_mail_instance_a_node() {
        let mut p = this_deployment();
        p.modules.insert("mail".into(), Module::new(Domain::Mail.into(), "Mail", n("edge")));
        p.check().unwrap();
        p.modules.insert("mail-2".into(), Module::new(Domain::Mail.into(), "Mail 2", n("edge")));
        assert!(p.check().is_err(), "two Stalwarts on one node");
    }

    #[test]
    fn web_sites() {
        use crate::sites::{Container, Content, Front, Route, Site, exposed};
        let mut p = this_deployment();
        p.nodes.insert(n("edge1"), Node::new([Role::EdgeHttp, Role::Worker], []));
        p.nodes.get_mut(&n("edge")).unwrap().roles.insert(Role::EdgeHttp);
        let site = |names: &[&str], content: Content| Site {
            names: names.iter().map(|s| s.to_string()).collect(),
            exposed_on: vec![n("edge1")],
            content,
            routes: vec![],
            front: Front::Cloudflare,
            ou: None,
            ..Default::default()
        };
        let web = |node: &str, s: Site| {
            let mut m = Module::new(ModuleKind::WEB, "", n(node));
            m.site = Some(s);
            m
        };
        let mut pplc = site(&["pplc.example.net"], Content::default());
        pplc.routes = vec![
            Route { prefix: "/api/pi/".into(), upstream: "http://10.0.0.2:8080/".into() },
            Route { prefix: "/api/".into(), upstream: "http://127.0.0.1:9".into() },
            Route { prefix: "/api/scan".into(), upstream: "http://127.0.0.1:8765".into() },
        ];
        p.modules.insert("pplc".into(), web("edge1", pplc));
        let searx = Container {
            image: "docker.io/searxng/searxng:latest".into(),
            port: 8080,
            volumes: vec!["searxng:/etc/searxng".into(), "searxng-cache:/var/cache/searxng".into()],
            env: [("BASE_URL".to_string(), "https://searx.example.org/".to_string())].into(),
            folders: vec![],
            egress: None,
        };
        p.modules.insert("searx".into(), web("edge1", site(&["searx.example.org"], Content::Container(searx.clone()))));
        // exposed on edge1 and the edge, its files at home (the decoupler)
        let mut k = site(&["kosto.example.net", "www.kosto.example.net"], Content::default());
        k.exposed_on.push(n("edge"));
        p.modules.insert("kosto".into(), web("home", k));
        p.check().unwrap();
        let s = &p.modules["pplc"].site.as_ref().unwrap();
        assert_eq!(s.route("/api/pi/temp").map(|r| r.upstream.as_str()), Some("http://10.0.0.2:8080/"));
        assert_eq!(s.route("/api/scan").map(|r| r.upstream.as_str()), Some("http://127.0.0.1:8765"));
        assert_eq!(s.route("/api/x").map(|r| r.upstream.as_str()), Some("http://127.0.0.1:9"));
        assert!(s.route("/index.html").is_none());
        assert_eq!(exposed(&p, &n("edge"), "www.kosto.example.net").map(|x| x.0.as_str()), Some("kosto"));
        assert!(exposed(&p, &n("edge"), "pplc.example.net").is_none());
        // Round 18: at /web/<name>/ without names; one office suite, under /web/; headers checked
        let mut office = site(&[], Content::Static { spa: false, link: None, not_found: None });
        assert!(p.changed(|p| { p.modules.insert("office".into(), web("home", office.clone())); }).is_err(), "no names and not under /web/");
        office.under_web = true;
        office.office = true;
        office.headers.insert("Cross-Origin-Opener-Policy".into(), "same-origin".into());
        let q = p.changed(|p| { p.modules.insert("office".into(), web("home", office.clone())); }).expect("an office suite at /web/office/");
        assert_eq!(crate::sites::office(&q).as_deref(), Some("/web/office/"));
        assert!(crate::sites::under_web(&q, "office").is_some() && crate::sites::under_web(&q, "kosto").is_none());
        assert!(q.changed(|p| { p.modules.insert("office2".into(), web("home", office.clone())); }).is_err(), "a second office suite");
        let mut bad = office.clone();
        bad.office = false;
        bad.headers.insert("Set-Cookie".into(), "a=b".into());
        assert!(p.changed(|p| { p.modules.insert("x".into(), web("home", bad.clone())); }).is_err(), "a cookie from a site's settings");
        assert!(crate::sites::valid_header("X-Thing", "a\nb").is_err());
        // what is refused
        let bad = |f: &dyn Fn(&mut Placement)| {
            let mut q = p.clone();
            f(&mut q);
            q.check().is_err()
        };
        assert!(bad(&|q| drop(q.modules.insert("dup".into(), web("edge1", site(&["pplc.example.net"], Content::default()))))));
        assert!(bad(&|q| drop(q.modules.insert("x".into(), web("edge1", site(&["Not A Host"], Content::default()))))));
        assert!(bad(&|q| q.modules.get_mut("searx").unwrap().node = n("edge"))); // no worker there
        assert!(bad(&|q| q.modules.get_mut("pplc").unwrap().site.as_mut().unwrap().exposed_on = vec![n("pkg1")]));
        assert!(bad(&|q| q.nodes.get_mut(&n("edge1")).unwrap().roles.remove(&Role::EdgeHttp).then_some(()).unwrap_or(())));
        assert!(bad(&|q| q.modules.get_mut("pplc").unwrap().site.as_mut().unwrap().routes[0].upstream = "ftp://x".into()));
        let mut c = searx.clone();
        c.volumes.push("../etc:/x".into());
        assert!(bad(&|q| q.modules.get_mut("searx").unwrap().site.as_mut().unwrap().content = Content::Container(c.clone())));
        assert!(bad(&|q| drop(q.modules.insert("w".into(), Module::new(ModuleKind::WEB, "", n("edge1"))))));
        // round trip, and a placement from before sites reads
        let j = serde_json::to_string(&p).unwrap();
        assert!(j.contains(r#""kind":"web""#) && j.contains(r#""front":"cloudflare""#) && j.contains(r#""kind":"container""#));
        assert_eq!(serde_json::from_str::<Placement>(&j).unwrap(), p);
    }

    #[test]
    fn node_names() {
        let mut p = this_deployment();
        p.nodes.get_mut(&n("home")).unwrap().name = Some("home".into());
        p.nodes.get_mut(&n("edge")).unwrap().name = Some("mail-1".into());
        p.check().unwrap();
        assert_eq!(p.resolve_node("HOME"), Some(n("home")));
        assert_eq!(p.resolve_node("edge"), Some(n("edge")));
        assert_eq!((p.node_label(&n("edge")), p.resolve_node("nobody")), ("mail-1".to_string(), None));
        assert_eq!(p.home_node(), Some(n("home")));
        let id = NodeId::fresh();
        assert!(id.0.len() == 36 && crate::pki::valid_node_name(&id.0) && id != NodeId::fresh());
        // a name taken (by a name or an id), or no name at all
        for bad in ["Home", "home", "two words", "-x", ""] {
            let mut q = p.clone();
            q.nodes.get_mut(&n("edge")).unwrap().name = Some(bad.into());
            let e = q.check().unwrap_err();
            assert!(e.iter().any(|x| matches!(x, Problem::NodeName(i, _) if *i == n("edge"))), "{bad}: {e:?}");
        }
    }

    #[test]
    fn module_instances() {
        let mut p = this_deployment();
        let m = |kind: Domain, node: &str| Module::new(kind.into(), "", n(node));
        p.modules.insert("wiki-main".into(), m(Wiki, "home"));
        p.modules.insert("wiki-lab".into(), m(Wiki, "edge"));
        p.modules.insert("mail-eu".into(), m(Mail, "edge"));
        p.check().unwrap();
        let mut q = p.clone();
        q.modules.insert("files-x".into(), m(Files, "edge"));
        q.modules.insert("mail-home".into(), m(Mail, "home"));
        q.modules.insert("Bad".into(), m(Wiki, "home"));
        q.modules.insert("dir".into(), m(Directory, "home"));
        let e = q.check().unwrap_err();
        let names: Vec<&str> = e.iter().filter_map(|x| if let Problem::Module(n, _) = x { Some(n.as_str()) } else { None }).collect();
        assert_eq!(names, ["Bad", "dir", "files-x", "mail-home"]);
        // A placement from before modules reads.
        let old: Placement = serde_json::from_str(r#"{"nodes":{},"ous":{},"defaults":{},"default_secrets":[]}"#).unwrap();
        assert!(old.modules.is_empty());
    }

    #[test]
    fn proxies() {
        let p = this_deployment();
        let proxy = |host: &str, upstream: &str| {
            let mut m = Module::new(ModuleKind::PROXY, "", n("home"));
            m.host = Some(host.into());
            m.upstream = Some(upstream.into());
            m
        };
        let mut q = p.clone();
        q.modules.insert("apptab".into(), proxy("apptab.example.org", "http://192.168.1.11"));
        q.modules.insert("grafana".into(), proxy("grafana.example.org", "https://10.0.0.5:3000"));
        q.check().unwrap();
        // The kinds read and write as they did, the proxy's too.
        let back: Placement = serde_json::from_str(&serde_json::to_string(&q).unwrap()).unwrap();
        assert_eq!(back, q);
        assert!(serde_json::to_string(&q.modules["apptab"]).unwrap().contains(r#""kind":"proxy""#));
        let wiki: Module = serde_json::from_str(r#"{"kind":"wiki","title":"","node":"home"}"#).unwrap();
        assert_eq!(wiki.kind, ModuleKind::Data(Domain::Wiki));
        let mut bad = p.clone();
        bad.modules.insert("a".into(), proxy("nodot", "http://x"));
        bad.modules.insert("b".into(), proxy("b.example.org", "http://x/path"));
        bad.modules.insert("c".into(), proxy("c.example.org", "ftp://x"));
        bad.modules.insert("d".into(), proxy("apptab.example.org", "http://y"));
        bad.modules.insert("e".into(), proxy("apptab.example.org", "http://z"));
        let mut w = Module::new(Domain::Wiki.into(), "", n("home"));
        w.host = Some("w.example.org".into());
        bad.modules.insert("f".into(), w);
        let e = bad.check().unwrap_err();
        let names: Vec<&str> = e.iter().filter_map(|x| if let Problem::Module(n, _) = x { Some(n.as_str()) } else { None }).collect();
        assert_eq!(names, ["a", "b", "c", "d", "e", "f"]);
    }
}
