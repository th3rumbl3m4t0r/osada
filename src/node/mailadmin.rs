//! admin → mail (D-U38): the mail instance's Stalwart (crate::mail), through the node it runs on.
//!
//! The mailboxes are osada's (D-U43): a user's mailbox (address, size, on or off) is in the
//! directory, set from admin → mail or the user card, and the voter makes Stalwart's accounts
//! match: once a minute and right after a change, it makes the accounts that are missing (and
//! their domains when the settings say so) and sets size and on/off where they differ. It never
//! removes an account: that is "remove the mailbox with its mail", by an administrator. Accounts
//! Stalwart has that no mailbox names are left as they are (shown as not osada's; adopting one
//! is setting it as a user's mailbox).

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::{Value, json};

use super::{Actor, Answer, Call, Node};
use crate::{
    directory::{self, DIRECTORY, DirOp, Mailbox, User},
    placement::{Domain, ModuleKind, NodeId, OuId},
    rbac::{Perm, Subject},
};

/// A reconcile under way (one at a time).
static RUNNING: AtomicBool = AtomicBool::new(false);
/// What the last reconcile could not do, by user id.
static ERRORS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

fn errors() -> std::sync::MutexGuard<'static, BTreeMap<String, String>> {
    ERRORS.lock().unwrap_or_else(|e| e.into_inner())
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(str::to_lowercase)).collect()).unwrap_or_default()
}

/// What makes Stalwart (its accounts and domains, as `state` lists them) what the users' mailboxes
/// say: the operations, each with the user it is for. `import` (done at home): a mailbox whose
/// aliases are not known yet gets Stalwart's (its aliases, `*@domain` for each domain whose
/// catch-all it is), and is planned with them, so that nothing is taken away before osada knows it.
/// `upn_domain` (Round 22): each mailbox is also its account's UPN, `<name>@<upn_domain>`, an
/// alias Stalwart is given (with the domain, made when it lacks it) but the mailbox does not list.
pub fn plan(users: &[User], accounts: &[Value], domains: &[Value], make_domains: bool, upn_domain: Option<&str>) -> Vec<(String, Value)> {
    let by_address: BTreeMap<String, &Value> =
        accounts.iter().filter_map(|a| a["email"].as_str().map(|e| (e.to_lowercase(), a))).collect();
    let catch: BTreeMap<String, String> = domains
        .iter()
        .filter_map(|d| Some((d["name"].as_str()?.to_lowercase(), d["catch_all"].as_str()?.to_lowercase())))
        .collect();
    let mut out = Vec::new();
    for u in users {
        let Some(m) = &u.mailbox else { continue };
        let off = m.off || u.disabled;
        let quota = m.quota.filter(|q| *q > 0);
        let acct = by_address.get(&m.address);
        let upn = upn_domain.map(|d| format!("{}@{d}", u.name.to_lowercase())).filter(|a| *a != m.address && crate::directory::valid_address(a));
        let aliases: Vec<String> = match &m.aliases {
            Some(list) => list.clone(),
            None => {
                let mut list: Vec<String> = acct.map(|a| strings(&a["aliases"])).unwrap_or_default();
                list.extend(catch.iter().filter(|(_, to)| **to == m.address).map(|(d, _)| format!("*@{d}")));
                list.retain(|a| *a != m.address && Some(a) != upn.as_ref());
                list.sort();
                list.dedup();
                out.push((u.id.clone(), json!({ "op": "import", "aliases": list })));
                list
            }
        };
        let mut plain: Vec<String> = aliases.iter().filter(|a| !a.starts_with("*@")).cloned().collect();
        plain.extend(upn.clone());
        plain.sort();
        plain.dedup();
        // the UPN's domain is made in Stalwart when it lacks it, whatever make_domains says
        let make_for = |had: &[String]| make_domains || upn.as_ref().is_some_and(|a| !had.contains(a));
        let stars: Vec<&str> = aliases.iter().filter_map(|a| a.strip_prefix("*@")).collect();
        match acct {
            None => {
                let description = if u.display.trim().is_empty() { u.name.clone() } else { u.display.clone() };
                out.push((u.id.clone(), json!({ "op": "account_create", "address": m.address, "quota": quota, "off": off,
                    "description": description, "make_domain": make_for(&[]), "aliases": plain })));
            }
            Some(a) => {
                let mut op = json!({ "op": "account_update", "id": a["id"] });
                let mut differs = false;
                if a["quota"].as_u64().filter(|q| *q > 0) != quota {
                    op["quota"] = json!(quota);
                    differs = true;
                }
                if a["disabled"].as_bool().unwrap_or(false) != off {
                    op["off"] = json!(off);
                    differs = true;
                }
                let mut have = strings(&a["aliases"]);
                have.sort();
                if have != plain {
                    op["aliases"] = json!(plain);
                    op["make_domain"] = json!(make_for(&have));
                    differs = true;
                }
                if differs {
                    out.push((u.id.clone(), op));
                }
                // catch-alls it no longer is
                for (d, to) in &catch {
                    if *to == m.address && !stars.contains(&d.as_str()) {
                        out.push((u.id.clone(), json!({ "op": "catch_all", "domain": d, "address": null })));
                    }
                }
            }
        }
        for d in stars {
            if catch.get(d) != Some(&m.address) {
                out.push((u.id.clone(), json!({ "op": "catch_all", "domain": d, "address": m.address, "make_domain": make_domains })));
            }
        }
    }
    out
}

