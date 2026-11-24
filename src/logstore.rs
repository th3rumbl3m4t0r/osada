//! The cluster's logs (D-U31). Every node writes its own lines: auth and admin in its logs as
//! before, http and files in an outbox here (`<data>/logs/outbox.db`, written by a thread from a
//! channel, so a request never waits for the disk). The nodes send their lines to the logs
//! authority (node/logs.rs), which keeps the cluster's copy in `<data>/logs/logs.db`: a table per
//! dataset (`ds_<name>`) with typed columns, queried with the pipes of ql.rs.
use crate::ql::{self, ColType, Schemas, COMMON};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};

/// One line as nodes send it: which log, its seq there, when (ms), what.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub log: String,
    pub seq: u64,
    pub time: u64,
    pub event: Value,
}

/// The datasets and their columns besides the common ones.
const DATASETS: &[(&str, &[(&str, ColType)])] = &[
    (
        "auth",
        &[("event", ColType::Text), ("user", ColType::Text), ("name", ColType::Text), ("actor", ColType::Text), ("actor_name", ColType::Text), ("sid", ColType::Text), ("addr", ColType::Text), ("why", ColType::Text)],
    ),
    ("admin", &[("event", ColType::Text), ("actor", ColType::Text), ("actor_name", ColType::Text), ("sid", ColType::Text), ("addr", ColType::Text), ("target", ColType::Text)]),
    (
        "http",
        &[
            ("front", ColType::Text),
            ("host", ColType::Text),
            ("site", ColType::Text),
            ("client", ColType::Text),
            ("method", ColType::Text),
            ("path", ColType::Text),
            ("query", ColType::Text),
            ("status", ColType::Int),
            ("bytes", ColType::Int),
            ("ms", ColType::Real),
            ("ua", ColType::Text),
            ("referer", ColType::Text),
            ("user", ColType::Text),
        ],
    ),
    (
        "files",
        &[("op", ColType::Text), ("user", ColType::Text), ("path", ColType::Text), ("to", ColType::Text), ("bytes", ColType::Int), ("how", ColType::Text), ("client", ColType::Text)],
    ),
    ("alerts", &[("rule", ColType::Text), ("severity", ColType::Text), ("key", ColType::Text), ("count", ColType::Int), ("note", ColType::Text), ("detail", ColType::Text)]),
    ("bans", &[("action", ColType::Text), ("addr", ColType::Text), ("rule", ColType::Text), ("until", ColType::Time), ("times", ColType::Int), ("by", ColType::Text)]),
];

/// Indexes besides `_time`'s.
const INDEXES: &[(&str, &str)] = &[("http", "client"), ("http", "status"), ("auth", "addr"), ("auth", "user"), ("files", "user"), ("bans", "addr")];

/// Every dataset with all its columns, the common ones first.
pub fn schemas() -> &'static Schemas {
    static S: OnceLock<Schemas> = OnceLock::new();
    S.get_or_init(|| {
        DATASETS
            .iter()
            .map(|(name, cols)| {
                let mut c: Vec<(String, ColType)> = COMMON.iter().map(|n| (n.to_string(), if *n == "_node" || *n == "_raw" { ColType::Text } else { ColType::Time })).collect();
                c.extend(cols.iter().map(|(n, t)| (n.to_string(), *t)));
                (name.to_string(), c)
            })
            .collect()
    })
}

