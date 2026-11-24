//! The pages' tables over the directory and the cluster (table.rs): each row
//! cut to what the viewer may see, the filter, the sort and the page done
//! here, so a table may hold a million rows.
//!
//! Tables, and the parameters that narrow them:
//! - `users`: `ou` (and `below=1`), `member_of` (a group's members);
//!   columns `member_of` (groups) and `access_to` (module instances)
//! - `access`: `subject_user` (directly and through groups),
//!   `subject_group`, `module` (who has access to it)
//! - `groups`: `ou`, `below`, `member` (the groups a user is in)
//! - `grants`: `ou`, `below` (where granted), `subject_user` (directly and
//!   through groups), `subject_group`
//! - `collections` (cluster rights): `ou`, `below`
//! - `objects`: everything in `ou` (and `below`): users, groups, grants,
//!   collections
//! - `ous`: `perm` (only where the viewer has it)

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::{Inner, Kind, Node};
use crate::{
    directory::{Access, Group, User},
    log::CollectionId,
    node::admin::perm_name,
    placement::{OuId, Placement},
    rbac::{Grant, Perm, Subject},
    table::{self, Cell, Page, Query, Row},
};

struct Ctx<'a> {
    inner: &'a Inner,
    p: &'a Placement,
    paths: BTreeMap<OuId, String>,
    /// "role @ /path" by the id of the user or group it is granted to.
    roles: BTreeMap<String, Vec<String>>,
}

impl<'a> Ctx<'a> {
    fn path(&self, ou: OuId) -> &str {
        self.paths.get(&ou).map_or("", String::as_str)
    }

    fn group(&self, id: &str) -> Option<&'a Group> {
        self.inner.dirs.values().find_map(|d| d.state.groups.get(id))
    }

    fn user(&self, id: &str) -> Option<&'a User> {
        self.inner.dirs.values().find_map(|d| d.state.users.get(id))
    }

    fn groups_of(&self, user: &str) -> Vec<&'a Group> {
        let mut v: Vec<&Group> = Vec::new();
        for d in self.inner.dirs.values() {
            if let Some(ids) = d.state.member_of.get(user) {
                v.extend(ids.iter().filter_map(|g| self.group(g)));
            }
        }
        v
    }

    /// The module instances a user may use: directly, and through groups.
    fn access_of(&self, u: &User) -> Vec<(&'a Access, String)> {
        let mut v: Vec<(&Access, String)> = Vec::new();
        if let Some(d) = self.inner.dirs.get(&u.ou) {
            v.extend(d.state.access_for(&u.id).into_iter().map(|a| (a, "directly".to_string())));
        }
        for g in self.groups_of(&u.id) {
            if let Some(d) = self.inner.dirs.get(&g.ou) {
                v.extend(d.state.access_for(&g.id).into_iter().map(|a| (a, format!("group {}", g.name))));
            }
        }
        v
    }

    fn subject_label(&self, s: &Subject) -> String {
        match s {
            Subject::User { id } => format!("user {}", self.user(id).map_or(id.as_str(), |u| u.name.as_str())),
            Subject::Group { id } => format!("group {}", self.group(id).map_or(id.as_str(), |g| g.name.as_str())),
        }
    }
}

fn text(s: &str) -> Cell {
    Cell::Text(s.to_string())
}

struct UserRow<'a> {
    c: &'a Ctx<'a>,
    u: &'a User,
}

