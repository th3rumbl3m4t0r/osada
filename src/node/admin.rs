//! What people do through the pages (web.rs), with the checks of §10.5:
//! each change must be allowed by the actor's roles over the OU it touches
//! (rbac.rs), and goes into the node's admin or auth log together with the
//! session it came from, so that everything a sign-in led to can be found
//! from it (§16.3). Log queries run on every node and are cut down to what
//! the viewer may read.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Answer, Call, Inner, LogLine, Node, Session, source_text};
use crate::{
    audit::{LogKind, Settings, Verbosity},
    cluster::Command,
    directory::{self, Access, DIRECTORY, DirOp, Group, MIN_PASSWORD, User, random_hex},
    log::CollectionId,
    placement::{Domain, NodeId, OuId, Placement},
    rbac::{self, Grant, Perm, Subject},
    table::{Cell, Filter},
};

/// Who does something, from which session and address.
#[derive(Clone, Debug)]
pub struct Actor {
    pub user: User,
    pub sid: String,
    pub addr: String,
}

impl Actor {
    /// The node itself, for what it does on its own (the first administrator).
    pub(super) fn node(me: &NodeId) -> Actor {
        let user = User { id: format!("node:{me}"), name: format!("node {me}"), display: String::new(), ou: OuId(0), admin: true, disabled: false, outside: String::new(), mailbox: None };
        Actor { user, sid: String::new(), addr: String::new() }
    }

    fn fields(&self, event: &str) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("event".into(), json!(event));
        m.insert("actor".into(), json!(self.user.id));
        m.insert("actor_name".into(), json!(self.user.name));
        m.insert("sid".into(), json!(self.sid));
        m.insert("addr".into(), json!(self.addr));
        m
    }

    pub(super) fn label(&self) -> String {
        format!("user:{}", self.user.name)
    }
}

fn event(actor: &Actor, name: &str, extra: Value) -> Value {
    let mut m = actor.fields(name);
    if let Value::Object(x) = extra {
        m.extend(x);
    }
    Value::Object(m)
}

pub fn perm_name(p: Perm) -> String {
    serde_json::to_value(p).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// A log query: entries of one kind of log (`auth`, `admin`) that meet every
/// condition, within a time range, newest first.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub conds: Vec<Cond>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    #[serde(default)]
    pub limit: usize,
    /// The pages' table filter (grid.js), besides `conds`.
    #[serde(default)]
    pub filter: Option<Filter>,
    /// Only entries after this one.
    #[serde(default)]
    pub before: Option<Cursor>,
}

/// Where a page of log entries ends: the entries after it come next.
/// Entries are ordered newest first, then by node, then by seq downwards.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub time: u64,
    pub node: String,
    pub seq: u64,
}

impl Cursor {
    /// Whether an entry comes after the cursor.
    fn passed(&self, time: u64, node: &str, seq: u64) -> bool {
        time < self.time || (time == self.time && (node > self.node.as_str() || (node == self.node && seq < self.seq)))
    }
}

/// One page of log entries from every node.
#[derive(Debug, Serialize)]
pub struct LogPage {
    pub lines: Vec<LogLine>,
    /// The nodes that did not answer.
    pub unreachable: Vec<String>,
    /// Where the next page starts, when there may be one.
    pub next: Option<Cursor>,
}

fn log_cell(ev: &Value, time: u64, node: &str, col: &str) -> Cell {
    if col == "time" { Cell::Num(time as f64) } else { Cell::Texts(field_values(ev, node, col)) }
}

/// `field` `op` `value`; fields: event, user, actor, session, from, node,
/// ou, role, detail, any; ops: is, not, contains, starts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cond {
    pub field: String,
    pub op: String,
    pub value: String,
}

fn field_values(ev: &Value, node: &str, field: &str) -> Vec<String> {
    let s = |k: &str| ev.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_string);
    let v: Vec<Option<String>> = match field {
        "event" => vec![s("event")],
        "user" => vec![s("user"), s("name"), s("group"), s("group_name")],
        "actor" => vec![s("actor"), s("actor_name")],
        "session" => vec![s("sid"), s("ended")],
        "from" => vec![s("addr")],
        "node" => vec![Some(node.to_string())],
        "ou" => vec![s("ou"), s("scope")],
        "role" => vec![s("role")],
        "detail" => vec![
            s("why"),
            ev.get("changes").map(|c| c.to_string()),
            s("role"),
            s("scope"),
            s("ou"),
            s("ended"),
            s("to"),
            ev.get("factors").map(|f| f.to_string()),
        ],
        _ => vec![Some(ev.to_string()), Some(node.to_string())],
    };
    v.into_iter().flatten().collect()
}

fn meets(c: &Cond, vals: &[String]) -> bool {
    let want = c.value.to_lowercase();
    let vals: Vec<String> = vals.iter().map(|v| v.to_lowercase()).collect();
    match c.op.as_str() {
        "not" => !vals.contains(&want),
        "contains" => vals.iter().any(|v| v.contains(&want)),
        "starts" => vals.iter().any(|v| v.starts_with(&want)),
        _ => vals.contains(&want),
    }
}

