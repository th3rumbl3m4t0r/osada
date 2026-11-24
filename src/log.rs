//! Collection logs (§5.2).
//!
//! Every collection is an append-only chain of entries. Only its authority
//! appends ([`Log::append_all`]); replicas take the authority's entries in
//! order ([`Log::accept_all`]). Each entry carries the hash of the one before
//! it, so two copies hold the same history exactly when their head hashes are
//! equal, and an entry changed on the way breaks the chain at the next one.
//!
//! Epochs fence a former authority (§7.2): [`Log::fence`] raises the lowest
//! epoch the log takes. After that nothing is appended in an older epoch, and
//! nothing is accepted from the authority of one. Along the chain, epochs
//! never go down.
//!
//! When a former authority comes back with entries the new one never had
//! (§7.4), [`Log::cut_after`] takes them off the end and keeps them in the
//! collection's recovery area, to be applied again or shown to an
//! administrator. Only entries from below the fence can be cut.
//!
//! One [`Log`] per collection on a node: it keeps the head in memory and puts
//! the writers to it in a row. Keys in the store, under the collection's
//! prefix `c` + its 16-byte id:
//!
//! ```text
//! h                         the head: epoch, seq, hash, fence
//! k                         the checkpoint pruning left: seq, epoch, hash
//! e + seq                   an entry
//! r + epoch + seq + hash    an entry cut off, in the recovery area
//! ```
//!
//! (numbers big-endian u64). Tables derived from the collection go under
//! other letters of the same prefix.

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use sha2::{Digest, Sha256};

use crate::kv::{self, Batch, Kv};

pub type Hash = [u8; 32];

/// Entries are meant to be small: file contents and mail bodies go to the
/// blob store (§9), and entries refer to them.
pub const MAX_PAYLOAD: usize = 16 << 20;
/// For `actor`, `module` and `op`.
pub const MAX_NAME: usize = 1024;
/// The encoding of entries, first byte of every encoded entry. Anything that
/// changes the bytes of an entry, or what is hashed, needs a new number, and
/// the old one must stay readable (§18).
pub const FORMAT: u8 = 1;

const GENESIS_DOMAIN: &[u8] = b"shelf.log.genesis\0";
const ENTRY_DOMAIN: &[u8] = b"shelf.log.entry\0";

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CollectionId(pub [u8; 16]);

impl CollectionId {
    pub fn parse(s: &str) -> Option<Self> {
        let v = hex::decode(s).ok()?;
        Some(CollectionId(v.try_into().ok()?))
    }

    /// The `prev` of the first entry. It is the collection's own, so a
    /// chain cannot be passed off as another collection's.
    pub fn genesis(&self) -> Hash {
        let mut h = Sha256::new();
        h.update(GENESIS_DOMAIN);
        h.update(self.0);
        h.finalize().into()
    }

    /// Where every key of the collection begins in the store.
    pub fn key_prefix(&self) -> Vec<u8> {
        self.prefix()
    }

    fn prefix(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(17);
        p.push(b'c');
        p.extend_from_slice(&self.0);
        p
    }
}

impl fmt::Display for CollectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl serde::Serialize for CollectionId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for CollectionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        CollectionId::parse(&s).ok_or_else(|| serde::de::Error::custom("not a collection id: 32 hex digits"))
    }
}

impl fmt::Debug for CollectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CollectionId({self})")
    }
}

/// What the authority wants written; the log gives it its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub actor: String,
    pub module: String,
    pub op: String,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub collection: CollectionId,
    /// The authority's epoch when it wrote this.
    pub epoch: u64,
    /// 1 for the first entry, one more for each after it.
    pub seq: u64,
    /// Milliseconds since 1970 on the authority's clock. Only informative:
    /// order is `seq`.
    pub time: u64,
    /// Who: `user:<id>`, `node:<id>`, `module:<name>`.
    pub actor: String,
    /// The module that owns the operation, and the operation, which it reads.
    pub module: String,
    pub op: String,
    pub payload: Vec<u8>,
    /// The hash of the entry before; for the first, the collection's genesis.
    pub prev: Hash,
}

impl Entry {
    /// The bytes that are stored, sent and hashed: FORMAT, the collection,
    /// epoch, seq and time as big-endian u64, actor, module, op and payload
    /// each as a big-endian u32 length and its bytes, then prev.
    pub fn encode(&self) -> Vec<u8> {
        let texts = [self.actor.as_bytes(), self.module.as_bytes(), self.op.as_bytes(), &self.payload];
        let mut b = Vec::with_capacity(1 + 16 + 24 + 32 + texts.iter().map(|t| 4 + t.len()).sum::<usize>());
        b.push(FORMAT);
        b.extend_from_slice(&self.collection.0);
        for n in [self.epoch, self.seq, self.time] {
            b.extend_from_slice(&n.to_be_bytes());
        }
        for t in texts {
            b.extend_from_slice(&(t.len() as u32).to_be_bytes());
            b.extend_from_slice(t);
        }
        b.extend_from_slice(&self.prev);
        b
    }