impl Row for UserRow<'_> {
    fn cell(&self, col: &str) -> Cell {
        let u = self.u;
        match col {
            "name" => text(&u.name),
            "display" => text(&u.display),
            "ou" => text(self.c.path(u.ou)),
            "state" => text(if u.disabled { "disabled" } else { "active" }),
            "roles" => Cell::Texts(self.c.roles.get(&u.id).cloned().unwrap_or_default()),
            "member_of" => Cell::Texts(self.c.groups_of(&u.id).iter().map(|g| g.name.clone()).collect()),
            "access_to" => Cell::Texts(self.c.access_of(u).iter().map(|(a, _)| a.module.clone()).collect()),
            "outside" => text(&u.outside),
            "id" => text(&u.id),
            _ => Cell::Empty,
        }
    }

    fn json(&self) -> Value {
        let u = self.u;
        let mut roles = self.c.roles.get(&u.id).cloned().unwrap_or_default();
        if u.admin {
            roles.push("admin (old flag)".into());
        }
        json!({
            "id": u.id, "name": u.name, "display": u.display, "ou": u.ou.0, "ou_path": self.c.path(u.ou),
            "disabled": u.disabled, "admin": u.admin, "roles": roles, "outside": u.outside,
            "member_of": self.c.groups_of(&u.id).iter().map(|g| json!({ "id": g.id, "name": g.name })).collect::<Vec<_>>(),
            "access_to": self.c.access_of(u).iter().map(|(a, via)| json!({ "module": a.module, "via": via })).collect::<Vec<_>>(),
        })
    }
}

struct GroupRow<'a> {
    c: &'a Ctx<'a>,
    g: &'a Group,
}

impl Row for GroupRow<'_> {
    fn cell(&self, col: &str) -> Cell {
        let g = self.g;
        match col {
            "name" => text(&g.name),
            "display" => text(&g.display),
            "ou" => text(self.c.path(g.ou)),
            "members" => Cell::Num(g.members.len() as f64),
            "roles" => Cell::Texts(self.c.roles.get(&g.id).cloned().unwrap_or_default()),
            "id" => text(&g.id),
            _ => Cell::Empty,
        }
    }

    fn json(&self) -> Value {
        let g = self.g;
        json!({
            "id": g.id, "name": g.name, "display": g.display, "ou": g.ou.0, "ou_path": self.c.path(g.ou),
            "members": g.members.len(), "roles": self.c.roles.get(&g.id).cloned().unwrap_or_default(),
        })
    }
}

struct AccessRow<'a> {
    c: &'a Ctx<'a>,
    a: &'a Access,
    via: String,
}

impl AccessRow<'_> {
    fn module(&self) -> Option<&crate::placement::Module> {
        self.c.p.modules.get(&self.a.module)
    }
}

impl Row for AccessRow<'_> {
    fn cell(&self, col: &str) -> Cell {
        match col {
            "module" => text(&self.a.module),
            "kind" => self.module().map_or(Cell::Empty, |m| Cell::Text(m.kind.name().into())),
            "title" => self.module().map_or(Cell::Empty, |m| Cell::Text(m.title.clone())),
            "node" => self.module().map_or(Cell::Empty, |m| Cell::Text(m.node.0.clone())),
            "via" => text(&self.via),
            "subject" => Cell::Text(self.c.subject_label(&self.a.subject)),
            _ => Cell::Empty,
        }
    }

    fn json(&self) -> Value {
        let m = self.module();
        json!({
            "id": self.a.id, "module": self.a.module, "kind": m.map(|m| m.kind.name()), "title": m.map(|m| m.title.clone()),
            "node": m.map(|m| m.node.0.clone()), "via": self.via, "subject": self.a.subject, "subject_label": self.c.subject_label(&self.a.subject),
        })
    }
}

struct GrantRow<'a> {
    c: &'a Ctx<'a>,
    g: &'a Grant,
    scope: OuId,
    via: String,
}

impl Row for GrantRow<'_> {
    fn cell(&self, col: &str) -> Cell {
        match col {
            "role" => text(&self.g.role),
            "scope" => text(self.c.path(self.scope)),
            "subject" => Cell::Text(self.c.subject_label(&self.g.subject)),
            "via" => text(&self.via),
            "id" => text(&self.g.id),
            _ => Cell::Empty,
        }
    }

    fn json(&self) -> Value {
        json!({
            "id": self.g.id, "role": self.g.role, "scope": self.c.path(self.scope), "scope_id": self.scope.0,
            "subject": self.g.subject, "subject_label": self.c.subject_label(&self.g.subject), "via": self.via,
        })
    }
}

struct CollectionRow<'a> {
    c: &'a Ctx<'a>,
    id: CollectionId,
    col: &'a crate::cluster::Collection,
}