/// What changed in node control, in words, for the admin log.
fn summarize(old: &Placement, new: &Placement) -> Vec<String> {
    let mut out = Vec::new();
    for (id, n) in &new.nodes {
        match old.nodes.get(id) {
            None => out.push(format!("node {id} added")),
            Some(o) if o != n => out.push(format!("node {id}: roles or rules changed")),
            _ => {}
        }
    }
    for id in old.nodes.keys().filter(|id| !new.nodes.contains_key(*id)) {
        out.push(format!("node {id} removed"));
    }
    if old.defaults != new.defaults {
        out.push("default authorities changed".into());
    }
    for (name, m) in &new.modules {
        match old.modules.get(name) {
            None => out.push(format!("module {name} ({}) added on {}", m.kind, m.node)),
            Some(o) if o.node != m.node => out.push(format!("module {name} moved from {} to {}", o.node, m.node)),
            Some(o) if o != m => out.push(format!("module {name} changed")),
            _ => {}
        }
    }
    for (name, c) in &new.certs {
        match old.certs.get(name) {
            None => out.push(format!("certificate {name} ({}) added, issued on {}", c.names.join(", "), c.issuer)),
            Some(o) if o != c => out.push(format!("certificate {name} changed")),
            _ => {}
        }
    }
    for name in old.certs.keys().filter(|n| !new.certs.contains_key(*n)) {
        out.push(format!("certificate {name} removed"));
    }
    for name in old.modules.keys().filter(|n| !new.modules.contains_key(*n)) {
        out.push(format!("module {name} removed"));
    }
    if old.default_secrets != new.default_secrets {
        let list = |p: &Placement| p.default_secrets.iter().map(|n| n.0.as_str()).collect::<Vec<_>>().join(", ");
        out.push(format!("default secrets: {} → {}", list(old), list(new)));
    }
    for (id, n) in &new.ous {
        let path = new.ou_path(*id);
        match old.ous.get(id) {
            None => out.push(format!("OU {path} added")),
            Some(o) => {
                if o.name != n.name || o.parent != n.parent {
                    out.push(format!("OU {} → {path}", old.ou_path(*id)));
                }
                if o.links != n.links {
                    out.push(format!("OU {path}: domain links changed"));
                }
                if o.tier0 != n.tier0 {
                    out.push(format!("OU {path}: tier 0 {}", if n.tier0 { "on" } else { "off" }));
                }
                if o.second_factor != n.second_factor {
                    out.push(format!("OU {path}: second factor {}", if n.second_factor { "required" } else { "not required" }));
                }
                if o.secrets != n.secrets {
                    out.push(format!("OU {path}: secrets placement changed"));
                }
                if o.storage_limit != n.storage_limit {
                    out.push(format!("OU {path}: storage limit of its accounts {}", n.storage_limit.map_or("as above".to_string(), |l| format!("{} MiB", l >> 20))));
                }
                if o.session_days != n.session_days {
                    out.push(format!("OU {path}: longest sign-in {}", n.session_days.map_or("as above".to_string(), |d| format!("{d} days"))));
                }
            }
        }
    }
    if old.web != new.web {
        out.push("web: its nodes or the authoritative node changed".into());
    }
    // anything else a change does not name above (so that no change is dropped as none)
    if out.is_empty() && old != new {
        out.push("node control changed".into());
    }
    out
}

impl Node {
    fn subjects(inner: &Inner, user: &User) -> Vec<Subject> {
        let mut v = vec![Subject::User { id: user.id.clone() }];
        for d in inner.dirs.values() {
            if let Some(ids) = d.state.member_of.get(&user.id) {
                v.extend(ids.iter().map(|g| Subject::Group { id: g.clone() }));
            }
        }
        v
    }

    pub(super) fn perms_in(inner: &Inner, user: &User, ou: OuId) -> BTreeSet<Perm> {
        if user.admin {
            return Perm::ALL.into();
        }
        let p = &inner.cluster.state().placement;
        let subjects = Self::subjects(inner, user);
        rbac::perms_at(p, ou, &subjects, |o| inner.dirs.get(&o).map(|d| d.state.grants.values().cloned().collect()).unwrap_or_default())
    }

    /// What `user` may do in `ou`.
    pub fn perms(&self, user: &User, ou: OuId) -> BTreeSet<Perm> {
        Self::perms_in(&self.lock(), user, ou)
    }

    pub fn may(&self, user: &User, perm: Perm, ou: OuId) -> bool {
        self.perms(user, ou).contains(&perm)
    }

    /// Whether `user` may do `perm` in some OU.
    pub fn may_somewhere(&self, user: &User, perm: Perm) -> bool {
        let inner = self.lock();
        inner.cluster.state().placement.ous.keys().any(|&ou| Self::perms_in(&inner, user, ou).contains(&perm))
    }

    pub(super) fn require(&self, actor: &Actor, perm: Perm, ou: OuId) -> Result<(), String> {
        if self.may(&actor.user, perm, ou) {
            return Ok(());
        }
        Err(format!("you have no {} right in {}", perm_name(perm), self.cluster_state().placement.ou_path(ou)))
    }

    /// Whether `user` holds any role anywhere: what opens the admin page.
    pub fn has_any_role(&self, user: &User) -> bool {
        if user.admin {
            return true;
        }
        let inner = self.lock();
        let subjects = Self::subjects(&inner, user);
        inner.dirs.values().any(|d| d.state.grants.values().any(|g| subjects.contains(&g.subject)))
    }

    pub(super) fn root_ou(&self) -> OuId {
        self.cluster_state().placement.root().unwrap_or(OuId(1))
    }

    pub(super) fn ou_path(&self, ou: OuId) -> String {
        self.cluster_state().placement.ou_path(ou)
    }

    pub fn find_user(&self, id: &str) -> Option<User> {
        self.lock().dirs.values().find_map(|d| d.state.users.get(id).cloned())
    }

    pub fn find_user_by_name(&self, name: &str) -> Option<User> {
        self.lock().dirs.values().find_map(|d| d.state.by_name(name).cloned())
    }

    fn find_group(&self, id: &str) -> Option<Group> {
        self.lock().dirs.values().find_map(|d| d.state.groups.get(id).cloned())
    }