    pub fn decode(bytes: &[u8]) -> Result<Entry, LogError> {
        let mut r = Reader(bytes);
        let format = r.take(1)?[0];
        if format != FORMAT {
            return Err(corrupt(format!("entry format {format}, this binary reads {FORMAT}")));
        }
        let e = Entry {
            collection: CollectionId(r.array()?),
            epoch: r.u64()?,
            seq: r.u64()?,
            time: r.u64()?,
            actor: r.text()?,
            module: r.text()?,
            op: r.text()?,
            payload: r.bytes(MAX_PAYLOAD)?.to_vec(),
            prev: r.array()?,
        };
        if !r.0.is_empty() {
            return Err(corrupt("bytes after the entry"));
        }
        Ok(e)
    }

    pub fn hash(&self) -> Hash {
        hash_encoded(&self.encode())
    }

    fn check_sizes(&self) -> Result<(), LogError> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(LogError::TooLarge("payload"));
        }
        for (what, s) in [("actor", &self.actor), ("module", &self.module), ("op", &self.op)] {
            if s.len() > MAX_NAME {
                return Err(LogError::TooLarge(what));
            }
        }
        Ok(())
    }
}

fn hash_encoded(encoded: &[u8]) -> Hash {
    let mut h = Sha256::new();
    h.update(ENTRY_DOMAIN);
    h.update(encoded);
    h.finalize().into()
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], LogError> {
        if self.0.len() < n {
            return Err(corrupt("entry cut short"));
        }
        let (a, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(a)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], LogError> {
        Ok(self.take(N)?.try_into().expect("took N bytes"))
    }

    fn u64(&mut self) -> Result<u64, LogError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn bytes(&mut self, max: usize) -> Result<&'a [u8], LogError> {
        let n = u32::from_be_bytes(self.array()?) as usize;
        if n > max {
            return Err(corrupt("field over its limit"));
        }
        self.take(n)
    }

    fn text(&mut self) -> Result<String, LogError> {
        String::from_utf8(self.bytes(MAX_NAME)?.to_vec()).map_err(|_| corrupt("text not UTF-8"))
    }
}

/// Where a log stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    /// Epoch of the last entry (0: no entries).
    pub epoch: u64,
    /// Seq of the last entry (0: no entries).
    pub seq: u64,
    /// Hash of the last entry; with no entries, the genesis.
    pub hash: Hash,
    /// The lowest epoch the log takes.
    pub fence: u64,
}

impl Head {
    fn empty(id: CollectionId) -> Head {
        Head { epoch: 0, seq: 0, hash: id.genesis(), fence: 0 }
    }

    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(56);
        b.extend_from_slice(&self.epoch.to_be_bytes());
        b.extend_from_slice(&self.seq.to_be_bytes());
        b.extend_from_slice(&self.hash);
        b.extend_from_slice(&self.fence.to_be_bytes());
        b
    }

    fn decode(bytes: &[u8]) -> Result<Head, LogError> {
        let mut r = Reader(bytes);
        let h = Head { epoch: r.u64()?, seq: r.u64()?, hash: r.array()?, fence: r.u64()? };
        if !r.0.is_empty() {
            return Err(corrupt("bytes after the head"));
        }
        Ok(h)
    }
}

/// Where the entries a log holds begin: the last entry pruning removed,
/// from which the chain is verified. Before any pruning, seq 0 and the
/// genesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub seq: u64,
    pub epoch: u64,
    pub hash: Hash,
}

impl Checkpoint {
    fn genesis(id: CollectionId) -> Checkpoint {
        Checkpoint { seq: 0, epoch: 0, hash: id.genesis() }
    }

    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(48);
        b.extend_from_slice(&self.seq.to_be_bytes());
        b.extend_from_slice(&self.epoch.to_be_bytes());
        b.extend_from_slice(&self.hash);
        b
    }

    fn decode(bytes: &[u8]) -> Result<Checkpoint, LogError> {
        let mut r = Reader(bytes);
        let c = Checkpoint { seq: r.u64()?, epoch: r.u64()?, hash: r.array()? };
        if !r.0.is_empty() {
            return Err(corrupt("bytes after the checkpoint"));
        }
        Ok(c)
    }
}

