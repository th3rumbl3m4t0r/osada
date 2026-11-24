//! Authorisation (§10.5): roles over parts of the OU tree.
//!
//! A grant gives a subject (a user, or a group: a function such as "EMEA
//! helpdesk") a role over one OU and everything below it. It is kept in that
//! OU's directory, so it replicates with it and every node can check it
//! locally. What someone may do in an OU is the union of the roles granted to
//! them, and to the groups they are in, on the way from the root down to it.
//! Data in collections (files, mail, …) takes the OU of its account or
//! shared space, so a role over `/AMER/US` reaches all of it there
//! (`data-read`: eDiscovery).
//!
//! Some permissions concern the whole cluster (node control, handovers, the
//! settings); they only come from a grant at the root.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::placement::{OuId, Placement};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Perm {
    /// Node control, handovers, the cluster's settings; from the root only.
    ClusterManage,
    /// Make OUs below, rename them, place their domains (links).
    OuManage,
    /// An OU's security: tier 0, second factor, where its secrets are kept.
    OuSecurity,
    /// See users and groups, their roles and sign-ins' outline.
    DirectoryRead,
    /// Make and change users and groups, and who is in a group.
    DirectoryManage,
    /// Set users' passwords.
    PasswordSet,
    /// Disable and enable accounts, end their sessions.
    AccountLock,
    /// Give and take roles: only roles whose permissions one holds there.
    RolesGrant,
    /// Read the authentication and admin logs about the accounts.
    LogsRead,
    /// Read the accounts' data: files, mail, … (eDiscovery).
    DataRead,
}

impl Perm {
    pub const ALL: [Perm; 10] = [
        Perm::ClusterManage,
        Perm::OuManage,
        Perm::OuSecurity,
        Perm::DirectoryRead,
        Perm::DirectoryManage,
        Perm::PasswordSet,
        Perm::AccountLock,
        Perm::RolesGrant,
        Perm::LogsRead,
        Perm::DataRead,
    ];
}

pub struct Role {
    pub name: &'static str,
    pub about: &'static str,
    pub perms: &'static [Perm],
}

use Perm::*;

/// The predefined roles.
pub const ROLES: &[Role] = &[
    Role { name: "admin", about: "everything, over its OUs", perms: &Perm::ALL },
    Role {
        name: "infrastructure-admin",
        about: "node control, handovers, log settings (granted at the root)",
        perms: &[ClusterManage, DirectoryRead],
    },
    Role {
        name: "security-admin",
        about: "OU security (tier 0, second factor, secrets), locking accounts, the logs",
        perms: &[DirectoryRead, OuSecurity, AccountLock, LogsRead],
    },
    Role { name: "user-admin", about: "users and groups, their passwords", perms: &[DirectoryRead, DirectoryManage, PasswordSet, AccountLock] },
    Role { name: "ou-admin", about: "the OU tree below and where its domains go", perms: &[DirectoryRead, OuManage] },
    Role { name: "auditor", about: "reads the directory and the logs", perms: &[DirectoryRead, LogsRead] },
    Role { name: "reader", about: "reads the directory and the OU settings", perms: &[DirectoryRead] },
    Role {
        name: "ediscovery-reader",
        about: "reads the accounts' data: files, mail, … as those modules come",
        perms: &[DirectoryRead, DataRead],
    },
];

pub fn role(name: &str) -> Option<&'static Role> {
    ROLES.iter().find(|r| r.name == name)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Subject {
    User { id: String },
    Group { id: String },
}

/// A role for a subject, over the OU whose directory keeps the grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub role: String,
    pub subject: Subject,
}

/// What `subjects` (a user and the groups it is in) may do in `ou`, given
/// the grants each OU keeps.
pub fn perms_at(p: &Placement, ou: OuId, subjects: &[Subject], grants_in: impl Fn(OuId) -> Vec<Grant>) -> BTreeSet<Perm> {
    let Ok(path) = p.path(ou) else { return BTreeSet::new() };
    let mut out = BTreeSet::new();
    for (i, o) in path.iter().enumerate() {
        for g in grants_in(*o) {
            if !subjects.contains(&g.subject) {
                continue;
            }
            let Some(r) = role(&g.role) else { continue };
            out.extend(r.perms.iter().copied().filter(|&perm| perm != ClusterManage || i == 0));
        }
    }
    out
}