    pub(super) fn log_admin(&self, actor: &Actor, name: &str, extra: Value) {
        self.log_event(&mut self.lock(), LogKind::Admin, Verbosity::Minimal, event(actor, name, extra));
    }

    pub(super) fn log_auth_as(&self, actor: &Actor, name: &str, level: Verbosity, extra: Value) {
        self.log_event(&mut self.lock(), LogKind::Auth, level, event(actor, name, extra));
    }

    // ---- users, groups -----------------------------------------------------

    /// A new user in `ou` with a password: `user_created` in the admin log,
    /// then `password_changed` in the auth log, both with the actor's session.
    pub async fn create_user(self: &Arc<Self>, actor: &Actor, name: &str, display: &str, ou: OuId, password: &str) -> Result<User, String> {
        self.require(actor, Perm::DirectoryManage, ou)?;
        let u = self.create_user_unchecked(actor, name, display, ou, password).await?;
        self.auto_mailbox(actor, &u).await;
        Ok(u)
    }

    pub(super) async fn create_user_unchecked(
        self: &Arc<Self>,
        actor: &Actor,
        name: &str,
        display: &str,
        ou: OuId,
        password: &str,
    ) -> Result<User, String> {
        if !directory::valid_user_name(name) {
            return Err("a user name is lower-case letters, digits, '.', '_' and '-'".into());
        }
        if password.chars().count() < MIN_PASSWORD {
            return Err(format!("a password has at least {MIN_PASSWORD} characters"));
        }
        {
            let inner = self.lock();
            if !inner.cluster.state().placement.ous.contains_key(&ou) {
                return Err("no such OU".into());
            }
            if inner.dirs.values().any(|d| d.state.by_name(name).is_some() || d.state.group_names.contains_key(name)) {
                return Err(format!("there is a user or group {name} already"));
            }
        }
        let user = User { id: random_hex(16), name: name.into(), display: display.into(), ou, admin: false, disabled: false, outside: String::new(), mailbox: None };
        let op = serde_json::to_value(DirOp::CreateUser { user: user.clone() }).map_err(|e| e.to_string())?;
        self.submit(directory::directory_of(ou), DIRECTORY, op, &actor.label()).await?;
        self.log_admin(actor, "user_created", json!({ "user": user.id, "name": user.name, "ou": self.ou_path(ou) }));
        self.set_password(&user, password, &actor.label()).await?;
        self.log_auth_as(actor, "password_changed", Verbosity::Minimal, json!({ "user": user.id, "name": user.name, "why": "set at creation" }));
        Ok(user)
    }

    /// Drops the admin flag from before roles (roles-grant at the root). Not
    /// one's own, unless a role at the root gives one the same.
    pub async fn drop_legacy_admin(self: &Arc<Self>, actor: &Actor, id: &str) -> Result<(), String> {
        let user = self.find_user(id).ok_or("no such user")?;
        let root = self.root_ou();
        self.require(actor, Perm::RolesGrant, root)?;
        if id == actor.user.id {
            let without = User { admin: false, ..actor.user.clone() };
            if !self.perms(&without, root).contains(&Perm::RolesGrant) {
                return Err("grant yourself admin over / first, or you would lock yourself out".into());
            }
        }
        let op = DirOp::UpdateUser { id: id.into(), display: None, admin: Some(false), disabled: None, outside: None };
        self.submit(directory::directory_of(user.ou), DIRECTORY, serde_json::to_value(op).map_err(|e| e.to_string())?, &actor.label()).await?;
        self.log_admin(actor, "user_updated", json!({ "user": id, "name": user.name, "ou": self.ou_path(user.ou), "changes": { "old_admin_flag": "dropped" } }));
        Ok(())
    }

    /// Changes a user: the display name (directory-manage), disabled or not
    /// (account-lock), the password (password-set).
    pub async fn update_user(
        self: &Arc<Self>,
        actor: &Actor,
        id: &str,
        display: Option<String>,
        disabled: Option<bool>,
        password: Option<String>,
        outside: Option<String>,
    ) -> Result<(), String> {
        let user = self.find_user(id).ok_or("no such user")?;
        // outside networks: who may use the account from where, a lock of sorts
        let outside = outside.map(|o| crate::osada1::edge::normalize(&o)).transpose()?;
        if outside.is_some() {
            self.require(actor, Perm::AccountLock, user.ou)?;
        }
        if display.is_some() {
            self.require(actor, Perm::DirectoryManage, user.ou)?;
        }
        if disabled.is_some() {
            self.require(actor, Perm::AccountLock, user.ou)?;
            if id == actor.user.id && disabled == Some(true) {
                return Err("you cannot disable yourself".into());
            }
        }
        if let Some(pw) = &password {
            self.require(actor, Perm::PasswordSet, user.ou)?;
            if pw.chars().count() < MIN_PASSWORD {
                return Err(format!("a password has at least {MIN_PASSWORD} characters"));
            }
        }
        if display.is_some() || disabled.is_some() || outside.is_some() {
            let changes = json!({ "display": display, "disabled": disabled, "outside": outside });
            let op = DirOp::UpdateUser { id: id.into(), display, admin: None, disabled, outside };
            self.submit(directory::directory_of(user.ou), DIRECTORY, serde_json::to_value(op).map_err(|e| e.to_string())?, &actor.label())
                .await?;
            self.log_admin(actor, "user_updated", json!({ "user": id, "name": user.name, "ou": self.ou_path(user.ou), "changes": changes }));
        }
        if disabled == Some(true) {
            let ended: Vec<String> = self.sessions_of(id).into_iter().map(|s| s.sid).collect();
            for sid in ended {
                let _ = self.end_session(actor, id, &sid);
            }
        }
        if let Some(pw) = password {
            self.set_password(&user, &pw, &actor.label()).await?;
            self.log_auth_as(actor, "password_changed", Verbosity::Minimal, json!({ "user": id, "name": user.name, "why": "set by an administrator" }));
        }
        Ok(())
    }

