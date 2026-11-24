//! Replication of one collection among its members (§7, §8.1), as a state
//! machine without I/O. It takes the term from the cluster log, client
//! writes and messages from the other members, and gives back messages to
//! send and writes acknowledged. The node's runtime will drive it over node
//! links; the simulation (sim.rs) drives it over a simulated network.
//!
//! The authority sends each replica the entries after the last point it
//! believes they share, with that point's hash. A replica that holds the same
//! entry there takes them and acknowledges; one that does not says so, and
//! the authority steps back until they agree. Whatever a replica holds past
//! the agreed point that the authority's chain does not is a former
//! authority's divergent tail (§7.4), and goes into the recovery area.
//!
//! No leases (§7.2, one voter): an authority writes until it learns of a
//! later epoch, from the cluster log or from a member that was fenced.

use std::{collections::BTreeMap, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{
    log::{Draft, Entry, Hash, Log, LogError},
    placement::NodeId,
};

/// The most entries in one message.
pub const BATCH: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// Acknowledged once on the authority's disk.
    Local,
    /// The same; the replicas follow as they can.
    Async,
    /// Acknowledged once a majority of the members have it on disk.
    Sync,
}

/// Who is in charge of the collection, as the cluster log says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Term {
    pub epoch: u64,
    pub authority: NodeId,
    /// The authority and its replicas.
    pub members: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    /// From the authority: its entries after `after`, where its chain's hash
    /// is `prev`; its log ends at `head`. Without entries, a heartbeat.
    Entries { epoch: u64, after: u64, prev: Hash, head: u64, entries: Vec<Entry> },
    /// From a replica: it holds the authority's chain up to `seq`.
    Ack { epoch: u64, seq: u64 },
    /// From a replica: it does not hold the authority's entry `after`; its
    /// log ends at `head`.
    Mismatch { epoch: u64, after: u64, head: u64 },
    /// From a member that knows a later epoch than the sender's.
    Stale { epoch: u64 },
}

pub type WriteId = u64;

impl Msg {
    /// The bytes on the wire: a type byte, then the fields as big-endian
    /// numbers; entries as a count and each with its length (Entry::encode).
    pub fn encode(&self) -> Vec<u8> {
        fn put(b: &mut Vec<u8>, x: u64) {
            b.extend_from_slice(&x.to_be_bytes());
        }
        let mut b = Vec::new();
        match self {
            Msg::Entries { epoch, after, prev, head, entries } => {
                b.push(1);
                put(&mut b, *epoch);
                put(&mut b, *after);
                b.extend_from_slice(prev);
                put(&mut b, *head);
                b.extend_from_slice(&(entries.len() as u32).to_be_bytes());
                for e in entries {
                    let enc = e.encode();
                    b.extend_from_slice(&(enc.len() as u32).to_be_bytes());
                    b.extend_from_slice(&enc);
                }
            }
            Msg::Ack { epoch, seq } => {
                b.push(2);
                put(&mut b, *epoch);
                put(&mut b, *seq);
            }
            Msg::Mismatch { epoch, after, head } => {
                b.push(3);
                put(&mut b, *epoch);
                put(&mut b, *after);
                put(&mut b, *head);
            }
            Msg::Stale { epoch } => {
                b.push(4);
                put(&mut b, *epoch);
            }
        }
        b
    }