#[derive(Debug)]
pub enum LogError {
    Store(kv::Error),
    Corrupt(String),
    /// The epoch is below the log's fence: it is a former authority's.
    Fenced { epoch: u64, fence: u64 },
    /// The epoch is below that of the log's last entry.
    EpochBehind { epoch: u64, head: u64 },
    /// The entry would leave a hole: the log ends at `head`.
    Gap { head: u64, seq: u64 },
    /// The entry does not continue this log: at `seq` this log holds, or
    /// leads to, something else. The two histories have parted.
    Diverged { seq: u64 },
    /// Cutting would take an entry from the current epoch or later, which
    /// is not a former authority's.
    NotSuperseded { seq: u64, epoch: u64, fence: u64 },
    /// The entry is another collection's.
    OtherCollection,
    TooLarge(&'static str),
    /// Entries up to `start` were pruned; `seq` is before that. A client or
    /// replica that far behind starts over from a snapshot.
    Pruned { seq: u64, start: u64 },
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogError::Store(e) => write!(f, "{e}"),
            LogError::Corrupt(m) => write!(f, "log corrupt: {m}"),
            LogError::Fenced { epoch, fence } => write!(f, "epoch {epoch} is fenced, the log takes {fence} and later"),
            LogError::EpochBehind { epoch, head } => write!(f, "epoch {epoch} is behind the log's {head}"),
            LogError::Gap { head, seq } => write!(f, "entry {seq} after {head} leaves a gap"),
            LogError::Diverged { seq } => write!(f, "histories differ at entry {seq}"),
            LogError::NotSuperseded { seq, epoch, fence } => {
                write!(f, "entry {seq} is from epoch {epoch}, not below the fence {fence}; it cannot be cut")
            }
            LogError::OtherCollection => write!(f, "entry of another collection"),
            LogError::TooLarge(what) => write!(f, "{what} too large"),
            LogError::Pruned { seq, start } => write!(f, "entries up to {start} were pruned; {seq} is before that"),
        }
    }
}

impl std::error::Error for LogError {}

impl From<kv::Error> for LogError {
    fn from(e: kv::Error) -> Self {
        LogError::Store(e)
    }
}

fn corrupt(m: impl Into<String>) -> LogError {
    LogError::Corrupt(m.into())
}

fn check_epoch(head: &Head, epoch: u64) -> Result<(), LogError> {
    if epoch < head.fence {
        Err(LogError::Fenced { epoch, fence: head.fence })
    } else if epoch < head.epoch {
        Err(LogError::EpochBehind { epoch, head: head.epoch })
    } else {
        Ok(())
    }
}

struct State {
    head: Head,
    start: Checkpoint,
}

pub struct Log {
    kv: Arc<dyn Kv>,
    id: CollectionId,
    prefix: Vec<u8>,
    state: Mutex<State>,
}

impl Log {
    pub fn open(kv: Arc<dyn Kv>, id: CollectionId) -> Result<Log, LogError> {
        let prefix = id.prefix();
        let head = match kv.get(&key(&prefix, b'h', &[]))? {
            Some(b) => Head::decode(&b)?,
            None => Head::empty(id),
        };
        let start = match kv.get(&key(&prefix, b'k', &[]))? {
            Some(b) => Checkpoint::decode(&b)?,
            None => Checkpoint::genesis(id),
        };
        Ok(Log { kv, id, prefix, state: Mutex::new(State { head, start }) })
    }

    pub fn id(&self) -> CollectionId {
        self.id
    }

    pub fn head(&self) -> Head {
        self.lock().head
    }

    /// Where the entries this log holds begin.
    pub fn start(&self) -> Checkpoint {
        self.lock().start
    }

    /// From now on, take nothing from an epoch below `epoch`. The fence
    /// only goes up.
    pub fn fence(&self, epoch: u64) -> Result<(), LogError> {
        let mut st = self.lock();
        if epoch <= st.head.fence {
            return Ok(());
        }
        let next = Head { fence: epoch, ..st.head };
        let mut b = Batch::new();
        b.put(self.head_key(), next.encode());
        self.kv.write(b)?;
        st.head = next;
        Ok(())
    }

    pub fn append(&self, epoch: u64, time: u64, draft: Draft) -> Result<Entry, LogError> {
        Ok(self.append_all(epoch, time, vec![draft])?.pop().expect("one draft, one entry"))
    }

    /// As the authority in `epoch`: writes the drafts as the next entries,
    /// all of them or none, and returns them.
    pub fn append_all(&self, epoch: u64, time: u64, drafts: Vec<Draft>) -> Result<Vec<Entry>, LogError> {
        let mut st = self.lock();
        check_epoch(&st.head, epoch)?;
        let mut next = st.head;
        let mut batch = Batch::new();
        let mut out = Vec::with_capacity(drafts.len());
        for d in drafts {
            let e = Entry {
                collection: self.id,
                epoch,
                seq: next.seq + 1,
                time,
                actor: d.actor,
                module: d.module,
                op: d.op,
                payload: d.payload,
                prev: next.hash,
            };
            e.check_sizes()?;
            let enc = e.encode();
            next = Head { epoch, seq: e.seq, hash: hash_encoded(&enc), fence: next.fence };
            batch.put(self.entry_key(e.seq), enc);
            out.push(e);
        }
        if out.is_empty() {
            return Ok(out);
        }
        batch.put(self.head_key(), next.encode());
        self.kv.write(batch)?;
        st.head = next;
        Ok(out)
    }