/// The instance a new mailbox of an account in `ou` goes to (Round 18): the one on the OU's mail
/// master (the mail tab: the master node for each OU, nearest OU first); when that node runs none,
/// the only instance there is, else the one on the master of /, else the first by name.
pub fn instance_for(p: &crate::placement::Placement, ou: OuId) -> Option<String> {
    let instances: Vec<(&String, &crate::placement::Module)> = p.modules.iter().filter(|(_, m)| m.kind == ModuleKind::Data(Domain::Mail)).collect();
    let on = |node: &NodeId| instances.iter().find(|(_, m)| m.node == *node).map(|(n, _)| (*n).clone());
    if let Some(i) = p.authority(Domain::Mail, ou).ok().and_then(|a| on(&a.node)) {
        return Some(i);
    }
    if instances.len() == 1 {
        return Some(instances[0].0.clone());
    }
    p.root().and_then(|r| p.authority(Domain::Mail, r).ok()).and_then(|a| on(&a.node)).or_else(|| instances.first().map(|(n, _)| (*n).clone()))
}

impl Node {
    /// As a node of a mail instance: an operation on its Stalwart, for the voter (whose admin API
    /// checked who asks).
    pub(super) fn answer_mail(&self, from: &NodeId, op: Value) -> Answer {
        let state = self.cluster_state();
        if from != &self.me && self.voter_in(&state).as_ref() != Some(from) {
            return Answer::Refused { why: "mail is managed through the voter".into() };
        }
        if !state.placement.modules.values().any(|m| m.kind == ModuleKind::Data(Domain::Mail) && m.node == self.me) {
            return Answer::Refused { why: "no mail instance runs on this node".into() };
        }
        let Some(data) = self.data.get() else { return Answer::Refused { why: "no data folder".into() } };
        let sw = match crate::mail::Stalwart::from_data(data) {
            Ok(s) => s,
            Err(why) => return Answer::Refused { why },
        };
        match tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(sw.op(&op))) {
            Ok(value) => Answer::Web { value },
            Err(why) => Answer::Refused { why },
        }
    }

    /// admin → mail: `op` on a mail instance's Stalwart (`instance`, else the one of `user`'s
    /// mailbox, else the first). Reading needs directory-read, a mailbox's size directory-manage,
    /// domains cluster-manage (at the root).
    pub async fn mail(self: &Arc<Self>, actor: &Actor, op: Value) -> Result<Value, String> {
        match op["op"].as_str().unwrap_or("") {
            "mailbox_set" | "mailbox_remove" | "mail_settings" | "upn_domain" | "reconcile" => return self.mail_manage(actor, op).await,
            o if o.starts_with("account_") => return Err("accounts follow osada's mailboxes: set or remove the mailbox".into()),
            _ => {}
        }
        let perm = match op["op"].as_str() {
            Some("state") => Perm::DirectoryRead,
            Some("quota") => Perm::DirectoryManage,
            _ => Perm::ClusterManage,
        };
        self.require(actor, perm, self.root_ou())?;
        let p = self.cluster_state().placement;
        let instances = self.mail_instances();
        let list: Vec<Value> = instances
            .iter()
            .map(|(n, m)| json!({ "name": n, "title": m.title, "node": m.node, "node_name": p.node_label(&m.node), "host": m.host }))
            .collect();
        let of_user = op["user"].as_str().and_then(|id| self.find_user(id).or_else(|| self.find_user_by_name(id))).and_then(|u| self.mailbox_instance(&u));
        let want = op["instance"].as_str().map(str::to_string).or(of_user);
        let picked = match &want {
            Some(w) => instances.iter().find(|(n, _)| n == w),
            None => instances.first(),
        };
        let Some((name, m)) = picked else {
            let why = match want {
                Some(w) if !instances.is_empty() => format!("no mail instance {w}"),
                _ => "no mail instance yet: modules → add one of kind mail, on the node where Stalwart runs".into(),
            };
            return Ok(json!({ "instance": null, "instances": list, "why": why }));
        };
        let instance = json!({ "name": name, "title": m.title, "node": m.node, "node_name": p.node_label(&m.node), "host": m.host });
        let result = match self.instance_call(name, op.clone()).await {
            Ok(value) => value,
            Err(why) if perm == Perm::DirectoryRead => return Ok(json!({ "instance": instance, "instances": list, "why": why })),
            Err(why) => return Err(why),
        };
        if perm != Perm::DirectoryRead {
            self.log_admin(actor, "mail_changed", json!({ "instance": name, "op": op }));
        }
        Ok(json!({ "instance": instance, "instances": list, "result": result }))
    }
}

impl Node {
    /// The mail instances (D-U16), by name: each a Stalwart of its own on its node (Round 18).
    pub fn mail_instances(&self) -> Vec<(String, crate::placement::Module)> {
        let p = self.cluster_state().placement;
        p.modules.iter().filter(|(_, m)| m.kind == ModuleKind::Data(Domain::Mail)).map(|(n, m)| (n.clone(), m.clone())).collect()
    }

