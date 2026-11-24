//! The node's runtime: drives the replication (repl.rs) of the cluster log
//! and of the collections this node is a member of over its links (link.rs),
//! keeps the cluster state (cluster.rs) and the directory (directory.rs), and
//! signs people in.
//!
//! - The cluster log's authority is the voter (one, §7.5); every node of
//!   node control is a member and gets it. A node that has just joined does
//!   not know node control yet: it takes the cluster log from the node it
//!   joined through, which it remembers (`n/voter`).
//! - A collection's members are those of its term in the cluster state. The
//!   node keeps a replica of each collection it is a member of.
//! - The voter reconciles once a second: every OU gets its directory
//!   collection (on every store node) and its credentials collection (on the
//!   nodes its secrets policy names, §10.7); members follow node control;
//!   collections node control has moved are handed over.
//! - Planned handover (§7.3), run by the voter: the authority stops, the
//!   target catches up to exactly its chain, the voter commits the next term.
//! - Writes to the directory go to the collection's authority, forwarded
//!   over the link when that is another node, and are checked there.
//! - Sign-in (§10.2) happens on a node that keeps the account's secrets;
//!   others say where to go (proxying comes later). Every sign-in has a
//!   session id; sign-ins, failures, sign-outs and password changes go to the
//!   node's own auth log (§16.3), as its switches say. Sessions live in
//!   memory and are read back from the auth log at start.
//! - A node that no longer keeps an OU's secrets erases its copy.
//!
//! On the wire a replication message is one QUIC stream: the collection's 16
//! bytes, then the message (`repl::Msg::encode`). Everything else between
//! nodes is a call.

use std::{
    collections::{BTreeMap, btree_map::Entry as Slot},
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    audit::{LogKind, Verbosity},
    cluster::{CLUSTER, Cluster, ClusterState, Command},
    directory::{
        self, CREDENTIALS, CredOp, CredentialState, DIRECTORY, DirOp, DirectoryState, Folded, MIN_PASSWORD, User,
        random_hex,
    },
    join,
    kv::{self, Batch, Kv},
    link::{JoinGrant, JoinRequest, Link},
    log::{CollectionId, Draft, Hash, Log},
    pki::Ca,
    placement::{Domain, NodeId, OuId, Role, Source},
    repl::{Durability, Msg, Out, Replica, Term},
    signin::{Factor, Missing},
    totp,
};

mod admin;
mod bulk;
mod chat;
mod expose;
mod idp;
pub mod mailadmin;
pub mod dial;
mod factors;
mod fido2;
pub(crate) mod logs;
mod rules;
pub use fido2::Fido2Answer;
mod tables;
mod rescue;
pub(crate) mod ship;
mod storages;
mod upgrade;
mod websites;
pub use rescue::Rescue;
pub use upgrade::Imported;
pub use bulk::{Action as BulkAction, Job, Selection};
pub use mailadmin::plan as mail_plan;
pub use mailadmin::UPN_FALLBACK;

pub use admin::{Actor, Cond, Cursor, LogPage, LogQuery, perm_name};

const TICK: Duration = Duration::from_millis(100);
const VOTER_KEY: &[u8] = b"n/voter";
const HANDOVER_WAIT: Duration = Duration::from_secs(30);
const RECONCILE_EVERY: u64 = 10;
const PRUNE_EVERY: u64 = 36_000;
/// The mailboxes into Stalwart once a minute (D-U43).
const MAIL_EVERY: u64 = 600;