fn col_value(v: Option<&Value>, t: ColType) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    let Some(v) = v.filter(|v| !v.is_null()) else { return V::Null };
    match t {
        ColType::Text => V::Text(match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
        ColType::Int | ColType::Time => match v {
            Value::Number(n) => n.as_i64().map(V::Integer).or_else(|| n.as_f64().map(|f| V::Integer(f as i64))).unwrap_or(V::Null),
            Value::String(s) => s.trim().parse::<i64>().map(V::Integer).unwrap_or(V::Null),
            Value::Bool(b) => V::Integer(i64::from(*b)),
            _ => V::Null,
        },
        ColType::Real => match v {
            Value::Number(n) => n.as_f64().map(V::Real).unwrap_or(V::Null),
            Value::String(s) => s.trim().parse::<f64>().map(V::Real).unwrap_or(V::Null),
            _ => V::Null,
        },
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn open_rw(path: &Path) -> Result<Connection, String> {
    let c = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;").map_err(|e| e.to_string())?;
    Ok(c)
}

/// The logs authority's copy of every node's lines.
pub struct Store {
    w: Mutex<Connection>,
    r: Mutex<Connection>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join("logs.db");
        let w = open_rw(&path)?;
        w.execute_batch(
            "CREATE TABLE IF NOT EXISTS received (node TEXT NOT NULL, log TEXT NOT NULL, seq INTEGER NOT NULL, PRIMARY KEY (node, log));
             CREATE TABLE IF NOT EXISTS rule_state (name TEXT PRIMARY KEY, last_run INTEGER NOT NULL, mark INTEGER NOT NULL, error TEXT, fired INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS rule_keys (rule TEXT NOT NULL, key TEXT NOT NULL, last INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY (rule, key));
             CREATE TABLE IF NOT EXISTS ban_history (addr TEXT PRIMARY KEY, times INTEGER NOT NULL, last INTEGER NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        for (name, cols) in schemas() {
            let defs: Vec<String> = cols.iter().map(|(c, t)| format!("\"{c}\" {}", t.sql())).collect();
            w.execute_batch(&format!("CREATE TABLE IF NOT EXISTS \"ds_{name}\" ({}); CREATE INDEX IF NOT EXISTS \"ds_{name}_time\" ON \"ds_{name}\" (\"_time\");", defs.join(", ")))
                .map_err(|e| e.to_string())?;
            // columns added since the table was made
            let have: Vec<String> = {
                let mut st = w.prepare(&format!("PRAGMA table_info(\"ds_{name}\")")).map_err(|e| e.to_string())?;
                let names = st.query_map([], |r| r.get::<_, String>(1)).map_err(|e| e.to_string())?;
                names.filter_map(Result::ok).collect()
            };
            for (c, t) in cols {
                if !have.contains(c) {
                    w.execute_batch(&format!("ALTER TABLE \"ds_{name}\" ADD COLUMN \"{c}\" {}", t.sql())).map_err(|e| e.to_string())?;
                }
            }
        }
        for (ds, col) in INDEXES {
            w.execute_batch(&format!("CREATE INDEX IF NOT EXISTS \"ds_{ds}_{col}\" ON \"ds_{ds}\" (\"{col}\", \"_time\")")).map_err(|e| e.to_string())?;
        }
        let r = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| e.to_string())?;
        r.execute_batch("PRAGMA query_only=1; PRAGMA busy_timeout=5000;").map_err(|e| e.to_string())?;
        ql::register(&r).map_err(|e| e.to_string())?;
        Ok(Store { w: Mutex::new(w), r: Mutex::new(r) })
    }

    /// What has come from `node`: the last seq of each log.
    pub fn received(&self, node: &str) -> BTreeMap<String, u64> {
        let w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(mut st) = w.prepare("SELECT log, seq FROM received WHERE node = ?1") else { return BTreeMap::new() };
        st.query_map([node], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))).map(|it| it.filter_map(Result::ok).collect()).unwrap_or_default()
    }

    /// Starts `node`'s `logs` over (its outbox was made anew): what comes next is new.
    pub fn reset(&self, node: &str, logs: &[String]) {
        let w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        for l in logs {
            let _ = w.execute("DELETE FROM received WHERE node = ?1 AND log = ?2", rusqlite::params![node, l]);
        }
    }

    /// Keeps the lines of the node `node` (its id; `label`: its name, for `_node`) that are new
    /// (seq above what came before, per log); answers what has come from it now. Lines of a log
    /// that is no dataset are counted as come and dropped.
    pub fn ingest(&self, node: &str, label: &str, lines: &[Line]) -> Result<BTreeMap<String, u64>, String> {
        let now = now_ms() as i64;
        let mut w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let tx = w.transaction().map_err(|e| e.to_string())?;
        {
            let mut at: BTreeMap<String, u64> = {
                let mut st = tx.prepare("SELECT log, seq FROM received WHERE node = ?1").map_err(|e| e.to_string())?;
                let it = st.query_map([node], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))).map_err(|e| e.to_string())?;
                it.filter_map(Result::ok).collect()
            };
            let mut inserts: BTreeMap<&str, rusqlite::Statement<'_>> = BTreeMap::new();
            for l in lines {
                let last = at.get(&l.log).copied().unwrap_or(0);
                if l.seq <= last {
                    continue;
                }
                at.insert(l.log.clone(), l.seq);
                let Some(cols) = schemas().get(&l.log) else { continue };
                if !inserts.contains_key(l.log.as_str()) {
                    let names: Vec<String> = cols.iter().map(|(c, _)| format!("\"{c}\"")).collect();
                    let marks: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
                    let st = tx.prepare(&format!("INSERT INTO \"ds_{}\" ({}) VALUES ({})", l.log, names.join(", "), marks.join(", "))).map_err(|e| e.to_string())?;
                    let key: &str = schemas().get_key_value(&l.log).map(|(k, _)| k.as_str()).unwrap_or("");
                    inserts.insert(key, st);
                }
                let st = inserts.get_mut(l.log.as_str()).expect("prepared");
                let raw = l.event.to_string();
                let vals: Vec<rusqlite::types::Value> = cols
                    .iter()
                    .map(|(c, t)| match c.as_str() {
                        "_time" => rusqlite::types::Value::Integer(l.time as i64),
                        "_node" => rusqlite::types::Value::Text(label.to_string()),
                        "_ingested" => rusqlite::types::Value::Integer(now),
                        "_raw" => rusqlite::types::Value::Text(raw.clone()),
                        other => col_value(l.event.get(other), *t),
                    })
                    .collect();
                st.execute(rusqlite::params_from_iter(vals.iter())).map_err(|e| e.to_string())?;
            }
            drop(inserts);
            for (log, seq) in &at {
                tx.execute("INSERT INTO received (node, log, seq) VALUES (?1, ?2, ?3) ON CONFLICT (node, log) DO UPDATE SET seq = excluded.seq", rusqlite::params![node, log, *seq as i64])
                    .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            Ok(at)
        }
    }

    /// Rows the authority writes itself (alerts, bans): no seq, no dedupe.
    pub fn insert(&self, log: &str, label: &str, rows: &[(u64, Value)]) -> Result<(), String> {
        let Some(cols) = schemas().get(log) else { return Err(format!("no dataset {log}")) };
        if rows.is_empty() {
            return Ok(());
        }
        let now = now_ms() as i64;
        let mut w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let tx = w.transaction().map_err(|e| e.to_string())?;
        {
            let names: Vec<String> = cols.iter().map(|(c, _)| format!("\"{c}\"")).collect();
            let marks: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
            let mut st = tx.prepare(&format!("INSERT INTO \"ds_{log}\" ({}) VALUES ({})", names.join(", "), marks.join(", "))).map_err(|e| e.to_string())?;
            for (time, ev) in rows {
                let raw = ev.to_string();
                let vals: Vec<rusqlite::types::Value> = cols
                    .iter()
                    .map(|(c, t)| match c.as_str() {
                        "_time" => rusqlite::types::Value::Integer(*time as i64),
                        "_node" => rusqlite::types::Value::Text(label.to_string()),
                        "_ingested" => rusqlite::types::Value::Integer(now),
                        "_raw" => rusqlite::types::Value::Text(raw.clone()),
                        other => col_value(ev.get(other), *t),
                    })
                    .collect();
                st.execute(rusqlite::params_from_iter(vals.iter())).map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// A rule's last run and mark (what arrived up to then), when it is due (`every` ms after
    /// the last run); a rule never run: (0, now - window).
    pub fn rule_due(&self, name: &str, every: u64, window: u64, now: u64) -> Option<(u64, u64)> {
        let w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let r: Option<(i64, i64)> = w.query_row("SELECT last_run, mark FROM rule_state WHERE name = ?1", [name], |r| Ok((r.get(0)?, r.get(1)?))).ok();
        match r {
            None => Some((0, now.saturating_sub(window))),
            Some((last, mark)) if now >= last as u64 + every => Some((last as u64, (mark as u64).max(now.saturating_sub(window)))),
            Some(_) => None,
        }
    }

    pub fn rule_ran(&self, name: &str, now: u64, mark: u64, error: Option<&str>, fired: u64) {
        let w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let _ = w.execute(
            "INSERT INTO rule_state (name, last_run, mark, error, fired) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (name) DO UPDATE SET last_run = excluded.last_run, mark = excluded.mark, error = excluded.error, fired = rule_state.fired + excluded.fired",
            rusqlite::params![name, now as i64, mark as i64, error, fired as i64],
        );
    }

    /// Whether a firing of `rule` for `key` is a new alert (none within `suppress` ms); a repeat
    /// is counted.
    pub fn fire_key(&self, rule: &str, key: &str, suppress: u64, now: u64) -> bool {
        let w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let last: Option<i64> = w.query_row("SELECT last FROM rule_keys WHERE rule = ?1 AND key = ?2", [rule, key], |r| r.get(0)).ok();
        let fresh = last.is_none_or(|l| now >= l as u64 + suppress);
        if fresh {
            let _ = w.execute(
                "INSERT INTO rule_keys (rule, key, last, count) VALUES (?1, ?2, ?3, 1) ON CONFLICT (rule, key) DO UPDATE SET last = excluded.last, count = 1",
                rusqlite::params![rule, key, now as i64],
            );
        } else {
            let _ = w.execute("UPDATE rule_keys SET count = count + 1 WHERE rule = ?1 AND key = ?2", [rule, key]);
        }
        // keys long past their suppression: gone
        let _ = w.execute("DELETE FROM rule_keys WHERE last < ?1", [now as i64 - 400 * 86_400_000]);
        fresh
    }

    /// The next ban of `addr`: how many times it has been banned with this one.
    pub fn next_ban(&self, addr: &str, now: u64) -> u32 {
        let w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let times: i64 = w.query_row("SELECT times FROM ban_history WHERE addr = ?1", [addr], |r| r.get(0)).unwrap_or(0) + 1;
        let _ = w.execute(
            "INSERT INTO ban_history (addr, times, last) VALUES (?1, ?2, ?3) ON CONFLICT (addr) DO UPDATE SET times = excluded.times, last = excluded.last",
            rusqlite::params![addr, times, now as i64],
        );
        times as u32
    }

    /// Every rule's last run, error and alerts.
    pub fn rule_states(&self) -> Value {
        let w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(mut st) = w.prepare("SELECT name, last_run, error, fired FROM rule_state") else { return Value::Null };
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, json!({ "last_run": r.get::<_, i64>(1)?, "error": r.get::<_, Option<String>>(2)?, "fired": r.get::<_, i64>(3)? }))));
        rows.map(|it| Value::Object(it.filter_map(Result::ok).collect())).unwrap_or(Value::Null)
    }

    /// Drops lines older than their dataset keeps them (days), in batches; how many went.
    pub fn prune(&self, keep_days: &BTreeMap<String, u64>) -> usize {
        let now = now_ms() as i64;
        let w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let mut n = 0;
        for name in schemas().keys() {
            let days = keep_days.get(name).copied().unwrap_or(365).max(1) as i64;
            let before = now - days * 86_400_000;
            n += w.execute(&format!("DELETE FROM \"ds_{name}\" WHERE \"_time\" < ?1"), [before]).unwrap_or(0);
        }
        n
    }

    /// A query over the range (ms): {kind, columns, rows, ms, sql}.
    pub fn query(&self, text: &str, from: i64, to: i64, limit: usize) -> Result<Value, String> {
        let range = ql::Range { from, to, now: now_ms() as i64, ingested: None };
        let c = ql::compile(text, schemas(), range, limit.clamp(1, 10_000))?;
        let t = std::time::Instant::now();
        let r = self.r.lock().unwrap_or_else(|e| e.into_inner());
        let rows = ql::run(&r, &c, std::time::Duration::from_secs(30))?;
        Ok(json!({ "kind": c.kind, "columns": c.columns, "rows": rows, "ms": t.elapsed().as_millis() as u64 }))
    }

    /// The rows an event rule sees: matching lines that arrived in (after, upto] (ms).
    pub fn query_arrived(&self, text: &str, from: i64, after: i64, upto: i64, limit: usize) -> Result<(Vec<String>, Vec<Vec<Value>>), String> {
        let range = ql::Range { from, to: upto + 1, now: now_ms() as i64, ingested: Some((after, upto)) };
        let c = ql::compile(text, schemas(), range, limit)?;
        let r = self.r.lock().unwrap_or_else(|e| e.into_inner());
        Ok((c.columns.clone(), ql::run(&r, &c, std::time::Duration::from_secs(30))?))
    }

    /// Per dataset: rows, the oldest and newest time.
    pub fn sizes(&self) -> Value {
        let r = self.r.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = serde_json::Map::new();
        for name in schemas().keys() {
            let v = r
                .query_row(&format!("SELECT count(*), min(\"_time\"), max(\"_time\") FROM \"ds_{name}\""), [], |x| Ok(json!({ "rows": x.get::<_, i64>(0)?, "oldest": x.get::<_, Option<i64>>(1)?, "newest": x.get::<_, Option<i64>>(2)? })))
                .unwrap_or(Value::Null);
            out.insert(name.clone(), v);
        }
        Value::Object(out)
    }

    /// The columns of every dataset, for the page's help.
    pub fn describe() -> Value {
        schemas()
            .iter()
            .map(|(n, cols)| (n.clone(), Value::from(cols.iter().map(|(c, t)| json!({ "name": c, "type": format!("{t:?}").to_lowercase() })).collect::<Vec<_>>())))
            .collect::<serde_json::Map<_, _>>()
            .into()
    }
}