    pub async fn create_group(self: &Arc<Self>, actor: &Actor, name: &str, display: &str, ou: OuId) -> Result<Group, String> {
        self.require(actor, Perm::DirectoryManage, ou)?;
        if self.lock().dirs.values().any(|d| d.state.by_name(name).is_some() || d.state.group_names.contains_key(name)) {
            return Err(format!("there is a user or group {name} already"));
        }
        let group = Group { id: random_hex(16), name: name.into(), display: display.into(), ou, members: BTreeSet::new() };
        let op = serde_json::to_value(DirOp::CreateGroup { group: group.clone() }).map_err(|e| e.to_string())?;
        self.submit(directory::directory_of(ou), DIRECTORY, op, &actor.label()).await?;
        self.log_admin(actor, "group_created", json!({ "group": group.id, "group_name": group.name, "ou": self.ou_path(ou) }));
        Ok(group)
    }

    /// Puts `user` into `group` or takes them out (directory-manage on the
    /// group's OU, directory-read on the user's).
    pub async fn set_member(self: &Arc<Self>, actor: &Actor, group: &str, user: &str, member: bool) -> Result<(), String> {
        let g = self.find_group(group).ok_or("no such group")?;
        let u = self.find_user(user).ok_or("no such user")?;
        self.require(actor, Perm::DirectoryManage, g.ou)?;
        self.require(actor, Perm::DirectoryRead, u.ou)?;
        let op = serde_json::to_value(DirOp::SetMember { group: group.into(), user: user.into(), member }).map_err(|e| e.to_string())?;
        self.submit(directory::directory_of(g.ou), DIRECTORY, op, &actor.label()).await?;
        let name = if member { "member_added" } else { "member_removed" };
        self.log_admin(actor, name, json!({ "group": g.id, "group_name": g.name, "user": u.id, "name": u.name, "ou": self.ou_path(g.ou) }));
        Ok(())
    }

    // ---- roles -------------------------------------------------------------

    fn grant_check(&self, actor: &Actor, scope: OuId, role: &str) -> Result<(), String> {
        let r = rbac::role(role).ok_or("no such role")?;
        let mine = self.perms(&actor.user, scope);
        if !mine.contains(&Perm::RolesGrant) {
            return Err(format!("you cannot grant roles in {}", self.ou_path(scope)));
        }
        let root = self.root_ou();
        if let Some(p) = r.perms.iter().find(|&&p| (p != Perm::ClusterManage || scope == root) && !mine.contains(&p)) {
            return Err(format!("you cannot grant {role} in {}: you do not have {} there yourself", self.ou_path(scope), perm_name(*p)));
        }
        Ok(())
    }

    fn subject_label(&self, s: &Subject) -> Option<(String, &'static str, String)> {
        match s {
            Subject::User { id } => self.find_user(id).map(|u| (u.id, "user", u.name)),
            Subject::Group { id } => self.find_group(id).map(|g| (g.id, "group", g.name)),
        }
    }

    /// Grants `role` over `scope` and below to `subject`.
    pub async fn grant(self: &Arc<Self>, actor: &Actor, scope: OuId, role: &str, subject: Subject) -> Result<Grant, String> {
        self.grant_check(actor, scope, role)?;
        self.grant_unchecked(actor, scope, role, subject).await
    }

    pub(super) async fn grant_unchecked(self: &Arc<Self>, actor: &Actor, scope: OuId, role: &str, subject: Subject) -> Result<Grant, String> {
        let (id, kind, name) = self.subject_label(&subject).ok_or("no such user or group")?;
        let grant = Grant { id: random_hex(8), role: role.into(), subject };
        let op = serde_json::to_value(DirOp::Grant { grant: grant.clone() }).map_err(|e| e.to_string())?;
        self.submit(directory::directory_of(scope), DIRECTORY, op, &actor.label()).await?;
        let (idk, namek) = if kind == "user" { ("user", "name") } else { ("group", "group_name") };
        self.log_admin(actor, "role_granted", json!({ "role": role, "scope": self.ou_path(scope), idk: id, namek: name, "grant": grant.id }));
        Ok(grant)
    }

    pub async fn revoke(self: &Arc<Self>, actor: &Actor, scope: OuId, id: &str) -> Result<(), String> {
        let grant = self.lock().dirs.get(&scope).and_then(|d| d.state.grants.get(id).cloned()).ok_or("no such grant there")?;
        self.grant_check(actor, scope, &grant.role)?;
        let op = serde_json::to_value(DirOp::Revoke { id: id.into() }).map_err(|e| e.to_string())?;
        self.submit(directory::directory_of(scope), DIRECTORY, op, &actor.label()).await?;
        let (sid, kind, name) = self.subject_label(&grant.subject).unwrap_or_default();
        let (idk, namek) = if kind == "group" { ("group", "group_name") } else { ("user", "name") };
        self.log_admin(actor, "role_revoked", json!({ "role": grant.role, "scope": self.ou_path(scope), idk: sid, namek: name, "grant": id }));
        Ok(())
    }

    // ---- access to module instances (A24) ----------------------------------