    /// Whether a mail instance runs on this node.
    pub(super) fn mail_here(&self) -> bool {
        self.mail_instances().iter().any(|(_, m)| m.node == self.me)
    }

    /// The nodes that answer for a mail instance, the one to ask first first: today the node it
    /// runs on. (A Stalwart cluster, later, is one instance whose nodes share its stores: any of
    /// them answers, and this lists them.)
    fn instance_nodes(&self, name: &str) -> Vec<NodeId> {
        self.cluster_state().placement.modules.get(name).filter(|m| m.kind == ModuleKind::Data(Domain::Mail)).map(|m| vec![m.node.clone()]).unwrap_or_default()
    }

    /// The instance a new mailbox of an account in `ou` goes to ([`instance_for`]).
    pub fn mail_instance_for(&self, ou: OuId) -> Option<String> {
        instance_for(&self.cluster_state().placement, ou)
    }

    /// The instance a user's mailbox is on: its own (once known), else the one for its OU.
    pub fn mailbox_instance(&self, u: &User) -> Option<String> {
        let m = u.mailbox.as_ref()?;
        m.instance.clone().or_else(|| self.mail_instance_for(u.ou))
    }

    /// `op` on the instance's Stalwart: its nodes in turn, until one answers (Refused is an answer).
    async fn instance_call(self: &Arc<Self>, name: &str, op: Value) -> Result<Value, String> {
        let nodes = self.instance_nodes(name);
        let p = self.cluster_state().placement;
        let mut last = format!("no mail instance {name}");
        for n in &nodes {
            match self.ask(n, Call::Mail { op: op.clone() }).await {
                Ok(Answer::Web { value }) => return Ok(value),
                Ok(Answer::Refused { why }) => return Err(why),
                Ok(other) => return Err(format!("{other:?}")),
                Err(e) => last = format!("{} does not answer: {e}", p.node_label(n)),
            }
        }
        Err(last)
    }