    pub fn accept(&self, epoch: u64, entry: Entry) -> Result<(), LogError> {
        self.accept_all(epoch, vec![entry])
    }

    /// As a replica: takes, in order, all or none, the entries the authority
    /// of `epoch` sends. The fence rises to `epoch`; a sender from below the
    /// fence is a former authority and is refused. The entries themselves may
    /// be from earlier epochs: a new authority passes on what it holds of its
    /// predecessors. Entries this log already holds are passed over, so a
    /// repeated delivery does no harm; a different entry where this log has
    /// one is [`LogError::Diverged`].
    pub fn accept_all(&self, epoch: u64, entries: Vec<Entry>) -> Result<(), LogError> {
        let mut st = self.lock();
        let (head, start) = (st.head, st.start);
        if epoch < head.fence {
            return Err(LogError::Fenced { epoch, fence: head.fence });
        }
        let mut next = Head { fence: epoch, ..head };
        let mut batch = Batch::new();
        // Hashes of the entries taken in this call, for seq head.seq + 1 on.
        let mut taken: Vec<Hash> = Vec::new();
        for e in entries {
            if e.collection != self.id {
                return Err(LogError::OtherCollection);
            }
            if e.seq == 0 {
                return Err(corrupt("entry with seq 0"));
            }
            e.check_sizes()?;
            if e.seq <= next.seq {
                let have = if e.seq <= head.seq {
                    match self.hash_from(e.seq, &start)? {
                        Some(h) => h,
                        None if e.seq < start.seq => return Err(LogError::Pruned { seq: e.seq, start: start.seq }),
                        None => return Err(corrupt(format!("entry {} missing", e.seq))),
                    }
                } else {
                    taken[(e.seq - head.seq - 1) as usize]
                };
                if have != e.hash() {
                    return Err(LogError::Diverged { seq: e.seq });
                }
                continue;
            }
            if e.seq != next.seq + 1 {
                return Err(LogError::Gap { head: next.seq, seq: e.seq });
            }
            if e.epoch > epoch {
                return Err(corrupt(format!("entry {} is from epoch {}, after its sender's {epoch}", e.seq, e.epoch)));
            }
            if e.epoch < next.epoch {
                return Err(LogError::EpochBehind { epoch: e.epoch, head: next.epoch });
            }
            if e.prev != next.hash {
                return Err(LogError::Diverged { seq: e.seq });
            }
            let enc = e.encode();
            let hash = hash_encoded(&enc);
            batch.put(self.entry_key(e.seq), enc);
            taken.push(hash);
            next = Head { epoch: e.epoch, seq: e.seq, hash, fence: next.fence };
        }
        if next == head {
            return Ok(());
        }
        batch.put(self.head_key(), next.encode());
        self.kv.write(batch)?;
        st.head = next;
        Ok(())
    }

    /// Up to `limit` entries after seq `after`, in order: what changed since
    /// a client's state (§5.3), or what a replica is missing. From before
    /// the checkpoint, [`LogError::Pruned`]: the range is no longer whole.
    pub fn read(&self, after: u64, limit: usize) -> Result<Vec<Entry>, LogError> {
        let start = self.start();
        if after < start.seq {
            return Err(LogError::Pruned { seq: after, start: start.seq });
        }
        self.entries(after, limit)
    }

    /// Entry `seq`, if the log holds it (not once pruned).
    pub fn get(&self, seq: u64) -> Result<Option<Entry>, LogError> {
        self.entry(seq)
    }

    /// The hash of entry `seq`: of the checkpoint for its seq, the genesis
    /// for 0, nothing for what was pruned before. Two logs agree up to the
    /// last seq where these are equal.
    pub fn hash_at(&self, seq: u64) -> Result<Option<Hash>, LogError> {
        let start = self.start();
        self.hash_from(seq, &start)
    }

    /// Takes every entry after `seq` off the end, into the recovery area
    /// (§7.4), and returns them in order; the log then ends at `seq`. Every
    /// entry cut must be from an epoch below the fence: fence first, at the
    /// new authority's epoch, then cut.
    pub fn cut_after(&self, seq: u64) -> Result<Vec<Entry>, LogError> {
        let mut st = self.lock();
        if seq >= st.head.seq {
            return Ok(Vec::new());
        }
        if seq < st.start.seq {
            return Err(LogError::Pruned { seq, start: st.start.seq });
        }
        let fence = st.head.fence;
        let cut = self.entries(seq, usize::MAX)?;
        let mut next = Head { epoch: st.start.epoch, seq: st.start.seq, hash: st.start.hash, fence };
        if seq > st.start.seq {
            let last = self.entry(seq)?.ok_or_else(|| corrupt(format!("entry {seq} missing")))?;
            next = Head { epoch: last.epoch, seq, hash: last.hash(), fence };
        }
        let mut batch = Batch::new();
        for e in &cut {
            if e.epoch >= fence {
                return Err(LogError::NotSuperseded { seq: e.seq, epoch: e.epoch, fence });
            }
            let enc = e.encode();
            let mut rk = key(&self.prefix, b'r', &e.epoch.to_be_bytes());
            rk.extend_from_slice(&e.seq.to_be_bytes());
            rk.extend_from_slice(&hash_encoded(&enc));
            batch.delete(self.entry_key(e.seq));
            batch.put(rk, enc);
        }
        batch.put(self.head_key(), next.encode());
        self.kv.write(batch)?;
        st.head = next;
        Ok(cut)
    }