impl CollectionRow<'_> {
    fn kind(&self) -> &str {
        match self.c.inner.kinds.get(&self.id) {
            Some((Kind::Directory, _)) => "directory",
            Some((Kind::Credentials, _)) => "credentials",
            Some((Kind::Room, _)) => "chat room",
            None => self.col.domain.name(),
        }
    }

    fn moving_to(&self) -> Option<String> {
        self.c.p.authority(self.col.domain, self.col.ou).ok().map(|a| a.node.0).filter(|w| *w != self.col.term.authority.0)
    }

    fn here(&self) -> Option<u64> {
        self.c.inner.replicas.get(&self.id).map(|r| r.log().head().seq)
    }
}

impl Row for CollectionRow<'_> {
    fn cell(&self, col: &str) -> Cell {
        let t = &self.col.term;
        match col {
            "id" => Cell::Text(self.id.to_string()),
            "kind" => text(self.kind()),
            "ou" => text(self.c.path(self.col.ou)),
            "epoch" => Cell::Num(t.epoch as f64),
            "authority" => text(&t.authority.0),
            "members" => Cell::Texts(t.members.iter().map(|m| m.0.clone()).collect()),
            "here" => self.here().map_or(Cell::Empty, |h| Cell::Num(h as f64)),
            "moving_to" => self.moving_to().map_or(Cell::Empty, Cell::Text),
            _ => Cell::Empty,
        }
    }

    fn json(&self) -> Value {
        let t = &self.col.term;
        json!({
            "id": self.id.to_string(), "kind": self.kind(), "domain": self.col.domain, "ou": self.col.ou.0, "ou_path": self.c.path(self.col.ou),
            "epoch": t.epoch, "authority": t.authority, "members": t.members, "here": self.here(), "moving_to": self.moving_to(),
        })
    }
}

/// One thing in an OU, of any kind.
struct Object {
    kind: &'static str,
    name: String,
    detail: String,
    ou: String,
    go: String,
}

impl Row for Object {
    fn cell(&self, col: &str) -> Cell {
        match col {
            "type" => text(self.kind),
            "name" => text(&self.name),
            "detail" => text(&self.detail),
            "ou" => text(&self.ou),
            _ => Cell::Empty,
        }
    }

    fn json(&self) -> Value {
        json!({ "type": self.kind, "name": self.name, "detail": self.detail, "ou": self.ou, "go": self.go })
    }
}

struct OuRow<'a> {
    c: &'a Ctx<'a>,
    id: OuId,
    can: Vec<String>,
}

impl Row for OuRow<'_> {
    fn cell(&self, col: &str) -> Cell {
        let ou = &self.c.p.ous[&self.id];
        let dir = self.c.inner.dirs.get(&self.id);
        match col {
            "path" => text(self.c.path(self.id)),
            "users" => Cell::Num(dir.map_or(0, |d| d.state.users.len()) as f64),
            "groups" => Cell::Num(dir.map_or(0, |d| d.state.groups.len()) as f64),
            "tier0" => Cell::Bool(ou.tier0),
            "second_factor" => Cell::Bool(ou.second_factor),
            _ => Cell::Empty,
        }
    }

    fn json(&self) -> Value {
        let ou = &self.c.p.ous[&self.id];
        let dir = self.c.inner.dirs.get(&self.id);
        json!({
            "id": self.id.0, "path": self.c.path(self.id), "users": dir.map_or(0, |d| d.state.users.len()),
            "groups": dir.map_or(0, |d| d.state.groups.len()), "tier0": ou.tier0, "second_factor": ou.second_factor, "can": self.can,
        })
    }
}

enum Want {
    Page,
    Ids,
}

enum Got {
    Page(Page),
    Ids(Vec<String>),
}

impl Node {
    /// One page of table `name` for `viewer`.
    pub fn table(&self, viewer: &User, name: &str, q: &Query) -> Result<Page, String> {
        match self.rows(viewer, name, q, Want::Page)? {
            Got::Page(p) => Ok(p),
            Got::Ids(_) => Err("no page".into()),
        }
    }