    /// The instance whose Stalwart has an account for `address` (those that answer).
    async fn find_account(self: &Arc<Self>, address: &str) -> Option<String> {
        for (name, _) in self.mail_instances() {
            let Ok(st) = self.instance_call(&name, json!({ "op": "state" })).await else { continue };
            if st["accounts"].as_array().is_some_and(|l| l.iter().any(|a| a["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(address)))) {
                return Some(name);
            }
        }
        None
    }

    /// What the last reconcile could not do, by user id.
    pub fn mail_errors(&self) -> BTreeMap<String, String> {
        errors().clone()
    }

    /// As the voter: makes Stalwart's accounts what the mailboxes say (see the top), on each
    /// instance; how many operations it did. One at a time; another call meanwhile does nothing.
    pub async fn mail_reconcile(self: &Arc<Self>) -> Result<usize, String> {
        if self.mail_instances().is_empty() {
            return Ok(0);
        }
        // nothing to keep before the first mailbox is osada's (no asking Stalwart for nothing)
        if !self.lock().dirs.values().any(|d| d.state.users.values().any(|u| u.mailbox.is_some())) {
            return Ok(0);
        }
        if RUNNING.swap(true, Ordering::AcqRel) {
            return Ok(0);
        }
        let r = self.mail_reconcile_now().await;
        RUNNING.store(false, Ordering::Release);
        r
    }

    async fn mail_reconcile_now(self: &Arc<Self>) -> Result<usize, String> {
        let instances = self.mail_instances();
        let mut states: BTreeMap<String, Value> = BTreeMap::new();
        let mut away: BTreeMap<String, String> = BTreeMap::new();
        for (name, _) in &instances {
            match self.instance_call(name, json!({ "op": "state" })).await {
                Ok(v) => {
                    states.insert(name.clone(), v);
                }
                Err(e) => {
                    away.insert(name.clone(), e);
                }
            }
        }
        if states.is_empty() {
            return Err(away.into_values().next().unwrap_or_else(|| "no mail instance answers".into()));
        }
        let has = |st: &Value, address: &str| st["accounts"].as_array().is_some_and(|l| l.iter().any(|a| a["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(address))));
        let mut users = self.users();
        let mut failed = BTreeMap::new();
        // a mailbox not yet known to be on an instance: the one that has its account, else (when
        // every instance answered) the one for its OU (Round 18)
        for u in users.iter_mut() {
            let Some(m) = u.mailbox.as_mut() else { continue };
            if m.instance.as_ref().is_some_and(|i| instances.iter().any(|(n, _)| n == i)) {
                continue;
            }
            let found = states.iter().find(|(_, st)| has(st, &m.address)).map(|(n, _)| n.clone());
            let target = match found {
                Some(f) => f,
                None if away.is_empty() => match instance_for(&self.cluster_state().placement, u.ou) {
                    Some(t) => t,
                    None => continue,
                },
                None => continue,
            };
            let mut pinned = m.clone();
            pinned.instance = Some(target.clone());
            let dir = DirOp::SetMailbox { id: u.id.clone(), mailbox: Some(pinned.clone()) };
            let r = match serde_json::to_value(dir) {
                Ok(v) => self.submit(directory::directory_of(u.ou), DIRECTORY, v, &format!("node:{}", self.me)).await.map(|_| ()),
                Err(e) => Err(e.to_string()),
            };
            match r {
                Ok(()) => {
                    self.log_admin(&Actor::node(&self.me), "mailbox_instance", json!({ "user": u.name, "address": m.address, "instance": target }));
                    *m = pinned;
                }
                Err(why) => {
                    failed.insert(u.id.clone(), format!("noting its instance {target}: {why}"));
                }
            }
        }
        let make_domains = self.cluster_state().settings.mail.make_domains;
        let upn_domain = self.upn_domain();
        let mut done = 0;
        for (name, state) in &states {
            let accounts = state["accounts"].as_array().cloned().unwrap_or_default();
            let domains = state["domains"].as_array().cloned().unwrap_or_default();
            let mine: Vec<User> = users.iter().filter(|u| u.mailbox.as_ref().is_some_and(|m| m.instance.as_deref() == Some(name.as_str()))).cloned().collect();
            for (user, op) in plan(&mine, &accounts, &domains, make_domains, upn_domain.as_deref()) {
                if op["op"] == "import" {
                    // the aliases and catch-alls Stalwart has, into the mailbox (once)
                    let Some(u) = mine.iter().find(|u| u.id == user) else { continue };
                    let Some(mut m) = u.mailbox.clone() else { continue };
                    m.aliases = Some(strings(&op["aliases"]));
                    let dir = DirOp::SetMailbox { id: u.id.clone(), mailbox: Some(m) };
                    let r = match serde_json::to_value(dir) {
                        Ok(v) => self.submit(directory::directory_of(u.ou), DIRECTORY, v, &format!("node:{}", self.me)).await.map(|_| ()),
                        Err(e) => Err(e.to_string()),
                    };
                    match r {
                        Ok(()) => self.log_admin(&Actor::node(&self.me), "mailbox_imported", json!({ "user": u.name, "aliases": op["aliases"] })),
                        Err(why) => {
                            failed.insert(user, format!("taking its addresses from Stalwart: {why}"));
                        }
                    }
                    continue;
                }
                match self.instance_call(name, op.clone()).await {
                    Ok(_) => {
                        done += 1;
                        let who = mine.iter().find(|u| u.id == user).map_or(user.clone(), |u| u.name.clone());
                        self.log_admin(&Actor::node(&self.me), "mail_changed", json!({ "user": who, "instance": name, "op": op }));
                    }
                    Err(why) => {
                        failed.insert(user, why);
                    }
                }
            }
        }
        // the mailboxes of an instance that did not answer, or of one there is not
        for u in &users {
            let Some(m) = &u.mailbox else { continue };
            match m.instance.as_deref() {
                Some(i) if away.contains_key(i) => {
                    failed.entry(u.id.clone()).or_insert_with(|| format!("its instance {i}: {}", away[i]));
                }
                Some(i) if !instances.iter().any(|(n, _)| n == i) => {
                    failed.entry(u.id.clone()).or_insert_with(|| format!("its instance {i} is not there (modules)"));
                }
                None if !away.is_empty() => {
                    failed.entry(u.id.clone()).or_insert_with(|| "not known yet which instance has it (one does not answer)".into());
                }
                _ => {}
            }
        }
        *errors() = failed;
        Ok(done)
    }

    /// admin → mail and the user card: a user's mailbox made or changed (`mailbox_set`: user,
    /// address (a new one: the template's when none), quota (bytes, null: no limit), off,
    /// instance (a new one: the one for its OU when none)), taken away (`mailbox_remove`: user,
    /// with `delete` its account and mail too), the settings of new mailboxes (`mail_settings`),
    /// or a reconcile now (`reconcile`).
    async fn mail_manage(self: &Arc<Self>, actor: &Actor, op: Value) -> Result<Value, String> {
        if self.mail_instances().is_empty() && op["op"] != "upn_domain" {
            return Err("no mail instance yet: modules → add one of kind mail".into());
        }
        let user = || -> Result<User, String> {
            let id = op["user"].as_str().unwrap_or("");
            self.find_user(id).or_else(|| self.find_user_by_name(id)).ok_or_else(|| format!("no user {id}"))
        };
        match op["op"].as_str().unwrap_or("") {
            "mailbox_set" => {
                let u = user()?;
                self.require(actor, Perm::DirectoryManage, u.ou)?;
                let settings = self.cluster_state().settings.mail;
                let new = u.mailbox.is_none();
                let mut m = u.mailbox.clone().unwrap_or(Mailbox { address: String::new(), aliases: None, quota: settings.quota, off: false, instance: None });
                // where it is now (Round 18): noted, else the instance that has its account, else the one for its OU
                let now = match u.mailbox.as_ref() {
                    Some(m) if m.instance.is_none() => self.find_account(&m.address).await.or_else(|| self.mail_instance_for(u.ou)),
                    Some(_) => self.mailbox_instance(&u),
                    None => None,
                };
                let mut renamed = None;
                if let Some(a) = op["address"].as_str().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()) {
                    if !new && a != m.address {
                        renamed = Some(m.address.clone());
                    }
                    m.address = a;
                }
                if m.address.is_empty() {
                    m.address = settings.address_for(&u.name).ok_or("no address given, and no template for new mailboxes (admin → mail)")?;
                }
                // its instance: asked for; else where it is; a new one where its account is (adopted), else the one for its OU
                let instance = match op["instance"].as_str().filter(|i| !i.is_empty()) {
                    Some(i) => {
                        if !self.mail_instances().iter().any(|(n, _)| n == i) {
                            return Err(format!("no mail instance {i}"));
                        }
                        i.to_string()
                    }
                    None if new => match self.find_account(&m.address).await {
                        Some(i) => i,
                        None => self.mail_instance_for(u.ou).ok_or("no mail instance for its OU")?,
                    },
                    None => now.clone().ok_or("no mail instance for its OU")?,
                };
                if let Some(was) = now.as_deref().filter(|n| !new && *n != instance) {
                    // its account there, with its mail, would stay behind: moving mail is not built yet
                    let old = renamed.as_deref().unwrap_or(&m.address).to_string();
                    let st = self.instance_call(was, json!({ "op": "state" })).await?;
                    let there = st["accounts"].as_array().is_some_and(|l| l.iter().any(|a| a["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(&old))));
                    if there {
                        return Err(format!(
                            "{old} is on {was} with its mail; moving a mailbox's mail to another instance is not built yet (remove it there with its mail first, or keep it where it is)"
                        ));
                    }
                }
                m.instance = Some(instance.clone());
                // its other addresses (D-U44): name@domain, or *@domain for the domain's catch-all
                if let Some(list) = op["aliases"].as_array() {
                    let mut v: Vec<String> = list.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty() && *x != m.address).collect();
                    v.dedup();
                    m.aliases = Some(v);
                }
                m.check()?;
                // a new address: Stalwart's account is renamed now (its mail stays); not there yet: nothing to rename
                if let Some(old) = &renamed {
                    let state = self.instance_call(&instance, json!({ "op": "state" })).await?;
                    let id = state["accounts"].as_array().and_then(|l| l.iter().find(|a| a["email"].as_str().map(str::to_lowercase).as_deref() == Some(old.as_str())).map(|a| a["id"].clone()));
                    if let Some(id) = id {
                        let r = json!({ "op": "account_update", "id": id, "address": m.address, "make_domain": settings.make_domains });
                        self.instance_call(&instance, r).await?;
                        self.log_admin(actor, "mailbox_renamed", json!({ "user": u.name, "from": old, "to": m.address, "instance": instance }));
                    }
                }
                if op.get("quota").is_some() {
                    m.quota = op["quota"].as_u64().filter(|q| *q > 0);
                }
                if let Some(off) = op["off"].as_bool() {
                    m.off = off;
                }
                let dir = DirOp::SetMailbox { id: u.id.clone(), mailbox: Some(m.clone()) };
                self.submit(directory::directory_of(u.ou), DIRECTORY, serde_json::to_value(dir).map_err(|e| e.to_string())?, &actor.label()).await?;
                self.log_admin(actor, "mailbox_set", json!({ "user": u.name, "mailbox": m }));
                if new {
                    // the webmail is for those with access to the instance (D-U16)
                    let has = self.lock().dirs.values().any(|d| d.state.access_for(&u.id).iter().any(|a| a.module == instance));
                    if !has {
                        if let Err(e) = self.grant_access(actor, &instance, Subject::User { id: u.id.clone() }).await {
                            eprintln!("{}: access to {instance} for {}: {e}", self.me, u.name);
                        }
                    }
                }
                let done = self.mail_reconcile().await;
                let error = match done {
                    Err(e) => Some(e),
                    Ok(_) => errors().get(&u.id).cloned(),
                };
                Ok(json!({ "mailbox": m, "error": error }))
            }
            "mailbox_remove" => {
                let u = user()?;
                self.require(actor, Perm::DirectoryManage, u.ou)?;
                let m = u.mailbox.clone().ok_or_else(|| format!("{} has no mailbox", u.name))?;
                let delete = op["delete"].as_bool().unwrap_or(false);
                let instance = self.mailbox_instance(&u);
                if delete {
                    let instance = instance.clone().ok_or("no mail instance for it")?;
                    let state = self.instance_call(&instance, json!({ "op": "state" })).await?;
                    let id = state["accounts"].as_array().and_then(|l| l.iter().find(|a| a["email"].as_str().map(str::to_lowercase).as_deref() == Some(&m.address)).map(|a| a["id"].clone()));
                    if let Some(id) = id {
                        self.instance_call(&instance, json!({ "op": "account_destroy", "id": id })).await?;
                    }
                }
                let dir = DirOp::SetMailbox { id: u.id.clone(), mailbox: None };
                self.submit(directory::directory_of(u.ou), DIRECTORY, serde_json::to_value(dir).map_err(|e| e.to_string())?, &actor.label()).await?;
                self.log_admin(actor, "mailbox_removed", json!({ "user": u.name, "address": m.address, "with_mail": delete, "instance": instance }));
                errors().remove(&u.id);
                Ok(json!({}))
            }
            "mail_settings" => {
                self.require(actor, Perm::ClusterManage, self.root_ou())?;
                let mail: crate::audit::MailSettings = serde_json::from_value(op["settings"].clone()).map_err(|e| e.to_string())?;
                mail.check()?;
                let mut s = self.cluster_state().settings;
                s.mail = mail.clone();
                self.commit_anywhere(&actor.label(), crate::cluster::Command::Settings(s)).await?;
                self.log_admin(actor, "mail_settings", json!({ "settings": mail }));
                Ok(json!({}))
            }
            "upn_domain" => {
                // Round 22: the domain of every account's UPN
                self.require(actor, Perm::ClusterManage, self.root_ou())?;
                let d = op["domain"].as_str().unwrap_or("").trim().trim_start_matches('@').to_ascii_lowercase();
                if !crate::directory::valid_address(&format!("x@{d}")) {
                    return Err(format!("{d:?} is not a domain"));
                }
                let mut s = self.cluster_state().settings;
                s.upn_domain = d.clone();
                self.commit_anywhere(&actor.label(), crate::cluster::Command::Settings(s)).await?;
                self.log_admin(actor, "upn_domain", json!({ "domain": d }));
                Ok(json!({}))
            }
            "reconcile" => {
                self.require(actor, Perm::DirectoryManage, self.root_ou())?;
                let done = self.mail_reconcile().await?;
                Ok(json!({ "done": done, "errors": self.mail_errors() }))
            }
            other => Err(format!("unknown mail operation {other:?}")),
        }
    }

    /// A new account's mailbox, when the settings say every new account gets one (D-U43).
    pub(super) async fn auto_mailbox(self: &Arc<Self>, actor: &Actor, user: &User) {
        let s = self.cluster_state().settings.mail;
        if !s.auto || s.template.trim().is_empty() || self.mail_instances().is_empty() {
            return;
        }
        if let Err(e) = self.mail_manage(actor, json!({ "op": "mailbox_set", "user": user.id })).await {
            eprintln!("{}: a mailbox for {}: {e}", self.me, user.name);
        }
    }

    /// Whether `sub` may use the webmail: access to a mail instance (D-U16; Round 18: any of them).
    pub fn may_use_mail(&self, sub: &str) -> bool {
        let names: Vec<String> = self.mail_instances().into_iter().map(|(n, _)| n).collect();
        self.lock().dirs.values().any(|d| d.state.access_for(sub).iter().any(|a| names.contains(&a.module)))
    }
}

impl Node {
    /// JMAP calls to the user's mail instance as `user` (Round 15): a token this node signs for
    /// their mailbox (5 minutes; audience `maildot`, as Stalwart's OIDC directory wants), the calls
    /// run by a node of the instance their mailbox is on. The method responses.
    pub async fn mail_as(self: &Arc<Self>, user: &User, issuer: &str, calls: Value) -> Result<Value, String> {
        let m = user.mailbox.as_ref().ok_or("you have no mailbox")?;
        let instance = self.mailbox_instance(user).ok_or("no mail instance")?;
        let key = self.idp_key().filter(|_| crate::oidc::signs_for(self, &self.me, user)).ok_or("this node does not sign for your account")?;
        let now = super::now_ms() / 1000;
        let claims = json!({
            "iss": issuer, "sub": user.id, "aud": "maildot", "client_id": "maildot", "scope": "openid email profile",
            "email": m.address, "email_verified": true, "name": user.name, "preferred_username": user.name,
            "iat": now, "exp": now + 300, "auth_time": now, "jti": directory::random_hex(12),
        });
        let token = key.sign("at+jwt", &claims)?;
        let p = self.cluster_state().placement;
        let mut last = format!("no node runs the mail instance {instance}");
        for node in self.instance_nodes(&instance) {
            match self.ask(&node, Call::MailAs { token: token.clone(), calls: calls.clone() }).await {
                Ok(Answer::Web { value }) => return Ok(value),
                Ok(Answer::Refused { why }) => return Err(why),
                Ok(other) => return Err(format!("{other:?}")),
                Err(e) => last = format!("{} does not answer: {e}", p.node_label(&node)),
            }
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::OuId;


    fn user(name: &str, mailbox: Option<(&str, Option<u64>, bool)>, disabled: bool) -> User {
        User {
            id: format!("id-{name}"),
            name: name.into(),
            display: name.to_uppercase(),
            ou: OuId(1),
            admin: false,
            disabled,
            outside: String::new(),
            mailbox: mailbox.map(|(a, q, off)| Mailbox { address: a.into(), aliases: Some(Vec::new()), quota: q, off, instance: None }),
        }
    }

    #[test]
    fn instances_by_ou() {
        use crate::placement::{Link, Module, Ou, Placement};
        let n = |s: &str| NodeId(s.into());
        let mut p = Placement::default();
        p.ous.insert(OuId(1), Ou::new("org", None));
        p.ous.insert(OuId(2), Ou::new("emea", Some(OuId(1))));
        p.ous.insert(OuId(3), Ou::new("sales", Some(OuId(2))));
        assert_eq!(instance_for(&p, OuId(2)), None, "no instance");
        p.modules.insert("mail".into(), Module::new(Domain::Mail.into(), "Mail", n("a")));
        // one instance: everyone's, whoever the masters are
        assert_eq!(instance_for(&p, OuId(3)).as_deref(), Some("mail"));
        p.modules.insert("mail-emea".into(), Module::new(Domain::Mail.into(), "Mail EMEA", n("b")));
        let link = |p: &mut Placement, ou: u64, node: &str| {
            p.ous.get_mut(&OuId(ou)).unwrap().links.insert(Domain::Mail, Link { node: n(node), enforced: false });
        };
        link(&mut p, 1, "a");
        link(&mut p, 2, "b");
        assert_eq!(instance_for(&p, OuId(1)).as_deref(), Some("mail"));
        assert_eq!(instance_for(&p, OuId(2)).as_deref(), Some("mail-emea"));
        assert_eq!(instance_for(&p, OuId(3)).as_deref(), Some("mail-emea"), "the nearest OU's master");
        // a master that runs none: the instance of /'s
        link(&mut p, 2, "c");
        assert_eq!(instance_for(&p, OuId(3)).as_deref(), Some("mail"));
    }

    fn ops<'a>(p: &'a [(String, Value)], who: &str) -> Vec<&'a Value> {
        p.iter().filter(|(u, _)| u == who).map(|(_, o)| o).collect()
    }

    #[test]
    fn stalwart_follows_the_mailboxes() {
        let users = vec![
            user("anna", Some(("anna@example.org", Some(1 << 30), false)), false),
            user("ben", Some(("ben@example.org", None, false)), false),
            user("cyril", Some(("cyril@cyril.ca.example.org", None, false)), false),
            user("dora", Some(("dora@example.org", None, false)), true),
            user("emil", None, false),
        ];
        let accounts = vec![
            json!({ "id": "a1", "email": "Anna@example.org", "quota": 1 << 30, "disabled": false, "aliases": [] }),
            json!({ "id": "b1", "email": "ben@example.org", "quota": 5 << 30, "disabled": true, "aliases": [] }),
            json!({ "id": "d1", "email": "dora@example.org", "quota": null, "disabled": false, "aliases": [] }),
            json!({ "id": "x1", "email": "stranger@example.org", "quota": null, "disabled": false, "aliases": [] }),
        ];
        let p = plan(&users, &accounts, &[], true, None);
        assert_eq!(p.len(), 3, "{p:?}");
        // ben: no limit and on again; anna as she should be; nothing for the stranger or emil
        assert_eq!(ops(&p, "id-ben"), [&json!({ "op": "account_update", "id": "b1", "quota": null, "off": false })]);
        let c = ops(&p, "id-cyril");
        assert_eq!((c[0]["op"].as_str(), c[0]["address"].as_str(), c[0]["description"].as_str(), c[0]["make_domain"].as_bool()),
            (Some("account_create"), Some("cyril@cyril.ca.example.org"), Some("CYRIL"), Some(true)));
        // a disabled account's mailbox is off
        assert_eq!(ops(&p, "id-dora"), [&json!({ "op": "account_update", "id": "d1", "off": true })]);
        assert!(plan(&users[..1], &accounts, &[], false, None).is_empty());
    }

    #[test]
    fn upn_is_an_alias() {
        // Round 22: each mailbox gets its account's UPN as an alias, the UPN's domain made with it
        let anna = user("Anna", Some(("anna@example.org", None, false)), false);
        let p = plan(std::slice::from_ref(&anna), &[], &[], false, Some("osada.example.org"));
        assert_eq!(p[0].1["op"], "account_create");
        assert_eq!(p[0].1["aliases"], json!(["anna@osada.example.org"]));
        assert_eq!(p[0].1["make_domain"], json!(true));
        let mut acct = json!({ "id": "a1", "email": "anna@example.org", "quota": null, "disabled": false, "aliases": [] });
        let p = plan(std::slice::from_ref(&anna), std::slice::from_ref(&acct), &[], false, Some("osada.example.org"));
        assert_eq!(p, [("id-Anna".to_string(), json!({ "op": "account_update", "id": "a1", "aliases": ["anna@osada.example.org"], "make_domain": true }))]);
        acct["aliases"] = json!(["anna@osada.example.org"]);
        assert!(plan(std::slice::from_ref(&anna), std::slice::from_ref(&acct), &[], false, Some("osada.example.org")).is_empty());
        // a mailbox at the UPN itself: no alias; an import leaves the UPN out of the mailbox's list
        let ben = user("ben", Some(("ben@osada.example.org", None, false)), false);
        assert_eq!(plan(&[ben], &[], &[], false, Some("osada.example.org"))[0].1["aliases"], json!([]));
        let mut cy = user("cy", Some(("cy@example.org", None, false)), false);
        cy.mailbox.as_mut().unwrap().aliases = None;
        let acct = json!({ "id": "c1", "email": "cy@example.org", "quota": null, "disabled": false, "aliases": ["cy@osada.example.org", "c@example.org"] });
        let p = plan(&[cy], &[acct], &[], false, Some("osada.example.org"));
        assert_eq!(p, [("id-cy".to_string(), json!({ "op": "import", "aliases": ["c@example.org"] }))]);
    }

    #[test]
    fn aliases_and_catch_alls() {
        let domains = vec![
            json!({ "name": "example.org", "catch_all": "anna@example.org" }),
            json!({ "name": "anna.ca.example.org", "catch_all": "anna@example.org" }),
            json!({ "name": "other.org", "catch_all": null }),
        ];
        let accounts = vec![json!({ "id": "a1", "email": "anna@example.org", "quota": null, "disabled": false, "aliases": ["a@other.org", "ann@example.org"] })];
        // adopted before aliases: Stalwart's are taken, and nothing else is done
        let mut anna = user("anna", Some(("anna@example.org", None, false)), false);
        anna.mailbox.as_mut().unwrap().aliases = None;
        let p = plan(std::slice::from_ref(&anna), &accounts, &domains, false, None);
        assert_eq!(p, [("id-anna".to_string(), json!({ "op": "import", "aliases": ["*@anna.ca.example.org", "*@example.org", "a@other.org", "ann@example.org"] }))]);
        // known: Stalwart follows; an alias goes, another comes; a catch-all goes, another comes
        anna.mailbox.as_mut().unwrap().aliases = Some(vec!["ann@example.org".into(), "anna2@example.org".into(), "*@example.org".into(), "*@other.org".into()]);
        let p = plan(std::slice::from_ref(&anna), &accounts, &domains, true, None);
        let o = ops(&p, "id-anna");
        assert_eq!(o[0], &json!({ "op": "account_update", "id": "a1", "aliases": ["ann@example.org", "anna2@example.org"], "make_domain": true }));
        assert_eq!(o[1], &json!({ "op": "catch_all", "domain": "anna.ca.example.org", "address": null }));
        assert_eq!(o[2], &json!({ "op": "catch_all", "domain": "other.org", "address": "anna@example.org", "make_domain": true }));
        assert_eq!(o.len(), 3);
        // a new mailbox: its aliases with it, its catch-alls after
        let mut ben = user("ben", Some(("ben@b.org", None, false)), false);
        ben.mailbox.as_mut().unwrap().aliases = Some(vec!["*@b.org".into(), "benny@b.org".into()]);
        let p = plan(&[ben], &[], &[], true, None);
        assert_eq!(p[0].1["op"], "account_create");
        assert_eq!(p[0].1["aliases"], json!(["benny@b.org"]));
        assert_eq!(p[1].1, json!({ "op": "catch_all", "domain": "b.org", "address": "ben@b.org", "make_domain": true }));
    }

    #[test]
    fn addresses_and_templates() {
        use crate::audit::{MailSettings, label_of};
        assert!(directory::valid_address("anna.m+x@mail.example.org"));
        for bad in ["anna", "Anna@example.org", "anna@localhost", ".a@example.org", "a b@example.org", "a@exa_mple.org"] {
            assert!(!directory::valid_address(bad), "{bad}");
        }
        assert_eq!(label_of("Jan.Novák_2"), "jan-nov-k-2");
        let s = MailSettings { template: "{user}@{user}.ca.example.org".into(), make_domains: true, quota: None, auto: true };
        assert!(s.check().is_ok());
        assert_eq!(s.address_for("JOE").unwrap(), "joe@joe.ca.example.org");
        assert!(MailSettings { template: "mail@example.org".into(), ..s.clone() }.check().is_err());
        assert!(MailSettings { template: String::new(), ..s.clone() }.check().is_err());
        assert!(MailSettings { template: String::new(), auto: false, ..s }.check().is_ok());
        let m = |a: &[&str]| Mailbox { address: "anna@example.org".into(), aliases: Some(a.iter().map(|x| x.to_string()).collect()), quota: None, off: false, instance: None };
        assert!(m(&["a@example.org", "*@example.org", "*@other.example.org"]).check().is_ok());
        for bad in [&["*@localhost"][..], &["*"], &["A@example.org"], &["a@example.org", "a@example.org"], &["x@@y.org"]] {
            assert!(m(bad).check().is_err(), "{bad:?}");
        }
    }
}

/// The drop-in's public host: the UPN domain until the cluster has one (Round 22).
pub static UPN_FALLBACK: std::sync::OnceLock<String> = std::sync::OnceLock::new();

impl Node {
    /// The domain of the accounts' UPNs (`<name>@<domain>`, Round 22), if there is one.
    pub fn upn_domain(&self) -> Option<String> {
        let d = self.cluster_state().settings.upn_domain;
        if !d.is_empty() { Some(d) } else { UPN_FALLBACK.get().cloned() }
    }

    /// What a person typed to sign in, as the account's name: the name itself, or its UPN.
    pub fn login_name(&self, typed: &str) -> String {
        let t = typed.trim();
        match (t.rsplit_once('@'), self.upn_domain()) {
            (Some((local, dom)), Some(d)) if dom.eq_ignore_ascii_case(&d) && !local.is_empty() => local.to_string(),
            _ => t.to_string(),
        }
    }

    /// At a drop-in's start: the cluster's UPN domain becomes `host` when it has none yet.
    pub async fn default_upn_domain(self: &Arc<Self>, host: &str) -> Result<bool, String> {
        let mut s = self.cluster_state().settings;
        if !s.upn_domain.is_empty() || host.is_empty() || !crate::directory::valid_address(&format!("x@{host}")) {
            return Ok(false);
        }
        s.upn_domain = host.to_ascii_lowercase();
        self.commit_anywhere(&Actor::node(&self.me).label(), crate::cluster::Command::Settings(s)).await?;
        self.log_admin(&Actor::node(&self.me), "upn_domain", json!({ "domain": host, "why": "the public host, at the start" }));
        Ok(true)
    }
}
