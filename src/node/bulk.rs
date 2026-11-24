//! Mass updates of users (A24): into or out of a group, access to a module
//! instance given or taken, disabled or enabled; for the users picked, or
//! for every user a table query finds, however many. A job on the node:
//! each user is checked against the actor's rights (the refused ones are
//! counted), and the writes go to each OU's authority in batches of 500
//! users per entry. The job is in the admin log at its start and its end,
//! with the actor's session.

use std::{
    collections::BTreeMap,
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Actor, Node, now_ms};
use crate::{
    directory::{self, Access, DIRECTORY, DirOp, User, random_hex},
    placement::OuId,
    rbac::{Perm, Subject},
    table::Query,
};

const BATCH: usize = 500;
const KEEP_JOBS: usize = 100;

#[derive(Clone, Debug, Serialize)]
pub struct Job {
    pub id: String,
    pub what: String,
    pub by: String,
    pub started: u64,
    pub total: usize,
    pub done: usize,
    pub refused: usize,
    pub failed: usize,
    /// The first reasons, a few.
    pub notes: Vec<String>,
    pub finished: bool,
}

/// Which users: these ids, or every user a query of the users table finds.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub query: Option<Query>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    AddToGroup { group: String },
    RemoveFromGroup { group: String },
    GrantAccess { module: String },
    RevokeAccess { module: String },
    Disable,
    Enable,
    /// The same outside networks for all of them (D-U15).
    SetOutside { outside: String },
}

impl Action {
    fn describe(&self) -> String {
        match self {
            Action::AddToGroup { group } => format!("into group {group}"),
            Action::RemoveFromGroup { group } => format!("out of group {group}"),
            Action::GrantAccess { module } => format!("access to {module}"),
            Action::RevokeAccess { module } => format!("no access to {module}"),
            Action::Disable => "disable".into(),
            Action::Enable => "enable".into(),
            Action::SetOutside { outside } => format!("outside networks: {}", if outside.is_empty() { "none (from anywhere)" } else { outside }),
        }
    }
}