    /// Access to `module` for a user or a group (directory-manage in its OU).
    pub async fn grant_access(self: &Arc<Self>, actor: &Actor, module: &str, subject: Subject) -> Result<(), String> {
        if !self.cluster_state().placement.modules.contains_key(module) {
            return Err(format!("no module {module}"));
        }
        let (ou, kind, name) = match &subject {
            Subject::User { id } => self.find_user(id).map(|u| (u.ou, "user", u.name)),
            Subject::Group { id } => self.find_group(id).map(|g| (g.ou, "group", g.name)),
        }
        .ok_or("no such user or group")?;
        self.require(actor, Perm::DirectoryManage, ou)?;
        let id = match &subject {
            Subject::User { id } | Subject::Group { id } => id.clone(),
        };
        let op = DirOp::GrantAccess { list: vec![Access { id: random_hex(8), module: module.into(), subject }] };
        self.submit(directory::directory_of(ou), DIRECTORY, serde_json::to_value(op).map_err(|e| e.to_string())?, &actor.label()).await?;
        let (idk, namek) = if kind == "user" { ("user", "name") } else { ("group", "group_name") };
        self.log_admin(actor, "access_granted", json!({ "module": module, idk: id, namek: name, "ou": self.ou_path(ou) }));
        Ok(())
    }

    pub async fn revoke_access(self: &Arc<Self>, actor: &Actor, id: &str) -> Result<(), String> {
        let (ou, access) = {
            let inner = self.lock();
            inner.dirs.iter().find_map(|(o, d)| d.state.access.get(id).map(|a| (*o, a.clone()))).ok_or("no such access")?
        };
        self.require(actor, Perm::DirectoryManage, ou)?;
        let op = DirOp::RevokeAccess { ids: vec![id.into()] };
        self.submit(directory::directory_of(ou), DIRECTORY, serde_json::to_value(op).map_err(|e| e.to_string())?, &actor.label()).await?;
        let who = self.subject_label(&access.subject).unwrap_or_default();
        let (idk, namek) = if who.1 == "group" { ("group", "group_name") } else { ("user", "name") };
        self.log_admin(actor, "access_revoked", json!({ "module": access.module, idk: who.0, namek: who.2, "ou": self.ou_path(ou) }));
        Ok(())
    }

    // ---- the cluster -------------------------------------------------------

    /// A new node control, if every part that changed is the actor's to change.
    pub async fn set_placement(self: &Arc<Self>, actor: &Actor, new: Placement) -> Result<(), String> {
        let old = self.cluster_state().placement;
        rbac::check_change(&old, &new, |perm, ou| self.may(&actor.user, perm, ou)).map_err(|e| format!("you cannot save this: {e}"))?;
        let changes = summarize(&old, &new);
        if changes.is_empty() {
            return Ok(());
        }
        // the IdP master of / holds the cluster log (Round 14: no voter of its own); moving it moves
        // the cluster log, which is not built yet
        let root = self.root_ou();
        let voter = self.voter_in(&self.cluster_state());
        if let (Some(v), Ok(a)) = (voter, new.authority(crate::placement::Domain::Directory, root)) {
            if a.node != v && old.authority(crate::placement::Domain::Directory, root).is_ok_and(|o| o.node == v) {
                return Err(format!(
                    "the IdP master of / ({}) holds the cluster log: moving it to {} is not built yet",
                    new.node_label(&v),
                    new.node_label(&a.node)
                ));
            }
        }
        self.commit_anywhere(&actor.label(), Command::Placement(new)).await?;
        self.log_admin(actor, "node_control_changed", json!({ "changes": changes }));
        Ok(())
    }

    pub async fn set_settings(self: &Arc<Self>, actor: &Actor, mut s: Settings) -> Result<(), String> {
        self.require(actor, Perm::ClusterManage, self.root_ou())?;
        let old = self.cluster_state().settings;
        let mut changes: Vec<String> = Vec::new();
        // other storage is changed on its own (set_storage), and so are rules and the never-ban list
        s.storages = old.storages.clone();
        s.rules = old.rules.clone();
        s.rules_seeded = old.rules_seeded;
        s.never_ban = old.never_ban.clone();
        s.mail = old.mail.clone();
        s.backups = old.backups.clone();
        // the page never has the secret: an empty one keeps it
        if s.ship.clear_secret {
            s.ship.secret.clear();
        } else if s.ship.secret.is_empty() {
            s.ship.secret = old.ship.secret.clone();
        }
        s.ship.clear_secret = false;
        if s.ship != old.ship {
            let what = if s.ship.enabled {
                let logs: Vec<String> = s.ship.logs.iter().map(|k| format!("{k:?}").to_lowercase()).collect();
                format!("log shipping: {} to {}", logs.join(", "), s.ship.url)
            } else {
                "log shipping: off".to_string()
            };
            changes.push(what);
            if s.ship.secret != old.ship.secret {
                changes.push(if s.ship.secret.is_empty() { "log shipping: secret cleared" } else { "log shipping: secret replaced" }.into());
            }
        }
        for (k, v) in &s.logs {
            if old.logs.get(k) != Some(v) {
                changes.push(format!("{k:?} log: {:?}, {} days", v.verbosity, v.retention_days).to_lowercase());
            }
        }
        if old.session_days != s.session_days {
            changes.push(format!("sessions: {} days", s.session_days));
        }
        self.commit_anywhere(&actor.label(), Command::Settings(s)).await?;
        self.log_admin(actor, "settings_changed", json!({ "changes": changes }));
        Ok(())
    }

    pub async fn handover_as(self: &Arc<Self>, actor: &Actor, collection: CollectionId, to: NodeId) -> Result<(), String> {
        self.require(actor, Perm::ClusterManage, self.root_ou())?;
        self.request_handover(collection, to.clone()).await?;
        self.log_admin(actor, "handover_requested", json!({ "collection": collection.to_string(), "to": to }));
        Ok(())
    }

    // ---- sessions ----------------------------------------------------------

