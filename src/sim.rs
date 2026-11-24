//! Deterministic simulation (§19): one collection replicated among a few
//! nodes over a simulated network (delays, reordering, loss, duplicates,
//! partitions), with crashes and restarts, planned handovers and
//! promotions, all decided by one seed. A run ends by healing everything and
//! letting the members settle, then checks:
//!
//! 1. at most one node wrote in any epoch, and only the authority the
//!    cluster log named for it (checked at every write);
//! 2. no write acknowledged in `sync` mode was lost;
//! 3. the members hold one chain, which verifies;
//! 4. a planned handover lost nothing: it completes only when the new
//!    authority holds exactly the old one's chain;
//!
//! and in every mode, that no acknowledged write vanished without a trace:
//! it is in the chain or in a recovery area (§7.4).
//!
//! Simplified: the voters' fencing before a promotion is one instant round
//! trip to the members the voter can reach; disks (MemKv) survive crashes.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
    sync::Arc,
};

use crate::{
    kv::{Kv, mem::MemKv},
    log::{CollectionId, Draft, Entry, Log},
    placement::NodeId,
    repl::{Durability, Msg, Out, Replica, Term, WriteId},
};

const C: CollectionId = CollectionId([0x5e; 16]);

/// splitmix64: small, and the same everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, permille: u64) -> bool {
        self.below(1000) < permille
    }
}

#[derive(Clone)]
enum Payload {
    Repl(Msg),
    Term(Term),
}

struct Envelope {
    from: usize,
    to: usize,
    payload: Payload,
}

struct Member {
    id: NodeId,
    kv: Arc<dyn Kv>,
    /// None while crashed.
    replica: Option<Replica>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub writes: u64,
    pub acked: u64,
    pub promotions: u64,
    pub handovers: u64,
    pub crashes: u64,
    pub recovered: u64,
    pub messages: u64,
}

impl Stats {
    fn add(&mut self, o: &Stats) {
        self.writes += o.writes;
        self.acked += o.acked;
        self.promotions += o.promotions;
        self.handovers += o.handovers;
        self.crashes += o.crashes;
        self.recovered += o.recovered;
        self.messages += o.messages;
    }
}

pub struct Sim {
    seed: u64,
    rng: Rng,
    now: u64,
    members: Vec<Member>,
    /// The index of the voter, which holds the cluster log; not a member.
    voter: usize,
    net: BTreeMap<(u64, u64), Envelope>,
    sent: u64,
    blocked: BTreeSet<(usize, usize)>,
    drop_permille: u64,
    durability: Durability,
    term: Term,
    authority_of: BTreeMap<u64, usize>,
    wrote_in: BTreeMap<u64, usize>,
    acked: BTreeSet<WriteId>,
    next_write: WriteId,
    pub stats: Stats,
}

impl Sim {
    pub fn new(seed: u64, n: usize, durability: Durability) -> Sim {
        let members: Vec<Member> = (0..n)
            .map(|i| {
                let id = NodeId(format!("n{i}"));
                let kv: Arc<dyn Kv> = Arc::new(MemKv::new());
                let replica = Replica::new(id.clone(), Arc::new(Log::open(kv.clone(), C).unwrap()), durability);
                Member { id, kv, replica: Some(replica) }
            })
            .collect();
        let term = Term { epoch: 1, authority: members[0].id.clone(), members: members.iter().map(|m| m.id.clone()).collect() };
        Sim {
            seed,
            rng: Rng(seed),
            now: 0,
            voter: n,
            members,
            net: BTreeMap::new(),
            sent: 0,
            blocked: BTreeSet::new(),
            drop_permille: 20,
            durability,
            term,
            authority_of: [(1, 0)].into(),
            wrote_in: BTreeMap::new(),
            acked: BTreeSet::new(),
            next_write: 1,
            stats: Stats::default(),
        }
    }