    /// The ids of every user a query of the users table finds, unpaged (a
    /// mass update over them).
    pub fn user_ids(&self, viewer: &User, q: &Query) -> Result<Vec<String>, String> {
        match self.rows(viewer, "users", q, Want::Ids)? {
            Got::Ids(v) => Ok(v),
            Got::Page(_) => Err("no ids".into()),
        }
    }

    fn rows(&self, viewer: &User, name: &str, q: &Query, want: Want) -> Result<Got, String> {
        let inner = self.lock();
        let p = &inner.cluster.state().placement;
        let perms: BTreeMap<OuId, BTreeSet<Perm>> = p.ous.keys().map(|&o| (o, Self::perms_in(&inner, viewer, o))).collect();
        let may = |perm: Perm, ou: OuId| perms.get(&ou).is_some_and(|s| s.contains(&perm));
        let root = p.root().unwrap_or(OuId(1));
        let paths: BTreeMap<OuId, String> = p.ous.keys().map(|&o| (o, p.ou_path(o))).collect();
        let mut roles: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (&ou, d) in &inner.dirs {
            for g in d.state.grants.values() {
                let id = match &g.subject {
                    Subject::User { id } | Subject::Group { id } => id.clone(),
                };
                roles.entry(id).or_default().push(format!("{} @ {}", g.role, paths.get(&ou).map_or("", String::as_str)));
            }
        }
        let c = Ctx { inner: &inner, p, paths, roles };
        let scope: Option<(OuId, bool)> = q.param("ou").and_then(|s| s.parse().ok()).map(|o| (OuId(o), q.param("below") == Some("1")));
        let base = scope.map(|(o, _)| c.path(o).to_string());
        let in_scope = |ou: OuId| match (scope, &base) {
            (None, _) => true,
            (Some((s, false)), _) => ou == s,
            (Some((_, true)), Some(b)) => {
                let path = c.path(ou);
                path == b || b == "/" || path.starts_with(&format!("{b}/"))
            }
            _ => false,
        };
        let users = || c.inner.dirs.values().flat_map(|d| d.state.users.values()).filter(|u| may(Perm::DirectoryRead, u.ou) && in_scope(u.ou));
        let groups = || c.inner.dirs.values().flat_map(|d| d.state.groups.values()).filter(|g| may(Perm::DirectoryRead, g.ou) && in_scope(g.ou));
        let grants = || {
            c.inner
                .dirs
                .iter()
                .filter(|(o, _)| may(Perm::DirectoryRead, **o) && in_scope(**o))
                .flat_map(|(&o, d)| d.state.grants.values().map(move |g| (o, g)))
        };
        match name {
            "users" => {
                let member_of = q.param("member_of").map(|g| c.group(g).map(|g| g.members.clone()).unwrap_or_default());
                let rows = users().filter(|u| member_of.as_ref().is_none_or(|m| m.contains(&u.id))).map(|u| UserRow { c: &c, u });
                match want {
                    Want::Ids => table::matching(rows, q.filter.as_ref()).map(|v| Got::Ids(v.into_iter().map(|r| r.u.id.clone()).collect())),
                    Want::Page => table::page(rows, q).map(Got::Page),
                }
            }
            "access" => {
                let mut rows: Vec<AccessRow> = Vec::new();
                if let Some(id) = q.param("subject_user") {
                    let u = c.user(id).filter(|u| may(Perm::DirectoryRead, u.ou)).ok_or("no such user, or not yours to see")?;
                    rows.extend(c.access_of(u).into_iter().map(|(a, via)| AccessRow { c: &c, a, via }));
                } else if let Some(id) = q.param("subject_group") {
                    let g = c.group(id).filter(|g| may(Perm::DirectoryRead, g.ou)).ok_or("no such group, or not yours to see")?;
                    if let Some(d) = c.inner.dirs.get(&g.ou) {
                        rows.extend(d.state.access_for(&g.id).into_iter().map(|a| AccessRow { c: &c, a, via: "directly".into() }));
                    }
                } else {
                    let module = q.param("module");
                    for (o, d) in &c.inner.dirs {
                        if !may(Perm::DirectoryRead, *o) || !in_scope(*o) {
                            continue;
                        }
                        rows.extend(d.state.access.values().filter(|a| module.is_none_or(|m| a.module == m)).map(|a| AccessRow { c: &c, a, via: "directly".into() }));
                    }
                }
                table::page(rows.into_iter(), q).map(Got::Page)
            }
            "groups" => {
                let member = q.param("member");
                let rows = groups().filter(|g| member.is_none_or(|m| g.members.contains(m))).map(|g| GroupRow { c: &c, g });
                table::page(rows, q).map(Got::Page)
            }
            "grants" => {
                let via = |g: &Grant| -> Option<String> {
                    match (q.param("subject_user"), q.param("subject_group"), &g.subject) {
                        (None, None, _) => Some(String::new()),
                        (Some(u), _, Subject::User { id }) if id == u => Some("directly".into()),
                        (Some(u), _, Subject::Group { id }) => c.group(id).filter(|gr| gr.members.contains(u)).map(|gr| format!("group {}", gr.name)),
                        (None, Some(x), Subject::Group { id }) if id == x => Some("directly".into()),
                        _ => None,
                    }
                };
                let rows = grants().filter_map(|(o, g)| via(g).map(|v| GrantRow { c: &c, g, scope: o, via: v }));
                table::page(rows, q).map(Got::Page)
            }
            "collections" => {
                if !may(Perm::ClusterManage, root) {
                    return Err("you have no cluster rights, which collections need".into());
                }
                let rows = c.inner.cluster.state().collections.iter().filter(|(_, col)| in_scope(col.ou)).map(|(id, col)| CollectionRow { c: &c, id: *id, col });
                table::page(rows, q).map(Got::Page)
            }
            "objects" => {
                if scope.is_none() {
                    return Err("objects of which OU?".into());
                }
                let mut rows: Vec<Object> = Vec::new();
                rows.extend(users().map(|u| Object {
                    kind: "user",
                    name: u.name.clone(),
                    detail: [u.display.as_str(), if u.disabled { "disabled" } else { "" }].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · "),
                    ou: c.path(u.ou).into(),
                    go: format!("/admin/users/{}", u.id),
                }));
                rows.extend(groups().map(|g| Object {
                    kind: "group",
                    name: g.name.clone(),
                    detail: format!("{}{} member(s)", if g.display.is_empty() { String::new() } else { format!("{} · ", g.display) }, g.members.len()),
                    ou: c.path(g.ou).into(),
                    go: format!("/admin/groups/{}", g.id),
                }));
                rows.extend(grants().map(|(o, g)| Object {
                    kind: "role",
                    name: g.role.clone(),
                    detail: format!("to {}", c.subject_label(&g.subject)),
                    ou: c.path(o).into(),
                    go: match &g.subject {
                        Subject::User { id } => format!("/admin/users/{id}"),
                        Subject::Group { id } => format!("/admin/groups/{id}"),
                    },
                }));
                if may(Perm::ClusterManage, root) {
                    rows.extend(c.inner.cluster.state().collections.iter().filter(|(_, col)| in_scope(col.ou)).map(|(id, col)| {
                        let r = CollectionRow { c: &c, id: *id, col };
                        Object {
                            kind: "collection",
                            name: r.kind().into(),
                            detail: format!("authority {}, epoch {}, on {}", col.term.authority, col.term.epoch, col.term.members.iter().map(|m| m.0.as_str()).collect::<Vec<_>>().join(", ")),
                            ou: c.path(col.ou).into(),
                            go: "/admin/collections".into(),
                        }
                    }));
                }
                table::page(rows.into_iter(), q).map(Got::Page)
            }
            "ous" => {
                let need = q.param("perm").and_then(|p| Perm::ALL.into_iter().find(|x| perm_name(*x) == p));
                let rows = p.ous.keys().filter(|&&o| need.is_none_or(|n| may(n, o)) && in_scope(o)).map(|&o| OuRow {
                    c: &c,
                    id: o,
                    can: perms.get(&o).map(|s| s.iter().map(|x| perm_name(*x)).collect()).unwrap_or_default(),
                });
                table::page(rows, q).map(Got::Page)
            }
            other => Err(format!("no table {other}")),
        }
    }
}