    /// Ends session `sid` of `user` on this node: one's own, or with
    /// account-lock rights over the user.
    pub fn end_session(&self, actor: &Actor, user: &str, sid: &str) -> Result<(), String> {
        let u = self.find_user(user).ok_or("no such user")?;
        if user != actor.user.id {
            self.require(actor, Perm::AccountLock, u.ou)?;
        }
        let ended = {
            let mut inner = self.lock();
            let key = inner.sessions.iter().find(|(_, s)| s.sid == sid && s.user == user).map(|(k, _)| k.clone());
            key.and_then(|k| inner.sessions.remove(&k))
        };
        ended.ok_or("no such session here")?;
        self.log_auth_as(actor, "session_ended", Verbosity::Minimal, json!({ "user": u.id, "name": u.name, "ended": sid }));
        Ok(())
    }

    /// `user`'s sessions on every node.
    pub async fn sessions_everywhere(self: &Arc<Self>, user: &str) -> (Vec<Session>, Vec<String>) {
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for n in self.cluster_state().placement.nodes.keys() {
            if n == self.me() {
                out.extend(self.sessions_of(user));
                continue;
            }
            match self.ask(n, Call::SessionsOf { user: user.into() }).await {
                Ok(Answer::Sessions { sessions }) => out.extend(sessions),
                _ => missing.push(n.0.clone()),
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.started));
        (out, missing)
    }

    // ---- logs --------------------------------------------------------------

    /// This node's entries that meet `q`, newest first.
    pub(super) fn query_local(&self, q: &LogQuery) -> Vec<LogLine> {
        let log = {
            let inner = self.lock();
            if q.kind == "admin" { inner.admin_log.clone() } else { inner.auth.clone() }
        };
        let limit = q.limit.clamp(1, 1000);
        let me = self.me().0.clone();
        let Ok(filter) = q.filter.as_ref().map(Filter::prepare).transpose() else { return Vec::new() };
        let mut out = Vec::new();
        let start = log.start().seq;
        let mut upto = log.head().seq;
        let mut scanned = 0usize;
        while upto > start && out.len() < limit && scanned < 200_000 {
            let from = upto.saturating_sub(512).max(start);
            let entries = log.read(from, (upto - from) as usize).unwrap_or_default();
            for e in entries.into_iter().rev() {
                scanned += 1;
                if q.until.is_some_and(|u| e.time > u) || q.before.as_ref().is_some_and(|c| !c.passed(e.time, &me, e.seq)) {
                    continue;
                }
                if q.since.is_some_and(|s| e.time < s) {
                    return out;
                }
                let mut ev: Value = serde_json::from_slice(&e.payload).unwrap_or(Value::Null);
                if let Some(o) = ev.as_object_mut() {
                    o.remove("token");
                }
                if q.conds.iter().all(|c| meets(c, &field_values(&ev, &me, &c.field)))
                    && filter.as_ref().is_none_or(|f| f.admits(|col| log_cell(&ev, e.time, &me, col)))
                {
                    out.push(LogLine { seq: e.seq, time: e.time, node: me.clone(), event: ev });
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
            upto = from;
        }
        out
    }

    /// Every node's entries that meet `q` (only one node's, when a condition
    /// says `node is …`), newest first, one page from `q.before` on, cut to
    /// what `viewer` may read: the entries about accounts in OUs where they
    /// have logs-read, the others (unknown names, the cluster) with logs-read
    /// at the root.
    pub async fn query_logs(self: &Arc<Self>, viewer: &User, q: LogQuery) -> Result<LogPage, String> {
        if !self.may_somewhere(viewer, Perm::LogsRead) {
            return Err("you have no logs-read right anywhere".into());
        }
        if let Some(f) = &q.filter {
            f.prepare()?;
        }
        let limit = q.limit.clamp(1, 1000);
        let state = self.cluster_state();
        let nodes: Vec<NodeId> = match q.conds.iter().find(|c| c.field == "node" && c.op == "is") {
            Some(c) => vec![NodeId(c.value.clone())],
            None => state.placement.nodes.keys().cloned().collect(),
        };
        let mut all = Vec::new();
        let mut unreachable = Vec::new();
        for n in nodes {
            if &n == self.me() {
                all.extend(self.query_local(&q));
                continue;
            }
            match self.ask(&n, Call::QueryLog { q: q.clone() }).await {
                Ok(Answer::Lines { lines }) => all.extend(lines),
                _ => unreachable.push(n.0),
            }
        }
        let root = state.placement.root().unwrap_or(OuId(1));
        let mut homes: BTreeMap<String, Option<OuId>> = BTreeMap::new();
        let mut allowed: BTreeMap<OuId, bool> = BTreeMap::new();
        all.retain(|l| {
            let about = ["user", "actor"].iter().find_map(|k| {
                let id = l.event.get(*k).and_then(|v| v.as_str())?;
                *homes.entry(id.to_string()).or_insert_with(|| self.find_user(id).map(|u| u.ou))
            });
            let ou = about.unwrap_or(root);
            *allowed.entry(ou).or_insert_with(|| self.may(viewer, Perm::LogsRead, ou))
        });
        all.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| a.node.cmp(&b.node)).then_with(|| b.seq.cmp(&a.seq)));
        all.truncate(limit);
        let next = (all.len() == limit).then(|| all.last().map(|l| Cursor { time: l.time, node: l.node.clone(), seq: l.seq })).flatten();
        Ok(LogPage { lines: all, unreachable, next })
    }

    // ---- what the pages show -----------------------------------------------