impl Node {
    /// Starts a mass update; returns the job's id.
    pub fn start_bulk(self: &Arc<Self>, actor: Actor, sel: Selection, action: Action) -> Result<String, String> {
        let ids = match &sel.query {
            Some(q) => self.user_ids(&actor.user, q)?,
            None => sel.ids.clone(),
        };
        let mut what = action.describe();
        if let Action::AddToGroup { group } | Action::RemoveFromGroup { group } = &action {
            let g = self.lock().dirs.values().find_map(|d| d.state.groups.get(group).cloned()).ok_or("no such group")?;
            self.require(&actor, Perm::DirectoryManage, g.ou)?;
            what = what.replace(group.as_str(), &g.name);
        }
        if let Action::GrantAccess { module } | Action::RevokeAccess { module } = &action {
            if !self.cluster_state().placement.modules.contains_key(module) {
                return Err(format!("no module {module}"));
            }
        }
        let id = random_hex(6);
        let job = Job {
            id: id.clone(),
            what: format!("{what}: {} user(s)", ids.len()),
            by: actor.user.name.clone(),
            started: now_ms(),
            total: ids.len(),
            done: 0,
            refused: 0,
            failed: 0,
            notes: Vec::new(),
            finished: false,
        };
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            while jobs.len() >= KEEP_JOBS {
                let oldest = jobs.values().filter(|j| j.finished).min_by_key(|j| j.started).map(|j| j.id.clone());
                match oldest {
                    Some(o) => jobs.remove(&o),
                    None => break,
                };
            }
            jobs.insert(id.clone(), job);
        }
        let node = self.clone();
        let jid = id.clone();
        tokio::spawn(async move { node.run_bulk(&jid, actor, ids, action).await });
        Ok(id)
    }

    pub fn job(&self, id: &str) -> Option<Job> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }

    fn update_job(&self, id: &str, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).get_mut(id) {
            f(j);
        }
    }

    fn note(&self, id: &str, refused: usize, failed: usize, why: Option<String>) {
        self.update_job(id, |j| {
            j.refused += refused;
            j.failed += failed;
            if let Some(w) = why {
                if j.notes.len() < 10 {
                    j.notes.push(w);
                }
            }
        });
    }

    async fn run_bulk(self: Arc<Self>, job: &str, actor: Actor, ids: Vec<String>, action: Action) {
        let what = self.job(job).map(|j| j.what).unwrap_or_default();
        self.log_admin(&actor, "bulk_update_started", json!({ "job": job, "changes": [what] }));
        let users: Vec<User> = ids.iter().filter_map(|id| self.find_user(id)).collect();
        if users.len() < ids.len() {
            self.note(job, 0, ids.len() - users.len(), Some(format!("{} user(s) not found", ids.len() - users.len())));
        }
        // Whether the actor may do `perm` in an OU, worked out once per OU.
        let mut can: BTreeMap<(OuId, Perm), bool> = BTreeMap::new();
        let mut may = |ou: OuId, perm: Perm| *can.entry((ou, perm)).or_insert_with(|| self.may(&actor.user, perm, ou));
        let label = actor.label();
        match &action {
            Action::AddToGroup { group } | Action::RemoveFromGroup { group } => {
                let member = matches!(action, Action::AddToGroup { .. });
                let g = self.lock().dirs.values().find_map(|d| d.state.groups.get(group).cloned());
                if let Some(g) = g {
                    let (ok, refused): (Vec<&User>, Vec<&User>) = users.iter().partition(|u| may(u.ou, Perm::DirectoryRead));
                    if !refused.is_empty() {
                        self.note(job, refused.len(), 0, Some(format!("{} user(s) you may not see", refused.len())));
                    }
                    for chunk in ok.chunks(BATCH) {
                        let op = DirOp::SetMembers { group: g.id.clone(), users: chunk.iter().map(|u| u.id.clone()).collect(), member };
                        self.write_chunk(job, directory::directory_of(g.ou), &op, chunk.len(), &label).await;
                    }
                }
            }
            Action::GrantAccess { module } | Action::RevokeAccess { module } => {
                let grant = matches!(action, Action::GrantAccess { .. });
                let mut by_ou: BTreeMap<OuId, Vec<&User>> = BTreeMap::new();
                for u in &users {
                    if may(u.ou, Perm::DirectoryManage) {
                        by_ou.entry(u.ou).or_default().push(u);
                    } else {
                        self.note(job, 1, 0, Some(format!("{}: no directory-manage right in its OU", u.name)));
                    }
                }
                for (ou, list) in by_ou {
                    for chunk in list.chunks(BATCH) {
                        let op = if grant {
                            let list = chunk
                                .iter()
                                .map(|u| Access { id: random_hex(8), module: module.clone(), subject: Subject::User { id: u.id.clone() } })
                                .collect();
                            DirOp::GrantAccess { list }
                        } else {
                            DirOp::RevokeAccess { ids: self.direct_access(ou, chunk, module) }
                        };
                        self.write_chunk(job, directory::directory_of(ou), &op, chunk.len(), &label).await;
                    }
                }
            }
            Action::SetOutside { outside } => match crate::osada1::edge::normalize(outside) {
                Err(e) => self.note(job, 0, users.len(), Some(e)),
                Ok(outside) => {
                    let mut by_ou: BTreeMap<OuId, Vec<&User>> = BTreeMap::new();
                    for u in &users {
                        if may(u.ou, Perm::AccountLock) {
                            by_ou.entry(u.ou).or_default().push(u);
                        } else {
                            self.note(job, 1, 0, Some(format!("{}: no account-lock right in its OU", u.name)));
                        }
                    }
                    for (ou, list) in by_ou {
                        for chunk in list.chunks(BATCH) {
                            let op = DirOp::SetOutside { ids: chunk.iter().map(|u| u.id.clone()).collect(), outside: outside.clone() };
                            self.write_chunk(job, directory::directory_of(ou), &op, chunk.len(), &label).await;
                        }
                    }
                }
            },
            Action::Disable | Action::Enable => {
                let disabled = matches!(action, Action::Disable);
                let mut by_ou: BTreeMap<OuId, Vec<&User>> = BTreeMap::new();
                for u in &users {
                    if disabled && u.id == actor.user.id {
                        self.note(job, 1, 0, Some("you cannot disable yourself".into()));
                    } else if may(u.ou, Perm::AccountLock) {
                        by_ou.entry(u.ou).or_default().push(u);
                    } else {
                        self.note(job, 1, 0, Some(format!("{}: no account-lock right in its OU", u.name)));
                    }
                }
                for (ou, list) in by_ou {
                    for chunk in list.chunks(BATCH) {
                        let op = DirOp::SetDisabled { ids: chunk.iter().map(|u| u.id.clone()).collect(), disabled };
                        self.write_chunk(job, directory::directory_of(ou), &op, chunk.len(), &label).await;
                        if disabled {
                            for u in chunk {
                                for s in self.sessions_of(&u.id) {
                                    let _ = self.end_session(&actor, &u.id, &s.sid);
                                }
                            }
                        }
                    }
                }
            }
        }
        self.finish(job, &actor);
    }

    /// The ids of the direct access to `module` that these users of `ou` have.
    fn direct_access(&self, ou: OuId, users: &[&User], module: &str) -> Vec<String> {
        let inner = self.lock();
        let Some(d) = inner.dirs.get(&ou) else { return Vec::new() };
        users.iter().flat_map(|u| d.state.access_for(&u.id)).filter(|a| a.module == module).map(|a| a.id.clone()).collect()
    }

    async fn write_chunk(self: &Arc<Self>, job: &str, collection: crate::log::CollectionId, op: &DirOp, n: usize, actor: &str) {
        let value = match serde_json::to_value(op) {
            Ok(v) => v,
            Err(e) => return self.note(job, 0, n, Some(e.to_string())),
        };
        match self.submit(collection, DIRECTORY, value, actor).await {
            Ok(_) => self.update_job(job, |j| j.done += n),
            Err(e) => self.note(job, 0, n, Some(e)),
        }
    }

    fn finish(&self, job: &str, actor: &Actor) {
        self.update_job(job, |j| j.finished = true);
        if let Some(j) = self.job(job) {
            let summary = format!("{}: {} done, {} refused, {} failed", j.what, j.done, j.refused, j.failed);
            self.log_admin(actor, "bulk_update_finished", json!({ "job": job, "changes": [summary] }));
        }
    }
}