    pub fn run(&mut self, steps: usize) {
        self.publish_term();
        for _ in 0..steps {
            self.now += 1 + self.rng.below(5);
            self.deliver();
            match self.rng.below(1000) {
                0..400 => self.client_write(),
                400..700 => self.tick_all(),
                700..740 => self.partition(),
                740..800 => self.blocked.clear(),
                800..815 => self.crash(),
                815..860 => self.restart(),
                860..875 => self.promote(),
                875..890 => self.handover(),
                _ => {}
            }
        }
        self.quiesce();
        self.check();
    }

    fn fail(&self, what: impl Display) -> ! {
        panic!("seed {} ({:?}, {} nodes), t={}: {what}", self.seed, self.durability, self.members.len(), self.now)
    }

    fn n(&self) -> usize {
        self.members.len()
    }

    fn up(&self, i: usize) -> bool {
        self.members[i].replica.is_some()
    }

    fn reaches(&self, a: usize, b: usize) -> bool {
        !self.blocked.contains(&(a, b))
    }

    fn index(&self, id: &NodeId) -> usize {
        self.members.iter().position(|m| &m.id == id).unwrap_or_else(|| self.fail(format!("unknown node {id}")))
    }

    fn pick(&mut self) -> usize {
        self.rng.below(self.n() as u64) as usize
    }

    fn send(&mut self, from: usize, to: usize, payload: Payload) {
        if self.rng.chance(self.drop_permille) {
            return;
        }
        let copies = if self.rng.chance(5) { 2 } else { 1 };
        for _ in 0..copies {
            let at = self.now + 1 + self.rng.below(20);
            self.sent += 1;
            self.stats.messages += 1;
            self.net.insert((at, self.sent), Envelope { from, to, payload: payload.clone() });
        }
    }

    fn handle(&mut self, i: usize, out: Out) {
        for (to, msg) in out.send {
            let j = self.index(&to);
            self.send(i, j, Payload::Repl(msg));
        }
        for (w, _) in out.acked {
            self.acked.insert(w);
            self.stats.acked += 1;
        }
    }

    fn deliver(&mut self) {
        while let Some(entry) = self.net.first_entry() {
            if entry.key().0 > self.now {
                break;
            }
            let env = entry.remove();
            if !self.reaches(env.from, env.to) {
                continue;
            }
            let i = env.to;
            let from = (env.from != self.voter).then(|| self.members[env.from].id.clone());
            let res = match (self.members[i].replica.as_mut(), env.payload) {
                (None, _) => continue,
                (Some(r), Payload::Term(t)) => r.on_term(t),
                (Some(r), Payload::Repl(m)) => r.on_msg(from.as_ref().expect("members send messages"), m),
            };
            match res {
                Ok(out) => self.handle(i, out),
                Err(e) => self.fail(format!("n{i}: {e}")),
            }
        }
    }

    /// The voter tells every member it reaches what the cluster log says.
    fn publish_term(&mut self) {
        for i in 0..self.n() {
            if self.up(i) {
                let t = self.term.clone();
                self.send(self.voter, i, Payload::Term(t));
            }
        }
    }

    fn tick_all(&mut self) {
        for i in 0..self.n() {
            let res = match self.members[i].replica.as_mut() {
                Some(r) => r.tick(),
                None => continue,
            };
            match res {
                Ok(out) => self.handle(i, out),
                Err(e) => self.fail(format!("n{i}: {e}")),
            }
        }
        self.publish_term();
    }