/// Whether a change of node control from `old` to `new` stays within what
/// `may` allows, part by part: the nodes, the defaults and the default
/// secrets need `cluster-manage` (at the root); a new OU, a new name, a new
/// place in the tree or new links need `ou-manage` there (for a new or moved
/// OU, at its parent); tier 0, the second factor and the secrets need
/// `ou-security` on the OU.
pub fn check_change(old: &Placement, new: &Placement, may: impl Fn(Perm, OuId) -> bool) -> Result<(), String> {
    let root = old.ous.iter().find(|(_, o)| o.parent.is_none()).map(|(id, _)| *id).ok_or("no root OU")?;
    // sites (D-U28) are the OU's; every other module, certificates, nodes and defaults the root's
    use crate::placement::{Module, ModuleKind};
    let others = |p: &Placement| p.modules.iter().filter(|(_, m)| m.kind != ModuleKind::WEB).map(|(k, m)| (k.clone(), m.clone())).collect::<std::collections::BTreeMap<_, _>>();
    if (old.nodes != new.nodes || old.defaults != new.defaults || old.default_secrets != new.default_secrets || old.certs != new.certs || old.web_ou != new.web_ou || others(old) != others(new))
        && !may(ClusterManage, root)
    {
        return Err("changing the nodes, the defaults, certificates or modules needs infrastructure rights at the root".into());
    }
    if !may(ClusterManage, root) {
        let web = |p: &Placement| p.modules.iter().filter(|(_, m)| m.kind == ModuleKind::WEB).map(|(k, _)| k.clone()).collect::<Vec<_>>();
        let mut names = web(old);
        names.extend(web(new));
        names.sort();
        names.dedup();
        for name in names {
            let (o, n) = (old.modules.get(&name), new.modules.get(&name));
            if o == n {
                continue;
            }
            let site = |m: Option<&Module>| m.and_then(|m| m.site.clone());
            if site(o).is_some_and(|s| s.infra()) || site(n).is_some_and(|s| s.infra()) {
                return Err(format!("site {name}: a container, an upstream or routes need infrastructure rights at the root"));
            }
            for s in [site(o), site(n)].into_iter().flatten() {
                if !may(OuManage, s.ou.unwrap_or(root)) {
                    return Err(format!("site {name}: changing it needs ou-manage in its OU"));
                }
            }
        }
    }
    // A new OU under another new one: the rights that count are those at the
    // nearest OU that exists already.
    let anchor = |mut at: OuId| -> Option<OuId> {
        for _ in 0..=new.ous.len() {
            if old.ous.contains_key(&at) {
                return Some(at);
            }
            at = new.ous.get(&at)?.parent?;
        }
        None
    };
    for (id, n) in &new.ous {
        let name = &n.name;
        match old.ous.get(id) {
            None => {
                let parent = n.parent.and_then(anchor).ok_or("an OU that hangs nowhere")?;
                if !may(OuManage, parent) {
                    return Err(format!("making OU {name} there needs ou-manage on its parent"));
                }
                if (n.tier0 || n.second_factor || n.secrets.is_some() || n.session_days.is_some()) && !may(OuSecurity, parent) {
                    return Err(format!("the security settings of OU {name} need ou-security"));
                }
            }
            Some(o) => {
                if (o.name != n.name || o.links != n.links || o.storage_limit != n.storage_limit) && !may(OuManage, *id) {
                    return Err(format!("renaming OU {name}, placing its domains or its storage limit needs ou-manage there"));
                }
                if o.parent != n.parent {
                    let to = n.parent.and_then(anchor).ok_or("an OU that hangs nowhere")?;
                    if !may(OuManage, *id) || !may(OuManage, to) {
                        return Err(format!("moving OU {name} needs ou-manage on it and on where it goes"));
                    }
                }
                if (o.tier0, o.second_factor, &o.secrets, o.session_days) != (n.tier0, n.second_factor, &n.secrets, n.session_days) && !may(OuSecurity, *id) {
                    return Err(format!("the security settings of OU {name} need ou-security there"));
                }
            }
        }
    }
    for (id, o) in &old.ous {
        if !new.ous.contains_key(id) && !o.parent.is_some_and(|p| may(OuManage, p)) {
            return Err(format!("removing OU {} needs ou-manage on its parent", o.name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::{Node, NodeId, Ou, Role as NodeRole};
    use std::collections::BTreeMap;

    const ROOT: OuId = OuId(1);
    const EMEA: OuId = OuId(2);
    const DE: OuId = OuId(3);
    const CZ: OuId = OuId(4);

    fn tree() -> Placement {
        let mut p = Placement::default();
        p.nodes.insert(NodeId("home".into()), Node::new([NodeRole::Voter, NodeRole::Idp, NodeRole::Store], []));
        p.ous.insert(ROOT, Ou::new("organisation", None));
        p.ous.insert(EMEA, Ou::new("EMEA", Some(ROOT)));
        p.ous.insert(DE, Ou::new("DE", Some(EMEA)));
        p.ous.insert(CZ, Ou::new("CZ", Some(EMEA)));
        p
    }

    fn user(id: &str) -> Subject {
        Subject::User { id: id.into() }
    }

    #[test]
    fn roles_reach_down_not_across() {
        let p = tree();
        let grants: BTreeMap<OuId, Vec<Grant>> = [
            (DE, vec![Grant { id: "g1".into(), role: "user-admin".into(), subject: user("anna") }]),
            (EMEA, vec![Grant { id: "g2".into(), role: "auditor".into(), subject: Subject::Group { id: "audit".into() } }]),
            (ROOT, vec![Grant { id: "g3".into(), role: "admin".into(), subject: user("root") }]),
            (CZ, vec![Grant { id: "g4".into(), role: "admin".into(), subject: user("cz") }]),
        ]
        .into();
        let at = |ou, who: &[Subject]| perms_at(&p, ou, who, |o| grants.get(&o).cloned().unwrap_or_default());
        assert!(at(DE, &[user("anna")]).contains(&DirectoryManage));
        assert!(at(CZ, &[user("anna")]).is_empty());
        assert!(at(EMEA, &[user("anna")]).is_empty());
        // Through a group, from above.
        let bob = [user("bob"), Subject::Group { id: "audit".into() }];
        assert!(at(DE, &bob).contains(&LogsRead) && at(CZ, &bob).contains(&LogsRead));
        assert!(!at(DE, &bob).contains(&DirectoryManage));
        // Cluster-wide rights only from the root.
        assert!(at(CZ, &[user("root")]).contains(&ClusterManage));
        assert!(!at(CZ, &[user("cz")]).contains(&ClusterManage));
        assert!(at(CZ, &[user("cz")]).contains(&RolesGrant));
    }

    #[test]
    fn node_control_changes_are_checked_part_by_part() {
        let old = tree();
        // An ou-admin of /EMEA/DE: below DE yes, below CZ no, nodes no.
        let de_admin = |perm: Perm, ou: OuId| perm == OuManage && ou == DE;
        let mut new = old.clone();
        new.ous.insert(OuId(5), Ou::new("Berlin", Some(DE)));
        check_change(&old, &new, de_admin).unwrap();
        let mut new = old.clone();
        new.ous.insert(OuId(5), Ou::new("City", Some(CZ)));
        assert!(check_change(&old, &new, de_admin).is_err());
        let mut new = old.clone();
        new.nodes.insert(NodeId("edge".into()), Node::default());
        assert!(check_change(&old, &new, de_admin).is_err());
        // Security settings need ou-security, not ou-manage.
        let mut new = old.clone();
        new.ous.get_mut(&DE).unwrap().tier0 = true;
        assert!(check_change(&old, &new, de_admin).is_err());
        check_change(&old, &new, |perm, ou| perm == OuSecurity && ou == DE).unwrap();
        // Nothing changed: nothing needed.
        check_change(&old, &old, |_, _| false).unwrap();
        // Several levels at once count from where they hang on.
        let mut new = old.clone();
        new.ous.insert(OuId(5), Ou::new("Berlin", Some(DE)));
        new.ous.insert(OuId(6), Ou::new("Mitte", Some(OuId(5))));
        check_change(&old, &new, de_admin).unwrap();
        new.ous.insert(OuId(7), Ou::new("Brno", Some(CZ)));
        new.ous.insert(OuId(8), Ou::new("Centre", Some(OuId(7))));
        assert!(check_change(&old, &new, de_admin).is_err());
    }
}


#[cfg(test)]
mod site_rights {
    use super::*;
    use crate::{
        placement::{Module, ModuleKind, Node, NodeId, Ou, OuId, Placement, Role},
        sites::{Container, Content, Front, Route, Site},
    };

    /// D-U28: a site's OU decides who changes it; containers, routes and certificates the root.
    #[test]
    fn sites_are_the_ous() {
        let (root, lab) = (OuId(1), OuId(2));
        let mut p = Placement::default();
        p.nodes.insert(NodeId("n".into()), Node::new([Role::EdgeHttp, Role::Worker], []));
        p.ous.insert(root, Ou::new("organisation", None));
        p.ous.insert(lab, Ou::new("lab", Some(root)));
        let site = |ou: Option<OuId>, content: Content| {
            let mut m = Module::new(ModuleKind::WEB, "", NodeId("n".into()));
            m.site = Some(Site { names: vec!["a.example.org".into()], exposed_on: vec![], content, routes: vec![], front: Front::Tls, ou, ..Default::default() });
            m
        };
        let lab_admin = |perm: Perm, ou: OuId| perm == OuManage && ou == lab;
        let root_admin = |_: Perm, _: OuId| true;
        let with = |name: &str, m: Module| {
            let mut q = p.clone();
            q.modules.insert(name.into(), m);
            q
        };
        // a static site in lab: lab's admin may make, change and remove it
        let a = with("a", site(Some(lab), Content::default()));
        assert!(check_change(&p, &a, lab_admin).is_ok());
        let mut b = a.clone();
        b.modules.get_mut("a").unwrap().site.as_mut().unwrap().names.push("b.example.org".into());
        assert!(check_change(&a, &b, lab_admin).is_ok());
        assert!(check_change(&a, &p, lab_admin).is_ok());
        // not into the root, nor the root's sites
        let mut c = a.clone();
        c.modules.get_mut("a").unwrap().site.as_mut().unwrap().ou = None;
        assert!(check_change(&a, &c, lab_admin).is_err());
        assert!(check_change(&p, &with("r", site(None, Content::default())), lab_admin).is_err());
        // a container or routes: the root's
        let cont = Content::Container(Container { image: "x/y:1".into(), port: 80, volumes: vec![], env: Default::default(), folders: vec![], egress: None });
        assert!(check_change(&p, &with("k", site(Some(lab), cont.clone())), lab_admin).is_err());
        assert!(check_change(&p, &with("k", site(Some(lab), cont)), root_admin).is_ok());
        let mut r = a.clone();
        r.modules.get_mut("a").unwrap().site.as_mut().unwrap().routes.push(Route { prefix: "/api/".into(), upstream: "http://127.0.0.1:9".into() });
        assert!(check_change(&a, &r, lab_admin).is_err());
        // certificates: the root's
        let mut cert = p.clone();
        cert.certs.insert("x".into(), crate::acme::CertSpec { names: vec!["a.example.org".into()], issuer: NodeId("n".into()), copies: vec![] });
        assert!(check_change(&p, &cert, lab_admin).is_err());
        assert!(check_change(&p, &cert, root_admin).is_ok());
    }
}
