//! Rules and bans on the nodes (D-U32, D-U33): the logs authority runs the cluster's rules over
//! its store and commits the bans they make; every node copies the bans (crate::bans) for its
//! listeners. Security administrators at the root change rules, bans and the never-ban list.

use std::{net::IpAddr, sync::Arc};

use serde_json::{Value, json};

use super::{Actor, Answer, Call, Node, now_ms};
use crate::{
    bans::Ban,
    cluster::Command,
    directory::User,
    rbac::Perm,
    rules::{self, Mode, Rule},
};

/// Bans a rule run makes at most.
const MAX_BANS: usize = 200;

impl Node {
    /// The nodes' own addresses (never banned).
    fn node_addrs(&self) -> Vec<IpAddr> {
        self.cluster_state()
            .placement
            .nodes
            .values()
            .flat_map(|n| n.addrs.iter())
            .filter_map(|a| a.split_once('/').map_or(a.as_str(), |x| x.0).trim().trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().ok())
            .collect()
    }

    /// As the logs authority, each round: the rules that are due, the bans they make, the bans
    /// that ended. The rules a cluster starts with are added once.
    pub(super) async fn run_rules(self: &Arc<Self>) {
        let st = self.cluster_state();
        if !st.settings.rules_seeded {
            let mut s = st.settings.clone();
            for r in rules::builtin() {
                if !s.rules.iter().any(|x| x.name == r.name) {
                    s.rules.push(r);
                }
            }
            s.rules_seeded = true;
            match self.commit_anywhere("rules (the built-in ones)", Command::Settings(s)).await {
                Ok(()) => eprintln!("{}: rules osada-login and osada-scan added (fail2ban's jails)", self.me),
                Err(e) => eprintln!("{}: adding the built-in rules: {e}", self.me),
            }
            return;
        }
        let Some(store) = self.log_store() else { return };
        let now = now_ms();
        let label = format!("{} (rules)", st.placement.node_label(&self.me));
        let nodes = self.node_addrs();
        let mut add: Vec<Ban> = Vec::new();
        let mut alerts: Vec<(u64, Value)> = Vec::new();
        let mut ban_rows: Vec<(u64, Value)> = Vec::new();
        for r in st.settings.rules.iter().filter(|r| r.enabled) {
            let (Some(every), Some(window), Some(suppress)) = (rules::secs(&r.every), rules::secs(&r.window), rules::secs(&r.suppress)) else { continue };
            let Some((_, mark)) = store.rule_due(&r.name, every * 1000, window * 1000, now) else { continue };
            let s2 = store.clone();
            let (q, mode) = (r.query.clone(), r.mode);
            let res = tokio::task::block_in_place(move || match mode {
                Mode::Aggregate => s2.query(&q, (now - window * 1000) as i64, now as i64 + 1, 1000).map(|v| {
                    let cols: Vec<String> = serde_json::from_value(v["columns"].clone()).unwrap_or_default();
                    let rows: Vec<Vec<Value>> = serde_json::from_value(v["rows"].clone()).unwrap_or_default();
                    (cols, rows)
                }),
                Mode::Event => s2.query_arrived(&q, (now - window * 1000) as i64, mark as i64, now as i64, 1000),
            });
            let (cols, rows) = match res {
                Ok(x) => x,
                Err(e) => {
                    store.rule_ran(&r.name, now, mark, Some(&e), 0);
                    continue;
                }
            };
            let mut fired = 0u64;
            for row in rows {
                let obj: serde_json::Map<String, Value> = cols.iter().cloned().zip(row.iter().cloned()).collect();
                let key = if r.key.is_empty() {
                    Value::Object(obj.clone()).to_string()
                } else {
                    r.key.iter().map(|k| obj.get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default()).collect::<Vec<_>>().join("|")
                };
                if !store.fire_key(&r.name, &key, suppress * 1000, now) {
                    continue;
                }
                fired += 1;
                let mut ev = json!({ "rule": r.name, "severity": r.severity, "key": key, "count": 1, "detail": Value::Object(obj.clone()).to_string() });
                if let Some(b) = &r.ban {
                    let addr = obj.get(&b.field).and_then(|v| v.as_str()).and_then(|a| a.trim().parse::<IpAddr>().ok()).map(|ip| ip.to_canonical());
                    match addr {
                        None => ev["note"] = Value::from(format!("no address in {}", b.field)),
                        Some(ip) if rules::never_banned(ip, &st.settings.never_ban, &nodes) => ev["note"] = Value::from("never banned"),
                        Some(ip) if st.bans.get(&ip.to_string()).is_some_and(|x| x.until > now) || add.iter().any(|x| x.addr == ip.to_string()) => {
                            ev["note"] = Value::from("banned already")
                        }
                        Some(_) if add.len() >= MAX_BANS => ev["note"] = Value::from("too many bans in one run"),
                        Some(ip) => {
                            let a = ip.to_string();
                            let times = store.next_ban(&a, now);
                            let secs = rules::ban_secs(b, times);
                            let ban = Ban { addr: a.clone(), until: now + secs * 1000, since: now, rule: r.name.clone(), times };
                            ev["ban"] = json!({ "addr": a, "secs": secs, "times": times });
                            ban_rows.push((now, json!({ "action": "ban", "addr": a, "rule": r.name, "until": ban.until, "times": times, "by": r.name })));
                            add.push(ban);
                        }
                    }
                }
                alerts.push((now, ev));
            }
            store.rule_ran(&r.name, now, now, None, fired);
        }
        // bans that ended go from the list (once a minute is enough)
        let expired: Vec<String> = if now % 60_000 < 6_000 || !add.is_empty() { st.bans.values().filter(|b| b.until <= now).map(|b| b.addr.clone()).collect() } else { vec![] };
        for a in &expired {
            ban_rows.push((now, json!({ "action": "expired", "addr": a, "by": "rules" })));
        }
        if !add.is_empty() || !expired.is_empty() {
            let n = add.len();
            if let Err(e) = self.commit_anywhere("rules", Command::Bans { add, remove: expired }).await {
                eprintln!("{}: committing {n} bans: {e}", self.me);
            }
        }
        let _ = store.insert("alerts", &label, &alerts);
        let _ = store.insert("bans", &label, &ban_rows);
    }