    /// A client writes at a node that takes writes; it may be a former
    /// authority that has not heard of the next epoch yet.
    fn client_write(&mut self) {
        let i = self.pick();
        let id = self.next_write;
        let draft = Draft { actor: "client".into(), module: "sim".into(), op: format!("w{id}"), payload: id.to_be_bytes().to_vec() };
        let now = self.now;
        let (epoch, res) = match self.members[i].replica.as_mut() {
            Some(r) if r.is_authority() => (r.term().map_or(0, |t| t.epoch), r.write(id, draft, now)),
            _ => return,
        };
        self.next_write += 1;
        self.stats.writes += 1;
        if self.authority_of.get(&epoch) != Some(&i) {
            self.fail(format!("n{i} wrote in epoch {epoch}, whose authority is {:?}", self.authority_of.get(&epoch)));
        }
        if *self.wrote_in.entry(epoch).or_insert(i) != i {
            self.fail(format!("two nodes wrote in epoch {epoch}"));
        }
        match res {
            Ok(out) => self.handle(i, out),
            Err(e) => self.fail(format!("n{i}: {e}")),
        }
    }

    fn block(&mut self, a: usize, b: usize) {
        self.blocked.insert((a, b));
        self.blocked.insert((b, a));
    }

    /// Cuts one link, or one node (the voter too) off from everyone.
    fn partition(&mut self) {
        let total = self.n() as u64 + 1;
        let a = self.rng.below(total) as usize;
        if self.rng.chance(500) {
            for b in 0..total as usize {
                if b != a {
                    self.block(a, b);
                }
            }
        } else {
            let b = self.rng.below(total) as usize;
            if a != b {
                self.block(a, b);
            }
        }
    }

    fn crash(&mut self) {
        let i = self.pick();
        if self.up(i) {
            self.members[i].replica = None;
            self.stats.crashes += 1;
        }
    }

    fn restart(&mut self) {
        let i = self.pick();
        if !self.up(i) {
            self.start(i);
        }
    }

    fn start(&mut self, i: usize) {
        let log = Log::open(self.members[i].kv.clone(), C).unwrap_or_else(|e| self.fail(e));
        self.members[i].replica = Some(Replica::new(self.members[i].id.clone(), Arc::new(log), self.durability));
    }

    fn commit(&mut self, epoch: u64, authority: usize) {
        self.term =
            Term { epoch, authority: self.members[authority].id.clone(), members: self.members.iter().map(|m| m.id.clone()).collect() };
        self.authority_of.insert(epoch, authority);
        self.publish_term();
    }

    /// Unplanned failover (§7.4), or an administrator's `promote`: fence
    /// the members the voter reaches (in `sync` mode, only with a majority),
    /// and give the next epoch to the most caught-up of them.
    fn promote(&mut self) {
        let n = self.n();
        let reached: Vec<usize> = (0..n).filter(|&i| self.up(i) && self.reaches(self.voter, i)).collect();
        if reached.is_empty() || (self.durability == Durability::Sync && reached.len() * 2 <= n) {
            return;
        }
        let epoch = self.term.epoch + 1;
        for &i in &reached {
            if let Err(e) = self.members[i].replica.as_mut().unwrap().fence(epoch) {
                self.fail(format!("n{i}: {e}"));
            }
        }
        let best = *reached
            .iter()
            .max_by_key(|&&i| {
                let h = self.members[i].replica.as_ref().unwrap().log().head();
                (h.epoch, h.seq)
            })
            .unwrap();
        self.commit(epoch, best);
        self.stats.promotions += 1;
    }

    /// Planned handover (§7.3) to a random member: the authority stops, the
    /// target catches up to exactly its chain, then the next epoch is
    /// committed. Called off when the target cannot catch up.
    fn handover(&mut self) {
        let a = self.authority_of[&self.term.epoch];
        let t = self.pick();
        if t == a || !self.up(a) || !self.up(t) || !self.reaches(self.voter, a) || !self.reaches(self.voter, t) {
            return;
        }
        let last = match self.members[a].replica.as_mut() {
            Some(r) if r.is_authority() => r.stop(),
            _ => return,
        };
        let want = self.members[a].replica.as_ref().unwrap().log().hash_at(last).unwrap();
        for _ in 0..100 {
            self.now += 5;
            self.tick_all();
            self.deliver();
            let h = self.members[t].replica.as_ref().unwrap().log().head();
            if h.seq == last && Some(h.hash) == want {
                let epoch = self.term.epoch + 1;
                self.commit(epoch, t);
                self.stats.handovers += 1;
                return;
            }
        }
        self.members[a].replica.as_mut().unwrap().resume();
    }

