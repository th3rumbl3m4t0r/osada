//! What a node knows about its links (D-U40): how fast bytes come to it from each peer
//! (Mbit/s), from a short test once a week (`BW_KIND`: the peer sends TEST_BYTES) and from every
//! transfer of a megabyte or more on the way; and how busy this node's own uplink is now (its
//! interface's outgoing rate, sampled every 2 s, against the highest rate seen: its capacity).
//! An edge picks where to fetch a file from with it (relay.rs: the sources by size). Kept in
//! `<data>/links.json`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{link, placement::NodeId};

/// The stream kind of the link test and the "how busy are you" question.
pub const BW_KIND: &str = "bw";
/// What a test sends: enough to fill a slow line for a moment, not to hog it (4 MiB: 2 s at 17 Mbit/s).
pub const TEST_BYTES: u64 = 4 << 20;
/// How often each link is tested.
pub const TEST_EVERY: Duration = Duration::from_secs(7 * 86400);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Peer {
    /// Mbit/s from that peer to this node (moving average).
    pub mbit_in: f64,
    /// When the last test ran (ms since 1970); passive samples do not move it.
    #[serde(default)]
    pub tested: u64,
    /// When the estimate last changed.
    #[serde(default)]
    pub updated: u64,
    #[serde(default)]
    pub samples: u32,
    /// The most that came from that peer over the link in 10 s (Mbit/s, the rates' samples): a
    /// line carries at least that, whatever a short test said (Round 19). Counts for 30 days.
    #[serde(default)]
    pub seen_in: f64,
    #[serde(default)]
    pub seen_at: u64,
}

/// How long a rate seen in traffic counts for a link's speed.
const SEEN_FOR_MS: u64 = 30 * 86400 * 1000;

impl Peer {
    /// The link's speed from that peer: what was measured, or more if traffic has shown more.
    pub fn best(&self) -> f64 {
        let seen = if now_ms().saturating_sub(self.seen_at) < SEEN_FOR_MS { self.seen_in } else { 0.0 };
        self.mbit_in.max(seen)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    peers: BTreeMap<String, Peer>,
    /// The highest outgoing rate this node has seen (Mbit/s): its uplink, roughly.
    #[serde(default)]
    tx_peak: f64,
}

pub struct Stats {
    file: Option<PathBuf>,
    saved: Mutex<Saved>,
    /// Outgoing Mbit/s of this machine now.
    tx_now: Mutex<f64>,
}

static STATS: OnceLock<Stats> = OnceLock::new();

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// This node's statistics (loaded from `data` the first time; in memory only without).
pub fn init(data: Option<&Path>) -> &'static Stats {
    STATS.get_or_init(|| {
        let file = data.map(|d| d.join("links.json"));
        let saved = file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let s = Stats { file, saved: Mutex::new(saved), tx_now: Mutex::new(0.0) };
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(sample_tx());
        }
        s
    })
}

pub fn stats() -> &'static Stats {
    init(None)
}

impl Stats {
    fn save(&self, s: &Saved) {
        if let Some(f) = &self.file {
            let tmp = f.with_extension("tmp");
            if std::fs::write(&tmp, serde_json::to_vec_pretty(s).unwrap_or_default()).is_ok() {
                let _ = std::fs::rename(&tmp, f);
            }
        }
    }

    /// A transfer from `peer`: `bytes` in `took` (only a megabyte or more, over 200 ms, counts).
    pub fn record(&self, peer: &NodeId, bytes: u64, took: Duration, test: bool) {
        if bytes < (1 << 20) || took < Duration::from_millis(200) {
            return;
        }
        let rate = bytes as f64 * 8.0 / took.as_secs_f64() / 1e6;
        let mut s = self.saved.lock().unwrap_or_else(|e| e.into_inner());
        let p = s.peers.entry(peer.0.clone()).or_default();
        // a test sets it; a transfer moves it (background fills only: nothing slows them but the line)
        p.mbit_in = if p.samples == 0 || test { rate } else { 0.7 * p.mbit_in + 0.3 * rate };
        p.samples += 1;
        p.updated = now_ms();
        if test {
            p.tested = p.updated;
        }
        let snapshot = std::mem::take(&mut *s);
        self.save(&snapshot);
        *s = snapshot;
    }

    /// Mbit/s from `peer` to here, if known (measured, or seen in traffic when more).
    pub fn mbit_in(&self, peer: &NodeId) -> Option<f64> {
        self.saved.lock().unwrap_or_else(|e| e.into_inner()).peers.get(&peer.0).map(Peer::best).filter(|r| *r > 0.0)
    }