    pub(super) fn answer_rule_states(&self) -> Answer {
        match self.log_store() {
            Some(s) => Answer::Web { value: s.rule_states() },
            None => Answer::Refused { why: "this node keeps no logs".into() },
        }
    }

    /// Rules, bans, the never-ban list and how the rules ran, for logs-read at the root.
    pub async fn rules_view(self: &Arc<Self>, viewer: &User) -> Result<Value, String> {
        let root = self.root_ou();
        if !self.may(viewer, Perm::LogsRead, root) {
            return Err("this needs logs-read at the root".into());
        }
        let st = self.cluster_state();
        let states = match self.logs_authority() {
            Some(a) => match self.ask(&a, Call::RuleState).await {
                Ok(Answer::Web { value }) => value,
                _ => Value::Null,
            },
            None => Value::Null,
        };
        let now = now_ms();
        let bans: Vec<&Ban> = st.bans.values().filter(|b| b.until > now).collect();
        Ok(json!({
            "rules": st.settings.rules, "states": states, "bans": bans, "never_ban": st.settings.never_ban,
            "may_change": self.may(viewer, Perm::OuSecurity, root),
            "refused": crate::bans::REFUSED.load(std::sync::atomic::Ordering::Relaxed),
        }))
    }

    /// Adds, changes (`old`: its name before) or removes (`rule` None) a rule.
    pub async fn set_rule(self: &Arc<Self>, actor: &Actor, name: &str, rule: Option<Rule>) -> Result<(), String> {
        self.require(actor, Perm::OuSecurity, self.root_ou())?;
        let mut s = self.cluster_state().settings;
        let what = match rule {
            None => {
                let before = s.rules.len();
                s.rules.retain(|r| r.name != name);
                if s.rules.len() == before {
                    return Err(format!("no rule {name}"));
                }
                format!("rule {name}: removed")
            }
            Some(r) => {
                r.check(crate::logstore::schemas())?;
                if r.name != name && s.rules.iter().any(|x| x.name == r.name) {
                    return Err(format!("there is a rule {} already", r.name));
                }
                let what = match s.rules.iter_mut().find(|x| x.name == name) {
                    Some(x) => {
                        if *x == r {
                            return Ok(());
                        }
                        *x = r.clone();
                        format!("rule {name}: changed{}", if r.name != name { format!(" (now {})", r.name) } else { String::new() })
                    }
                    None => {
                        s.rules.push(r.clone());
                        format!("rule {}: added", r.name)
                    }
                };
                what
            }
        };
        self.commit_anywhere(&actor.label(), Command::Settings(s)).await?;
        self.log_admin(actor, "rule_changed", json!({ "rule": name, "changes": [what] }));
        Ok(())
    }

