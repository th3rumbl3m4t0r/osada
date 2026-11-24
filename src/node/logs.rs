//! The cluster's logs on the nodes (D-U31): every node sends its own lines (auth and admin from
//! its logs, http and files from its outbox) to the logs authority (the directory's authority at
//! the root) every few seconds; the authority keeps them (logstore::Store) and answers queries.
//! Where a node is: `<data>/logs/sent.json` (the authority and the last seq it has of each log);
//! how it goes: `<data>/logs/status.json`, shown with the node.

use std::{collections::BTreeMap, path::Path, sync::Arc, sync::OnceLock, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Answer, Call, Node};
use crate::{
    audit::{LogKind, Verbosity},
    directory::User,
    logstore::{self, Line, Store},
    placement::{Domain, NodeId},
    rbac::Perm,
};

/// The logs every node sends.
pub const SENT: [&str; 4] = ["auth", "admin", "http", "files"];
/// Lines in one batch at most.
const BATCH: usize = 500;

static STORE: OnceLock<Arc<Store>> = OnceLock::new();

#[derive(Default, Serialize, Deserialize)]
struct Sent {
    authority: String,
    at: BTreeMap<String, u64>,
}

fn load(dir: &Path) -> Sent {
    std::fs::read(dir.join("sent.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_json(dir: &Path, name: &str, v: &impl Serialize) {
    let tmp = dir.join(format!("{name}.tmp"));
    if std::fs::write(&tmp, serde_json::to_vec(v).expect("json")).is_ok() {
        let _ = std::fs::rename(tmp, dir.join(name));
    }
}

/// How sending goes on this node, for the overview.
pub fn read_status(data: &Path) -> Value {
    std::fs::read(data.join("logs").join("status.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
}

fn level(v: Verbosity) -> u8 {
    match v {
        Verbosity::Off => 0,
        Verbosity::Minimal => 1,
        Verbosity::Standard => 2,
        Verbosity::Detailed | Verbosity::Debug => 3,
    }
}

/// The settings' kind for a dataset sent by the nodes.
fn kind_of(log: &str) -> Option<LogKind> {
    Some(match log {
        "auth" => LogKind::Auth,
        "admin" => LogKind::Admin,
        "http" => LogKind::Http,
        "files" => LogKind::Access,
        _ => return None,
    })
}

impl Node {
    /// The node that keeps the cluster's logs: the directory's authority at the root.
    pub fn logs_authority(&self) -> Option<NodeId> {
        let p = self.cluster_state().placement;
        let root = p.root()?;
        p.authority(Domain::Directory, root).ok().map(|a| a.node)
    }

    /// The logs store, on the node that keeps it (opened on first use).
    pub(crate) fn log_store(&self) -> Option<Arc<Store>> {
        if let Some(s) = STORE.get() {
            return Some(s.clone());
        }
        let dir = self.data.get()?.join("logs");
        match Store::open(&dir) {
            Ok(s) => Some(STORE.get_or_init(|| Arc::new(s)).clone()),
            Err(e) => {
                eprintln!("{}: the logs store: {e}", self.me);
                None
            }
        }
    }

    /// Up to `limit` lines of this node's `log` after seq `after`.
    fn own_lines(&self, log: &str, after: u64, limit: usize) -> Vec<Line> {
        match log {
            "auth" | "admin" => {
                let inner = self.lock();
                let l = if log == "admin" { inner.admin_log.clone() } else { inner.auth.clone() };
                drop(inner);
                let after = after.max(l.start().seq);
                l.read(after, limit)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|e| {
                        let mut event: Value = serde_json::from_slice(&e.payload).unwrap_or(Value::Null);
                        if let Some(o) = event.as_object_mut() {
                            o.remove("token");
                            o.entry("actor").or_insert_with(|| Value::from(e.actor.clone()));
                        }
                        Line { log: log.to_string(), seq: e.seq, time: e.time, event }
                    })
                    .collect()
            }
            _ => match self.data.get().map(|d| logstore::outbox(&d.join("logs"))) {
                Some(Ok(o)) => o.read(log, after, limit),
                _ => vec![],
            },
        }
    }

    /// The last seq of this node's `log`.
    fn own_head(&self, log: &str) -> u64 {
        match log {
            "auth" => self.lock().auth.head().seq,
            "admin" => self.lock().admin_log.head().seq,
            _ => match self.data.get().map(|d| logstore::outbox(&d.join("logs"))) {
                Some(Ok(o)) => o.head(log),
                _ => 0,
            },
        }
    }

    /// As the logs authority: a node's lines (`reset`: its logs that begin anew). Answers what
    /// the store has of that node now.
    pub(super) fn answer_log_batch(&self, from: &NodeId, lines: Vec<Line>, reset: Vec<String>) -> Answer {
        let Some(s) = self.log_store() else { return Answer::Refused { why: "this node keeps no logs".into() } };
        if !reset.is_empty() {
            s.reset(&from.0, &reset);
        }
        let label = self.cluster_state().placement.node_label(from);
        match s.ingest(&from.0, &label, &lines) {
            Ok(at) => Answer::Web { value: json!(at) },
            Err(why) => Answer::Refused { why },
        }
    }

    /// As the logs authority: a query.
    pub(super) fn answer_log_ql(&self, text: &str, from: i64, to: i64, limit: usize) -> Answer {
        let Some(s) = self.log_store() else { return Answer::Refused { why: "this node keeps no logs".into() } };
        if text.is_empty() {
            return Answer::Web { value: tokio::task::block_in_place(|| s.sizes()) };
        }
        let r = tokio::task::block_in_place(|| s.query(text, from, to, limit));
        match r {
            Ok(value) => Answer::Web { value },
            Err(why) => Answer::Refused { why },
        }
    }

    /// A query over the cluster's logs, for someone with logs-read at the root.
    pub async fn logs_ql(self: &Arc<Self>, viewer: &User, text: String, from: i64, to: i64, limit: usize) -> Result<Value, String> {
        let root = self.cluster_state().placement.root().ok_or("no OUs yet")?;
        if !self.may(viewer, Perm::LogsRead, root) {
            return Err("queries reach every log: they need logs-read at the root".into());
        }
        if text.len() > 8000 {
            return Err("a query of at most 8000 characters".into());
        }
        let a = self.logs_authority().ok_or("no node keeps the logs (the directory has no authority)")?;
        let label = self.cluster_state().placement.node_label(&a);
        match self.ask(&a, Call::LogQl { text, from, to, limit }).await {
            Ok(Answer::Web { mut value }) => {
                value["authority"] = Value::from(label);
                Ok(value)
            }
            Ok(Answer::Refused { why }) => Err(why),
            Ok(other) => Err(format!("{label} answered {other:?}")),
            Err(e) => Err(format!("{label}, which keeps the logs: {e}")),
        }
    }

    /// The datasets, their columns and sizes, and how sending goes on every node.
    pub async fn logs_info(self: &Arc<Self>, viewer: &User) -> Result<Value, String> {
        let root = self.cluster_state().placement.root().ok_or("no OUs yet")?;
        if !self.may(viewer, Perm::LogsRead, root) {
            return Err("this needs logs-read at the root".into());
        }
        let a = self.logs_authority();
        let label = a.as_ref().map(|a| self.cluster_state().placement.node_label(a));
        let sizes = match &a {
            Some(a) if a == &self.me => self.log_store().map(|s| s.sizes()).unwrap_or(Value::Null),
            Some(a) => match self.ask(a, Call::LogQl { text: String::new(), from: 0, to: 0, limit: 0 }).await {
                Ok(Answer::Web { value }) => value,
                _ => Value::Null,
            },
            None => Value::Null,
        };
        Ok(json!({ "authority": label, "datasets": Store::describe(), "sizes": sizes }))
    }

    /// Sends this node's lines to the logs authority while the node runs.
    pub fn spawn_logs(self: &Arc<Self>) {
        let node = self.clone();
        tokio::spawn(async move {
            // OSADA_LOGS_ROUND: seconds between rounds (5; tests use less)
            let every = std::env::var("OSADA_LOGS_ROUND").ok().and_then(|v| v.parse::<u64>().ok()).filter(|s| *s > 0).unwrap_or(5);
            let mut failures = 0u32;
            let mut more = false;
            let mut last_prune = 0u64;
            loop {
                let wait = if more { 0 } else { every << failures.min(4) };
                tokio::time::sleep(Duration::from_secs(wait).max(Duration::from_millis(200))).await;
                more = false;
                let Some(data) = node.data.get().cloned() else { continue };
                let dir = data.join("logs");
                let settings = node.cluster_state().settings;
                logstore::set_levels(level(settings.switches(LogKind::Http).verbosity), level(settings.switches(LogKind::Access).verbosity));
                if let Err(e) = logstore::outbox(&dir) {
                    if failures == 0 {
                        eprintln!("{}: the logs outbox: {e}", node.me);
                    }
                    failures += 1;
                    continue;
                }
                // the cluster's bans, for this node's listeners (D-U33)
                crate::bans::sync(&node.cluster_state().bans);
                let Some(a) = node.logs_authority() else { continue };
                if a == node.me {
                    node.run_rules().await;
                }
                // this node's batch (node control, D-U41): the default 500 sends ~1,900 lines a second
                let batch = node.cluster_state().placement.nodes.get(&node.me).and_then(|n| n.log_batch).map_or(BATCH, |b| b as usize);
                let mut sent = load(&dir);
                let mut error: Option<String> = None;
                let mut reset = Vec::new();
                if sent.authority != a.0 {
                    // a new authority: from what it has of this node
                    match node.ask(&a, Call::LogBatch { lines: vec![], reset: vec![] }).await {
                        Ok(Answer::Web { value }) => {
                            sent = Sent { authority: a.0.clone(), at: serde_json::from_value(value).unwrap_or_default() };
                        }
                        other => error = Some(format!("{}: {other:?}", a.0)),
                    }
                }
                if error.is_none() {
                    for log in SENT {
                        let head = node.own_head(log);
                        let mut after = sent.at.get(log).copied().unwrap_or(0);
                        if after > head {
                            // this node's log began anew (its outbox was made again)
                            reset.push(log.to_string());
                            after = 0;
                        }
                        let lines = node.own_lines(log, after, batch);
                        if lines.is_empty() && !reset.iter().any(|r| r == log) {
                            continue;
                        }
                        let n = lines.len();
                        let r = std::mem::take(&mut reset);
                        match node.ask(&a, Call::LogBatch { lines, reset: r }).await {
                            Ok(Answer::Web { value }) => {
                                let at: BTreeMap<String, u64> = serde_json::from_value(value).unwrap_or_default();
                                sent.at.insert(log.to_string(), at.get(log).copied().unwrap_or(after));
                                more |= n == batch;
                            }
                            Ok(Answer::Refused { why }) => {
                                error = Some(why);
                                break;
                            }
                            Ok(other) => {
                                error = Some(format!("{other:?}"));
                                break;
                            }
                            Err(e) => {
                                error = Some(e);
                                break;
                            }
                        }
                    }
                    write_json(&dir, "sent.json", &sent);
                }
                match &error {
                    Some(e) => {
                        failures += 1;
                        if failures == 1 || failures % 50 == 0 {
                            eprintln!("{}: sending logs to {a}: {e}", node.me);
                        }
                    }
                    None => failures = 0,
                }
                let now = super::now_ms();
                let backlog: BTreeMap<&str, u64> = SENT.iter().map(|l| (*l, node.own_head(l).saturating_sub(sent.at.get(*l).copied().unwrap_or(0)))).collect();
                write_json(
                    &dir,
                    "status.json",
                    &json!({
                        "authority": a.0, "state": if error.is_some() { "failing" } else { "ok" }, "error": error,
                        "at": now, "backlog": backlog, "dropped": logstore::DROPPED.load(std::sync::atomic::Ordering::Relaxed),
                    }),
                );
                // once an hour: what is older than its log keeps it goes
                if now.saturating_sub(last_prune) > 3_600_000 {
                    last_prune = now;
                    let days = |k: LogKind| settings.switches(k).retention.as_secs() / 86_400;
                    if let Ok(o) = logstore::outbox(&dir) {
                        for log in ["http", "files"] {
                            let keep = kind_of(log).map(days).unwrap_or(30);
                            o.prune(log, now.saturating_sub(keep * 86_400_000), sent.at.get(log).copied().unwrap_or(0));
                        }
                    }
                    if a == node.me {
                        if let Some(s) = node.log_store() {
                            let mut keep: BTreeMap<String, u64> = SENT.iter().filter_map(|l| kind_of(l).map(|k| (l.to_string(), days(k)))).collect();
                            keep.insert("alerts".into(), 365);
                            keep.insert("bans".into(), 365);
                            let n = tokio::task::block_in_place(|| s.prune(&keep));
                            if n > 0 {
                                eprintln!("{}: logs: {n} lines past their retention removed", node.me);
                            }
                        }
                    }
                }
            }
        });
    }
}