    /// What came from `peer` over the link in the last sample (Mbit/s): kept when it is the most
    /// so far (by a twentieth: not a save every 10 s) or the last most is too old.
    pub fn seen(&self, peer: &NodeId, mbit: f64) {
        if !(mbit > 1.0) {
            return;
        }
        let mut s = self.saved.lock().unwrap_or_else(|e| e.into_inner());
        let p = s.peers.entry(peer.0.clone()).or_default();
        let now = now_ms();
        if mbit > p.seen_in * 1.05 || now.saturating_sub(p.seen_at) > SEEN_FOR_MS {
            p.seen_in = mbit;
            p.seen_at = now;
            let snapshot = std::mem::take(&mut *s);
            self.save(&snapshot);
            *s = snapshot;
        }
    }

    /// Whether `peer`'s link is due a test.
    pub fn due(&self, peer: &NodeId) -> bool {
        let s = self.saved.lock().unwrap_or_else(|e| e.into_inner());
        s.peers.get(&peer.0).is_none_or(|p| now_ms().saturating_sub(p.tested) > TEST_EVERY.as_millis() as u64)
    }

    /// This node's uplink now: (Mbit/s going out, the most seen), and how busy: 0..1.
    pub fn busy(&self) -> (f64, f64, f64) {
        let now = *self.tx_now.lock().unwrap_or_else(|e| e.into_inner());
        let peak = self.saved.lock().unwrap_or_else(|e| e.into_inner()).tx_peak;
        (now, peak, if peak > 0.0 { (now / peak).min(1.0) } else { 0.0 })
    }

    /// For the admin pages and the "how busy" answer.
    pub fn report(&self) -> Value {
        let s = self.saved.lock().unwrap_or_else(|e| e.into_inner());
        let (now, peak, busy) = self.busy_unlocked(s.tx_peak);
        // each peer with its best: the speed to show
        let peers: serde_json::Map<String, Value> = s
            .peers
            .iter()
            .map(|(k, p)| {
                let mut v = serde_json::to_value(p).unwrap_or_default();
                v["best"] = json!(p.best());
                (k.clone(), v)
            })
            .collect();
        json!({ "peers": peers, "tx_mbit": now, "tx_peak_mbit": peak, "busy": busy })
    }

    fn busy_unlocked(&self, peak: f64) -> (f64, f64, f64) {
        let now = *self.tx_now.lock().unwrap_or_else(|e| e.into_inner());
        (now, peak, if peak > 0.0 { (now / peak).min(1.0) } else { 0.0 })
    }
}

/// The default route's interface (where the bytes for others go).
fn iface() -> Option<String> {
    std::fs::read_to_string("/proc/net/route").ok()?.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.get(1) == Some(&"00000000")).then(|| f[0].to_string())
    })
}

fn tx_bytes(ifc: &str) -> Option<u64> {
    std::fs::read_to_string(format!("/sys/class/net/{ifc}/statistics/tx_bytes")).ok()?.trim().parse().ok()
}

/// Every 2 s: the outgoing rate now, and the highest seen (saved when it grows by a tenth).
async fn sample_tx() {
    let Some(ifc) = iface() else { return };
    let mut last = (Instant::now(), tx_bytes(&ifc).unwrap_or(0));
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let Some(b) = tx_bytes(&ifc) else { continue };
        let rate = b.saturating_sub(last.1) as f64 * 8.0 / last.0.elapsed().as_secs_f64() / 1e6;
        last = (Instant::now(), b);
        let s = stats();
        *s.tx_now.lock().unwrap_or_else(|e| e.into_inner()) = rate;
        let mut g = s.saved.lock().unwrap_or_else(|e| e.into_inner());
        if rate > g.tx_peak * 1.1 {
            g.tx_peak = rate;
            let snapshot = std::mem::take(&mut *g);
            s.save(&snapshot);
            *g = snapshot;
        }
    }
}

/// The `bw` stream kind: `{"op": "busy"}` → this node's uplink now; `{"op": "send", "bytes": n}` →
/// n bytes (up to 64 MiB) for the asker's test.
pub fn server() -> link::StreamHandler {
    use futures_util::FutureExt;
    std::sync::Arc::new(move |_peer: NodeId, _kind: String, head: Value, mut send: quinn::SendStream, _recv: quinn::RecvStream| {
        async move {
            let (now, peak, busy) = stats().busy();
            let n = match head["op"].as_str() {
                Some("send") => head["bytes"].as_u64().unwrap_or(TEST_BYTES).min(64 << 20),
                _ => 0,
            };
            if link::write_head(&mut send, json!({ "tx_mbit": now, "tx_peak_mbit": peak, "busy": busy, "bytes": n })).await.is_err() {
                return;
            }
            let chunk = vec![0u8; 64 << 10];
            let mut left = n;
            while left > 0 {
                let k = left.min(chunk.len() as u64) as usize;
                if send.write_all(&chunk[..k]).await.is_err() {
                    return;
                }
                left -= k as u64;
            }
            let _ = send.finish();
        }
        .boxed()
    })
}