    /// A user's card: who, where, groups, roles (direct and through groups),
    /// recent sign-ins (with logs-read), sessions on every node.
    pub async fn user_card(self: &Arc<Self>, viewer: &User, id: &str) -> Result<Value, String> {
        let u = self.find_user(id).ok_or("no such user")?;
        if !self.may(viewer, Perm::DirectoryRead, u.ou) {
            return Err("you have no directory-read right over that user".into());
        }
        let (groups, grants) = {
            let inner = self.lock();
            let p = &inner.cluster.state().placement;
            let groups: Vec<&Group> = inner
                .dirs
                .values()
                .filter_map(|d| d.state.member_of.get(id))
                .flat_map(|ids| ids.iter())
                .filter_map(|g| inner.dirs.values().find_map(|d| d.state.groups.get(g)))
                .collect();
            let mut grants = Vec::new();
            for (&ou, d) in &inner.dirs {
                for g in d.state.grants.values() {
                    let via = match &g.subject {
                        Subject::User { id: x } if x == id => Some("directly".to_string()),
                        Subject::Group { id: x } => groups.iter().find(|gr| gr.id == *x).map(|gr| format!("group {}", gr.name)),
                        _ => None,
                    };
                    if let Some(via) = via {
                        grants.push(json!({ "id": g.id, "role": g.role, "scope": p.ou_path(ou), "scope_id": ou.0, "via": via }));
                    }
                }
            }
            let groups: Vec<Value> = groups.iter().map(|g| json!({ "id": g.id, "name": g.name, "ou": p.ou_path(g.ou) })).collect();
            (groups, grants)
        };
        let sign_ins = if self.may(viewer, Perm::LogsRead, u.ou) {
            let q = LogQuery { kind: "auth".into(), conds: vec![Cond { field: "user".into(), op: "is".into(), value: id.into() }], since: None, until: None, limit: 25, filter: None, before: None };
            self.query_logs(viewer, q).await.map(|p| json!(p.lines)).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let (sessions, missing) = self.sessions_everywhere(id).await;
        let can: Vec<String> = self.perms(viewer, u.ou).into_iter().map(perm_name).collect();
        let has: Vec<String> = self.perms(&u, u.ou).into_iter().map(perm_name).collect();
        Ok(json!({
            "user": u, "ou_path": self.ou_path(u.ou), "groups": groups, "grants": grants,
            "sign_ins": sign_ins, "sessions": sessions, "unreachable": missing,
            "can": can, "has_in_own_ou": has, "legacy_admin": u.admin, "totp": self.has_totp(&u),
            "fido2": self.fido2_keys(&u).map(|l| l.len()),
        }))
    }

    /// What the admin page needs besides its tables: node control, the OUs
    /// (with counts and the viewer's rights in each), the roles, the
    /// settings. The users, groups, grants and collections are tables
    /// (tables.rs), a page at a time.
    pub fn overview(&self, viewer: &User) -> Value {
        let inner = self.lock();
        let state = inner.cluster.state();
        let p: &Placement = &state.placement;
        let root = p.root().unwrap_or(OuId(1));
        let can_at = |ou: OuId| Self::perms_in(&inner, viewer, ou);
        let cluster_admin = can_at(root).contains(&Perm::ClusterManage);
        let ous: Vec<Value> = p
            .ous
            .iter()
            .map(|(&id, ou)| {
                let resolved: Map<String, Value> = Domain::ALL
                    .into_iter()
                    .filter(|&d| d != Domain::Ledger)
                    .map(|d| {
                        let v = match p.authority(d, id) {
                            Ok(a) => json!({ "node": a.node, "why": source_text(a.source) }),
                            Err(e) => json!({ "problem": format!("{e:?}") }),
                        };
                        (d.name().to_string(), v)
                    })
                    .collect();
                let secrets = match p.secret_holders(id) {
                    Ok(s) => json!({ "nodes": s.nodes, "why": source_text(s.source) }),
                    Err(e) => json!({ "problem": format!("{e:?}") }),
                };
                let rule = p.sign_in_rule(id).unwrap_or_default();
                let dir = inner.dirs.get(&id);
                json!({
                    "id": id.0, "name": ou.name, "parent": ou.parent.map(|o| o.0), "path": p.ou_path(id),
                    "tier0": ou.tier0, "second_factor": ou.second_factor,
                    "tier0_from": rule.tier0.map(|o| p.ou_path(o)), "second_factor_from": rule.second_factor.map(|o| p.ou_path(o)),
                    "resolved": resolved, "secrets": secrets,
                    "users": dir.map_or(0, |d| d.state.users.len()), "groups": dir.map_or(0, |d| d.state.groups.len()),
                    "secrets_here": inner.creds.contains_key(&id),
                    "can": can_at(id).into_iter().map(perm_name).collect::<Vec<_>>(),
                })
            })
            .collect();
        let roles: Vec<Value> = rbac::ROLES
            .iter()
            .map(|r| json!({ "name": r.name, "about": r.about, "perms": r.perms.iter().map(|p| perm_name(*p)).collect::<Vec<_>>() }))
            .collect();
        let problems: Vec<String> = p.check().err().unwrap_or_default().iter().map(|e| format!("{e:?}")).collect();
        let rtts = self.rtts.lock().unwrap_or_else(|e| e.into_inner()).clone();
        json!({
            "me": self.me(),
            "voter": self.voter_in(state),
            "holds_cluster_log": Self::holds_cluster_log(&inner),
            "cluster_admin": cluster_admin,
            "logs_read": p.ous.keys().any(|&o| can_at(o).contains(&Perm::LogsRead)),
            // queries over the cluster's logs reach everything (D-U31)
            "logs_root": p.root().is_some_and(|r| can_at(r).contains(&Perm::LogsRead)),
            // rules, bans and the never-ban list (D-U32, D-U33)
            "security_root": p.root().is_some_and(|r| can_at(r).contains(&Perm::OuSecurity)),
            "cluster_log": { "applied": inner.cluster.applied(), "head": hex::encode(&inner.cluster.log().head().hash[..6]) },
            "links": rtts,
            "placement": p,
            "ous": ous,
            "roles": roles,
            "collections": if cluster_admin { state.collections.len() } else { 0 },
            "settings": shown_settings(&state.settings),
            "ship_secret_set": !state.settings.ship.secret.is_empty(),
            "storages": if cluster_admin { state.settings.storages.iter().map(|(n, s)| (n.clone(), s.shown())).collect::<Map<String, Value>>() } else { Map::new() },
            "sessions_outlive_logs": state.settings.sessions_outlive_logs(),
            "problems": problems,
        })
    }
}

/// The settings as the page sees them: without the shipping secret.
fn shown_settings(s: &Settings) -> Settings {
    let mut s = s.clone();
    s.ship.secret.clear();
    s.storages.clear();
    s.rules.clear();
    s.never_ban.clear();
    // a log kind newer than the cluster's settings: its defaults (http, D-U31)
    for k in LogKind::ALL {
        s.logs.entry(k).or_insert_with(|| {
            let d = k.defaults();
            crate::audit::LogSetting { verbosity: d.verbosity, retention_days: (d.retention.as_secs() / 86_400) as u32 }
        });
    }
    s
}

impl Node {
    /// admin → files → backups: the live backups (Round 14), each target a storage or a node there
    /// is; the nightly ones (Round 17), each to SFTP storage there is.
    pub async fn set_backups(self: &Arc<Self>, actor: &Actor, list: Vec<crate::audit::LiveBackup>) -> Result<(), String> {
        self.require(actor, Perm::ClusterManage, self.root_ou())?;
        let mut s = self.cluster_state().settings;
        let p = self.cluster_state().placement;
        for b in &list {
            let ok = match b.target.split_once(':') {
                Some(("storage", n)) => s.storages.contains_key(n),
                Some(("node", n)) => p.nodes.contains_key(&crate::placement::NodeId(n.into())) && !b.nightly(),
                _ => false,
            };
            if !ok {
                return Err(format!("{} backup {}: no {}", b.kind, b.name, b.target));
            }
            if b.nightly() {
                if s.storages.get(&b.target[8..]).is_some_and(|st| st.kind() != "sftp") {
                    return Err(format!("nightly backup {}: {} is S3; a nightly backup goes to SFTP for now (S3 comes with A12)", b.name, &b.target[8..]));
                }
                if b.source != "all" {
                    return Err(format!("nightly backup {}: it is of the whole cluster (source all)", b.name));
                }
            } else if b.source != "all" && !p.nodes.contains_key(&crate::placement::NodeId(b.source.clone())) {
                return Err(format!("live backup {}: no node {}", b.name, b.source));
            }
        }
        s.backups = list.clone();
        s.check()?;
        self.commit_anywhere(&actor.label(), Command::Settings(s)).await?;
        self.log_admin(actor, "backups_changed", json!({ "backups": list }));
        Ok(())
    }