    /// What a rule would do now, without doing it: the rows it sees and the addresses it bans.
    pub async fn test_rule(self: &Arc<Self>, actor: &Actor, r: Rule) -> Result<Value, String> {
        self.require(actor, Perm::LogsRead, self.root_ou())?;
        r.check(crate::logstore::schemas())?;
        let window = rules::secs(&r.window).unwrap_or(600);
        let now = now_ms() as i64;
        let v = self.logs_ql(&actor.user, r.query.clone(), now - window as i64 * 1000, now + 1, 1000).await?;
        let cols: Vec<String> = serde_json::from_value(v["columns"].clone()).unwrap_or_default();
        let st = self.cluster_state();
        let nodes = self.node_addrs();
        let mut would = Vec::new();
        if let Some(b) = &r.ban {
            if let Some(i) = cols.iter().position(|c| *c == b.field) {
                for row in v["rows"].as_array().into_iter().flatten() {
                    let Some(ip) = row[i].as_str().and_then(|a| a.trim().parse::<IpAddr>().ok()) else { continue };
                    let why = if rules::never_banned(ip, &st.settings.never_ban, &nodes) {
                        "never banned"
                    } else if st.bans.get(&ip.to_canonical().to_string()).is_some_and(|x| x.until > now as u64) {
                        "banned already"
                    } else {
                        "would be banned"
                    };
                    would.push(json!({ "addr": ip.to_canonical().to_string(), "what": why }));
                }
            }
        }
        Ok(json!({ "columns": v["columns"], "rows": v["rows"], "bans": would, "note": if r.mode == Mode::Event { "an event rule: every line in the window, not only new ones" } else { "" } }))
    }

    /// Bans an address by hand (for `secs`), or lifts a ban (`secs` 0).
    pub async fn set_ban(self: &Arc<Self>, actor: &Actor, addr: &str, secs: u64) -> Result<(), String> {
        self.require(actor, Perm::OuSecurity, self.root_ou())?;
        let ip: IpAddr = addr.trim().parse().map_err(|_| format!("not an address: {addr:?}"))?;
        let a = ip.to_canonical().to_string();
        let now = now_ms();
        let cmd = if secs == 0 {
            if !self.cluster_state().bans.contains_key(&a) {
                return Err(format!("{a} is not banned"));
            }
            Command::Bans { add: vec![], remove: vec![a.clone()] }
        } else {
            if rules::never_banned(ip, &self.cluster_state().settings.never_ban, &self.node_addrs()) {
                return Err(format!("{a} is never banned (a private range, Cloudflare's, a node's or in the never-ban list)"));
            }
            if secs > 400 * 86_400 {
                return Err("a ban of at most 400 days".into());
            }
            Command::Bans { add: vec![Ban { addr: a.clone(), until: now + secs * 1000, since: now, rule: format!("admin:{}", actor.user.name), times: 1 }], remove: vec![] }
        };
        self.commit_anywhere(&actor.label(), cmd).await?;
        let what = if secs == 0 { format!("{a}: unbanned") } else { format!("{a}: banned for {secs} s") };
        self.log_admin(actor, "ban_changed", json!({ "addr": a, "changes": [what] }));
        if let Some(s) = self.logs_authority().filter(|x| x == &self.me).and_then(|_| self.log_store()) {
            let _ = s.insert("bans", &self.cluster_state().placement.node_label(&self.me), &[(now, json!({ "action": if secs == 0 { "unban" } else { "ban" }, "addr": a, "by": actor.user.name, "until": if secs == 0 { Value::Null } else { Value::from(now + secs * 1000) } }))]);
        }
        Ok(())
    }

    /// The addresses and subnets never banned (besides private ranges, Cloudflare's, the nodes').
    pub async fn set_never_ban(self: &Arc<Self>, actor: &Actor, list: Vec<String>) -> Result<(), String> {
        self.require(actor, Perm::OuSecurity, self.root_ou())?;
        let list: Vec<String> = list.into_iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        rules::check_never(&list)?;
        let mut s = self.cluster_state().settings;
        if s.never_ban == list {
            return Ok(());
        }
        s.never_ban = list.clone();
        self.commit_anywhere(&actor.label(), Command::Settings(s)).await?;
        self.log_admin(actor, "never_ban_changed", json!({ "changes": [format!("never banned: {}", if list.is_empty() { "(only the built-in ranges)".to_string() } else { list.join(", ") })] }));
        Ok(())
    }
}