    fn quiesce(&mut self) {
        self.blocked.clear();
        self.drop_permille = 0;
        for i in 0..self.n() {
            if !self.up(i) {
                self.start(i);
            }
        }
        for _ in 0..500 {
            self.now += 25;
            self.tick_all();
            self.now += 25;
            self.deliver();
            if self.converged() {
                return;
            }
        }
        self.fail("the members did not converge");
    }

    fn converged(&self) -> bool {
        let a = self.authority_of[&self.term.epoch];
        let want = self.members[a].replica.as_ref().unwrap().log().head();
        self.members.iter().all(|m| {
            let h = m.replica.as_ref().unwrap().log().head();
            (h.seq, h.hash) == (want.seq, want.hash)
        })
    }

    fn check(&mut self) {
        let ids = |es: Vec<Entry>| -> BTreeSet<WriteId> {
            es.iter().map(|e| u64::from_be_bytes(e.payload[..8].try_into().unwrap())).collect()
        };
        let a = self.authority_of[&self.term.epoch];
        let auth = self.members[a].replica.as_ref().unwrap().log();
        let head = auth.verify().unwrap_or_else(|e| self.fail(e));
        let chain = ids(auth.read(0, usize::MAX).unwrap_or_else(|e| self.fail(e)));
        let mut recovered = BTreeSet::new();
        for m in &self.members {
            let log = m.replica.as_ref().unwrap().log();
            if log.verify().unwrap_or_else(|e| self.fail(format!("{}: {e}", m.id))) != head {
                self.fail(format!("{} holds another chain", m.id));
            }
            recovered.extend(ids(log.recovered().unwrap_or_else(|e| self.fail(e))));
        }
        for &w in &self.acked {
            if chain.contains(&w) {
                continue;
            }
            if self.durability == Durability::Sync {
                self.fail(format!("sync write {w} was acknowledged and lost"));
            }
            if !recovered.contains(&w) {
                self.fail(format!("write {w} was acknowledged and vanished"));
            }
        }
        self.stats.recovered = recovered.len() as u64;
    }
}

fn sweep(n: usize, durability: Durability, seeds: std::ops::Range<u64>) -> Stats {
    let mut total = Stats::default();
    for seed in seeds {
        let mut sim = Sim::new(seed, n, durability);
        sim.run(2000);
        total.add(&sim.stats);
    }
    total
}

#[test]
fn one_node() {
    let s = sweep(1, Durability::Local, 0..20);
    assert!(s.acked > 0, "{s:?}");
}

#[test]
fn two_nodes_async() {
    let s = sweep(2, Durability::Async, 0..80);
    assert!(s.promotions > 0 && s.handovers > 0 && s.crashes > 0 && s.recovered > 0, "{s:?}");
}

#[test]
fn three_nodes_async() {
    let s = sweep(3, Durability::Async, 0..80);
    assert!(s.promotions > 0 && s.handovers > 0 && s.recovered > 0, "{s:?}");
}

#[test]
fn three_nodes_sync() {
    let s = sweep(3, Durability::Sync, 0..80);
    assert!(s.promotions > 0 && s.handovers > 0 && s.acked > 0, "{s:?}");
}

#[test]
fn five_nodes_sync() {
    let s = sweep(5, Durability::Sync, 0..40);
    assert!(s.promotions > 0 && s.handovers > 0 && s.acked > 0, "{s:?}");
}

#[test]
fn a_seed_replays_exactly() {
    let run = || {
        let mut sim = Sim::new(7, 3, Durability::Async);
        sim.run(1500);
        sim.stats
    };
    assert_eq!(run(), run());
}