    pub fn decode(bytes: &[u8]) -> Result<Msg, String> {
        struct R<'a>(&'a [u8]);
        impl<'a> R<'a> {
            fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
                if self.0.len() < n {
                    return Err("message cut short".into());
                }
                let (a, rest) = self.0.split_at(n);
                self.0 = rest;
                Ok(a)
            }
            fn u64(&mut self) -> Result<u64, String> {
                Ok(u64::from_be_bytes(self.take(8)?.try_into().expect("8 bytes")))
            }
            fn u32(&mut self) -> Result<u32, String> {
                Ok(u32::from_be_bytes(self.take(4)?.try_into().expect("4 bytes")))
            }
        }
        let mut r = R(bytes);
        let msg = match r.take(1)?[0] {
            1 => {
                let (epoch, after) = (r.u64()?, r.u64()?);
                let prev: Hash = r.take(32)?.try_into().expect("32 bytes");
                let head = r.u64()?;
                let count = r.u32()? as usize;
                if count > BATCH {
                    return Err(format!("{count} entries in one message"));
                }
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let len = r.u32()? as usize;
                    entries.push(Entry::decode(r.take(len)?).map_err(|e| e.to_string())?);
                }
                Msg::Entries { epoch, after, prev, head, entries }
            }
            2 => Msg::Ack { epoch: r.u64()?, seq: r.u64()? },
            3 => Msg::Mismatch { epoch: r.u64()?, after: r.u64()?, head: r.u64()? },
            4 => Msg::Stale { epoch: r.u64()? },
            t => return Err(format!("message type {t}")),
        };
        if !r.0.is_empty() {
            return Err("bytes after the message".into());
        }
        Ok(msg)
    }
}

/// What a step gives back.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub send: Vec<(NodeId, Msg)>,
    /// Writes acknowledged now, with their seq.
    pub acked: Vec<(WriteId, u64)>,
    /// Writes this node will never acknowledge, because its term is over.
    /// They may still be in the chain, or end up in a recovery area.
    pub failed: Vec<WriteId>,
}

#[derive(Debug)]
pub enum ReplError {
    /// Not the authority, or no longer sure of being it.
    NotAuthority,
    /// A planned handover has stopped writes here.
    Stopped,
    Log(LogError),
}