    /// Removes the entries up to and including `seq` (at most the head) from
    /// the front, for retention (§16.3) and compaction (§5.3). The last one
    /// removed becomes the checkpoint the chain is verified from. Returns how
    /// many were removed.
    pub fn prune_through(&self, seq: u64) -> Result<u64, LogError> {
        let mut st = self.lock();
        let seq = seq.min(st.head.seq);
        if seq <= st.start.seq {
            return Ok(0);
        }
        let cp = if seq == st.head.seq {
            Checkpoint { seq, epoch: st.head.epoch, hash: st.head.hash }
        } else {
            let e = self.entry(seq)?.ok_or_else(|| corrupt(format!("entry {seq} missing")))?;
            Checkpoint { seq, epoch: e.epoch, hash: e.hash() }
        };
        let gone = self.kv.scan(
            &self.entry_key(st.start.seq + 1),
            Some(&self.entry_key(seq.saturating_add(1))),
            usize::MAX,
        )?;
        let mut batch = Batch::new();
        for (k, _) in &gone {
            batch.delete(k.clone());
        }
        batch.put(self.start_key(), cp.encode());
        self.kv.write(batch)?;
        st.start = cp;
        Ok(gone.len() as u64)
    }

    /// Removes from the front every entry written before `cutoff`
    /// (milliseconds since 1970), up to the first that was not.
    pub fn prune_older_than(&self, cutoff: u64) -> Result<u64, LogError> {
        let mut last = self.start().seq;
        'scan: loop {
            let entries = self.entries(last, 1024)?;
            if entries.is_empty() {
                break;
            }
            for e in entries {
                if e.time >= cutoff {
                    break 'scan;
                }
                last = e.seq;
            }
        }
        self.prune_through(last)
    }

    /// What was ever cut off this log, by epoch and seq.
    pub fn recovered(&self) -> Result<Vec<Entry>, LogError> {
        let rows = kv::scan_prefix(&*self.kv, &key(&self.prefix, b'r', &[]), usize::MAX)?;
        rows.iter().map(|(_, v)| Entry::decode(v)).collect()
    }

    /// Walks the chain from the checkpoint and checks every link and the
    /// head. Returns the head's hash.
    pub fn verify(&self) -> Result<Hash, LogError> {
        let st = self.lock();
        let (mut seq, mut epoch, mut hash) = (st.start.seq, st.start.epoch, st.start.hash);
        loop {
            let entries = self.entries(seq, 1024)?;
            if entries.is_empty() {
                break;
            }
            for e in entries {
                if e.seq != seq + 1 || e.prev != hash || e.epoch < epoch {
                    return Err(corrupt(format!("chain broken at entry {}", e.seq)));
                }
                (seq, epoch, hash) = (e.seq, e.epoch, e.hash());
            }
        }
        if seq != st.head.seq || hash != st.head.hash || epoch != st.head.epoch {
            return Err(corrupt(format!("head says entry {}, the chain ends at {seq}", st.head.seq)));
        }
        Ok(hash)
    }

    fn entries(&self, after: u64, limit: usize) -> Result<Vec<Entry>, LogError> {
        if after == u64::MAX {
            return Ok(Vec::new());
        }
        let start = self.entry_key(after + 1);
        let end = key(&self.prefix, b'e' + 1, &[]);
        let rows = self.kv.scan(&start, Some(&end), limit)?;
        let mut out = Vec::with_capacity(rows.len());
        for (k, v) in rows {
            let e = Entry::decode(&v)?;
            if e.collection != self.id || k != self.entry_key(e.seq) {
                return Err(corrupt(format!("entry stored under the wrong key: {}", hex::encode(&k))));
            }
            out.push(e);
        }
        Ok(out)
    }

    fn entry(&self, seq: u64) -> Result<Option<Entry>, LogError> {
        if seq == 0 {
            return Ok(None);
        }
        Ok(self.entries(seq - 1, 1)?.into_iter().find(|e| e.seq == seq))
    }

    fn hash_from(&self, seq: u64, start: &Checkpoint) -> Result<Option<Hash>, LogError> {
        if seq < start.seq {
            return Ok(None);
        }
        if seq == start.seq {
            return Ok(Some(start.hash));
        }
        Ok(self.entry(seq)?.map(|e| e.hash()))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn head_key(&self) -> Vec<u8> {
        key(&self.prefix, b'h', &[])
    }

    fn start_key(&self) -> Vec<u8> {
        key(&self.prefix, b'k', &[])
    }

    fn entry_key(&self, seq: u64) -> Vec<u8> {
        key(&self.prefix, b'e', &seq.to_be_bytes())
    }
}

