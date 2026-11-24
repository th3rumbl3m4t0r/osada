//! What the drop-in upgrade of osada 0.1 needs of a node (docs/PLAN-0.2.0.md,
//! D-U2, D-U3): its accounts brought over once, with their password hashes
//! and OIDC subjects, and sessions looked up the way osada 0.1's pages ask.

use std::sync::Arc;

use serde_json::json;

use super::{Actor, Node, Session, now_ms};
use crate::{
    directory::{self, CREDENTIALS, CredOp, DIRECTORY, DirOp, User},
    placement::{Domain, Module, ModuleKind, NodeId, OuId},
    rbac::{Perm, Subject},
};

/// An account of osada 0.1 to bring over.
#[derive(Clone, Debug)]
pub struct Imported {
    /// Its OIDC subject: the user id from now on, so other programs that sign
    /// in with osada see the same person.
    pub id: String,
    pub name: String,
    /// Its argon2id hash, as it is.
    pub hash: Option<String>,
    pub disabled: bool,
    /// An administrator of osada 0.1: the role admin over /.
    pub admin: bool,
    /// Its outside networks (normalized), D-U15.
    pub outside: String,
}

impl Node {
    /// The root OU, once this node is the authority of its directory and
    /// credentials (a new cluster, after its first reconcile).
    pub fn root_ready(&self) -> Option<OuId> {
        let inner = self.lock();
        let p = &inner.cluster.state().placement;
        let root = p.ous.iter().find(|(_, o)| o.parent.is_none()).map(|(id, _)| *id)?;
        [directory::directory_of(root), directory::credentials_of(root)]
            .iter()
            .all(|id| inner.replicas.get(id).is_some_and(|r| r.is_authority()))
            .then_some(root)
    }

    /// Whether the directory has any user.
    pub fn has_users(&self) -> bool {
        self.lock().dirs.values().any(|d| !d.state.users.is_empty())
    }

    /// The session whose token has this sha256 (hex), as `session` does.
    pub fn session_by_hash(&self, key: &str) -> Option<(Session, User)> {
        let inner = self.lock();
        let s = inner.sessions.get(key)?.clone();
        if s.expires <= now_ms() {
            return None;
        }
        let u = inner.dirs.values().find_map(|d| d.state.users.get(&s.user)).cloned()?;
        (!u.disabled).then_some((s, u))
    }

    /// Whether `user` runs the cluster (node control at the root): osada
    /// 0.1's admin pages are for them.
    pub fn runs_cluster(&self, user: &User) -> bool {
        self.may(user, Perm::ClusterManage, self.root_ou())
    }

    /// Brings one account of osada 0.1 over into the root OU. False when it is
    /// there already (by id or by name): importing again changes nothing.
    pub async fn import_user(self: &Arc<Self>, u: &Imported) -> Result<bool, String> {
        let root = self.root_ready().ok_or("the root OU is not ready on this node")?;
        if !directory::valid_user_name(&u.name) {
            return Err(format!("{:?} is not a user name osada 0.2 takes", u.name));
        }
        {
            let inner = self.lock();
            if inner.dirs.values().any(|d| d.state.users.contains_key(&u.id) || d.state.by_name(&u.name).is_some()) {
                return Ok(false);
            }
        }
        let actor = Actor::node(&self.me);
        let outside = crate::osada1::edge::normalize(&u.outside).unwrap_or_default();
        let user = User { id: u.id.clone(), name: u.name.clone(), display: String::new(), ou: root, admin: false, disabled: u.disabled, outside, mailbox: None };
        let op = serde_json::to_value(DirOp::CreateUser { user: user.clone() }).map_err(|e| e.to_string())?;
        self.submit(directory::directory_of(root), DIRECTORY, op, &actor.label()).await?;
        self.log_admin(&actor, "user_imported", json!({ "user": user.id, "name": user.name, "ou": self.ou_path(root), "from": "osada 0.1", "disabled": u.disabled }));
        if let Some(hash) = u.hash.as_ref().filter(|h| h.starts_with("$argon2id$")) {
            let op = serde_json::to_value(CredOp::SetPassword { user: user.id.clone(), hash: hash.clone() }).map_err(|e| e.to_string())?;
            self.submit(directory::credentials_of(root), CREDENTIALS, op, &actor.label()).await?;
        }
        if u.admin {
            self.grant_unchecked(&actor, root, "admin", Subject::User { id: user.id.clone() }).await?;
        }
        Ok(true)
    }

    /// osada 0.1's outside networks for accounts imported before they were
    /// the directory's (D-U15): set where the directory has none. How many.
    pub async fn import_outside(self: &Arc<Self>, list: &[(String, String)]) -> Result<usize, String> {
        let actor = Actor::node(&self.me);
        let mut n = 0;
        for (id, outside) in list {
            let Ok(outside) = crate::osada1::edge::normalize(outside) else { continue };
            let Some(u) = self.find_user(id) else { continue };
            if outside.is_empty() || !u.outside.is_empty() {
                continue;
            }
            let op = DirOp::UpdateUser { id: id.clone(), display: None, admin: None, disabled: None, outside: Some(outside.clone()) };
            self.submit(directory::directory_of(u.ou), DIRECTORY, serde_json::to_value(op).map_err(|e| e.to_string())?, &actor.label()).await?;
            self.log_admin(&actor, "user_updated", json!({ "user": id, "name": u.name, "ou": self.ou_path(u.ou), "changes": { "outside": outside, "from": "osada 0.1" } }));
            n += 1;
        }
        Ok(n)
    }