/// How busy `peer`'s uplink is (0..1) and its Mbit/s going out now, within `wait`.
pub async fn ask_busy(l: &link::Link, peer: &NodeId, wait: Duration) -> Option<(f64, f64)> {
    let go = async {
        let (mut send, mut recv) = l.open_stream(peer, BW_KIND, json!({ "op": "busy" })).await.ok()?;
        let _ = send.finish();
        let h = link::read_head(&mut recv).await.ok()?;
        Some((h["busy"].as_f64().unwrap_or(0.0), h["tx_mbit"].as_f64().unwrap_or(0.0)))
    };
    tokio::time::timeout(wait, go).await.ok().flatten()
}

/// The test of `peer`'s link: it sends TEST_BYTES, the rate is kept.
pub async fn test(l: &link::Link, peer: &NodeId) -> Result<f64, String> {
    let (mut send, mut recv) = l.open_stream(peer, BW_KIND, json!({ "op": "send", "bytes": TEST_BYTES })).await.map_err(|e| e.0)?;
    let _ = send.finish();
    let h = tokio::time::timeout(Duration::from_secs(30), link::read_head(&mut recv)).await.map_err(|_| "no answer".to_string())?.map_err(|e| e.0)?;
    let want = h["bytes"].as_u64().unwrap_or(0);
    let start = Instant::now();
    let mut got = 0u64;
    let mut buf = vec![0u8; 256 << 10];
    let read = async {
        while got < want {
            match recv.read(&mut buf).await {
                Ok(Some(k)) => got += k as u64,
                _ => break,
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(120), read).await.map_err(|_| "too slow".to_string())?;
    if got < want {
        return Err(format!("{got} of {want} bytes"));
    }
    let took = start.elapsed();
    stats().record(peer, got, took, true);
    Ok(got as f64 * 8.0 / took.as_secs_f64() / 1e6)
}

/// Every hour: the links due a test (a week since the last), one at a time, when this node is not
/// busy; spread out so the nodes do not all test at once.
pub fn spawn_tests(l: std::sync::Arc<link::Link>) {
    tokio::spawn(async move {
        let first = 300 + rand_secs(1800);
        tokio::time::sleep(Duration::from_secs(first)).await;
        loop {
            for p in l.peers() {
                if !stats().due(&p) || stats().busy().2 > 0.3 {
                    continue;
                }
                match test(&l, &p).await {
                    Ok(r) => println!("{}: link from {p}: {r:.1} Mbit/s (weekly test)", l.me()),
                    Err(e) => eprintln!("{}: link test with {p}: {e}", l.me()),
                }
                tokio::time::sleep(Duration::from_secs(30 + rand_secs(60))).await;
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
}

fn rand_secs(max: u64) -> u64 {
    use rand::Rng;
    rand::thread_rng().gen_range(0..max.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates() {
        let s = Stats { file: None, saved: Mutex::new(Saved::default()), tx_now: Mutex::new(5.0) };
        let p = NodeId("peer".into());
        assert!(s.mbit_in(&p).is_none() && s.due(&p));
        s.record(&p, 100 << 10, Duration::from_secs(1), false);
        assert!(s.mbit_in(&p).is_none(), "under a megabyte: not a measure");
        s.record(&p, 10 << 20, Duration::from_secs(1), true);
        let r = s.mbit_in(&p).unwrap();
        assert!((r - 83.9).abs() < 0.5, "{r}");
        assert!(!s.due(&p));
        // a slow transfer (a slow client) does not pull the line's estimate below what it was
        s.record(&p, 2 << 20, Duration::from_secs(10), false);
        assert!(s.mbit_in(&p).unwrap() > 1.0);
        // traffic showed more than the test: that is the speed now (Round 19)
        let tested = s.mbit_in(&p).unwrap();
        s.seen(&p, tested * 2.0);
        assert!((s.mbit_in(&p).unwrap() - tested * 2.0).abs() < 0.01);
        s.seen(&p, tested);
        assert!((s.mbit_in(&p).unwrap() - tested * 2.0).abs() < 0.01, "a lower rate does not lower it");
        s.saved.lock().unwrap().peers.get_mut("peer").unwrap().seen_at = 1;
        assert!((s.mbit_in(&p).unwrap() - tested).abs() < 0.01, "an old one does not count");
        s.saved.lock().unwrap().tx_peak = 20.0;
        assert_eq!(s.busy(), (5.0, 20.0, 0.25));
    }
}

// ---- what each link carries (D-U34) ----------------------------------------------

/// A sample every 10 s, 60 kept (10 minutes) a link; nothing else kept, nothing saved.
const RATE_EVERY: Duration = Duration::from_secs(10);
const RATE_KEEP: usize = 61;

/// One link's samples: when, and the bytes sent and received since this process began (a new
/// connection's counters start again at 0: what the old one had is added on).
#[derive(Default)]
pub struct Ring {
    samples: std::collections::VecDeque<(f64, u64, u64)>,
    base: (u64, u64),
    raw: (u64, u64),
    rtt_ms: f64,
    /// When the last sample was taken: a ring of a link that is gone stops (stale).
    last: Option<Instant>,
}

impl Ring {
    /// A sample at `secs` of the connection's counters now.
    pub fn sample(&mut self, secs: f64, tx: u64, rx: u64, rtt_ms: f64) {
        if tx < self.raw.0 || rx < self.raw.1 {
            self.base = (self.base.0 + self.raw.0, self.base.1 + self.raw.1);
        }
        self.raw = (tx, rx);
        self.rtt_ms = rtt_ms;
        self.last = Some(Instant::now());
        self.samples.push_back((secs, self.base.0 + tx, self.base.1 + rx));
        while self.samples.len() > RATE_KEEP {
            self.samples.pop_front();
        }
    }

    /// Bytes a second sent and received over the last `n` samples (fewer when there are not yet).
    pub fn rate(&self, n: usize) -> Option<(f64, f64)> {
        let last = self.samples.back()?;
        let first = self.samples.get(self.samples.len().saturating_sub(n + 1))?;
        let dt = last.0 - first.0;
        (dt > 0.0).then(|| ((last.1 - first.1) as f64 / dt, (last.2 - first.2) as f64 / dt))
    }

    pub fn report(&self) -> Value {
        let (m1, m10) = (self.rate(6), self.rate(60));
        json!({
            "rtt_ms": self.rtt_ms,
            "tx_1m": m1.map(|r| r.0), "rx_1m": m1.map(|r| r.1),
            "tx_10m": m10.map(|r| r.0), "rx_10m": m10.map(|r| r.1),
            "minutes": self.samples.back().zip(self.samples.front()).map(|(b, f)| (b.0 - f.0) / 60.0),
            "transport": "QUIC",
            // no sample for 30 s: the link is gone
            "live": self.last.is_some_and(|t| t.elapsed() < Duration::from_secs(30)),
        })
    }
}

fn rings() -> &'static Mutex<BTreeMap<String, Ring>> {
    static R: std::sync::OnceLock<Mutex<BTreeMap<String, Ring>>> = std::sync::OnceLock::new();
    R.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Every link's rates and round trip, by peer (the map, admin → nodes).
pub fn rates() -> Value {
    let r = rings().lock().unwrap_or_else(|e| e.into_inner());
    Value::Object(r.iter().map(|(p, ring)| (p.clone(), ring.report())).collect())
}

/// Samples the links every 10 s.
pub fn spawn_rates(l: std::sync::Arc<link::Link>) {
    tokio::spawn(async move {
        let start = Instant::now();
        let mut t = tokio::time::interval(RATE_EVERY);
        loop {
            t.tick().await;
            let secs = start.elapsed().as_secs_f64();
            let now = l.counters();
            let mut seen = Vec::new();
            {
                let mut r = rings().lock().unwrap_or_else(|e| e.into_inner());
                for (peer, tx, rx, rtt) in now {
                    let ring = r.entry(peer.0.clone()).or_default();
                    ring.sample(secs, tx, rx, rtt.as_secs_f64() * 1000.0);
                    if let Some((_, rx)) = ring.rate(1) {
                        seen.push((peer, rx * 8.0 / 1e6));
                    }
                }
            }
            // what came in the last 10 s: a link carries at least that
            for (peer, mbit) in seen {
                stats().seen(&peer, mbit);
            }
        }
    });
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    #[test]
    fn rates_over_one_and_ten_minutes_across_a_new_connection() {
        let mut r = Ring::default();
        assert!(r.rate(6).is_none());
        // 1000 B/s out, 100 in, for 5 minutes; then a new connection (counters from 0), 2000 B/s out
        let (mut tx, mut rx) = (0u64, 0u64);
        for i in 0..=30 {
            r.sample(i as f64 * 10.0, tx, rx, 12.5);
            tx += 10_000;
            rx += 1_000;
        }
        assert_eq!(r.rate(6), Some((1000.0, 100.0)));
        let (mut tx, mut rx) = (0u64, 0u64);
        for i in 31..=90 {
            tx += 20_000;
            rx += 1_000;
            r.sample(i as f64 * 10.0, tx, rx, 20.0);
        }
        assert_eq!(r.rate(6), Some((2000.0, 100.0)));
        // the 10 minutes: the last 60 samples, all at the new rate but the first step
        let (t10, _) = r.rate(60).unwrap();
        assert!((t10 - 2000.0).abs() < 1.0, "{t10}");
        assert_eq!(r.samples.len(), RATE_KEEP);
        assert_eq!(r.report()["rtt_ms"], 20.0);
    }
}