fn key(prefix: &[u8], kind: u8, rest: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(prefix.len() + 1 + rest.len());
    k.extend_from_slice(prefix);
    k.push(kind);
    k.extend_from_slice(rest);
    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        kv::{mem::MemKv, sqlite::SqliteKv},
        testutil::TempDir,
    };

    const C: CollectionId = CollectionId([7; 16]);

    fn store() -> Arc<dyn Kv> {
        Arc::new(MemKv::new())
    }

    fn open() -> Log {
        Log::open(store(), C).unwrap()
    }

    fn d(op: &str) -> Draft {
        Draft { actor: "user:test".into(), module: "test".into(), op: op.into(), payload: op.as_bytes().to_vec() }
    }

    fn ops(es: &[Entry]) -> Vec<&str> {
        es.iter().map(|e| e.op.as_str()).collect()
    }

    /// Pinned against an independent implementation (Python, hashlib), so
    /// that the stored and hashed bytes of an entry cannot change unnoticed.
    #[test]
    fn format_is_pinned() {
        let c = CollectionId(core::array::from_fn(|i| i as u8));
        assert_eq!(hex::encode(c.genesis()), "c33a491063c41401926b1847656c25c8bf881a09d938b86d461188ea7ebc514e");
        let e = Entry {
            collection: c,
            epoch: 1,
            seq: 1,
            time: 1_700_000_000_000,
            actor: "user:test".into(),
            module: "files".into(),
            op: "mkdir".into(),
            payload: b"/Photos".to_vec(),
            prev: c.genesis(),
        };
        assert_eq!(e.encode().len(), 115);
        assert_eq!(hex::encode(e.hash()), "cb4c49c3700ec1820c64cb5e751fb0e145db411b7b227187010a20ed28ae8278");
    }

    #[test]
    fn decode_refuses_damage() {
        let e = open().append(1, 5, d("a")).unwrap();
        let enc = e.encode();
        assert_eq!(Entry::decode(&enc).unwrap(), e);
        let mut longer = enc.clone();
        longer.push(0);
        assert!(Entry::decode(&longer).is_err());
        assert!(Entry::decode(&enc[..enc.len() - 1]).is_err());
        let mut other = enc.clone();
        other[0] = FORMAT + 1;
        assert!(Entry::decode(&other).is_err());
        assert_eq!(CollectionId::parse(&C.to_string()), Some(C));
    }

    #[test]
    fn appends_chain_and_reopen() {
        let kv = store();
        let log = Log::open(kv.clone(), C).unwrap();
        assert_eq!(log.head(), Head { epoch: 0, seq: 0, hash: C.genesis(), fence: 0 });
        let a = log.append(1, 100, d("a")).unwrap();
        let bc = log.append_all(1, 101, vec![d("b"), d("c")]).unwrap();
        assert_eq!((a.seq, bc[0].seq, bc[1].seq), (1, 2, 3));
        assert_eq!(a.prev, C.genesis());
        assert_eq!(bc[0].prev, a.hash());
        assert_eq!(log.verify().unwrap(), bc[1].hash());
        assert_eq!(ops(&log.read(0, 10).unwrap()), ["a", "b", "c"]);
        assert_eq!(ops(&log.read(2, 10).unwrap()), ["c"]);
        assert_eq!(ops(&log.read(0, 2).unwrap()), ["a", "b"]);
        assert!(log.read(3, 10).unwrap().is_empty());
        assert_eq!(log.get(2).unwrap(), Some(bc[0].clone()));
        assert!(log.append_all(1, 102, vec![]).unwrap().is_empty());
        drop(log);
        let log = Log::open(kv, C).unwrap();
        assert_eq!(log.head().hash, bc[1].hash());
        assert_eq!(log.append(1, 103, d("d")).unwrap().seq, 4);
        log.verify().unwrap();
    }

    #[test]
    fn collections_do_not_mix() {
        let kv = store();
        let a = Log::open(kv.clone(), C).unwrap();
        let b = Log::open(kv, CollectionId([8; 16])).unwrap();
        a.append_all(1, 1, vec![d("a1"), d("a2")]).unwrap();
        b.append(1, 1, d("b1")).unwrap();
        assert_eq!(ops(&a.read(0, 10).unwrap()), ["a1", "a2"]);
        assert_eq!(ops(&b.read(0, 10).unwrap()), ["b1"]);
        assert_ne!(C.genesis(), b.id().genesis());
        assert!(matches!(b.accept(1, a.get(1).unwrap().unwrap()), Err(LogError::OtherCollection)));
    }

    #[test]
    fn replica_follows_and_takes_repeats() {
        let (auth, rep) = (open(), open());
        auth.append_all(1, 1, vec![d("a"), d("b"), d("c")]).unwrap();
        let es = auth.read(0, 10).unwrap();
        rep.accept_all(1, es[..2].to_vec()).unwrap();
        rep.accept_all(1, es.clone()).unwrap();
        assert_eq!((rep.head().seq, rep.head().hash), (auth.head().seq, auth.head().hash));
        rep.accept(1, es[0].clone()).unwrap();
        rep.accept_all(1, vec![es[2].clone(), es[2].clone()]).unwrap();
        assert_eq!((rep.head().seq, rep.head().hash), (auth.head().seq, auth.head().hash));
        assert_eq!(rep.verify().unwrap(), auth.verify().unwrap());
    }

    #[test]
    fn gap_refused_whole_batch_or_nothing() {
        let (auth, rep) = (open(), open());
        auth.append_all(1, 1, vec![d("a"), d("b"), d("c")]).unwrap();
        let es = auth.read(0, 10).unwrap();
        assert!(matches!(rep.accept_all(1, vec![es[0].clone(), es[2].clone()]), Err(LogError::Gap { head: 1, seq: 3 })));
        assert_eq!(rep.head().seq, 0);
        assert!(rep.read(0, 10).unwrap().is_empty());
    }

    #[test]
    fn changed_entry_breaks_the_chain() {
        let (auth, rep) = (open(), open());
        auth.append_all(1, 1, vec![d("a"), d("b"), d("c")]).unwrap();
        let mut es = auth.read(0, 10).unwrap();
        es[1].payload = b"forged".to_vec();
        // The changed entry still links to entry 1, so it gets in; the head
        // hashes now differ, and the next true entry no longer fits.
        rep.accept_all(1, es[..2].to_vec()).unwrap();
        assert_ne!(Some(rep.head().hash), auth.hash_at(2).unwrap());
        assert!(matches!(rep.accept(1, es[2].clone()), Err(LogError::Diverged { seq: 3 })));
        // A changed copy of an entry the replica holds is refused outright.
        let mut again = auth.get(1).unwrap().unwrap();
        again.payload = b"x".to_vec();
        assert!(matches!(rep.accept(1, again), Err(LogError::Diverged { seq: 1 })));
    }

    #[test]
    fn fence_keeps_a_former_authority_out() {
        let log = open();
        log.append(1, 1, d("a")).unwrap();
        log.fence(2).unwrap();
        log.fence(1).unwrap();
        assert_eq!(log.head().fence, 2);
        assert!(matches!(log.append(1, 2, d("b")), Err(LogError::Fenced { epoch: 1, fence: 2 })));
        assert_eq!(log.append(2, 2, d("b")).unwrap().seq, 2);

        let (old, rep) = (open(), open());
        old.append(1, 1, d("a")).unwrap();
        rep.fence(2).unwrap();
        assert!(matches!(rep.accept(1, old.get(1).unwrap().unwrap()), Err(LogError::Fenced { .. })));

        let kv = store();
        Log::open(kv.clone(), C).unwrap().fence(5).unwrap();
        assert_eq!(Log::open(kv, C).unwrap().head().fence, 5);
    }

    #[test]
    fn epoch_never_goes_back() {
        let log = open();
        log.append(2, 1, d("a")).unwrap();
        assert!(matches!(log.append(1, 2, d("b")), Err(LogError::EpochBehind { epoch: 1, head: 2 })));
    }

    /// §7.4: A is the authority in epoch 1 and writes 1 to 5; its replica B
    /// has 1 to 3 when A goes away. B is promoted (epoch 2) and writes on.
    /// A comes back: it is fenced, finds where the histories part, cuts its
    /// tail into the recovery area and follows B.
    #[test]
    fn divergent_tail_is_cut_and_kept() {
        let (a, b) = (open(), open());
        a.append_all(1, 1, (1..=5).map(|i| d(&format!("a{i}"))).collect()).unwrap();
        b.accept_all(1, a.read(0, 3).unwrap()).unwrap();

        b.fence(2).unwrap();
        b.append_all(2, 2, vec![d("b4"), d("b5"), d("b6")]).unwrap();

        a.fence(2).unwrap();
        assert!(matches!(a.append(1, 3, d("late")), Err(LogError::Fenced { .. })));
        let from_b = b.read(3, 10).unwrap();
        assert!(matches!(a.accept_all(2, from_b.clone()), Err(LogError::Diverged { seq: 4 })));

        let common = (0..=a.head().seq).rev().find(|&s| a.hash_at(s).unwrap() == b.hash_at(s).unwrap()).unwrap();
        assert_eq!(common, 3);
        let cut = a.cut_after(common).unwrap();
        assert_eq!(ops(&cut), ["a4", "a5"]);
        assert_eq!(a.head().seq, 3);
        a.accept_all(2, from_b).unwrap();
        assert_eq!(a.head(), b.head());
        assert_eq!(a.verify().unwrap(), b.verify().unwrap());
        assert_eq!(a.recovered().unwrap(), cut);
    }

    #[test]
    fn current_epoch_is_never_cut() {
        let log = open();
        log.append_all(1, 1, vec![d("a"), d("b")]).unwrap();
        assert!(matches!(log.cut_after(1), Err(LogError::NotSuperseded { seq: 2, epoch: 1, fence: 0 })));
        assert_eq!(log.head().seq, 2);
        log.fence(2).unwrap();
        assert_eq!(ops(&log.cut_after(0).unwrap()), ["a", "b"]);
        assert_eq!(log.head(), Head { fence: 2, ..Head::empty(C) });
        log.verify().unwrap();
    }

    #[test]
    fn on_disk_across_reopen() {
        let dir = TempDir::new("log");
        let path = dir.path().join("store.db");
        let hash = {
            let log = Log::open(Arc::new(SqliteKv::open(&path).unwrap()), C).unwrap();
            log.append_all(3, 1, vec![d("a"), d("b")]).unwrap();
            log.fence(4).unwrap();
            log.verify().unwrap()
        };
        let log = Log::open(Arc::new(SqliteKv::open(&path).unwrap()), C).unwrap();
        assert_eq!(log.head(), Head { epoch: 3, seq: 2, hash, fence: 4 });
        assert_eq!(log.verify().unwrap(), hash);
    }

    #[test]
    fn pruned_log_stays_verifiable() {
        let kv = store();
        let log = Log::open(kv.clone(), C).unwrap();
        for (i, time) in [10, 20, 30, 40, 50].into_iter().enumerate() {
            log.append(1, time, d(&format!("e{}", i + 1))).unwrap();
        }
        let h3 = log.hash_at(3).unwrap().unwrap();
        assert_eq!(log.prune_older_than(35).unwrap(), 3);
        assert_eq!(log.start(), Checkpoint { seq: 3, epoch: 1, hash: h3 });
        assert!(matches!(log.read(0, 10), Err(LogError::Pruned { seq: 0, start: 3 })));
        assert_eq!(ops(&log.read(3, 10).unwrap()), ["e4", "e5"]);
        assert_eq!(log.get(2).unwrap(), None);
        assert_eq!(log.hash_at(2).unwrap(), None);
        assert_eq!(log.hash_at(3).unwrap(), Some(h3));
        let head = log.verify().unwrap();
        assert_eq!(log.prune_older_than(35).unwrap(), 0);
        drop(log);

        let log = Log::open(kv, C).unwrap();
        assert_eq!(log.start().seq, 3);
        assert_eq!(log.verify().unwrap(), head);
        assert_eq!(log.prune_through(99).unwrap(), 2);
        assert!(log.read(5, 10).unwrap().is_empty());
        assert_eq!(log.verify().unwrap(), head);
        let e6 = log.append(1, 60, d("e6")).unwrap();
        assert_eq!((e6.seq, e6.prev), (6, head));
        log.verify().unwrap();
    }

    #[test]
    fn pruned_entries_are_not_cut_or_taken_again() {
        let (a, b) = (open(), open());
        a.append_all(1, 1, vec![d("a"), d("b"), d("c")]).unwrap();
        let es = a.read(0, 10).unwrap();
        b.accept_all(1, es.clone()).unwrap();
        assert_eq!(b.prune_through(2).unwrap(), 2);
        assert!(matches!(b.accept(1, es[0].clone()), Err(LogError::Pruned { seq: 1, start: 2 })));
        b.accept(1, es[1].clone()).unwrap();
        b.accept(1, es[2].clone()).unwrap();
        b.fence(2).unwrap();
        assert!(matches!(b.cut_after(1), Err(LogError::Pruned { seq: 1, start: 2 })));
        assert_eq!(ops(&b.cut_after(2).unwrap()), ["c"]);
        assert_eq!(Some(b.head().hash), a.hash_at(2).unwrap());
        b.verify().unwrap();
    }

    /// B, promoted in epoch 2, holds entries of epoch 1 that replica R never
    /// got; R, fenced at 2, takes them from B, but from no one in epoch 1.
    #[test]
    fn a_new_authority_passes_on_older_entries() {
        let (b, r) = (open(), open());
        b.append_all(1, 1, vec![d("a"), d("b")]).unwrap();
        b.append(2, 2, d("c")).unwrap();
        r.fence(2).unwrap();
        let es = b.read(0, 10).unwrap();
        assert!(matches!(r.accept_all(1, es.clone()), Err(LogError::Fenced { epoch: 1, fence: 2 })));
        r.accept_all(2, es).unwrap();
        assert_eq!((r.head().seq, r.head().hash), (b.head().seq, b.head().hash));
        r.verify().unwrap();

        let late = open();
        late.append(3, 3, d("x")).unwrap();
        assert!(matches!(open().accept_all(2, late.read(0, 1).unwrap()), Err(LogError::Corrupt(_))));
        let f = open();
        f.accept_all(4, vec![]).unwrap();
        assert_eq!(f.head().fence, 4);
    }
}
