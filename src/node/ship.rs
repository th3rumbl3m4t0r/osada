//! Shipping the logs out (T3): every node POSTs its own new auth and admin
//! lines to the cluster's URL as a JSON array, with the secret as a bearer
//! token. Where it is in each log is kept in `<data>/ship.json` (per URL: a
//! new URL gets the whole retained log); how it went in
//! `<data>/ship-status.json`, which the nodes' overview shows.

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Node;
use crate::audit::LogKind;

/// Lines in one POST at most.
const BATCH: usize = 500;

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    url: String,
    /// The last seq shipped, by log.
    at: BTreeMap<LogKind, u64>,
}

fn load(data: &Path) -> Cursor {
    std::fs::read(data.join("ship.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(data: &Path, c: &Cursor) {
    let tmp = data.join("ship.json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec(c).expect("json")).is_ok() {
        let _ = std::fs::rename(tmp, data.join("ship.json"));
    }
}

fn status(data: &Path, v: Value) {
    let tmp = data.join("ship-status.json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec(&v).expect("json")).is_ok() {
        let _ = std::fs::rename(tmp, data.join("ship-status.json"));
    }
}

/// How shipping went on this node, for the overview.
pub fn read_status(data: &Path) -> Value {
    std::fs::read(data.join("ship-status.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
}

/// An error with its causes: reqwest's own says little.
fn err_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(c) = src {
        s.push_str(": ");
        s.push_str(&c.to_string());
        src = c.source();
    }
    s
}

/// `2026-11-16T04:05:06.789Z`
pub fn iso_utc(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, ms % 1000)
}

impl Node {
    /// Up to `limit` lines of this node's `kind` log after seq `after`
    /// (from where it begins if that was pruned), as shipped.
    fn ship_lines(&self, kind: LogKind, after: u64, limit: usize) -> Vec<Value> {
        let inner = self.lock();
        let log = if kind == LogKind::Admin { inner.admin_log.clone() } else { inner.auth.clone() };
        let name = inner.cluster.state().placement.node_label(&self.me);
        drop(inner);
        let after = after.max(log.start().seq);
        let log_name = format!("{kind:?}").to_lowercase();
        log.read(after, limit)
            .unwrap_or_default()
            .into_iter()
            .map(|e| {
                let mut event: Value = serde_json::from_slice(&e.payload).unwrap_or(Value::Null);
                if let Some(o) = event.as_object_mut() {
                    o.remove("token");
                }
                json!({
                    "@timestamp": iso_utc(e.time),
                    "id": format!("{}/{}/{}", self.me.0, log_name, e.seq),
                    "node": self.me.0,
                    "node_name": name,
                    "log": log_name,
                    "seq": e.seq,
                    "time_ms": e.time,
                    "actor": e.actor,
                    "event": event,
                })
            })
            .collect()
    }

    /// Ships this node's logs while the cluster's settings say so.
    pub fn spawn_ship(self: &Arc<Self>) {
        let node = self.clone();
        tokio::spawn(async move {
            // OSADA_SHIP_ROUND: seconds between rounds (15; tests use less)
            let every = std::env::var("OSADA_SHIP_ROUND").ok().and_then(|v| v.parse::<u64>().ok()).filter(|s| *s > 0).unwrap_or(15);
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent(format!("osada/{}", crate::VERSION))
                .build()
            {
                Ok(c) => c,
                Err(e) => return eprintln!("{}: log shipping: {e}", node.me),
            };
            let mut failures = 0u32;
            let mut more = false;
            let mut was_on = false;
            loop {
                let wait = if more { 1 } else { every << failures.min(5) };
                tokio::time::sleep(Duration::from_secs(wait)).await;
                more = false;
                let Some(data) = node.data.get().cloned() else { continue };
                let ship = node.cluster_state().settings.ship;
                if !ship.enabled || ship.url.is_empty() {
                    if was_on || !data.join("ship-status.json").exists() {
                        status(&data, json!({ "state": "off" }));
                    }
                    was_on = false;
                    failures = 0;
                    continue;
                }
                was_on = true;
                let mut cur = load(&data);
                if cur.url != ship.url {
                    cur = Cursor { url: ship.url.clone(), at: BTreeMap::new() };
                }
                let mut sent = 0usize;
                let mut error: Option<String> = None;
                for kind in &ship.logs {
                    let after = cur.at.get(kind).copied().unwrap_or(0);
                    let lines = node.ship_lines(*kind, after, BATCH);
                    let Some(last) = lines.last().and_then(|l| l["seq"].as_u64()) else { continue };
                    let n = lines.len();
                    let mut req = client.post(&ship.url).header("content-type", "application/json").body(serde_json::to_vec(&lines).expect("json"));
                    if !ship.secret.is_empty() {
                        req = req.bearer_auth(&ship.secret);
                    }
                    match req.send().await {
                        Ok(r) if r.status().is_success() => {
                            cur.at.insert(*kind, last);
                            save(&data, &cur);
                            sent += n;
                            more |= n == BATCH;
                        }
                        Ok(r) => {
                            error = Some(format!("{} answered {}", ship.url, r.status()));
                            break;
                        }
                        Err(e) => {
                            error = Some(format!("{}: {}", ship.url, err_chain(&e)));
                            break;
                        }
                    }
                }
                let prev = read_status(&data);
                let now = super::now_ms();
                let total = prev["sent"].as_u64().unwrap_or(0) + sent as u64;
                let last_ok = if error.is_none() { Value::from(now) } else { prev["last_ok"].clone() };
                match &error {
                    Some(e) => {
                        failures += 1;
                        if failures == 1 || failures % 20 == 0 {
                            eprintln!("{}: log shipping: {e}", node.me);
                        }
                    }
                    None => failures = 0,
                }
                let backlog: BTreeMap<String, u64> = ship
                    .logs
                    .iter()
                    .map(|k| {
                        let inner = node.lock();
                        let head = if *k == LogKind::Admin { inner.admin_log.head().seq } else { inner.auth.head().seq };
                        (format!("{k:?}").to_lowercase(), head.saturating_sub(cur.at.get(k).copied().unwrap_or(0)))
                    })
                    .collect();
                status(
                    &data,
                    json!({
                        "state": if error.is_some() { "failing" } else { "ok" },
                        "url": ship.url,
                        "sent": total,
                        "last_ok": last_ok,
                        "last_try": now,
                        "error": error,
                        "failures": failures,
                        "backlog": backlog,
                    }),
                );
            }
        });
    }
}

impl Node {
    /// This node's facts for the overview, how shipping goes included.
    pub(crate) fn node_info(&self) -> Value {
        let data = self.data.get().map(|p| p.as_path());
        let mut v = crate::nodeinfo::local(data);
        v["exposure"] = self.exposure_report();
        // its own signing key (D-U45), and whether the cluster log has it
        v["idp_key"] = self.idp_key().map_or(Value::Null, |k| {
            let published = self.cluster_state().keys.get(&self.me).is_some_and(|p| p.kid == k.public.kid);
            json!({ "kid": k.public.kid, "since": k.public.since, "published": published })
        });
        if let Some(d) = data {
            v["ship"] = read_status(d);
            v["logs"] = super::logs::read_status(d);
            // what it keeps of files (T2): an edge's caches
            let mut files = serde_json::Map::new();
            for part in ["cache", "keep"] {
                if let Some((bytes, n)) = crate::nodeinfo::du(&d.join(part)) {
                    files.insert(part.into(), json!({ "bytes": bytes, "files": n }));
                }
            }
            v["files"] = Value::Object(files);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn iso() {
        assert_eq!(super::iso_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(super::iso_utc(1_789_574_706_789), "2026-09-16T16:05:06.789Z");
        assert_eq!(super::iso_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }
}