    /// The proxy instances (D-U7), by name.
    pub fn proxies(&self) -> Vec<(String, Module)> {
        let p = self.cluster_state().placement;
        p.modules.into_iter().filter(|(_, m)| m.kind == ModuleKind::PROXY).collect()
    }

    /// Whether the user may use the module instance: access of their own or
    /// through a group (A24).
    pub fn may_use(&self, user: &str, module: &str) -> bool {
        self.modules_of(user).iter().any(|m| m == module)
    }

    /// The groups a user is in: (id, display name or name) (D-U29).
    pub fn groups_of(&self, user: &str) -> Vec<(String, String)> {
        let inner = self.lock();
        let mut out = Vec::new();
        for d in inner.dirs.values() {
            for g in d.state.member_of.get(user).into_iter().flatten() {
                if let Some(gr) = inner.dirs.values().find_map(|d| d.state.groups.get(g)) {
                    out.push((gr.id.clone(), if gr.display.is_empty() { gr.name.clone() } else { gr.display.clone() }));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// A group by its id or name: its id (D-U29: a site's folder as a group's).
    pub fn group_id(&self, s: &str) -> Option<String> {
        let inner = self.lock();
        inner.dirs.values().find_map(|d| d.state.groups.values().find(|g| g.id == s || g.name.eq_ignore_ascii_case(s)).map(|g| g.id.clone()))
    }

    /// The module instances the user may use.
    pub fn modules_of(&self, user: &str) -> Vec<String> {
        let inner = self.lock();
        let Some(u) = inner.dirs.values().find_map(|d| d.state.users.get(user)) else { return Vec::new() };
        if u.disabled {
            return Vec::new();
        }
        let mut subjects = vec![(u.ou, u.id.clone())];
        for d in inner.dirs.values() {
            for g in d.state.member_of.get(user).into_iter().flatten() {
                if let Some(g) = inner.dirs.values().find_map(|d| d.state.groups.get(g)) {
                    subjects.push((g.ou, g.id.clone()));
                }
            }
        }
        let mut out: Vec<String> = subjects
            .iter()
            .filter_map(|(ou, id)| inner.dirs.get(ou).map(|d| d.state.access_for(id).into_iter().map(|a| a.module.clone()).collect::<Vec<_>>()))
            .flatten()
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The mail server outside osada (Stalwart) as the mail instance `name`
    /// on the node `on`: its TLS at `host` passed on to `upstream`
    /// (`tls://host:port`), access for `users` (D-U16). Made once; access is
    /// given to whoever of `users` has none yet, so it can be run again.
    /// The number of accounts given access.
    pub async fn import_mail(self: &Arc<Self>, name: &str, on: &NodeId, host: &str, upstream: &str, users: &[String]) -> Result<usize, String> {
        let mut p = self.cluster_state().placement;
        let actor = Actor::node(&self.me);
        match p.modules.get(name) {
            Some(m) if m.kind != ModuleKind::Data(Domain::Mail) => return Err(format!("{name} is a module of the kind {}", m.kind)),
            Some(_) => {}
            None => {
                let mut m = Module::new(ModuleKind::Data(Domain::Mail), "mail", on.clone());
                m.host = Some(host.to_ascii_lowercase());
                m.upstream = Some(upstream.to_string());
                p.modules.insert(name.into(), m);
                self.set_placement(&actor, p).await?;
            }
        }
        let mut n = 0;
        for id in users {
            if self.find_user(id).is_none() || self.may_use(id, name) {
                continue;
            }
            match self.grant_access(&actor, name, Subject::User { id: id.clone() }).await {
                Ok(()) => n += 1,
                Err(e) => eprintln!("{}: access to {name} for {id}: {e}", self.me),
            }
        }
        Ok(n)
    }

    /// osada 0.1's apptab as the proxy instance `name`, with access for
    /// the users it was on for; nothing when the instance is there already.
    pub async fn import_proxy(self: &Arc<Self>, name: &str, host: &str, upstream: &str, users: &[String]) -> Result<bool, String> {
        let mut p = self.cluster_state().placement;
        if p.modules.contains_key(name) {
            return Ok(false);
        }
        let actor = Actor::node(&self.me);
        let mut m = Module::new(ModuleKind::PROXY, name, self.me.clone());
        m.host = Some(host.to_ascii_lowercase());
        m.upstream = Some(upstream.trim_end_matches('/').to_string());
        p.modules.insert(name.into(), m);
        self.set_placement(&actor, p).await?;
        for id in users {
            if !self.may_use(id, name) {
                self.grant_access(&actor, name, Subject::User { id: id.clone() }).await?;
            }
        }
        Ok(true)
    }
}