impl fmt::Display for ReplError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplError::NotAuthority => write!(f, "not the authority"),
            ReplError::Stopped => write!(f, "writes stopped for a handover"),
            ReplError::Log(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ReplError {}

impl From<LogError> for ReplError {
    fn from(e: LogError) -> Self {
        ReplError::Log(e)
    }
}

pub struct Replica {
    me: NodeId,
    /// Shared with whoever else reads this collection's log on this node
    /// (the cluster state, for the cluster log); one per collection.
    log: Arc<Log>,
    durability: Durability,
    term: Option<Term>,
    /// A later epoch was heard of; writes wait for the cluster log.
    stale: bool,
    /// A planned handover is under way (§7.3).
    stopped: bool,
    /// As the authority, per replica: how far it holds my chain, and where
    /// my next message to it starts.
    matched: BTreeMap<NodeId, u64>,
    next: BTreeMap<NodeId, u64>,
    /// Sync writes waiting for a majority, with their seq.
    waiting: Vec<(WriteId, u64)>,
}

impl Replica {
    pub fn new(me: NodeId, log: Arc<Log>, durability: Durability) -> Replica {
        Replica {
            me,
            log,
            durability,
            term: None,
            stale: false,
            stopped: false,
            matched: BTreeMap::new(),
            next: BTreeMap::new(),
            waiting: Vec::new(),
        }
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn term(&self) -> Option<&Term> {
        self.term.as_ref()
    }

    /// Whether this node takes writes now.
    pub fn is_authority(&self) -> bool {
        self.leads() && !self.stale && !self.stopped && self.epoch() >= self.log.head().fence
    }

    fn leads(&self) -> bool {
        self.term.as_ref().is_some_and(|t| t.authority == self.me)
    }

    fn epoch(&self) -> u64 {
        self.term.as_ref().map_or(0, |t| t.epoch)
    }

    fn replicas(&self) -> Vec<NodeId> {
        match &self.term {
            Some(t) => t.members.iter().filter(|m| **m != self.me).cloned().collect(),
            None => Vec::new(),
        }
    }

    /// A new term from the cluster log.
    pub fn on_term(&mut self, term: Term) -> Result<Out, ReplError> {
        let mut out = Out::default();
        // A term older than a fence is one the cluster log has moved past (a
        // message that came late, perhaps after a restart).
        if term.epoch <= self.epoch() || term.epoch < self.log.head().fence {
            return Ok(out);
        }
        self.log.fence(term.epoch)?;
        out.failed = self.waiting.drain(..).map(|(id, _)| id).collect();
        self.stale = false;
        self.stopped = false;
        self.matched.clear();
        self.next.clear();
        self.term = Some(term);
        if self.leads() {
            let head = self.log.head().seq;
            for r in self.replicas() {
                self.next.insert(r, head);
            }
            self.send_all(&mut out)?;
        }
        Ok(out)
    }

    /// The members changed within the same term (a node joined the
    /// cluster, which all get the cluster log); the authority stays.
    pub fn set_members(&mut self, members: Vec<NodeId>) {
        let Some(t) = self.term.as_mut() else { return };
        if t.members == members {
            return;
        }
        t.members = members;
        if self.leads() {
            let head = self.log.head().seq;
            let replicas = self.replicas();
            self.next.retain(|r, _| replicas.contains(r));
            self.matched.retain(|r, _| replicas.contains(r));
            for r in replicas {
                self.next.entry(r).or_insert(head);
            }
        }
    }

    /// Fenced by the voters before a promotion (§7.4): take nothing more
    /// from an authority of an earlier epoch, and write no more in one.
    pub fn fence(&mut self, epoch: u64) -> Result<(), ReplError> {
        self.log.fence(epoch)?;
        if epoch > self.epoch() && self.leads() {
            self.stale = true;
        }
        Ok(())
    }

    /// Planned handover, step 3 (§7.3): no more writes. Returns the seq the
    /// new authority must reach.
    pub fn stop(&mut self) -> u64 {
        self.stopped = true;
        self.log.head().seq
    }

    /// The handover was called off.
    pub fn resume(&mut self) {
        self.stopped = false;
    }

    pub fn write(&mut self, id: WriteId, draft: Draft, now: u64) -> Result<Out, ReplError> {
        if self.stopped {
            return Err(ReplError::Stopped);
        }
        if !self.is_authority() {
            return Err(ReplError::NotAuthority);
        }
        let e = self.log.append(self.epoch(), now, draft)?;
        let mut out = Out::default();
        if self.durability == Durability::Sync {
            self.waiting.push((id, e.seq));
            self.settle(&mut out);
        } else {
            out.acked.push((id, e.seq));
        }
        self.send_all(&mut out)?;
        Ok(out)
    }

    /// Time passes: the authority sends each replica what it may be missing,
    /// or a heartbeat.
    pub fn tick(&mut self) -> Result<Out, ReplError> {
        let mut out = Out::default();
        if self.leads() && !self.stale {
            self.send_all(&mut out)?;
        }
        Ok(out)
    }

    pub fn on_msg(&mut self, from: &NodeId, msg: Msg) -> Result<Out, ReplError> {
        let mut out = Out::default();
        match msg {
            Msg::Entries { epoch, after, prev, head, entries } => {
                self.on_entries(from, epoch, after, prev, head, entries, &mut out)?;
            }
            Msg::Ack { epoch, seq } => {
                if epoch != self.epoch() || !self.leads() {
                    return Ok(out);
                }
                let m = self.matched.entry(from.clone()).or_insert(0);
                *m = (*m).max(seq);
                if seq < self.log.head().seq {
                    self.next.insert(from.clone(), seq);
                    self.send_to(from, &mut out)?;
                }
                self.settle(&mut out);
            }
            Msg::Mismatch { epoch, after, head } => {
                if epoch != self.epoch() || !self.leads() {
                    return Ok(out);
                }
                let back = if head < after { head } else { after.saturating_sub(1) };
                self.next.insert(from.clone(), back);
                self.send_to(from, &mut out)?;
            }
            Msg::Stale { epoch } => {
                if epoch > self.epoch() && self.leads() {
                    self.stale = true;
                }
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn on_entries(
        &mut self,
        from: &NodeId,
        epoch: u64,
        after: u64,
        prev: Hash,
        their_head: u64,
        entries: Vec<Entry>,
        out: &mut Out,
    ) -> Result<(), ReplError> {
        let known = self.epoch().max(self.log.head().fence);
        if epoch < known {
            out.send.push((from.clone(), Msg::Stale { epoch: known }));
            return Ok(());
        }
        let from_authority = self.term.as_ref().is_some_and(|t| t.epoch == epoch && &t.authority == from);
        if !from_authority || self.leads() {
            // A later epoch the cluster log has not told us of yet: wait for it.
            return Ok(());
        }
        let mine = self.log.head();
        if after > mine.seq || self.log.hash_at(after)? != Some(prev) {
            out.send.push((from.clone(), Msg::Mismatch { epoch, after, head: mine.seq }));
            return Ok(());
        }
        // We agree up to `after`. Where does what I hold part from what came?
        let upto = entries.last().map_or(after, |e| e.seq);
        let mut cut_at = None;
        let mut fresh = Vec::new();
        for e in entries {
            if cut_at.is_none() && e.seq <= mine.seq {
                if self.log.hash_at(e.seq)? == Some(e.hash()) {
                    continue;
                }
                cut_at = Some(e.seq - 1);
            }
            fresh.push(e);
        }
        // Past the authority's head as it was when it sent this, its chain
        // holds only entries of its own epoch: an entry of an earlier epoch
        // there is a former authority's tail. (One of this epoch is newer
        // than the message, which came late.)
        if cut_at.is_none()
            && upto == their_head
            && mine.seq > upto
            && self.log.get(upto + 1)?.is_some_and(|e| e.epoch < epoch)
        {
            cut_at = Some(upto);
        }
        if let Some(c) = cut_at {
            self.log.cut_after(c)?;
        }
        self.log.accept_all(epoch, fresh)?;
        out.send.push((from.clone(), Msg::Ack { epoch, seq: upto }));
        Ok(())
    }

    /// The highest seq a majority of the members hold, the authority
    /// counted with its whole log.
    fn majority_seq(&self) -> u64 {
        let mut seqs = vec![self.log.head().seq];
        seqs.extend(self.replicas().iter().map(|r| self.matched.get(r).copied().unwrap_or(0)));
        seqs.sort_unstable_by(|a, b| b.cmp(a));
        seqs[seqs.len() / 2]
    }

    fn settle(&mut self, out: &mut Out) {
        let m = self.majority_seq();
        let (done, wait): (Vec<_>, Vec<_>) = self.waiting.drain(..).partition(|(_, seq)| *seq <= m);
        self.waiting = wait;
        out.acked.extend(done);
    }

    fn send_all(&mut self, out: &mut Out) -> Result<(), ReplError> {
        for r in self.replicas() {
            self.send_to(&r, out)?;
        }
        Ok(())
    }

    fn send_to(&mut self, r: &NodeId, out: &mut Out) -> Result<(), ReplError> {
        let head = self.log.head();
        let start = self.log.start().seq;
        let after = self.next.get(r).copied().unwrap_or(head.seq).min(head.seq).max(start);
        let prev = self.log.hash_at(after)?.ok_or_else(|| LogError::Corrupt(format!("no hash at {after}")))?;
        let entries = self.log.read(after, BATCH)?;
        if let Some(last) = entries.last() {
            self.next.insert(r.clone(), last.seq);
        }
        out.send.push((r.clone(), Msg::Entries { epoch: self.epoch(), after, prev, head: head.seq, entries }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::CollectionId;

    #[test]
    fn messages_read_back() {
        let e = Entry {
            collection: CollectionId([3; 16]),
            epoch: 2,
            seq: 7,
            time: 9,
            actor: "a".into(),
            module: "m".into(),
            op: "o".into(),
            payload: vec![1, 2, 3],
            prev: [4; 32],
        };
        for m in [
            Msg::Entries { epoch: 2, after: 6, prev: [4; 32], head: 7, entries: vec![e.clone(), e] },
            Msg::Entries { epoch: 2, after: 6, prev: [4; 32], head: 6, entries: vec![] },
            Msg::Ack { epoch: 2, seq: 7 },
            Msg::Mismatch { epoch: 2, after: 6, head: 3 },
            Msg::Stale { epoch: 5 },
        ] {
            let b = m.encode();
            assert_eq!(Msg::decode(&b).unwrap(), m);
            assert!(Msg::decode(&b[..b.len() - 1]).is_err());
            let mut longer = b.clone();
            longer.push(0);
            assert!(Msg::decode(&longer).is_err());
        }
        assert!(Msg::decode(&[9]).is_err());
    }
}