// ---- the outbox: http and files lines on the node that saw them -------------------------

/// What a node keeps of its own http and files lines until the authority has them (and its
/// retention after that).
pub struct Outbox {
    db: Mutex<Connection>,
}

impl Outbox {
    pub fn open(dir: &Path) -> Result<Outbox, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let c = open_rw(&dir.join("outbox.db"))?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS lines (seq INTEGER PRIMARY KEY AUTOINCREMENT, log TEXT NOT NULL, time INTEGER NOT NULL, event TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS lines_log ON lines (log, seq);
             CREATE INDEX IF NOT EXISTS lines_time ON lines (time);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Outbox { db: Mutex::new(c) })
    }

    pub fn push(&self, items: &[(String, u64, Value)]) -> Result<(), String> {
        let mut c = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = c.transaction().map_err(|e| e.to_string())?;
        {
            let mut st = tx.prepare("INSERT INTO lines (log, time, event) VALUES (?1, ?2, ?3)").map_err(|e| e.to_string())?;
            for (log, time, ev) in items {
                st.execute(rusqlite::params![log, *time as i64, ev.to_string()]).map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Up to `limit` lines of `log` after seq `after`.
    pub fn read(&self, log: &str, after: u64, limit: usize) -> Vec<Line> {
        let c = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(mut st) = c.prepare("SELECT seq, time, event FROM lines WHERE log = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3") else { return vec![] };
        st.query_map(rusqlite::params![log, after as i64, limit as i64], |r| {
            Ok(Line { log: log.to_string(), seq: r.get::<_, i64>(0)? as u64, time: r.get::<_, i64>(1)? as u64, event: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(Value::Null) })
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    /// The last seq of `log` (0: none).
    pub fn head(&self, log: &str) -> u64 {
        let c = self.db.lock().unwrap_or_else(|e| e.into_inner());
        c.query_row("SELECT coalesce(max(seq), 0) FROM lines WHERE log = ?1", [log], |r| r.get::<_, i64>(0)).map(|x| x as u64).unwrap_or(0)
    }

    /// Drops `log`'s lines older than `before` (ms) that the authority has (seq ≤ `sent`).
    pub fn prune(&self, log: &str, before: u64, sent: u64) -> usize {
        let c = self.db.lock().unwrap_or_else(|e| e.into_inner());
        c.execute("DELETE FROM lines WHERE log = ?1 AND time < ?2 AND seq <= ?3", rusqlite::params![log, before as i64, sent as i64]).unwrap_or(0)
    }
}

// ---- recording: what request handlers and file code call --------------------------------

/// The level each log is written at: 0 off, 1 minimal, 2 standard, 3 detailed (audit::Verbosity's
/// order without debug). The node sets them from the cluster's settings.
static HTTP_LEVEL: AtomicU8 = AtomicU8::new(0);
static FILES_LEVEL: AtomicU8 = AtomicU8::new(0);
/// Lines dropped because the outbox could not keep up.
pub static DROPPED: AtomicU64 = AtomicU64::new(0);
static SINK: OnceLock<mpsc::SyncSender<(String, u64, Value)>> = OnceLock::new();
static OUTBOX: OnceLock<std::sync::Arc<Outbox>> = OnceLock::new();

pub fn set_levels(http: u8, files: u8) {
    HTTP_LEVEL.store(http, Ordering::Relaxed);
    FILES_LEVEL.store(files, Ordering::Relaxed);
}

/// Whether `log` is written at `level` (http/files; others: no).
pub fn on(log: &str, level: u8) -> bool {
    let l = match log {
        "http" => HTTP_LEVEL.load(Ordering::Relaxed),
        "files" => FILES_LEVEL.load(Ordering::Relaxed),
        _ => 0,
    };
    SINK.get().is_some() && l > 0 && l >= level
}

/// A line of `log` (http, files) at `level`, if that log is written at it; never waits.
pub fn record(log: &str, level: u8, event: Value) {
    if !on(log, level) {
        return;
    }
    if let Some(s) = SINK.get() {
        if s.try_send((log.to_string(), now_ms(), event)).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The node's outbox, its writer started on first use.
pub fn outbox(dir: &Path) -> Result<std::sync::Arc<Outbox>, String> {
    if let Some(o) = OUTBOX.get() {
        return Ok(o.clone());
    }
    let o = std::sync::Arc::new(Outbox::open(dir)?);
    let o = OUTBOX.get_or_init(|| o).clone();
    let (tx, rx) = mpsc::sync_channel::<(String, u64, Value)>(20_000);
    if SINK.set(tx).is_ok() {
        let w = o.clone();
        std::thread::Builder::new()
            .name("logs-outbox".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut batch = vec![first];
                    while batch.len() < 1000 {
                        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                            Ok(x) => batch.push(x),
                            Err(_) => break,
                        }
                    }
                    if let Err(e) = w.push(&batch) {
                        DROPPED.fetch_add(batch.len() as u64, Ordering::Relaxed);
                        eprintln!("logs outbox: {e}");
                    }
                }
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(log: &str, seq: u64, time: u64, ev: Value) -> Line {
        Line { log: log.into(), seq, time, event: ev }
    }

    #[test]
    fn ingest_once_and_query() {
        let dir = std::env::temp_dir().join(format!("osada-logstore-{}-{}", std::process::id(), now_ms()));
        let s = Store::open(&dir).unwrap();
        let t = now_ms();
        let lines = vec![
            line("http", 1, t - 3000, json!({ "client": "203.0.113.5", "path": "/.env", "status": 404, "bytes": 10, "ms": 1.5, "host": "shelf.example.org" })),
            line("http", 2, t - 2000, json!({ "client": "203.0.113.5", "path": "/wp-login.php", "status": "404" })),
            line("auth", 7, t - 1000, json!({ "event": "sign_in_failed", "name": "x", "addr": "203.0.113.5", "why": "wrong password" })),
            line("nothing", 1, t, json!({})),
        ];
        let at = s.ingest("id-1", "mail-1", &lines).unwrap();
        assert_eq!(at.get("http"), Some(&2));
        assert_eq!(at.get("auth"), Some(&7));
        // again, and one more: only the new one is kept
        let mut more = lines.clone();
        more.push(line("http", 3, t - 500, json!({ "client": "192.168.1.20", "path": "/", "status": 200 })));
        s.ingest("id-1", "mail-1", &more).unwrap();
        // the same seqs from another node are its own
        s.ingest("id-2", "node-x", &lines[..1]).unwrap();
        let v = s.query("dataset = http | stats count() as n by _node | sort _node", t as i64 - 60_000, t as i64 + 1000, 100).unwrap();
        assert_eq!(v["rows"], json!([["mail-1", 3], ["node-x", 1]]));
        let v = s.query("dataset = http | filter status = 404 and client insubnet \"203.0.113.0/24\" | stats count() as n, values(path) as p by client", t as i64 - 60_000, t as i64 + 1000, 100).unwrap();
        assert_eq!(v["rows"][0][1], json!(3));
        let v = s.query("dataset = auth | fields addr, why", t as i64 - 60_000, t as i64 + 1000, 100).unwrap();
        assert_eq!(v["rows"], json!([["203.0.113.5", "wrong password"]]));
        assert_eq!(s.received("id-1").get("http"), Some(&3));
        // a node whose outbox began anew: its seq 1 is new again
        s.reset("id-2", &["http".to_string()]);
        s.ingest("id-2", "node-x", &lines[..1]).unwrap();
        // what arrived after a moment, as an event rule sees it
        let (_, rows) = s.query_arrived("dataset = auth", t as i64 - 60_000, 0, now_ms() as i64 + 1, 100).unwrap();
        assert_eq!(rows.len(), 1);
        // writing through the query connection is refused
        assert!(s.r.lock().unwrap().execute("DELETE FROM ds_http", []).is_err());
        assert_eq!(s.sizes()["http"]["rows"], json!(5));
        // pruning keeps what is young
        assert_eq!(s.prune(&BTreeMap::new()), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn outbox_reads_in_order_and_prunes_what_was_sent() {
        let dir = std::env::temp_dir().join(format!("osada-outbox-{}-{}", std::process::id(), now_ms()));
        let o = Outbox::open(&dir).unwrap();
        let t = now_ms();
        o.push(&[("http".into(), t - 10_000, json!({ "path": "/a" })), ("files".into(), t - 9_000, json!({ "op": "write" })), ("http".into(), t, json!({ "path": "/b" }))]).unwrap();
        let l = o.read("http", 0, 10);
        assert_eq!(l.iter().map(|x| x.event["path"].as_str().unwrap()).collect::<Vec<_>>(), ["/a", "/b"]);
        assert_eq!(o.read("http", l[0].seq, 10).len(), 1);
        assert_eq!(o.head("http"), l[1].seq);
        // old and sent: gone; old and not sent: kept
        assert_eq!(o.prune("http", t - 1, 0), 0);
        assert_eq!(o.prune("http", t - 1, l[1].seq), 1);
        assert_eq!(o.read("http", 0, 10).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