    /// Round 17, once: the nightly backup of osada 0.1's settings becomes a nightly backup of the
    /// cluster's, its own SFTP target (`storage`, if it had one) other storage. A storage at the same
    /// place is taken as it is; else it is added as `backup-sftp` (or `backup-sftp-2`, …). Nothing
    /// is done if there is a nightly backup already. Returns the nightly backup's name.
    pub async fn adopt_nightly(self: &Arc<Self>, storage: Option<crate::storage::Storage>, mut entry: crate::audit::LiveBackup) -> Result<String, String> {
        let actor = Actor::node(self.me());
        let mut s = self.cluster_state().settings;
        if let Some(b) = s.backups.iter().find(|b| b.nightly()) {
            return Ok(b.name.clone());
        }
        let mut changes = Vec::new();
        if let Some(st) = storage {
            let same = |o: &crate::storage::Storage| match (&o.target, &st.target) {
                (crate::storage::Target::Sftp { host: h1, port: p1, username: u1, root: r1, .. }, crate::storage::Target::Sftp { host: h2, port: p2, username: u2, root: r2, .. }) => {
                    h1.eq_ignore_ascii_case(h2) && p1 == p2 && u1 == u2 && r1.trim_end_matches('/') == r2.trim_end_matches('/')
                }
                _ => false,
            };
            let name = match s.storages.iter().find(|(_, o)| same(o)) {
                Some((n, _)) => n.clone(),
                None => {
                    let name = (1..).map(|i| if i == 1 { "backup-sftp".to_string() } else { format!("backup-sftp-{i}") }).find(|n| !s.storages.contains_key(n)).unwrap_or_default();
                    st.check(&name)?;
                    changes.push(format!("storage {name}: added, {}; secret set (the nightly backup's own target, moved here)", st.place()));
                    s.storages.insert(name.clone(), st);
                    name
                }
            };
            entry.target = format!("storage:{name}");
        }
        if !s.storages.contains_key(entry.target.trim_start_matches("storage:")) {
            return Err(format!("no {}", entry.target));
        }
        while s.backups.iter().any(|b| b.name == entry.name) {
            entry.name = format!("{}-2", entry.name);
        }
        s.backups.push(entry.clone());
        s.check()?;
        let list = s.backups.clone();
        self.commit_anywhere("the nightly backup, moved to the cluster's settings", Command::Settings(s)).await?;
        if !changes.is_empty() {
            self.log_admin(&actor, "storage_changed", json!({ "storage": entry.target.trim_start_matches("storage:"), "changes": changes }));
        }
        self.log_admin(&actor, "backups_changed", json!({ "backups": list }));
        Ok(entry.name)
    }

    /// The node's data folder.
    pub fn data_dir(&self) -> Option<std::path::PathBuf> {
        self.data.get().cloned()
    }
}