/// On a node that has joined: which node holds the cluster log, until the
/// cluster log itself says (§6.2).
pub fn remember_voter(kv: &dyn Kv, voter: &NodeId) -> Result<(), String> {
    let mut b = Batch::new();
    b.put(VOTER_KEY, voter.0.as_bytes());
    kv.write(b).map_err(|e| e.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn source_text(s: Source) -> String {
    match s {
        Source::Always => "its node's always rule".into(),
        Source::Link(o) => format!("a link on OU {}", o.0),
        Source::Default => "the cluster default".into(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Directory,
    Credentials,
    /// A chat room (§12.1, D-U42).
    Room,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
enum Call {
    /// Planned handover, step 3: stop taking writes, say where the log ends.
    Stop { collection: CollectionId },
    /// The handover was called off.
    Resume { collection: CollectionId },
    Head { collection: CollectionId },
    /// As the collection's authority: check and write a module's operation.
    Write { collection: CollectionId, module: String, op: Value, actor: String },
    /// As the voter: commit a command.
    Commit { cmd: Command, actor: String },
    /// As the voter: hand a collection over.
    Handover { collection: CollectionId, to: NodeId },
    /// The latest entries of this node's auth log.
    AuthLog { limit: usize },
    /// This node's entries of a log that meet a query.
    QueryLog { q: LogQuery },
    /// A user's sessions on this node.
    SessionsOf { user: String },
    /// About the containers of the sites that run on the node (D-U23).
    Web { op: String, site: Option<String> },
    /// An http-01 token's key authorization, from the certificate's issuer (D-U26).
    AcmeToken { token: String },
    /// A certificate, from its issuer, for a node that gets a copy (D-U26).
    Cert { name: String },
    /// What the node is and has: load, memory, storage, disks (T10).
    NodeInfo,
    /// Make a file system on an empty disk, by the node's root agent (T10).
    DiskFormat { device: String, label: String },
    /// As the logs authority: a node's lines; `reset`: its logs that began anew (D-U31).
    LogBatch { lines: Vec<crate::logstore::Line>, #[serde(default)] reset: Vec<String> },
    /// As the logs authority: a query (an empty text: the datasets' sizes).
    LogQl { text: String, from: i64, to: i64, limit: usize },
    /// As the logs authority: how the rules ran (D-U32).
    RuleState,
    /// As the mail instance's node: an operation on its Stalwart (D-U38).
    Mail { op: Value },
    /// As the mail instance's node: JMAP calls as a person, with a token osada signed (Round 15).
    MailAs { token: String, calls: Value },
    /// As a peer: connect to these TCP ports at these addresses and where the asker comes from (D-U35).
    Probe { addrs: Vec<String>, ports: Vec<u16> },
    /// Ask a peer now to check what this node exposes (D-U35).
    ExposureNow,
    /// A site's files, on the node it runs on (T13).
    WebFiles { site: String, op: String, path: String, #[serde(default)] to: Option<String>, #[serde(default)] data: Option<String>, #[serde(default)] all: bool },
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
enum Answer {
    Head { seq: u64, hash: String },
    Done,
    Written { seq: u64 },
    Lines { lines: Vec<LogLine> },
    Sessions { sessions: Vec<Session> },
    Web { value: Value },
    Refused { why: String },
}

/// One entry of an auth log, as the admin UI shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    pub seq: u64,
    pub time: u64,
    pub node: String,
    pub event: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub sid: String,
    pub user: String,
    pub started: u64,
    pub expires: u64,
    pub node: String,
    pub addr: String,
}

/// A sign-in half done, or a TOTP set-up under way.
#[derive(Clone, Debug)]
struct Ticket {
    user: User,
    expires: u64,
    tries: u32,
    /// A new TOTP secret, to be kept once its first code is right.
    enrol: Option<Vec<u8>>,
    /// A FIDO2 challenge: to sign with a key of the account's, or with
    /// `fido2_enrol` to register the first one.
    challenge: Option<Vec<u8>>,
    fido2_enrol: bool,
}

/// Where a sign-in stands after the password.
#[derive(Debug)]
pub enum SignInStep {
    Done { token: String, session: Session },
    /// A second factor to give: `totp`, or `enrol_totp` (an account without
    /// one in an OU that asks for it: set it up now; `enrol` has the secret
    /// and its QR code).
    More { ticket: String, need: &'static str, enrol: Option<Value> },
}

/// What the auth log says of sessions, read back at start.
#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Replay {
    SignIn {
        sid: String,
        user: String,
        token: String,
        started: u64,
        expires: u64,
        addr: String,
        #[serde(default)]
        totp_step: Option<u64>,
    },
    SignOut { sid: String },
    SessionEnded { ended: String },
    #[serde(other)]
    Other,
}

pub struct Node {
    me: NodeId,
    kv: Arc<dyn Kv>,
    link: Arc<Link>,
    inner: Mutex<Inner>,
    /// Where the first administrator's password goes, on a new cluster.
    first_admin: Option<PathBuf>,
    /// The node's data folder (`set_data`): the sites' and containers' `web/`.
    data: std::sync::OnceLock<PathBuf>,
    /// Where sites linked to a osada folder are (home: osada's files; D-U22, T13).
    links: std::sync::OnceLock<crate::websrv::Links>,
    /// The sites' server for /web/<site>/ (Round 18), made when first asked.
    site_web: std::sync::OnceLock<Arc<crate::websrv::Web>>,
    reconciling: AtomicBool,
    rtts: Mutex<BTreeMap<NodeId, f64>>,
    jobs: Mutex<BTreeMap<String, Job>>,
    /// Chat's live part: what changed, who is typing (node/chat.rs).
    chat: chat::Live,
    /// An idp node's signing key (node/idp.rs, D-U45).
    idp_key: Mutex<Option<Arc<crate::idpkey::Key>>>,
}

struct Inner {
    cluster: Cluster,
    replicas: BTreeMap<CollectionId, Replica>,
    kinds: BTreeMap<CollectionId, (Kind, OuId)>,
    dirs: BTreeMap<OuId, Folded<DirectoryState>>,
    creds: BTreeMap<OuId, Folded<CredentialState>>,
    rooms: BTreeMap<CollectionId, Folded<crate::chat::RoomState>>,
    auth: Arc<Log>,
    admin_log: Arc<Log>,
    /// By the sha256 of their token.
    sessions: BTreeMap<String, Session>,
    /// Sign-ins half done (the password given, a second factor to come),
    /// and TOTP set-ups under way, by their ticket.
    tickets: BTreeMap<String, Ticket>,
    /// The last TOTP step each user used (no code counts twice).
    totp_used: BTreeMap<String, u64>,
    writes: u64,
    ticks: u64,
}

impl Node {
    /// Starts the runtime on `link`; with `ca`, this node also takes joins.
    pub fn start(
        kv: Arc<dyn Kv>,
        link: Arc<Link>,
        ca: Option<Arc<Ca>>,
        first_admin: Option<PathBuf>,
    ) -> Result<Arc<Node>, String> {
        let cluster = Cluster::open(kv.clone()).map_err(|e| e.to_string())?;
        let me = link.me().clone();
        let auth = Arc::new(Log::open(kv.clone(), directory::auth_log_of(&me)).map_err(|e| e.to_string())?);
        let admin_log = Arc::new(Log::open(kv.clone(), directory::admin_log_of(&me)).map_err(|e| e.to_string())?);
        let (sessions, totp_used) = sessions_from(&auth, &me);
        let inner = Inner {
            cluster,
            replicas: BTreeMap::new(),
            kinds: BTreeMap::new(),
            dirs: BTreeMap::new(),
            creds: BTreeMap::new(),
            rooms: BTreeMap::new(),
            auth,
            admin_log,
            sessions,
            tickets: BTreeMap::new(),
            totp_used,
            writes: 0,
            ticks: 0,
        };
        let node = Arc::new(Node {
            me,
            kv,
            link: link.clone(),
            inner: Mutex::new(inner),
            first_admin,
            data: std::sync::OnceLock::new(),
            links: std::sync::OnceLock::new(),
            site_web: std::sync::OnceLock::new(),
            reconciling: AtomicBool::new(false),
            rtts: Mutex::new(BTreeMap::new()),
            jobs: Mutex::new(BTreeMap::new()),
            chat: chat::Live::default(),
            idp_key: Mutex::new(None),
        });
        node.sync(&mut node.lock());

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        link.take_messages(tx);
        let weak = Arc::downgrade(&node);
        link.take_calls(Arc::new(move |from: &NodeId, body: Value| match weak.upgrade() {
            Some(n) => n.on_call(from, body),
            None => Value::Null,
        }));
        if let Some(ca) = ca {
            let weak = Arc::downgrade(&node);
            link.take_joins(Arc::new(move |req: JoinRequest| {
                let n = weak.upgrade().ok_or("the node is stopping")?;
                n.on_join(&ca, req)
            }));
        }
        // the addresses this node keeps dialing (n/dial/), and with the CA, invites there (dial.rs)
        node.spawn_dials();
        // and to every node node control knows a link address for
        node.spawn_mesh();
        // what it exposes to the internet, checked by a peer every hour (D-U35)
        node.spawn_exposure();
        // an idp node's own signing key, published (D-U45)
        node.spawn_idp_key();
        // the authoritative web node's copies of the sites (Round 16)
        node.spawn_web_mirror();
        let weak = Arc::downgrade(&node);
        tokio::spawn(async move {
            while let Some((from, bytes)) = rx.recv().await {
                let Some(n) = weak.upgrade() else { break };
                n.on_message(&from, &bytes);
            }
        });
        let weak = Arc::downgrade(&node);
        tokio::spawn(async move {
            let mut t = tokio::time::interval(TICK);
            loop {
                t.tick().await;
                let Some(n) = weak.upgrade() else { break };
                n.tick();
            }
        });
        let weak = Arc::downgrade(&node);
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(5));
            loop {
                t.tick().await;
                let Some(n) = weak.upgrade() else { break };
                let mut rtts = BTreeMap::new();
                for p in n.link.peers() {
                    if let Ok(d) = n.link.ping(&p).await {
                        rtts.insert(p, d.as_secs_f64() * 1000.0);
                    }
                }
                *n.rtts.lock().unwrap_or_else(|e| e.into_inner()) = rtts;
            }
        });
        Ok(node)
    }

    pub fn me(&self) -> &NodeId {
        &self.me
    }

    /// The node's link (a live backup to another node goes over it, Round 14).
    pub fn link_handle(&self) -> Arc<Link> {
        self.link.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn remembered_voter(&self) -> Option<NodeId> {
        let v = self.kv.get(VOTER_KEY).ok()??;
        String::from_utf8(v).ok().map(NodeId)
    }

    fn voter_in(&self, state: &ClusterState) -> Option<NodeId> {
        state.placement.voters().first().map(|v| (*v).clone()).or_else(|| self.remembered_voter())
    }

    /// Brings the replicas and the directory in line with the cluster state.
    fn sync(&self, inner: &mut Inner) {
        let state = inner.cluster.state().clone();
        if let Some(voter) = self.voter_in(&state) {
            let mut members: Vec<NodeId> = state.placement.nodes.keys().cloned().collect();
            for n in [&self.me, &voter] {
                if !members.contains(n) {
                    members.push(n.clone());
                }
            }
            members.sort();
            let log = inner.cluster.shared_log();
            let r = inner.replicas.entry(CLUSTER).or_insert_with(|| Replica::new(self.me.clone(), log, Durability::Async));
            self.give_term(CLUSTER, r, Term { epoch: 1, authority: voter, members });
        }
        inner.kinds.clear();
        for &ou in state.placement.ous.keys() {
            inner.kinds.insert(directory::directory_of(ou), (Kind::Directory, ou));
            inner.kinds.insert(directory::credentials_of(ou), (Kind::Credentials, ou));
        }
        for (id, col) in &state.collections {
            if col.domain == Domain::Chat {
                inner.kinds.insert(*id, (Kind::Room, col.ou));
            }
        }
        for (id, col) in &state.collections {
            if !col.term.members.contains(&self.me) {
                inner.replicas.remove(id);
                if let Some(&(kind, ou)) = inner.kinds.get(id) {
                    match kind {
                        Kind::Directory => drop(inner.dirs.remove(&ou)),
                        Kind::Credentials => {
                            inner.creds.remove(&ou);
                            self.erase(*id, ou);
                        }
                        Kind::Room => drop(inner.rooms.remove(id)),
                    }
                }
                continue;
            }
            let r = match inner.replicas.entry(*id) {
                Slot::Occupied(o) => o.into_mut(),
                Slot::Vacant(v) => match Log::open(self.kv.clone(), *id) {
                    Ok(log) => v.insert(Replica::new(self.me.clone(), Arc::new(log), Durability::Async)),
                    Err(e) => {
                        eprintln!("{}: collection {id}: {e}", self.me);
                        continue;
                    }
                },
            };
            self.give_term(*id, r, col.term.clone());
        }
        let ids: Vec<CollectionId> = inner.replicas.keys().copied().collect();
        for id in ids {
            self.refresh(inner, id);
        }
    }

    /// Erases this node's copy of a collection it no longer keeps: an OU's
    /// secrets no longer placed here.
    fn erase(&self, id: CollectionId, ou: OuId) {
        let prefix = id.key_prefix();
        match kv::delete_prefix(&*self.kv, &prefix) {
            Ok(0) => {}
            Ok(n) => println!("{}: no longer keeps the secrets of OU {}; erased them ({n} keys)", self.me, ou.0),
            Err(e) => eprintln!("{}: erasing the secrets of OU {}: {e}", self.me, ou.0),
        }
    }

    fn give_term(&self, id: CollectionId, r: &mut Replica, term: Term) {
        if r.term().is_some_and(|t| t.epoch == term.epoch) {
            r.set_members(term.members);
            return;
        }
        match r.on_term(term) {
            Ok(out) => self.emit(id, out),
            Err(e) => eprintln!("{}: collection {id}: {e}", self.me),
        }
    }

    /// Folds what is new in a directory or credentials collection.
    fn refresh(&self, inner: &mut Inner, id: CollectionId) {
        let Some(&(kind, ou)) = inner.kinds.get(&id) else { return };
        let Some(r) = inner.replicas.get(&id) else { return };
        let log = r.log();
        let res = match kind {
            Kind::Directory => inner.dirs.entry(ou).or_insert_with(|| Folded::new(id)).catch_up(log, |s, e| {
                s.apply(ou, &serde_json::from_slice(&e.payload).map_err(|e| e.to_string())?)
            }),
            Kind::Credentials => inner
                .creds
                .entry(ou)
                .or_insert_with(|| Folded::new(id))
                .catch_up(log, |s, e| s.apply(&serde_json::from_slice(&e.payload).map_err(|e| e.to_string())?)),
            Kind::Room => inner.rooms.entry(id).or_insert_with(|| Folded::new(id)).catch_up(log, |s, e| s.apply(e)),
        };
        match res {
            Ok(true) if kind == Kind::Room => self.chat.changed(),
            Ok(_) => {}
            Err(e) => eprintln!("{}: {e}", self.me),
        }
    }

    fn emit(&self, id: CollectionId, out: Out) {
        for (to, msg) in out.send {
            let mut bytes = id.0.to_vec();
            bytes.extend_from_slice(&msg.encode());
            self.link.send(&to, bytes);
        }
    }

    fn on_message(&self, from: &NodeId, bytes: &[u8]) {
        if bytes.len() < 16 {
            return;
        }
        let id = CollectionId(bytes[..16].try_into().expect("16 bytes"));
        let msg = match Msg::decode(&bytes[16..]) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{}: a message from {from}: {e}", self.me);
                return;
            }
        };
        let mut guard = self.lock();
        let inner = &mut *guard;
        let Some(r) = inner.replicas.get_mut(&id) else { return };
        match r.on_msg(from, msg) {
            Ok(out) => self.emit(id, out),
            Err(e) => {
                eprintln!("{}: collection {id}, from {from}: {e}", self.me);
                return;
            }
        }
        if id == CLUSTER {
            let was = inner.cluster.applied();
            match inner.cluster.catch_up() {
                Ok(()) if inner.cluster.applied() != was => self.sync(inner),
                Ok(()) => {}
                Err(e) => eprintln!("{}: cluster log: {e}", self.me),
            }
        } else {
            self.refresh(inner, id);
        }
    }

    fn tick(self: &Arc<Self>) {
        let (reconcile, prune) = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            inner.ticks += 1;
            for (id, r) in inner.replicas.iter_mut() {
                match r.tick() {
                    Ok(out) => self.emit(*id, out),
                    Err(e) => eprintln!("{}: collection {id}: {e}", self.me),
                }
            }
            let voter = Self::holds_cluster_log(inner);
            if voter && inner.ticks % MAIL_EVERY == MAIL_EVERY / 2 {
                // the mailboxes into Stalwart (D-U43)
                let node = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = node.mail_reconcile().await {
                        eprintln!("{}: mailboxes: {e}", node.me);
                    }
                });
            }
            (inner.ticks % RECONCILE_EVERY == 0 && voter, inner.ticks % PRUNE_EVERY == 1)
        };
        if prune {
            self.prune_auth_log();
        }
        if reconcile && !self.reconciling.swap(true, Ordering::AcqRel) {
            let node = self.clone();
            tokio::spawn(async move {
                node.reconcile().await;
                node.reconciling.store(false, Ordering::Release);
            });
        }
    }

    fn prune_auth_log(&self) {
        let inner = self.lock();
        let keep = inner.cluster.state().settings.switches(LogKind::Auth).retention;
        let cutoff = now_ms().saturating_sub(keep.as_millis() as u64);
        if let Err(e) = inner.auth.prune_older_than(cutoff) {
            eprintln!("{}: pruning the auth log: {e}", self.me);
        }
    }

    fn holds_cluster_log(inner: &Inner) -> bool {
        inner.replicas.get(&CLUSTER).is_some_and(|r| r.is_authority())
    }

    /// What reconciling would commit now, and which handovers it would run.
    fn plan(state: &ClusterState) -> (Vec<Command>, Vec<(CollectionId, NodeId)>) {
        let p = &state.placement;
        let mut cmds = Vec::new();
        let mut moves = Vec::new();
        let stores: Vec<NodeId> = p.nodes.iter().filter(|(_, n)| n.has(Role::Store)).map(|(id, _)| id.clone()).collect();
        for &ou in p.ous.keys() {
            let Ok(auth) = p.authority(Domain::Directory, ou) else { continue };
            let holders: Vec<NodeId> = p
                .secret_holders(ou)
                .map(|s| s.nodes.into_iter().filter(|n| stores.contains(n)).collect())
                .unwrap_or_default();
            for (id, mut want) in [(directory::directory_of(ou), stores.clone()), (directory::credentials_of(ou), holders)] {
                let Some(c) = state.collections.get(&id) else {
                    cmds.push(Command::Create { id, domain: Domain::Directory, ou });
                    continue;
                };
                want.push(c.term.authority.clone());
                want.push(auth.node.clone());
                want.sort();
                want.dedup();
                let mut have = c.term.members.clone();
                have.sort();
                if want != have {
                    let term = Term { epoch: c.term.epoch + 1, authority: c.term.authority.clone(), members: want };
                    cmds.push(Command::Term { id, term });
                }
            }
        }
        // a chat room is kept by every store node, as the directory is (D-U42)
        for (id, c) in &state.collections {
            if c.domain != Domain::Chat {
                continue;
            }
            let mut want = stores.clone();
            want.push(c.term.authority.clone());
            want.sort();
            want.dedup();
            let mut have = c.term.members.clone();
            have.sort();
            if want != have {
                cmds.push(Command::Term { id: *id, term: Term { epoch: c.term.epoch + 1, authority: c.term.authority.clone(), members: want } });
            }
        }
        for (id, at, to) in state.misplaced() {
            let c = &state.collections[&id];
            let pending = cmds.iter().any(|cmd| matches!(cmd, Command::Term { id: i, .. } if *i == id));
            if pending {
                continue;
            }
            if c.term.members.contains(&to) {
                moves.push((id, to));
            } else {
                let mut members = c.term.members.clone();
                members.push(to);
                cmds.push(Command::Term { id, term: Term { epoch: c.term.epoch + 1, authority: at, members } });
            }
        }
        (cmds, moves)
    }

    async fn reconcile(self: &Arc<Self>) {
        let (cmds, moves) = Self::plan(self.lock().cluster.state());
        for cmd in cmds {
            if let Err(e) = self.commit("reconcile", cmd) {
                eprintln!("{}: reconciling: {e}", self.me);
            }
        }
        if let Some((id, to)) = moves.into_iter().next() {
            match self.handover(id, to.clone()).await {
                Ok(()) => println!("{}: handed {id} over to {to}", self.me),
                Err(e) => eprintln!("{}: handing {id} over to {to}: {e}", self.me),
            }
        }
        self.ensure_first_admin().await;
    }

    /// On a new cluster: an administrator, `admin`, with a random password
    /// written to the `first_admin` file (0600).
    async fn ensure_first_admin(self: &Arc<Self>) {
        let Some(file) = self.first_admin.clone() else { return };
        if file.exists() {
            return;
        }
        let root = {
            let inner = self.lock();
            let p = &inner.cluster.state().placement;
            let Some(root) = p.ous.iter().find(|(_, o)| o.parent.is_none()).map(|(id, _)| *id) else { return };
            let ready = [directory::directory_of(root), directory::credentials_of(root)]
                .iter()
                .all(|id| inner.replicas.get(id).is_some_and(|r| r.is_authority()));
            if !ready || inner.dirs.values().any(|d| !d.state.users.is_empty()) {
                return;
            }
            root
        };
        let password = random_hex(10);
        let actor = Actor::node(&self.me);
        let made = match self.create_user_unchecked(&actor, "admin", "Administrator", root, &password).await {
            Ok(u) => self.grant_unchecked(&actor, root, "admin", crate::rbac::Subject::User { id: u.id }).await.map(|_| ()),
            Err(e) => Err(e),
        };
        match made {
            Ok(()) => {
                use std::{io::Write, os::unix::fs::OpenOptionsExt};
                let written = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&file)
                    .and_then(|mut f| f.write_all(format!("user: admin\npassword: {password}\n").as_bytes()));
                match written {
                    Ok(()) => println!("{}: first administrator: admin, password in {}", self.me, file.display()),
                    Err(e) => eprintln!("{}: {}: {e}", self.me, file.display()),
                }
            }
            Err(e) => eprintln!("{}: first administrator: {e}", self.me),
        }
    }

    /// As the voter: commits `cmd` to the cluster log, and acts on it.
    pub fn commit(&self, actor: &str, cmd: Command) -> Result<(), String> {
        let mut guard = self.lock();
        let inner = &mut *guard;
        if !Self::holds_cluster_log(inner) {
            return Err("this node does not hold the cluster log".into());
        }
        inner.cluster.commit(1, now_ms(), actor, cmd).map_err(|e| e.to_string())?;
        self.after_commit(inner);
        Ok(())
    }

    /// Commits `cmd`, here when this node is the voter, else through it.
    pub async fn commit_anywhere(self: &Arc<Self>, actor: &str, cmd: Command) -> Result<(), String> {
        if Self::holds_cluster_log(&self.lock()) {
            return self.commit(actor, cmd);
        }
        let voter = self.voter_in(&self.cluster_state()).ok_or("no voter known")?;
        match self.ask(&voter, Call::Commit { cmd, actor: actor.into() }).await? {
            Answer::Done => Ok(()),
            Answer::Refused { why } => Err(why),
            other => Err(format!("{other:?}")),
        }
    }

    fn after_commit(&self, inner: &mut Inner) {
        self.sync(inner);
        if let Some(r) = inner.replicas.get_mut(&CLUSTER) {
            if let Ok(out) = r.tick() {
                self.emit(CLUSTER, out);
            }
        }
    }

    fn on_join(&self, ca: &Ca, req: JoinRequest) -> Result<JoinGrant, String> {
        let mut guard = self.lock();
        let inner = &mut *guard;
        if !Self::holds_cluster_log(inner) {
            return Err("this node does not hold the cluster log".into());
        }
        let grant = join::grant(&*self.kv, ca, &mut inner.cluster, &req, now_ms());
        match &grant {
            Ok(_) => {
                println!("{}: {} joined the cluster", self.me, req.node);
                self.after_commit(inner);
            }
            Err(e) => println!("{}: join of {:?} refused: {e}", self.me, req.node),
        }
        grant
    }

    fn write_locked(&self, inner: &mut Inner, collection: CollectionId, draft: Draft) -> Result<u64, String> {
        inner.writes += 1;
        let r = inner.replicas.get_mut(&collection).ok_or("not a member of that collection")?;
        let out = r.write(inner.writes, draft, now_ms()).map_err(|e| e.to_string())?;
        let seq = r.log().head().seq;
        self.emit(collection, out);
        self.refresh(inner, collection);
        Ok(seq)
    }

    /// As the authority of `collection`: writes `draft`, returns its seq.
    pub fn write(&self, collection: CollectionId, draft: Draft) -> Result<u64, String> {
        self.write_locked(&mut self.lock(), collection, draft)
    }

    /// As the authority: checks a directory or credentials operation against
    /// the state, then writes it.
    fn write_op(&self, collection: CollectionId, module: &str, op: Value, actor: &str) -> Result<u64, String> {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let &(kind, ou) = inner.kinds.get(&collection).ok_or("not a collection of the directory")?;
        let (name, payload) = match kind {
            Kind::Directory if module == DIRECTORY => {
                let op: DirOp = serde_json::from_value(op).map_err(|e| e.to_string())?;
                if let DirOp::CreateUser { user } = &op {
                    if inner.dirs.values().any(|d| d.state.by_name(&user.name).is_some()) {
                        return Err(format!("there is a user {} already", user.name));
                    }
                }
                // an address is one mailbox's (D-U43)
                // and so is each alias and catch-all (D-U44)
                if let DirOp::SetMailbox { id, mailbox: Some(m) } = &op {
                    let mine = m.claims();
                    for u in inner.dirs.values().flat_map(|d| d.state.users.values()).filter(|u| u.id != *id) {
                        let Some(x) = &u.mailbox else { continue };
                        if let Some(a) = x.claims().into_iter().find(|c| mine.contains(c)) {
                            return Err(if a == x.address { format!("{a} is {}'s mailbox already", u.name) } else { format!("{a} is {}'s already", u.name) });
                        }
                    }
                }
                // A check, not a copy: an OU may hold a million users.
                match inner.dirs.get(&ou) {
                    Some(f) => f.state.check(ou, &op)?,
                    None => DirectoryState::default().check(ou, &op)?,
                }
                let modules = &inner.cluster.state().placement.modules;
                if let Some(m) = op.modules().into_iter().find(|m| !modules.contains_key(*m)) {
                    return Err(format!("no module {m}"));
                }
                (op.name(), serde_json::to_vec(&op).map_err(|e| e.to_string())?)
            }
            Kind::Credentials if module == CREDENTIALS => {
                let op: CredOp = serde_json::from_value(op).map_err(|e| e.to_string())?;
                CredentialState::default().apply(&op)?;
                (op.name(), serde_json::to_vec(&op).map_err(|e| e.to_string())?)
            }
            Kind::Room if module == crate::chat::CHAT => {
                let op: crate::chat::ChatOp = serde_json::from_value(op).map_err(|e| e.to_string())?;
                let user = actor.strip_prefix("user:").ok_or("chat is written by people")?;
                let empty = crate::chat::RoomState::default();
                let room = inner.rooms.get(&collection).map_or(&empty, |f| &f.state);
                room.check(collection, user, &op)?;
                // who is put in a room is someone the directory has, and enabled
                let named: Vec<&String> = match &op {
                    crate::chat::ChatOp::Create { members, .. } => members.iter().collect(),
                    crate::chat::ChatOp::Add { users } => users.iter().collect(),
                    _ => Vec::new(),
                };
                for u in named {
                    if !inner.dirs.values().any(|d| d.state.users.get(u).is_some_and(|u| !u.disabled)) {
                        return Err(format!("no user {u}"));
                    }
                }
                (op.name(), serde_json::to_vec(&op).map_err(|e| e.to_string())?)
            }
            _ => return Err(format!("{module} does not write to that collection")),
        };
        let draft = Draft { actor: actor.into(), module: module.into(), op: name.into(), payload };
        self.write_locked(inner, collection, draft)
    }

    /// Writes an operation at the collection's authority, here or over the
    /// link.
    async fn submit(self: &Arc<Self>, collection: CollectionId, module: &str, op: Value, actor: &str) -> Result<u64, String> {
        let authority = {
            let inner = self.lock();
            let col = inner.cluster.state().collections.get(&collection).ok_or("that collection does not exist yet")?;
            col.term.authority.clone()
        };
        if authority == self.me {
            return self.write_op(collection, module, op, actor);
        }
        let call = Call::Write { collection, module: module.into(), op, actor: actor.into() };
        match self.ask(&authority, call).await? {
            Answer::Written { seq } => Ok(seq),
            Answer::Refused { why } => Err(why),
            other => Err(format!("{other:?}")),
        }
    }

    /// Writes `ev` (it names its `event`) to this node's auth or admin log,
    /// when that log's verbosity takes `level`.
    fn log_event(&self, inner: &mut Inner, kind: LogKind, level: Verbosity, ev: Value) {
        if !inner.cluster.state().settings.switches(kind).logs(level) {
            return;
        }
        let (log, module) = if kind == LogKind::Admin { (inner.admin_log.clone(), "admin") } else { (inner.auth.clone(), "auth") };
        let op = ev.get("event").and_then(|e| e.as_str()).unwrap_or("event").to_string();
        let draft = Draft { actor: format!("node:{}", self.me), module: module.into(), op, payload: serde_json::to_vec(&ev).expect("json") };
        if let Err(e) = log.append(1, now_ms(), draft) {
            eprintln!("{}: {module} log: {e}", self.me);
        }
    }

    /// The latest `limit` entries of this node's auth log, newest first.
    pub fn auth_lines(&self, limit: usize) -> Vec<LogLine> {
        let inner = self.lock();
        let (head, start) = (inner.auth.head().seq, inner.auth.start().seq);
        let after = head.saturating_sub(limit as u64).max(start);
        let entries = inner.auth.read(after, limit).unwrap_or_default();
        entries
            .into_iter()
            .rev()
            .map(|e| {
                let mut event: Value = serde_json::from_slice(&e.payload).unwrap_or(Value::Null);
                if let Some(o) = event.as_object_mut() {
                    o.remove("token");
                }
                LogLine { seq: e.seq, time: e.time, node: self.me.0.clone(), event }
            })
            .collect()
    }

    /// The auth log of `node`, this one or another over the link.
    pub async fn auth_lines_of(self: &Arc<Self>, node: &NodeId, limit: usize) -> Result<Vec<LogLine>, String> {
        if node == &self.me {
            return Ok(self.auth_lines(limit));
        }
        match self.ask(node, Call::AuthLog { limit }).await? {
            Answer::Lines { lines } => Ok(lines),
            Answer::Refused { why } => Err(why),
            other => Err(format!("{other:?}")),
        }
    }

    /// Signs `name` in with a password alone, at this node: for callers that
    /// take no second factor (the tests). The pages use `sign_in_step`.
    pub async fn sign_in(&self, name: &str, password: &str, addr: &str) -> Result<(String, Session), String> {
        match self.sign_in_step(name, password, addr).await? {
            SignInStep::Done { token, session } => Ok((token, session)),
            SignInStep::More { need, .. } => Err(format!("a second factor is needed ({need})")),
        }
    }

    /// The first step of a sign-in at this node: the password; then a
    /// session, or what is needed next. The error is what the person is told.
    pub async fn sign_in_step(&self, name: &str, password: &str, addr: &str) -> Result<SignInStep, String> {
        const WRONG: &str = "wrong user name or password";
        // the account's name, or its UPN (Round 22)
        let login = self.login_name(name);
        let name = login.as_str();
        let found = {
            let inner = self.lock();
            let p = &inner.cluster.state().placement;
            inner.dirs.values().find_map(|d| d.state.by_name(name).cloned()).map(|u| {
                let holders: Vec<String> = p.secret_holders(u.ou).map(|s| s.nodes.into_iter().map(|n| n.0).collect()).unwrap_or_default();
                let creds = inner.creds.get(&u.ou);
                let hash = creds.and_then(|c| c.state.passwords.get(&u.id).cloned());
                let has_totp = creds.is_some_and(|c| c.state.totp.contains_key(&u.id));
                let keys: Vec<String> = creds.and_then(|c| c.state.fido2.get(&u.id)).map(|l| l.iter().map(|k| k.id.clone()).collect()).unwrap_or_default();
                let rule = p.sign_in_rule(u.ou).unwrap_or_default();
                (u, holders, hash, has_totp, keys, rule)
            })
        };
        let fail = |why: &str, told: &str| -> String {
            let ev = json!({ "event": "sign_in_failed", "name": name, "why": why, "addr": addr });
            self.log_event(&mut self.lock(), LogKind::Auth, Verbosity::Minimal, ev);
            told.to_string()
        };
        let Some((user, holders, hash, has_totp, keys, rule)) = found else {
            let pw = password.to_string();
            let _ = tokio::task::spawn_blocking(move || directory::verify_password(DUMMY_HASH, &pw)).await;
            return Err(fail("no such user", WRONG));
        };
        if !holders.contains(&self.me.0) {
            let told = format!(
                "this node ({}) does not keep the secrets of your account; sign in at {} (signing in through another node comes later)",
                self.me,
                holders.join(" or ")
            );
            return Err(fail("its secrets are not kept on this node", &told));
        }
        let Some(hash) = hash else { return Err(fail("no password set", WRONG)) };
        let pw = password.to_string();
        let ok = tokio::task::spawn_blocking(move || directory::verify_password(&hash, &pw)).await.unwrap_or(false);
        if !ok {
            return Err(fail("wrong password", WRONG));
        }
        if user.disabled {
            return Err(fail("account disabled", "this account is disabled"));
        }
        let tier0 = match rule.check(&[Factor::Password]) {
            Err(Missing::Fido2) => true,
            Err(Missing::FirstFactor) => return Err(fail("no first factor", WRONG)),
            Ok(()) if !has_totp && keys.is_empty() => {
                let (token, session) = self.open_session(&user, addr, &["pwd"], None);
                return Ok(SignInStep::Done { token, session });
            }
            _ => false,
        };
        // FIDO2: always in tier 0 (the first key registered now), and for an
        // account with keys; TOTP otherwise.
        if tier0 || !keys.is_empty() {
            let ticket = random_hex(24);
            let challenge = hex::decode(random_hex(32)).expect("hex");
            let enrol = keys.is_empty();
            let show = json!({
                "challenge": crate::webauthn::b64(&challenge), "allow": keys, "user": { "id": user.id, "name": user.name },
                "or_totp": !tier0 && has_totp,
            });
            let t = Ticket { user, expires: now_ms() + 300_000, tries: 0, enrol: None, challenge: Some(challenge), fido2_enrol: enrol };
            self.lock().tickets.insert(ticket.clone(), t);
            return Ok(SignInStep::More { ticket, need: if enrol { "enrol_fido2" } else { "fido2" }, enrol: Some(show) });
        }
        // A second factor: the account's TOTP, or, for one that has none, set
        // one up now (its OU asks for a second factor).
        let ticket = random_hex(24);
        let (need, enrol, secret) = if has_totp {
            ("totp", None, None)
        } else {
            let s = hex::decode(random_hex(totp::SECRET_LEN)).expect("hex");
            let uri = totp::uri("osada", &user.name, &s);
            let show = json!({ "secret": totp::base32(&s), "uri": uri, "qr": totp::qr_svg(&uri) });
            ("enrol_totp", Some(show), Some(s))
        };
        self.lock().tickets.insert(ticket.clone(), Ticket { user, expires: now_ms() + 300_000, tries: 0, enrol: secret, challenge: None, fido2_enrol: false });
        Ok(SignInStep::More { ticket, need, enrol })
    }

    /// The second step: the TOTP code for a ticket. For an account that set
    /// its TOTP up in this sign-in, the right first code keeps it.
    pub async fn sign_in_totp(self: &Arc<Self>, ticket: &str, typed: &str, addr: &str) -> Result<(String, Session), String> {
        let (t, secret, last) = {
            let mut inner = self.lock();
            let now = now_ms();
            inner.tickets.retain(|_, t| t.expires > now);
            let t = inner.tickets.get(ticket).cloned().ok_or("this sign-in ran out; start again")?;
            // TOTP does not stand in for FIDO2 where tier 0 asks for it
            if let Err(m) = inner.cluster.state().placement.sign_in_rule(t.user.ou).unwrap_or_default().check(&[Factor::Password, Factor::Totp]) {
                inner.tickets.remove(ticket);
                return Err(format!("this account signs in with FIDO2 ({m:?})"));
            }
            let secret = match &t.enrol {
                Some(s) => s.clone(),
                None => inner
                    .creds
                    .get(&t.user.ou)
                    .and_then(|c| c.state.totp.get(&t.user.id))
                    .and_then(|h| hex::decode(h).ok())
                    .ok_or("no TOTP for this account here")?,
            };
            let last = inner.totp_used.get(&t.user.id).copied();
            (t, secret, last)
        };
        let Some(step) = totp::check(&secret, typed, now_ms() / 1000, last) else {
            let mut inner = self.lock();
            let gone = match inner.tickets.get_mut(ticket) {
                Some(x) => {
                    x.tries += 1;
                    x.tries >= 5
                }
                None => true,
            };
            if gone {
                inner.tickets.remove(ticket);
            }
            let ev = json!({ "event": "sign_in_failed", "name": t.user.name, "user": t.user.id, "why": "wrong TOTP code", "addr": addr });
            self.log_event(&mut inner, LogKind::Auth, Verbosity::Minimal, ev);
            return Err(if gone { "wrong code; start again" } else { "wrong code" }.into());
        };
        {
            let mut inner = self.lock();
            inner.tickets.remove(ticket);
            inner.totp_used.insert(t.user.id.clone(), step);
        }
        if let Some(s) = &t.enrol {
            self.store_totp(&t.user, Some(s), &format!("user:{}", t.user.name)).await?;
        }
        let (token, session) = self.open_session(&t.user, addr, &["pwd", "otp"], Some(step));
        if t.enrol.is_some() {
            let actor = Actor { user: t.user.clone(), sid: session.sid.clone(), addr: addr.into() };
            self.log_auth_as(&actor, "totp_added", Verbosity::Minimal, json!({ "user": t.user.id, "name": t.user.name, "why": "set up at sign-in" }));
        }
        Ok((token, session))
    }

    /// A new session for `user`, in the auth log with the factors it took.
    fn open_session(&self, user: &User, addr: &str, factors: &[&str], totp_step: Option<u64>) -> (String, Session) {
        let mut inner = self.lock();
        let started = now_ms();
        let life = inner.cluster.state().settings.session().as_millis() as u64;
        // the OU's longest sign-in, when shorter (D-U30)
        let cap = inner.cluster.state().placement.session_cap(user.ou).map(|(d, _)| u64::from(d) * 86_400_000);
        let expires = started + cap.map_or(life, |c| c.min(life));
        let token = random_hex(32);
        let session = Session { sid: random_hex(16), user: user.id.clone(), started, expires, node: self.me.0.clone(), addr: addr.into() };
        let ev = json!({
            "event": "sign_in", "sid": session.sid, "user": user.id, "name": user.name, "actor": user.id, "actor_name": user.name,
            "token": sha256_hex(&token), "started": started, "expires": expires, "addr": addr, "factors": factors, "totp_step": totp_step,
        });
        self.log_event(&mut inner, LogKind::Auth, Verbosity::Minimal, ev);
        inner.sessions.insert(sha256_hex(&token), session.clone());
        (token, session)
    }

    /// Keeps a TOTP secret for `user`, or drops it (None), at the
    /// authority of its OU's credentials.
    async fn store_totp(self: &Arc<Self>, user: &User, secret: Option<&[u8]>, actor: &str) -> Result<(), String> {
        let op = match secret {
            Some(s) => CredOp::SetTotp { user: user.id.clone(), secret: hex::encode(s) },
            None => CredOp::RemoveTotp { user: user.id.clone() },
        };
        let op = serde_json::to_value(op).map_err(|e| e.to_string())?;
        self.submit(directory::credentials_of(user.ou), CREDENTIALS, op, actor).await.map(|_| ())
    }

    /// The session a token stands for, and its user, while both are good.
    pub fn session(&self, token: &str) -> Option<(Session, User)> {
        let mut inner = self.lock();
        let key = sha256_hex(token);
        let s = inner.sessions.get(&key)?.clone();
        if s.expires <= now_ms() {
            inner.sessions.remove(&key);
            return None;
        }
        let u = inner.dirs.values().find_map(|d| d.state.users.get(&s.user)).cloned()?;
        (!u.disabled).then_some((s, u))
    }

    /// Signs out the session `token` stands for.
    pub fn sign_out(&self, token: &str, addr: &str) {
        let mut inner = self.lock();
        if let Some(s) = inner.sessions.remove(&sha256_hex(token)) {
            let name = inner.dirs.values().find_map(|d| d.state.users.get(&s.user)).map(|u| u.name.clone()).unwrap_or_default();
            let ev = json!({ "event": "sign_out", "sid": s.sid, "user": s.user, "name": name, "actor": s.user, "actor_name": name, "addr": addr });
            self.log_event(&mut inner, LogKind::Auth, Verbosity::Standard, ev);
        }
    }

    pub fn sessions_of(&self, user: &str) -> Vec<Session> {
        let now = now_ms();
        let inner = self.lock();
        let mut v: Vec<Session> = inner.sessions.values().filter(|s| s.user == user && s.expires > now).cloned().collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.started));
        v
    }

    /// The actor's own password, given the current one.
    pub async fn change_password(self: &Arc<Self>, actor: &Actor, old: &str, new: &str) -> Result<(), String> {
        let user = &actor.user;
        if new.chars().count() < MIN_PASSWORD {
            return Err(format!("a password has at least {MIN_PASSWORD} characters"));
        }
        let hash = self.lock().creds.get(&user.ou).and_then(|c| c.state.passwords.get(&user.id).cloned()).ok_or("this node does not keep your secrets")?;
        let old = old.to_string();
        if !tokio::task::spawn_blocking(move || directory::verify_password(&hash, &old)).await.unwrap_or(false) {
            return Err("the current password is wrong".into());
        }
        self.set_password(user, new, &format!("user:{}", user.name)).await?;
        self.log_auth_as(actor, "password_changed", Verbosity::Minimal, json!({ "user": user.id, "name": user.name, "why": "changed by the user" }));
        Ok(())
    }

    async fn set_password(self: &Arc<Self>, user: &User, password: &str, actor: &str) -> Result<(), String> {
        let pw = password.to_string();
        let hash = tokio::task::spawn_blocking(move || directory::hash_password(&pw)).await.map_err(|e| e.to_string())??;
        let op = serde_json::to_value(CredOp::SetPassword { user: user.id.clone(), hash }).map_err(|e| e.to_string())?;
        self.submit(directory::credentials_of(user.ou), CREDENTIALS, op, actor).await?;
        // a new password: the tokens of earlier sign-ins no longer count anywhere (D-U45)
        let op = serde_json::to_value(CredOp::Revoke { user: user.id.clone(), before: now_ms() / 1000 }).map_err(|e| e.to_string())?;
        self.submit(directory::credentials_of(user.ou), CREDENTIALS, op, actor).await.map(|_| ())
    }

    /// Whether the tokens of `user`'s sign-in at `auth_time` (s) were revoked since (D-U45).
    pub fn revoked(&self, user: &str, auth_time: u64) -> bool {
        let inner = self.lock();
        inner.creds.values().any(|c| c.state.revoked.get(user).is_some_and(|before| auth_time < *before))
    }

    pub fn users(&self) -> Vec<User> {
        let inner = self.lock();
        let mut v: Vec<User> = inner.dirs.values().flat_map(|d| d.state.users.values().cloned()).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Where this node's copy of `collection` ends.
    pub fn head(&self, collection: CollectionId) -> Option<(u64, Hash)> {
        let inner = self.lock();
        inner.replicas.get(&collection).map(|r| {
            let h = r.log().head();
            (h.seq, h.hash)
        })
    }

    pub fn is_authority(&self, collection: CollectionId) -> bool {
        self.lock().replicas.get(&collection).is_some_and(|r| r.is_authority())
    }

    pub fn cluster_state(&self) -> ClusterState {
        self.lock().cluster.state().clone()
    }

    pub fn cluster_applied(&self) -> u64 {
        self.lock().cluster.applied()
    }

    /// Whether this node keeps any OU's secrets (D-U45: an edge then signs people in while home is away).
    pub fn keeps_any_secrets(&self) -> bool {
        !self.lock().creds.is_empty()
    }

    /// Whether this node keeps an OU's secrets now (its credentials folded).
    pub fn keeps_secrets_of(&self, ou: OuId) -> bool {
        self.lock().creds.contains_key(&ou)
    }

    fn on_call(self: &Arc<Self>, from: &NodeId, body: Value) -> Value {
        let answer = match serde_json::from_value::<Call>(body) {
            Ok(call) => self.answer(from, call),
            Err(e) => Answer::Refused { why: e.to_string() },
        };
        serde_json::to_value(answer).unwrap_or(Value::Null)
    }

    fn answer(self: &Arc<Self>, from: &NodeId, call: Call) -> Answer {
        let refused = |why: String| Answer::Refused { why };
        match call {
            Call::Write { collection, module, op, actor } => match self.write_op(collection, &module, op, &actor) {
                Ok(seq) => Answer::Written { seq },
                Err(why) => refused(why),
            },
            // a node publishes its own signing key, never another's (D-U45)
            Call::Commit { cmd: Command::Key { node, key: Some(_) }, .. } if node != *from => refused(format!("{from} cannot publish {node}'s key")),
            Call::Commit { cmd, actor } => match self.commit(&format!("{actor} via {from}"), cmd) {
                Ok(()) => Answer::Done,
                Err(why) => refused(why),
            },
            Call::Handover { collection, to } => {
                if !Self::holds_cluster_log(&self.lock()) {
                    return refused("this node does not hold the cluster log".into());
                }
                let node = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = node.handover(collection, to).await {
                        eprintln!("{}: handover: {e}", node.me);
                    }
                });
                Answer::Done
            }
            Call::AuthLog { limit } => Answer::Lines { lines: self.auth_lines(limit.min(1000)) },
            Call::QueryLog { q } => Answer::Lines { lines: self.query_local(&q) },
            Call::SessionsOf { user } => Answer::Sessions { sessions: self.sessions_of(&user) },
            Call::Web { op, site } => self.answer_web(&op, site),
            Call::AcmeToken { token } => self.answer_acme_token(&token),
            Call::Cert { name } => self.answer_cert(from, &name),
            Call::Mail { op } => self.answer_mail(from, op),
            Call::MailAs { token, calls } => match tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(crate::mail::jmap_as(&token, &calls))) {
                Ok(value) => Answer::Web { value },
                Err(why) => Answer::Refused { why },
            },
            Call::NodeInfo => Answer::Web { value: self.node_info() },
            Call::Probe { addrs, ports } => self.answer_probe(from, addrs, ports),
            Call::ExposureNow => {
                let node = self.clone();
                let p = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(node.check_exposure((now_ms() / 1000) as usize)));
                Answer::Web { value: json!({ "probe": p, "exposure": self.exposure_report() }) }
            }
            Call::DiskFormat { device, label } => match self.data.get().map(|d| crate::nodeinfo::request_format(d, &device, &label)) {
                Some(Ok(v)) => Answer::Web { value: v },
                Some(Err(why)) => Answer::Refused { why },
                None => Answer::Refused { why: "this node has no data folder".into() },
            },
            Call::LogBatch { lines, reset } => self.answer_log_batch(from, lines, reset),
            Call::LogQl { text, from: t0, to: t1, limit } => self.answer_log_ql(&text, t0, t1, limit),
            Call::RuleState => self.answer_rule_states(),
            Call::WebFiles { site, op, path, to, data, all } => self.answer_web_files(&site, &op, &path, to.as_deref(), data.as_deref(), all),
            Call::Stop { .. } | Call::Resume { .. } | Call::Head { .. } => self.answer_handover(from, call),
        }
    }

    fn answer_handover(&self, from: &NodeId, call: Call) -> Answer {
        let mut inner = self.lock();
        if self.voter_in(inner.cluster.state()).as_ref() != Some(from) {
            return Answer::Refused { why: "only the voter asks that".into() };
        }
        let head = |r: &Replica, seq: u64| match r.log().hash_at(seq) {
            Ok(Some(h)) => Answer::Head { seq, hash: hex::encode(h) },
            other => Answer::Refused { why: format!("no hash at {seq}: {other:?}") },
        };
        match call {
            Call::Stop { collection } => match inner.replicas.get_mut(&collection) {
                Some(r) if r.is_authority() => {
                    let seq = r.stop();
                    head(r, seq)
                }
                _ => Answer::Refused { why: "not the authority".into() },
            },
            Call::Resume { collection } => {
                if let Some(r) = inner.replicas.get_mut(&collection) {
                    r.resume();
                }
                Answer::Done
            }
            Call::Head { collection } => match inner.replicas.get(&collection) {
                Some(r) => head(r, r.log().head().seq),
                None => Answer::Refused { why: "not a member".into() },
            },
            _ => Answer::Refused { why: "not a handover call".into() },
        }
    }

    /// A call to `node`, or to this node itself without the network.
    async fn ask(self: &Arc<Self>, node: &NodeId, call: Call) -> Result<Answer, String> {
        if node == &self.me {
            return Ok(self.answer(&self.me.clone(), call));
        }
        let body = serde_json::to_value(&call).map_err(|e| e.to_string())?;
        let v = self.link.call(node, body).await.map_err(|e| e.to_string())?;
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    /// Asks the voter (this node or another) to hand `collection` over to
    /// `to`.
    pub async fn request_handover(self: &Arc<Self>, collection: CollectionId, to: NodeId) -> Result<(), String> {
        let voter = self.voter_in(&self.cluster_state()).ok_or("no voter known")?;
        match self.ask(&voter, Call::Handover { collection, to }).await? {
            Answer::Done => Ok(()),
            Answer::Refused { why } => Err(why),
            other => Err(format!("{other:?}")),
        }
    }

    /// Planned handover (§7.3), as the voter: the authority of `collection`
    /// stops, `to` catches up to exactly its chain, and the next term makes
    /// `to` the authority. Called off when `to` does not catch up in time.
    pub async fn handover(self: &Arc<Self>, collection: CollectionId, to: NodeId) -> Result<(), String> {
        let col = self.cluster_state().collections.get(&collection).cloned().ok_or("no such collection")?;
        if col.term.authority == to {
            return Ok(());
        }
        if !col.term.members.contains(&to) {
            return Err(format!("{to} is not a member of {collection}"));
        }
        let from = col.term.authority.clone();
        let Answer::Head { seq, hash } = self.ask(&from, Call::Stop { collection }).await? else {
            return Err(format!("{from} did not stop"));
        };
        let deadline = Instant::now() + HANDOVER_WAIT;
        loop {
            match self.ask(&to, Call::Head { collection }).await {
                Ok(Answer::Head { seq: s, hash: h }) if s == seq && h == hash => break,
                _ if Instant::now() > deadline => {
                    let _ = self.ask(&from, Call::Resume { collection }).await;
                    return Err(format!("{to} did not catch up with {from}; handover called off"));
                }
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        let term = Term { epoch: col.term.epoch + 1, authority: to, members: col.term.members };
        let res = self.commit("handover", Command::Term { id: collection, term });
        if res.is_err() {
            let _ = self.ask(&from, Call::Resume { collection }).await;
        }
        res
    }
}

/// A hash to check against when there is no user, so that an unknown name
/// takes as long as a wrong password.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$Zm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm8";

/// The sessions an auth log leaves open, and the last TOTP step of each user.
fn sessions_from(log: &Log, me: &NodeId) -> (BTreeMap<String, Session>, BTreeMap<String, u64>) {
    let now = now_ms();
    let mut out = BTreeMap::new();
    let mut steps: BTreeMap<String, u64> = BTreeMap::new();
    let mut after = log.start().seq;
    while let Ok(entries) = log.read(after, 1024) {
        if entries.is_empty() {
            break;
        }
        for e in entries {
            after = e.seq;
            match serde_json::from_slice::<Replay>(&e.payload) {
                Ok(Replay::SignIn { sid, user, token, started, expires, addr, totp_step }) => {
                    if let Some(s) = totp_step {
                        let last = steps.entry(user.clone()).or_insert(s);
                        *last = (*last).max(s);
                    }
                    if expires > now {
                        out.insert(token, Session { sid, user, started, expires, node: me.0.clone(), addr });
                    }
                }
                Ok(Replay::SignOut { sid }) | Ok(Replay::SessionEnded { ended: sid }) => out.retain(|_, s: &mut Session| s.sid != sid),
                _ => {}
            }
        }
    }
    (out, steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cluster::ROOT_OU,
        kv::mem::MemKv,
        link::request_join,
        pki::NodeCert,
        placement::{Ou, SecretLink},
        testutil::TempDir,
    };

    fn n(s: &str) -> NodeId {
        NodeId(s.into())
    }

    fn draft(i: usize) -> Draft {
        Draft { actor: "user:test".into(), module: "wiki".into(), op: "save".into(), payload: format!("rev {i}").into_bytes() }
    }

    async fn until(what: &str, f: impl Fn() -> bool) {
        for _ in 0..750 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    const WIKI: CollectionId = CollectionId([0x77; 16]);

    /// home starts a cluster (writing its first admin's password into
    /// `first_admin`); edge joins it with a token and dials it.
    async fn two_nodes(first_admin: Option<PathBuf>) -> (Arc<Node>, Arc<Node>, Arc<dyn Kv>) {
        let local = "127.0.0.1:0".parse().unwrap();
        let ca = Arc::new(Ca::new().unwrap());
        let kv_a: Arc<dyn Kv> = Arc::new(MemKv::new());
        Cluster::init(kv_a.clone(), n("home"), now_ms()).unwrap();
        let link_a = Link::bind(&ca.issue("home").unwrap().tls().unwrap(), local).unwrap();
        link_a.spawn_accept();
        let a = Node::start(kv_a.clone(), link_a.clone(), Some(ca.clone()), first_admin).unwrap();
        let addr_a = link_a.local_addr().unwrap();

        let roles = [Role::Idp, Role::Store, Role::Blob, Role::EdgeHttp, Role::EdgeMail].into();
        let token = join::make_token_for(&*kv_a, &ca, &n("edge"), "edge", roles, now_ms() + 60_000).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let req = JoinRequest { node: "edge".into(), token: token.secret, public_key: hex::encode(key.public_key_raw()) };
        let grant = request_join(addr_a, &token.ca_fingerprint, req).await.unwrap();
        let cert = NodeCert { node: grant.node.clone(), cert_pem: grant.cert_pem, key_pem: key.serialize_pem(), ca_pem: grant.ca_pem };

        let kv_b: Arc<dyn Kv> = Arc::new(MemKv::new());
        remember_voter(&*kv_b, &n("home")).unwrap();
        let link_b = Link::bind(&cert.tls().unwrap(), local).unwrap();
        link_b.spawn_accept();
        let b = Node::start(kv_b.clone(), link_b.clone(), None, None).unwrap();
        link_b.spawn_keep_dialing(n("home"), addr_a);
        until("edge to have the cluster log", || b.cluster_applied() == a.cluster_applied() && b.cluster_applied() > 1).await;
        (a, b, kv_b)
    }

    /// Edge gets the cluster log, a replica of the wiki, then the wiki itself
    /// by planned handover, and gives it back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_nodes_replicate_and_hand_over() {
        let (a, b, _) = two_nodes(None).await;
        assert!(b.cluster_state().placement.nodes.contains_key(&n("edge")));
        assert!(b.commit("edge", Command::Create { id: WIKI, domain: Domain::Wiki, ou: ROOT_OU }).is_err());

        a.commit("admin", Command::Create { id: WIKI, domain: Domain::Wiki, ou: ROOT_OU }).unwrap();
        let both = vec![n("edge"), n("home")];
        a.commit("admin", Command::Term { id: WIKI, term: Term { epoch: 2, authority: n("home"), members: both } }).unwrap();
        until("home to be the wiki's authority", || a.is_authority(WIKI)).await;
        for i in 0..20 {
            a.write(WIKI, draft(i)).unwrap();
        }
        until("edge to hold the wiki", || b.head(WIKI).is_some() && b.head(WIKI) == a.head(WIKI)).await;
        assert!(b.write(WIKI, draft(99)).is_err());

        a.handover(WIKI, n("edge")).await.unwrap();
        until("edge to be the wiki's authority", || b.is_authority(WIKI)).await;
        assert!(a.write(WIKI, draft(98)).is_err());
        assert_eq!(b.write(WIKI, draft(20)).unwrap(), 21);
        until("home to follow the edge", || a.head(WIKI) == b.head(WIKI)).await;

        a.handover(WIKI, n("home")).await.unwrap();
        until("home to be the wiki's authority again", || a.is_authority(WIKI)).await;
        assert_eq!(a.write(WIKI, draft(21)).unwrap(), 22);
        until("edge to follow", || a.head(WIKI) == b.head(WIKI)).await;

        let refused = b.ask(&n("home"), Call::Stop { collection: WIKI }).await.unwrap();
        assert!(matches!(refused, Answer::Refused { .. }), "{refused:?}");
        assert!(a.is_authority(WIKI));
    }

    /// The directory and sign-in across two nodes: the first admin, users
    /// made through the edge, secrets placed on the edge and taken off it
    /// again (and erased there), tier 0, the auth log.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identity_across_two_nodes() {
        let dir = TempDir::new("node");
        let file = dir.path().join("first-admin");
        let (a, b, kv_b) = two_nodes(Some(file.clone())).await;
        until("the first admin", || file.exists()).await;
        let text = std::fs::read_to_string(&file).unwrap();
        let pw = text.lines().find_map(|l| l.strip_prefix("password: ")).unwrap().to_string();

        // At home, which keeps the root OU's secrets.
        let (token, s) = a.sign_in("admin", &pw, "test").await.unwrap();
        let (_, admin) = a.session(&token).unwrap();
        assert!(!admin.admin && a.has_any_role(&admin) && s.sid.len() == 32);
        let actor = Actor { user: admin.clone(), sid: s.sid.clone(), addr: "test".into() };
        assert_eq!(a.sign_in("admin", "wrong", "test").await.unwrap_err(), "wrong user name or password");
        assert_eq!(a.sign_in("nobody", "wrong", "test").await.unwrap_err(), "wrong user name or password");

        // The edge knows the users, but not their secrets.
        until("edge to know admin and its role", || b.users().iter().any(|u| u.name == "admin") && b.has_any_role(&admin)).await;
        let e = b.sign_in("admin", &pw, "test").await.unwrap_err();
        assert!(e.contains("does not keep the secrets"), "{e}");

        // A user made through the edge: forwarded to home, the authority.
        b.create_user(&actor, "anna", "Anna", ROOT_OU, "anna's password").await.unwrap();
        until("home to know anna", || a.users().iter().any(|u| u.name == "anna")).await;
        a.sign_in("anna", "anna's password", "test").await.unwrap();

        // Secrets also on the edge: it signs anna in itself.
        let mut p = a.cluster_state().placement;
        p.default_secrets.insert(n("edge"));
        a.commit("admin", Command::Placement(p.clone())).unwrap();
        until("edge to keep the secrets", || b.keeps_secrets_of(ROOT_OU)).await;
        until("edge to have anna's password", || {
            b.lock().creds.get(&ROOT_OU).is_some_and(|c| c.state.passwords.len() == 2)
        })
        .await;
        let (t2, s2) = b.sign_in("anna", "anna's password", "test").await.unwrap();
        assert_eq!(s2.node, "edge");
        assert!(b.session(&t2).is_some() && a.session(&t2).is_none());

        // And off it again: the edge erases them.
        p.default_secrets.remove(&n("edge"));
        a.commit("admin", Command::Placement(p.clone())).unwrap();
        let prefix = directory::credentials_of(ROOT_OU).key_prefix();
        until("edge to erase the secrets", || {
            !b.keeps_secrets_of(ROOT_OU) && kv::scan_prefix(&*kv_b, &prefix, 1).unwrap().is_empty()
        })
        .await;
        assert!(b.sign_in("anna", "anna's password", "test").await.unwrap_err().contains("does not keep"));

        // Tier 0: a password is not enough.
        let mut p = a.cluster_state().placement;
        let t0 = OuId(2);
        let mut ou = Ou::new("tier0", Some(ROOT_OU));
        ou.tier0 = true;
        ou.secrets = Some(SecretLink { nodes: [n("home")].into(), enforced: false });
        p.ous.insert(t0, ou);
        a.commit("admin", Command::Placement(p)).unwrap();
        until("tier 0's directory", || a.is_authority(directory::directory_of(t0)) && a.is_authority(directory::credentials_of(t0)))
            .await;
        a.create_user(&actor, "root", "Root", t0, "root's password").await.unwrap();
        let e = a.sign_in("root", "root's password", "test").await.unwrap_err();
        assert!(e.contains("enrol_fido2"), "{e}");
        let Ok(SignInStep::More { ticket, need: "enrol_fido2", .. }) = a.sign_in_step("root", "root's password", "test").await else { panic!("no FIDO2 step") };
        // a TOTP code does not stand in for the key
        assert!(a.sign_in_totp(&ticket, "123456", "test").await.unwrap_err().contains("FIDO2"));

        // Changing a password, signing out; all of it in the auth log.
        a.change_password(&actor, &pw, "a new password").await.unwrap();
        a.sign_out(&token, "test");
        assert!(a.session(&token).is_none());
        let events: Vec<String> =
            a.auth_lines(50).iter().map(|l| l.event["event"].as_str().unwrap_or("").to_string()).collect();
        for want in ["sign_in", "sign_in_failed", "password_changed", "sign_out"] {
            assert!(events.iter().any(|e| e == want), "{want} in {events:?}");
        }
        let remote = a.auth_lines_of(&n("edge"), 50).await.unwrap();
        assert!(remote.iter().any(|l| l.event["event"] == "sign_in" && l.node == "edge"));
        assert!(a.auth_lines(50).iter().all(|l| l.event.get("token").is_none()));

        // Everything this session did, on both nodes, in both logs.
        let session = |kind: &str| LogQuery {
            kind: kind.into(),
            conds: vec![Cond { field: "session".into(), op: "is".into(), value: s.sid.clone() }],
            since: None,
            until: None,
            limit: 100,
            filter: None,
            before: None,
        };
        let page = a.query_logs(&admin, session("auth")).await.unwrap();
        let (auth, missing) = (page.lines, page.unreachable);
        assert!(missing.is_empty());
        let seen: Vec<(String, String)> = auth.iter().map(|l| (l.node.clone(), l.event["event"].as_str().unwrap().to_string())).collect();
        for want in [("home", "sign_in"), ("home", "password_changed"), ("home", "sign_out"), ("edge", "password_changed")] {
            assert!(seen.contains(&(want.0.to_string(), want.1.to_string())), "{want:?} in {seen:?}");
        }
        let adm = a.query_logs(&admin, session("admin")).await.unwrap().lines;
        let made: Vec<&str> = adm.iter().filter_map(|l| l.event["name"].as_str()).collect();
        assert!(made.contains(&"anna") && made.contains(&"root"), "{made:?}");

        // Roles: anna may read the logs of nobody; then of /, as an auditor.
        let anna = a.users().into_iter().find(|u| u.name == "anna").unwrap();
        assert!(a.query_logs(&anna, session("auth")).await.is_err());
        let anna_actor = Actor { user: anna.clone(), sid: "x".into(), addr: "test".into() };
        assert!(a.grant(&anna_actor, ROOT_OU, "auditor", crate::rbac::Subject::User { id: anna.id.clone() }).await.is_err());
        a.grant(&actor, ROOT_OU, "auditor", crate::rbac::Subject::User { id: anna.id.clone() }).await.unwrap();
        assert!(a.query_logs(&anna, session("auth")).await.is_ok());
        assert!(a.create_user(&anna_actor, "eve", "Eve", ROOT_OU, "eve's password").await.is_err());

        // The pages' table filter: OR-groups of AND-lines, on every node.
        let grid = |groups: serde_json::Value| LogQuery {
            kind: "auth".into(),
            conds: vec![],
            since: None,
            until: None,
            limit: 100,
            filter: Some(serde_json::from_value(json!({ "groups": groups })).unwrap()),
            before: None,
        };
        let l = a.query_logs(&admin, grid(json!([[{ "col": "event", "op": "=", "vals": ["password_changed"] }, { "col": "node", "op": "=", "vals": ["edge"] }]]))).await.unwrap().lines;
        assert!(!l.is_empty() && l.iter().all(|l| l.node == "edge" && l.event["event"] == "password_changed"));
        let l = a.query_logs(&admin, grid(json!([[{ "col": "event", "op": "=", "vals": ["sign_out"] }], [{ "col": "user", "op": "contains", "vals": ["ann"] }]]))).await.unwrap().lines;
        assert!(l.iter().any(|l| l.event["event"] == "sign_out") && l.iter().any(|l| l.event["name"] == "anna"));
        assert!(l.iter().all(|l| l.event["event"] == "sign_out" || l.event["name"].as_str().is_some_and(|n| n.contains("ann"))));
    }

    /// Module instances, access, and mass updates by query (A24).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn access_and_mass_updates() {
        let dir = TempDir::new("bulk");
        let file = dir.path().join("first-admin");
        let (a, _b, _) = two_nodes(Some(file.clone())).await;
        until("the first admin", || file.exists()).await;
        let pw = std::fs::read_to_string(&file).unwrap().lines().find_map(|l| l.strip_prefix("password: ")).unwrap().to_string();
        let (token, s) = a.sign_in("admin", &pw, "test").await.unwrap();
        let (_, admin) = a.session(&token).unwrap();
        let actor = Actor { user: admin.clone(), sid: s.sid.clone(), addr: "test".into() };
        for n in ["u1", "u2", "u3", "other"] {
            a.create_user(&actor, n, n, ROOT_OU, "a long password").await.unwrap();
        }
        let group = a.create_group(&actor, "staff", "Staff", ROOT_OU).await.unwrap();

        // A mail module on home; access to it for u1 alone, and not to a module that is not there.
        let mut p = a.cluster_state().placement;
        p.modules.insert("mail-eu".into(), crate::placement::Module::new(Domain::Mail.into(), "Mail EU", n("home")));
        a.set_placement(&actor, p).await.unwrap();
        let u = |name: &str| a.users().into_iter().find(|x| x.name == name).unwrap();
        a.grant_access(&actor, "mail-eu", crate::rbac::Subject::User { id: u("u1").id }).await.unwrap();
        assert!(a.grant_access(&actor, "mail-us", crate::rbac::Subject::User { id: u("u1").id }).await.is_err());

        let q = |groups: serde_json::Value, params: serde_json::Value| -> crate::table::Query {
            serde_json::from_value(json!({ "filter": { "groups": groups }, "params": params, "limit": 100 })).unwrap()
        };
        let names = |rows: &[serde_json::Value]| -> Vec<String> { let mut v: Vec<String> = rows.iter().map(|r| r["name"].as_str().unwrap().to_string()).collect(); v.sort(); v };
        let row = a.table(&admin, "users", &q(json!([[{ "col": "name", "op": "=", "vals": ["u1"] }]]), json!({}))).unwrap();
        assert_eq!(row.rows[0]["access_to"][0]["module"], "mail-eu");

        let wait = |id: String| { let a = a.clone(); async move { until("the job", || a.job(&id).is_some_and(|j| j.finished)).await; a.job(&id).unwrap() } };
        // Everyone whose name starts with "u", found by query, into the group.
        let sel = Selection { ids: vec![], query: Some(q(json!([[{ "col": "name", "op": "startswith", "vals": ["u"] }]]), json!({}))) };
        let j = wait(a.start_bulk(actor.clone(), sel, BulkAction::AddToGroup { group: group.id.clone() }).unwrap()).await;
        assert_eq!((j.total, j.done, j.refused, j.failed), (3, 3, 0, 0));
        let members = a.table(&admin, "users", &q(json!([]), json!({ "member_of": group.id }))).unwrap();
        assert_eq!(names(&members.rows), ["u1", "u2", "u3"]);
        // Access for the group's members, picked by id; u1 has it already.
        let ids = members.rows.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
        let j = wait(a.start_bulk(actor.clone(), Selection { ids, query: None }, BulkAction::GrantAccess { module: "mail-eu".into() }).unwrap()).await;
        assert_eq!(j.done, 3);
        let who = a.table(&admin, "access", &q(json!([]), json!({ "module": "mail-eu" }))).unwrap();
        assert_eq!(who.total, 3);
        // Disable everyone: not oneself.
        let all = Selection { ids: vec![], query: Some(q(json!([]), json!({}))) };
        let j = wait(a.start_bulk(actor.clone(), all, BulkAction::Disable).unwrap()).await;
        assert_eq!((j.total, j.done, j.refused), (5, 4, 1));
        assert!(!u("admin").disabled && u("other").disabled);
        assert!(a.sign_in("u2", "a long password", "test").await.unwrap_err().contains("disabled"));
        // All in the admin log, with the session.
        let lq = LogQuery { kind: "admin".into(), conds: vec![Cond { field: "session".into(), op: "is".into(), value: s.sid.clone() }], since: None, until: None, limit: 100, filter: None, before: None };
        let events: Vec<String> = a.query_logs(&admin, lq).await.unwrap().lines.iter().map(|l| l.event["event"].as_str().unwrap().to_string()).collect();
        for e in ["access_granted", "bulk_update_started", "bulk_update_finished", "node_control_changed"] {
            assert!(events.iter().any(|x| x == e), "{e} in {events:?}");
        }
    }

    /// TOTP: set up at sign-in where the OU asks for a second factor, asked
    /// for after that, a code never twice; reset by an administrator.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn totp_at_sign_in() {
        let dir = TempDir::new("totp");
        let file = dir.path().join("first-admin");
        let (a, _b, _) = two_nodes(Some(file.clone())).await;
        until("the first admin", || file.exists()).await;
        let pw = std::fs::read_to_string(&file).unwrap().lines().find_map(|l| l.strip_prefix("password: ")).unwrap().to_string();
        let (token, s) = a.sign_in("admin", &pw, "test").await.unwrap();
        let (_, admin) = a.session(&token).unwrap();
        let actor = Actor { user: admin.clone(), sid: s.sid.clone(), addr: "test".into() };
        let mut p = a.cluster_state().placement;
        let staff = OuId(2);
        let mut ou = crate::placement::Ou::new("staff", Some(ROOT_OU));
        ou.second_factor = true;
        p.ous.insert(staff, ou);
        a.set_placement(&actor, p).await.unwrap();
        until("staff's directory", || a.is_authority(directory::directory_of(staff)) && a.is_authority(directory::credentials_of(staff))).await;
        a.create_user(&actor, "tom", "Tom", staff, "tom's password").await.unwrap();
        let tom = a.users().into_iter().find(|u| u.name == "tom").unwrap();
        let now_code = |secret: &[u8], ahead: u64| format!("{:06}", totp::code(secret, now_ms() / 1000 / totp::STEP + ahead));

        // First sign-in: set it up.
        let SignInStep::More { ticket, need, enrol } = a.sign_in_step("tom", "tom's password", "test").await.unwrap() else { panic!("no second step") };
        assert_eq!(need, "enrol_totp");
        assert!(enrol.unwrap()["qr"].as_str().unwrap().starts_with("<svg"));
        let secret = a.lock().tickets[&ticket].enrol.clone().unwrap();
        assert_eq!(a.sign_in_totp(&ticket, "000000", "test").await.unwrap_err(), "wrong code");
        let code = now_code(&secret, 0);
        a.sign_in_totp(&ticket, &code, "test").await.unwrap();
        until("the TOTP kept", || a.has_totp(&tom) == Some(true)).await;
        // Then it is asked for; the same code does not count twice.
        let SignInStep::More { ticket, need, .. } = a.sign_in_step("tom", "tom's password", "test").await.unwrap() else { panic!() };
        assert_eq!(need, "totp");
        assert_eq!(a.sign_in_totp(&ticket, &code, "test").await.unwrap_err(), "wrong code");
        let (t2, s2) = a.sign_in_totp(&ticket, &now_code(&secret, 1), "test").await.unwrap();
        assert!(a.session(&t2).is_some());
        // It stays while the OU asks for it; an administrator can reset it.
        let tom_actor = Actor { user: tom.clone(), sid: s2.sid.clone(), addr: "test".into() };
        assert!(a.remove_totp(&tom_actor, &now_code(&secret, 1)).await.unwrap_err().contains("asks for a second factor"));
        a.reset_totp(&actor, &tom.id).await.unwrap();
        until("the TOTP gone", || a.has_totp(&tom) == Some(false)).await;
        let SignInStep::More { need, .. } = a.sign_in_step("tom", "tom's password", "test").await.unwrap() else { panic!() };
        assert_eq!(need, "enrol_totp");
        let events: Vec<String> = a.auth_lines(100).iter().map(|l| l.event["event"].as_str().unwrap_or("").to_string()).collect();
        assert!(events.contains(&"totp_added".into()) && events.contains(&"totp_removed".into()));
        let factors: Vec<&Value> = a.auth_lines(100).iter().filter(|l| l.event["event"] == "sign_in" && l.event["name"] == "tom").map(|l| l.event["factors"].clone()).collect::<Vec<_>>().leak().iter().collect();
        assert!(factors.iter().all(|f| **f == json!(["pwd", "otp"])), "{factors:?}");
    }

    /// Chat (D-U42) across two nodes: a room made through the edge (the cluster command through
    /// the voter, its entries at home, the authority), kept by both; messages, unread, history
    /// back and forward, edits and deletions, direct messages made once, who may do what.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_rooms_across_two_nodes() {
        use crate::chat::ChatOp;
        let dir = TempDir::new("chat");
        let file = dir.path().join("first-admin");
        let (a, b, _) = two_nodes(Some(file.clone())).await;
        until("the first admin", || file.exists()).await;
        let pw = std::fs::read_to_string(&file).unwrap().lines().find_map(|l| l.strip_prefix("password: ")).unwrap().to_string();
        let (token, s) = a.sign_in("admin", &pw, "test").await.unwrap();
        let (_, admin) = a.session(&token).unwrap();
        let root = Actor { user: admin, sid: s.sid, addr: "test".into() };
        for n in ["anna", "ben", "cyril"] {
            a.create_user(&root, n, &n.to_uppercase(), ROOT_OU, "a long password").await.unwrap();
        }
        until("edge to know everyone", || b.users().len() == 4).await;
        let who = |name: &str| {
            let user = a.users().into_iter().find(|x| x.name == name).unwrap();
            Actor { user, sid: String::new(), addr: "test".into() }
        };
        let (anna, ben, cyril) = (who("anna"), who("ben"), who("cyril"));

        // made at the edge
        let v0 = a.chat_version();
        let id = b.chat_create(&anna, "kitchen", &["ben".into()]).await.unwrap();
        let id = CollectionId::parse(&id).unwrap();
        assert_eq!(a.cluster_state().collections[&id].term.authority, n("home"));
        until("both nodes to keep the room", || {
            a.cluster_state().collections[&id].term.members.len() == 2 && b.head(id).is_some() && b.head(id) == a.head(id)
        })
        .await;
        assert!(a.chat_version() > v0);
        assert!(b.chat_create(&anna, "x", &["nobody".into()]).await.unwrap_err().contains("no user nobody"));

        let mut seqs = Vec::new();
        for i in 0..70 {
            seqs.push(b.chat_do(&anna, id, ChatOp::Say { text: format!("message {i}") }).await.unwrap());
        }
        let rooms = a.chat_rooms(&ben.user);
        assert_eq!(rooms["rooms"][0]["name"], "kitchen");
        assert_eq!(rooms["rooms"][0]["unread"], 70);
        assert_eq!(rooms["rooms"][0]["last"]["text"], "message 69");
        assert_eq!(a.chat_rooms(&cyril.user)["rooms"].as_array().unwrap().len(), 0);
        assert!(a.chat_room(&cyril.user, id, None, None, 50).unwrap_err().contains("not in this room"));
        assert!(a.chat_do(&cyril, id, ChatOp::Say { text: "let me in".into() }).await.unwrap_err().contains("not in this room"));

        // history: the latest page, then the one before (with the room's first line)
        let page = a.chat_room(&ben.user, id, None, None, 50).unwrap();
        let items = page["items"].as_array().unwrap();
        assert_eq!(items.len(), 50);
        assert_eq!(items[49]["text"], "message 69");
        assert_eq!(page["more"], true);
        let older = a.chat_room(&ben.user, id, items[0]["seq"].as_u64(), None, 50).unwrap();
        let older = older["items"].as_array().unwrap();
        assert_eq!(older.len(), 21);
        assert_eq!(older[0]["event"], "create");
        assert_eq!(older[0]["users"][0], "BEN");
        assert_eq!(older[20]["text"], "message 19");

        // reading, an edit and a deletion, seen by what comes after
        a.chat_read(&ben, id, seqs[59]).await.unwrap();
        assert_eq!(a.chat_rooms(&ben.user)["rooms"][0]["unread"], 10);
        let head = a.head(id).unwrap().0;
        a.chat_read(&ben, id, seqs[10]).await.unwrap();
        assert_eq!(a.head(id).unwrap().0, head, "reading back writes nothing");
        assert!(a.chat_do(&ben, id, ChatOp::Edit { seq: seqs[0], text: "mine now".into() }).await.unwrap_err().contains("author"));
        a.chat_do(&anna, id, ChatOp::Edit { seq: seqs[0], text: "message zero".into() }).await.unwrap();
        a.chat_do(&anna, id, ChatOp::Delete { seq: seqs[1] }).await.unwrap();
        let after = a.chat_room(&ben.user, id, None, Some(head), 50).unwrap();
        assert_eq!(after["changed"][0]["text"], "message zero");
        assert_eq!(after["deleted"][0], seqs[1]);
        let first = a.chat_room(&ben.user, id, Some(seqs[3]), None, 50).unwrap();
        let texts: Vec<&str> = first["items"].as_array().unwrap().iter().filter_map(|i| i["text"].as_str()).collect();
        assert_eq!(texts, ["message zero", "message 2"]);

        // people: added by a member, typing seen by the others, never written
        a.chat_do(&ben, id, ChatOp::Add { users: vec![cyril.user.id.clone()] }).await.unwrap();
        let head = a.head(id).unwrap().0;
        a.chat_typing(&cyril.user, id).unwrap();
        assert_eq!(a.chat_room(&anna.user, id, None, None, 1).unwrap()["typing"][0], "CYRIL");
        assert_eq!(a.chat_room(&cyril.user, id, None, None, 1).unwrap()["typing"].as_array().unwrap().len(), 0);
        assert_eq!(a.head(id).unwrap().0, head);
        assert!(a.chat_typing(&admin_user(&a), id).is_err());

        // a direct message: one room for the two, whoever starts it
        let dm = a.chat_dm(&anna, "ben").await.unwrap();
        assert_eq!(b.chat_dm(&ben, "anna").await.unwrap(), dm);
        assert_eq!(CollectionId::parse(&dm).unwrap(), crate::chat::dm_of(&anna.user.id, &ben.user.id));
        assert!(a.chat_dm(&anna, "anna").await.is_err());
        let dm = CollectionId::parse(&dm).unwrap();
        b.chat_do(&ben, dm, ChatOp::Say { text: "psst".into() }).await.unwrap();
        let rooms = a.chat_rooms(&anna.user);
        assert_eq!(rooms["rooms"][0]["name"], "BEN");
        assert_eq!(rooms["rooms"][0]["unread"], 1);
        assert!(a.chat_do(&anna, dm, ChatOp::Leave).await.is_err());
        let wait = {
            let a = a.clone();
            let v = a.chat_version();
            tokio::spawn(async move { a.chat_wait(v).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        b.chat_do(&anna, dm, ChatOp::Say { text: "yes?".into() }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), wait).await.expect("the wait to end with the change").unwrap();
    }

    /// Mailboxes kept by osada (D-U43), without a Stalwart: the directory takes them (an address is
    /// one mailbox's; from the template when none is given; kept when it would change), access to the
    /// mail instance comes with a new one, every new account gets one when the settings say so, and
    /// what Stalwart could not do is told.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mailboxes_in_the_directory() {
        let dir = TempDir::new("mailboxes");
        let file = dir.path().join("first-admin");
        let (a, _b, _) = two_nodes(Some(file.clone())).await;
        until("the first admin", || file.exists()).await;
        let pw = std::fs::read_to_string(&file).unwrap().lines().find_map(|l| l.strip_prefix("password: ")).unwrap().to_string();
        let (token, s) = a.sign_in("admin", &pw, "test").await.unwrap();
        let (_, admin) = a.session(&token).unwrap();
        let root = Actor { user: admin, sid: s.sid, addr: "test".into() };
        for n in ["anna", "ben"] {
            a.create_user(&root, n, n, ROOT_OU, "a long password").await.unwrap();
        }
        let set = |v: Value| { let a = a.clone(); let root = Actor { user: root.user.clone(), sid: root.sid.clone(), addr: "test".into() }; async move { a.mail(&root, v).await } };
        assert!(set(json!({ "op": "mailbox_set", "user": "anna" })).await.unwrap_err().contains("no mail instance"));
        let mut p = a.cluster_state().placement;
        p.modules.insert("mail".into(), crate::placement::Module::new(Domain::Mail.into(), "Mail", n("home")));
        a.set_placement(&root, p).await.unwrap();

        assert!(set(json!({ "op": "mailbox_set", "user": "anna" })).await.unwrap_err().contains("no template"));
        let r = set(json!({ "op": "mailbox_set", "user": "anna", "address": "Anna@Example.org", "quota": 1u64 << 30 })).await.unwrap();
        assert!(r["error"].as_str().is_some_and(|e| !e.is_empty()), "Stalwart is not there: {r}");
        let anna = a.find_user_by_name("anna").unwrap();
        assert_eq!(anna.mailbox, Some(directory::Mailbox { address: "anna@example.org".into(), aliases: None, quota: Some(1 << 30), off: false, instance: Some("mail".into()) }));
        assert!(a.lock().dirs.values().any(|d| d.state.access_for(&anna.id).iter().any(|x| x.module == "mail")));
        assert!(set(json!({ "op": "mailbox_set", "user": "ben", "address": "anna@example.org" })).await.unwrap_err().contains("anna's mailbox already"));
        // aliases (D-U44): one mailbox's each, a catch-all too
        set(json!({ "op": "mailbox_set", "user": "anna", "aliases": ["a@example.org", "*@example.org"] })).await.unwrap();
        assert!(set(json!({ "op": "mailbox_set", "user": "ben", "address": "ben@example.org", "aliases": ["*@example.org"] })).await.unwrap_err().contains("anna's already"));
        let e = set(json!({ "op": "mailbox_set", "user": "ben", "address": "a@example.org" })).await.unwrap_err();
        assert!(e.contains("anna's already"), "{e}");
        assert!(set(json!({ "op": "mailbox_set", "user": "ben", "address": "ben@example.org", "aliases": ["*"] })).await.is_err());
        set(json!({ "op": "mailbox_set", "user": "anna", "aliases": [] })).await.unwrap();
        assert!(set(json!({ "op": "mailbox_set", "user": "ben", "address": "not an address" })).await.is_err());
        set(json!({ "op": "mailbox_set", "user": "anna", "off": true, "quota": null })).await.unwrap();
        assert_eq!(a.find_user_by_name("anna").unwrap().mailbox.unwrap(), directory::Mailbox { address: "anna@example.org".into(), aliases: Some(Vec::new()), quota: None, off: true, instance: Some("mail".into()) });

        // the settings, then every new account gets one
        let bad = json!({ "op": "mail_settings", "settings": { "template": "mail@example.org", "auto": true } });
        assert!(set(bad).await.unwrap_err().contains("{user}"));
        set(json!({ "op": "mail_settings", "settings": { "template": "{user}@{user}.ca.example.org", "make_domains": true, "quota": 5u64 << 30, "auto": true } })).await.unwrap();
        a.create_user(&root, "cyril.k", "Cyril", ROOT_OU, "a long password").await.unwrap();
        let c = a.find_user_by_name("cyril.k").unwrap();
        assert_eq!(c.mailbox, Some(directory::Mailbox { address: "cyril-k@cyril-k.ca.example.org".into(), aliases: None, quota: Some(5 << 30), off: false, instance: Some("mail".into()) }));
        set(json!({ "op": "mailbox_set", "user": "ben" })).await.unwrap();
        assert_eq!(a.find_user_by_name("ben").unwrap().mailbox.unwrap().address, "ben@ben.ca.example.org");

        // taken away (keeping Stalwart's account: nothing asked of it)
        set(json!({ "op": "mailbox_remove", "user": "ben" })).await.unwrap();
        assert!(a.find_user_by_name("ben").unwrap().mailbox.is_none());
        assert!(set(json!({ "op": "mailbox_remove", "user": "ben" })).await.unwrap_err().contains("no mailbox"));
        assert!(set(json!({ "op": "account_destroy", "id": "x" })).await.unwrap_err().contains("follow osada's mailboxes"));
    }

    /// Each idp node's own signing key (D-U45): published by the node, none for another node,
    /// withdrawn by an administrator and then a new one, never the withdrawn one again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn signing_keys_per_idp_node() {
        let dir = TempDir::new("keys");
        let file = dir.path().join("first-admin");
        let (a, b, _) = two_nodes(Some(file.clone())).await;
        let (da, db) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir(&da).unwrap();
        std::fs::create_dir(&db).unwrap();
        a.set_data(da.clone());
        b.set_data(db.clone());
        until("both keys published", || a.cluster_state().keys.len() == 2 && b.cluster_state().keys.len() == 2).await;
        let st = a.cluster_state();
        assert_eq!(st.keys[&n("home")].kid, a.idp_key().unwrap().public.kid);
        assert_eq!(st.keys[&n("edge")].kid, b.idp_key().unwrap().public.kid);
        assert_eq!(a.idp_jwks().len(), 2);
        // a node never publishes another's
        let mine = b.idp_key().unwrap().public.clone();
        let forged = b.ask(&n("home"), Call::Commit { cmd: Command::Key { node: n("home"), key: Some(mine.clone()) }, actor: "edge".into() }).await.unwrap();
        assert!(matches!(forged, Answer::Refused { .. }), "{forged:?}");
        // withdrawn: the edge makes a new one; the old one is never taken again
        until("the first admin", || file.exists()).await;
        let pw = std::fs::read_to_string(&file).unwrap().lines().find_map(|l| l.strip_prefix("password: ")).unwrap().to_string();
        let (token, s) = a.sign_in("admin", &pw, "test").await.unwrap();
        let (_, admin) = a.session(&token).unwrap();
        let root = Actor { user: admin, sid: s.sid, addr: "test".into() };
        a.withdraw_key(&root, "edge").await.unwrap();
        assert!(a.cluster_state().withdrawn.contains(&mine.kid));
        for _ in 0..200 {
            if b.idp_key().is_some_and(|k| k.public.kid != mine.kid) {
                break;
            }
            b.keep_idp_key_now().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let new = b.idp_key().unwrap().public.clone();
        assert_ne!(new.kid, mine.kid);
        until("the new key published", || a.cluster_state().keys.get(&n("edge")) == Some(&new)).await;
        assert!(a.commit("edge", Command::Key { node: n("edge"), key: Some(mine) }).is_err());
    }

    /// Sign-in for other programs at an idp node (D-U45): the code flow with PKCE, the code good
    /// once, refresh, userinfo; signing follows the secrets (the edge, which keeps none, signs
    /// nothing; its key does not count for the account); a new password revokes earlier grants.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oidc_at_an_idp_node() {
        use crate::oidc::{AuthReq, Outcome, Provider, TokenReq};
        use base64::Engine;
        let dir = TempDir::new("oidc");
        let file = dir.path().join("first-admin");
        let (a, b, _) = two_nodes(Some(file.clone())).await;
        for (x, d) in [(&a, "a"), (&b, "b")] {
            let p = dir.path().join(d);
            std::fs::create_dir(&p).unwrap();
            x.set_data(p);
        }
        until("both keys", || a.cluster_state().keys.len() == 2 && b.cluster_state().keys.len() == 2).await;
        until("the first admin", || file.exists()).await;
        let pw = std::fs::read_to_string(&file).unwrap().lines().find_map(|l| l.strip_prefix("password: ")).unwrap().to_string();
        let (token, s) = a.sign_in("admin", &pw, "test").await.unwrap();
        let (_, admin) = a.session(&token).unwrap();
        let root = Actor { user: admin, sid: s.sid, addr: "test".into() };
        a.create_user(&root, "anna", "Anna", ROOT_OU, "a long password").await.unwrap();
        let secret = "s3cret-of-the-program";
        let client = crate::oidc::Client { secret_hash: Some(hex::encode(Sha256::digest(secret))), redirect_uris: vec!["https://app.example/cb".into()], users: vec![] };
        a.commit("admin", Command::OidcClient { name: "app".into(), client: Some(client) }).unwrap();
        until("the edge to know anna and the program", || b.find_user_by_name("anna").is_some() && b.cluster_state().oidc_clients.contains_key("app")).await;
        let anna = a.find_user_by_name("anna").unwrap();
        let now_ms = super::now_ms();
        let verifier = "a-verifier-of-at-least-forty-three-characters-0123";
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier));
        let req = AuthReq {
            response_type: Some("code".into()), client_id: Some("app".into()), redirect_uri: Some("https://app.example/cb".into()),
            scope: Some("openid profile email".into()), state: Some("st".into()), nonce: Some("n1".into()),
            code_challenge: Some(challenge), code_challenge_method: Some("S256".into()), ..Default::default()
        };
        let iss = "https://osada.example";
        let pa = Provider::default();
        // not signed in: the sign-in page, then on
        let Outcome::Login { id, .. } = pa.authorize(&a, iss, req.clone(), None) else { panic!() };
        let Outcome::Redirect(back) = pa.resume(&a, iss, &id, Some((anna.clone(), now_ms))) else { panic!() };
        assert!(back.starts_with("https://app.example/cb?code=") && back.contains("&state=st") && back.contains("&iss=https%3A%2F%2Fosada.example"), "{back}");
        let code = back.split("code=").nth(1).unwrap().split('&').next().unwrap().to_string();
        // a wrong redirect URI is never followed
        let mut bad = req.clone();
        bad.redirect_uri = Some("https://evil.example/cb".into());
        assert!(matches!(pa.authorize(&a, iss, bad, Some((anna.clone(), now_ms))), Outcome::Error(_)));
        // the edge keeps no secrets: it signs nothing for anna
        let pb = Provider::default();
        let Outcome::Redirect(e) = pb.authorize(&b, iss, req.clone(), Some((anna.clone(), now_ms))) else { panic!() };
        assert!(e.contains("error=temporarily_unavailable"), "{e}");
        // the token: the secret, the verifier, once
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("app:{secret}"));
        let tr = TokenReq { grant_type: Some("authorization_code".into()), code: Some(code.clone()), redirect_uri: Some("https://app.example/cb".into()), code_verifier: Some(verifier.into()), ..Default::default() };
        let wrong = base64::engine::general_purpose::STANDARD.encode("app:wrong");
        assert_eq!(pa.token(&a, iss, Some(&wrong), &tr).unwrap_err().0, 401);
        let mut no_pkce = TokenReq { code_verifier: Some("x".repeat(43)), ..tr.clone() };
        assert_eq!(pa.token(&a, iss, Some(&basic), &no_pkce).unwrap_err().1, "invalid_grant");
        let t = pa.token(&a, iss, Some(&basic), &tr).unwrap();
        assert_eq!(pa.token(&a, iss, Some(&basic), &tr).unwrap_err().2, "the code was used already");
        // the edge can check the code (the key is in the cluster log) but signs nothing for anna
        no_pkce.code_verifier = Some(verifier.into());
        let e = pb.token(&b, iss, Some(&basic), &no_pkce).unwrap_err();
        assert_eq!(e.1, "temporarily_unavailable", "{e:?}");
        // the tokens
        let a_key = a.cluster_state().keys[&n("home")].clone();
        let id = crate::idpkey::verify(t["id_token"].as_str().unwrap(), &a_key).unwrap();
        assert_eq!((id["sub"].as_str(), id["aud"].as_str(), id["nonce"].as_str(), id["preferred_username"].as_str()), (Some(anna.id.as_str()), Some("app"), Some("n1"), Some("anna")));
        let info = pa.userinfo(&a, t["access_token"].as_str().unwrap()).unwrap();
        assert_eq!(info["sub"], anna.id.as_str());
        assert!(pa.userinfo(&a, t["id_token"].as_str().unwrap()).is_none(), "an ID token is not an access token");
        // refresh, here or at another idp node that keeps anna's secrets
        let rr = TokenReq { grant_type: Some("refresh_token".into()), refresh_token: t["refresh_token"].as_str().map(String::from), ..Default::default() };
        let t2 = pa.token(&a, iss, Some(&basic), &rr).unwrap();
        assert!(t2["access_token"].as_str().is_some() && t2.get("id_token").is_some());
        // a token signed by the edge's key does not count for anna: the edge keeps none of her secrets
        let b_key = b.idp_key().unwrap();
        let forged = b_key.sign("rt+jwt", &json!({ "cid": "app", "sub": anna.id, "scope": "openid", "at": 0, "g": "x", "exp": 9_999_999_999u64, "jti": "y" })).unwrap();
        let fr = TokenReq { refresh_token: Some(forged), ..rr.clone() };
        assert_eq!(pa.token(&a, iss, Some(&basic), &fr).unwrap_err().2, "that node does not sign for the account");
        // a new password: the earlier grant is revoked
        tokio::time::sleep(Duration::from_millis(1100)).await;
        a.change_password(&Actor { user: anna.clone(), sid: String::new(), addr: "test".into() }, "a long password", "another long password").await.unwrap();
        let e = pa.token(&a, iss, Some(&basic), &rr).unwrap_err();
        assert_eq!(e.2, "signed out since; sign in again", "{e:?}");
    }

    fn admin_user(a: &Node) -> User {
        a.users().into_iter().find(|u| u.name == "admin").unwrap()
    }
}
