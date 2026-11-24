//! The directory (§10.1, §10.6) and the credentials (§10.7) as collections.
//! Every OU has a directory collection, which every store node keeps, and a
//! credentials collection, which only the nodes its secrets policy names
//! keep. Their ids follow from the OU, so every node finds them without
//! asking. Entries are operations in JSON; each node folds them into the
//! state it serves from ([`Folded`]), as it does the cluster state from the
//! cluster log.

use std::collections::{BTreeMap, BTreeSet};

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::SaltString,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    log::{CollectionId, Entry, Hash, Log},
    placement::{NodeId, OuId},
    rbac::{self, Grant, Subject},
};

pub const DIRECTORY: &str = "directory";
pub const CREDENTIALS: &str = "credentials";
pub const MIN_PASSWORD: usize = 8;

fn derived(kind: &str, rest: &[u8]) -> CollectionId {
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update([0]);
    h.update(rest);
    CollectionId(h.finalize()[..16].try_into().expect("16 bytes"))
}

/// The directory collection of `ou`.
pub fn directory_of(ou: OuId) -> CollectionId {
    derived("shelf.directory", &ou.0.to_be_bytes())
}

/// The credentials collection of `ou`.
pub fn credentials_of(ou: OuId) -> CollectionId {
    derived("shelf.credentials", &ou.0.to_be_bytes())
}

/// A node's own admin log (§16.3).
pub fn admin_log_of(node: &NodeId) -> CollectionId {
    derived("shelf.admin-log", node.0.as_bytes())
}

/// A node's own authentication log (§16.3).
pub fn auth_log_of(node: &NodeId) -> CollectionId {
    derived("shelf.auth-log", node.0.as_bytes())
}

/// Random bytes, hex.
pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    SystemRandom::new().fill(&mut b).expect("the system's randomness");
    hex::encode(b)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Permanent and never reused: the `sub` of tokens.
    pub id: String,
    /// Unique in the cluster; letters, digits, '.', '_' and '-'.
    pub name: String,
    pub display: String,
    pub ou: OuId,
    /// From before roles: administers everything. New accounts never get it;
    /// roles (rbac.rs) replace it.
    #[serde(default)]
    pub admin: bool,
    pub disabled: bool,
    /// The networks (CIDRs, AS numbers; osada1::edge::normalize's form) the
    /// account may be used from through an edge; empty: from anywhere (D-U15; the owner, 2026-11-17).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outside: String,
    /// The account's mailbox on the mail instance, made and kept as this says by osada (D-U43).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox: Option<Mailbox>,
}

/// A mailbox (D-U43): its address, its size (Stalwart's disk quota; none: no limit), and whether
/// it is turned off (it cannot be signed in to; mail still comes in). A disabled account's mailbox
/// is off whatever this says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mailbox {
    pub address: String,
    /// Its other addresses (D-U44): `name@domain`, or `*@domain` for every address of the domain
    /// no mailbox has (the domain's catch-all). None: not known yet (a mailbox adopted before
    /// there were aliases): the first reconcile takes them from Stalwart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
    /// The mail instance its account is on (Round 18: a Stalwart per OU's mail master). None: not
    /// known yet; the next reconcile sets it: the instance that has its account, else the one for
    /// its OU. Moving it to another instance moves no mail (not built yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

pub const MAX_ALIASES: usize = 100;

impl Mailbox {
    /// Every address it stands for: its own, its aliases, `*@domain` for a catch-all.
    pub fn claims(&self) -> Vec<&str> {
        let mut v = vec![self.address.as_str()];
        v.extend(self.aliases.iter().flatten().map(String::as_str));
        v
    }

    pub fn check(&self) -> Result<(), String> {
        if !valid_address(&self.address) {
            return Err(format!("{:?} is not a mail address (lower case, local@domain.tld)", self.address));
        }
        let aliases = self.aliases.as_deref().unwrap_or_default();
        if aliases.len() > MAX_ALIASES {
            return Err(format!("a mailbox has at most {MAX_ALIASES} other addresses"));
        }
        let mut seen = std::collections::BTreeSet::from([self.address.as_str()]);
        for a in aliases {
            let ok = match a.strip_prefix("*@") {
                Some(d) => d.contains('.') && d == d.to_ascii_lowercase() && crate::placement::valid_host(d),
                None => valid_address(a),
            };
            if !ok {
                return Err(format!("{a:?} is neither a mail address nor *@domain"));
            }
            if !seen.insert(a.as_str()) {
                return Err(format!("{a} is there twice"));
            }
        }
        Ok(())
    }
}

/// A mail address as osada makes them: lower case, `local@domain`, the local part letters, digits
/// and `.`, `_`, `+`, `-`, the domain a host name with a dot.
pub fn valid_address(a: &str) -> bool {
    let Some((local, domain)) = a.split_once('@') else { return false };
    !local.is_empty()
        && local.len() <= 64
        && local.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'+' | b'-'))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && domain.contains('.')
        && domain == domain.to_ascii_lowercase()
        && crate::placement::valid_host(domain)
}

/// Whether `list` is a list of outside networks in its normal form.
pub fn valid_outside(list: &str) -> bool {
    crate::osada1::edge::normalize(list).as_deref() == Ok(list)
}

/// Access to a module instance (A24) for a user or a group; kept in the
/// directory of the subject's OU.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    pub id: String,
    pub module: String,
    pub subject: Subject,
}

/// A group: a function such as "EMEA helpdesk", which roles can be granted
/// to; its members may be in any OU.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub display: String,
    pub ou: OuId,
    pub members: BTreeSet<String>,
}

pub fn valid_user_name(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
        && s.as_bytes()[0].is_ascii_alphanumeric()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DirOp {
    CreateUser { user: User },
    UpdateUser {
        id: String,
        display: Option<String>,
        admin: Option<bool>,
        disabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outside: Option<String>,
    },
    /// Many users of this OU get the same outside networks (mass updates).
    SetOutside { ids: Vec<String>, outside: String },
    CreateGroup { group: Group },
    SetMember { group: String, user: String, member: bool },
    /// A role over this OU and everything below it.
    Grant { grant: Grant },
    Revoke { id: String },
    /// Many users into a group or out of it at once (mass updates).
    SetMembers { group: String, users: Vec<String>, member: bool },
    /// Many users of this OU disabled or enabled at once.
    SetDisabled { ids: Vec<String>, disabled: bool },
    /// Access to module instances for users and groups of this OU; one
    /// that is there already is passed over.
    GrantAccess { list: Vec<Access> },
    /// Access taken back; ids not there are passed over.
    RevokeAccess { ids: Vec<String> },
    /// A user's mailbox set, changed or (None) taken away (D-U43).
    SetMailbox { id: String, mailbox: Option<Mailbox> },
}

impl DirOp {
    pub fn name(&self) -> &'static str {
        match self {
            DirOp::CreateUser { .. } => "create_user",
            DirOp::UpdateUser { .. } => "update_user",
            DirOp::SetOutside { .. } => "set_outside",
            DirOp::CreateGroup { .. } => "create_group",
            DirOp::SetMember { .. } => "set_member",
            DirOp::Grant { .. } => "grant",
            DirOp::Revoke { .. } => "revoke",
            DirOp::SetMembers { .. } => "set_members",
            DirOp::SetDisabled { .. } => "set_disabled",
            DirOp::GrantAccess { .. } => "grant_access",
            DirOp::RevokeAccess { .. } => "revoke_access",
            DirOp::SetMailbox { .. } => "set_mailbox",
        }
    }

    /// The module instances it gives access to.
    pub fn modules(&self) -> Vec<&str> {
        match self {
            DirOp::GrantAccess { list } => list.iter().map(|a| a.module.as_str()).collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectoryState {
    pub users: BTreeMap<String, User>,
    pub groups: BTreeMap<String, Group>,
    /// The roles granted over this OU (and below).
    pub grants: BTreeMap<String, Grant>,
    /// Indexes: user names, group names (to ids), and who is in which
    /// groups (user id to group ids), so that nothing has to go through every
    /// object to find one.
    pub names: BTreeMap<String, String>,
    pub group_names: BTreeMap<String, String>,
    pub member_of: BTreeMap<String, BTreeSet<String>>,
    /// Access to module instances, and by subject (user or group id).
    pub access: BTreeMap<String, Access>,
    pub access_of: BTreeMap<String, BTreeSet<String>>,
}

fn subject_id(s: &Subject) -> &str {
    match s {
        Subject::User { id } | Subject::Group { id } => id,
    }
}

impl DirectoryState {
    /// Applies `op` to the directory of `ou`, or says why not and changes
    /// nothing.
    pub fn apply(&mut self, ou: OuId, op: &DirOp) -> Result<(), String> {
        self.check(ou, op)?;
        self.mutate(op);
        Ok(())
    }

    /// Whether `op` would apply, without changing anything (or copying
    /// anything: an authority checks every write with this).
    pub fn check(&self, ou: OuId, op: &DirOp) -> Result<(), String> {
        let here = |s: &Subject| match s {
            Subject::User { id } => self.users.contains_key(id),
            Subject::Group { id } => self.groups.contains_key(id),
        };
        match op {
            DirOp::CreateUser { user } => {
                if user.ou != ou {
                    return Err("a user of another OU".into());
                }
                if !valid_user_name(&user.name) {
                    return Err("a user name is lower-case letters, digits, '.', '_' and '-'".into());
                }
                if self.users.contains_key(&user.id) || self.names.contains_key(&user.name) {
                    return Err(format!("there is a user {} already", user.name));
                }
            }
            DirOp::UpdateUser { id, outside, .. } => {
                self.users.get(id).ok_or("no such user")?;
                if outside.as_deref().is_some_and(|o| !valid_outside(o)) {
                    return Err("outside networks not in their normal form".into());
                }
            }
            DirOp::SetOutside { outside, .. } => {
                if !valid_outside(outside) {
                    return Err("outside networks not in their normal form".into());
                }
            }
            DirOp::CreateGroup { group } => {
                if group.ou != ou {
                    return Err("a group of another OU".into());
                }
                if !valid_user_name(&group.name) {
                    return Err("a group name is lower-case letters, digits, '.', '_' and '-'".into());
                }
                if self.groups.contains_key(&group.id) || self.group_names.contains_key(&group.name) {
                    return Err(format!("there is a group {} already", group.name));
                }
            }
            DirOp::SetMember { group, .. } | DirOp::SetMembers { group, .. } => {
                self.groups.get(group).ok_or("no such group")?;
            }
            DirOp::SetDisabled { ids, .. } => {
                if let Some(id) = ids.iter().find(|id| !self.users.contains_key(*id)) {
                    return Err(format!("no user {id} in this OU"));
                }
            }
            DirOp::SetMailbox { id, mailbox } => {
                if !self.users.contains_key(id) {
                    return Err(format!("no user {id} in this OU"));
                }
                if let Some(m) = mailbox {
                    m.check()?;
                }
            }
            DirOp::Grant { grant } => {
                if rbac::role(&grant.role).is_none() {
                    return Err(format!("no role {}", grant.role));
                }
                if self.grants.contains_key(&grant.id)
                    || self.grants.values().any(|g| g.role == grant.role && g.subject == grant.subject)
                {
                    return Err("granted already".into());
                }
            }
            DirOp::Revoke { id } => {
                self.grants.get(id).ok_or("no such grant")?;
            }
            DirOp::GrantAccess { list } => {
                for a in list {
                    if !here(&a.subject) {
                        return Err("access for a user or group of another OU".into());
                    }
                    if self.access.contains_key(&a.id) {
                        return Err("an access id that is taken".into());
                    }
                }
            }
            DirOp::RevokeAccess { .. } => {}
        }
        Ok(())
    }

    fn mutate(&mut self, op: &DirOp) {
        match op {
            DirOp::CreateUser { user } => {
                self.names.insert(user.name.clone(), user.id.clone());
                self.users.insert(user.id.clone(), user.clone());
            }
            DirOp::SetOutside { ids, outside } => {
                for id in ids {
                    if let Some(u) = self.users.get_mut(id) {
                        u.outside = outside.clone();
                    }
                }
            }
            DirOp::UpdateUser { id, display, admin, disabled, outside } => {
                if let Some(u) = self.users.get_mut(id) {
                    if let Some(o) = outside {
                        u.outside = o.clone();
                    }
                    if let Some(d) = display {
                        u.display = d.clone();
                    }
                    if let Some(a) = admin {
                        u.admin = *a;
                    }
                    if let Some(d) = disabled {
                        u.disabled = *d;
                    }
                }
            }
            DirOp::CreateGroup { group } => {
                self.group_names.insert(group.name.clone(), group.id.clone());
                for m in &group.members {
                    self.member_of.entry(m.clone()).or_default().insert(group.id.clone());
                }
                self.groups.insert(group.id.clone(), group.clone());
            }
            DirOp::SetMember { group, user, member } => self.set_members(group, std::slice::from_ref(user), *member),
            DirOp::SetMembers { group, users, member } => self.set_members(group, users, *member),
            DirOp::SetDisabled { ids, disabled } => {
                for id in ids {
                    if let Some(u) = self.users.get_mut(id) {
                        u.disabled = *disabled;
                    }
                }
            }
            DirOp::SetMailbox { id, mailbox } => {
                if let Some(u) = self.users.get_mut(id) {
                    u.mailbox = mailbox.clone();
                }
            }
            DirOp::Grant { grant } => {
                self.grants.insert(grant.id.clone(), grant.clone());
            }
            DirOp::Revoke { id } => {
                self.grants.remove(id);
            }
            DirOp::GrantAccess { list } => {
                for a in list {
                    let sid = subject_id(&a.subject).to_string();
                    let dup = self.access_of.get(&sid).is_some_and(|ids| ids.iter().any(|i| self.access.get(i).is_some_and(|x| x.module == a.module)));
                    if !dup {
                        self.access_of.entry(sid).or_default().insert(a.id.clone());
                        self.access.insert(a.id.clone(), a.clone());
                    }
                }
            }
            DirOp::RevokeAccess { ids } => {
                for id in ids {
                    if let Some(a) = self.access.remove(id) {
                        let sid = subject_id(&a.subject);
                        if let Some(set) = self.access_of.get_mut(sid) {
                            set.remove(id);
                            if set.is_empty() {
                                self.access_of.remove(sid);
                            }
                        }
                    }
                }
            }
        }
    }

    fn set_members(&mut self, group: &str, users: &[String], member: bool) {
        let Some(g) = self.groups.get_mut(group) else { return };
        for user in users {
            if member {
                g.members.insert(user.clone());
                self.member_of.entry(user.clone()).or_default().insert(group.to_string());
            } else {
                g.members.remove(user);
                if let Some(set) = self.member_of.get_mut(user) {
                    set.remove(group);
                    if set.is_empty() {
                        self.member_of.remove(user);
                    }
                }
            }
        }
    }

    pub fn by_name(&self, name: &str) -> Option<&User> {
        self.names.get(name).and_then(|id| self.users.get(id))
    }

    /// The module instances `subject` has access to here, with the access ids.
    pub fn access_for(&self, subject: &str) -> Vec<&Access> {
        self.access_of.get(subject).map(|ids| ids.iter().filter_map(|i| self.access.get(i)).collect()).unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CredOp {
    /// The argon2id hash of the user's password; never the password.
    SetPassword { user: String, hash: String },
    /// The user's TOTP secret (hex, 20 bytes); shown once, when set up.
    SetTotp { user: String, secret: String },
    RemoveTotp { user: String },
    /// A FIDO2 key of the user's (webauthn.rs): its public key, never more.
    AddFido2 { user: String, cred: crate::webauthn::Credential },
    RemoveFido2 { user: String, id: String },
    /// Every FIDO2 key of the user's (an administrator's reset).
    ClearFido2 { user: String },
    /// Tokens of the user's sign-ins before `before` (s since 1970) no longer count (D-U45).
    Revoke { user: String, before: u64 },
}

impl CredOp {
    pub fn name(&self) -> &'static str {
        match self {
            CredOp::SetPassword { .. } => "set_password",
            CredOp::SetTotp { .. } => "set_totp",
            CredOp::RemoveTotp { .. } => "remove_totp",
            CredOp::AddFido2 { .. } => "add_fido2",
            CredOp::RemoveFido2 { .. } => "remove_fido2",
            CredOp::ClearFido2 { .. } => "clear_fido2",
            CredOp::Revoke { .. } => "revoke",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialState {
    pub passwords: BTreeMap<String, String>,
    pub totp: BTreeMap<String, String>,
    pub fido2: BTreeMap<String, Vec<crate::webauthn::Credential>>,
    /// Sign-ins before this (s) no longer give tokens, by user (D-U45).
    pub revoked: BTreeMap<String, u64>,
}

/// The most FIDO2 keys one account keeps.
pub const MAX_FIDO2: usize = 16;

impl CredentialState {
    pub fn apply(&mut self, op: &CredOp) -> Result<(), String> {
        match op {
            CredOp::Revoke { user, before } => {
                let r = self.revoked.entry(user.clone()).or_insert(0);
                *r = (*r).max(*before);
            }
            CredOp::SetPassword { user, hash } => {
                if !hash.starts_with("$argon2id$") {
                    return Err("not an argon2id hash".into());
                }
                self.passwords.insert(user.clone(), hash.clone());
            }
            CredOp::SetTotp { user, secret } => {
                if hex::decode(secret).map(|s| s.len()) != Ok(crate::totp::SECRET_LEN) {
                    return Err("not a TOTP secret".into());
                }
                self.totp.insert(user.clone(), secret.clone());
            }
            CredOp::RemoveTotp { user } => {
                self.totp.remove(user);
            }
            CredOp::AddFido2 { user, cred } => {
                use base64::Engine;
                let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&cred.key).map_err(|_| "not a key")?;
                let fits = match cred.alg {
                    crate::webauthn::ES256 => key.len() == 65,
                    crate::webauthn::EDDSA => key.len() == 32,
                    _ => false,
                };
                if !fits || cred.id.is_empty() || cred.id.len() > 1400 {
                    return Err("not a FIDO2 key".into());
                }
                if self.fido2.values().flatten().any(|c| c.id == cred.id) {
                    return Err("that key is registered already".into());
                }
                let list = self.fido2.entry(user.clone()).or_default();
                if list.len() >= MAX_FIDO2 {
                    return Err(format!("at most {MAX_FIDO2} keys"));
                }
                list.push(cred.clone());
            }
            CredOp::RemoveFido2 { user, id } => {
                if let Some(list) = self.fido2.get_mut(user) {
                    list.retain(|c| c.id != *id);
                    if list.is_empty() {
                        self.fido2.remove(user);
                    }
                }
            }
            CredOp::ClearFido2 { user } => {
                self.fido2.remove(user);
            }
        }
        Ok(())
    }
}

/// The argon2id hash of `password` (slow on purpose: off the async threads).
pub fn hash_password(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    SystemRandom::new().fill(&mut salt).map_err(|_| "no randomness")?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    Ok(Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|e| e.to_string())?.to_string())
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// A state folded from a collection's log. When the log was cut below what
/// was folded (§7.4), it is folded again from the start.
pub struct Folded<S> {
    pub state: S,
    applied: (u64, Hash),
}

impl<S: Default> Folded<S> {
    pub fn new(id: CollectionId) -> Folded<S> {
        Folded { state: S::default(), applied: (0, id.genesis()) }
    }

    pub fn applied(&self) -> u64 {
        self.applied.0
    }

    /// Folds in what `log` holds past what was folded; whether anything
    /// changed.
    pub fn catch_up(
        &mut self,
        log: &Log,
        mut apply: impl FnMut(&mut S, &Entry) -> Result<(), String>,
    ) -> Result<bool, String> {
        let mut changed = false;
        if log.hash_at(self.applied.0).map_err(|e| e.to_string())? != Some(self.applied.1) {
            self.state = S::default();
            self.applied = (0, log.id().genesis());
            changed = true;
        }
        loop {
            let entries = log.read(self.applied.0, 1024).map_err(|e| e.to_string())?;
            if entries.is_empty() {
                return Ok(changed);
            }
            for e in entries {
                apply(&mut self.state, &e).map_err(|m| format!("{}, entry {}: {m}", log.id(), e.seq))?;
                self.applied = (e.seq, e.hash());
                changed = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        kv::mem::MemKv,
        log::Draft,
    };
    use std::sync::Arc;

    fn user(name: &str, ou: u64) -> User {
        User { id: random_hex(16), name: name.into(), display: name.into(), ou: OuId(ou), admin: false, disabled: false, outside: String::new(), mailbox: None }
    }

    #[test]
    fn ids_follow_from_the_ou() {
        assert_eq!(directory_of(OuId(1)), directory_of(OuId(1)));
        assert_ne!(directory_of(OuId(1)), directory_of(OuId(2)));
        assert_ne!(directory_of(OuId(1)), credentials_of(OuId(1)));
        assert_ne!(auth_log_of(&NodeId("a".into())), auth_log_of(&NodeId("b".into())));
    }

    #[test]
    fn directory_rules() {
        let mut d = DirectoryState::default();
        let anna = user("anna", 1);
        d.apply(OuId(1), &DirOp::CreateUser { user: anna.clone() }).unwrap();
        assert!(d.apply(OuId(1), &DirOp::CreateUser { user: user("anna", 1) }).is_err());
        assert!(d.apply(OuId(1), &DirOp::CreateUser { user: user("bob", 2) }).is_err());
        assert!(d.apply(OuId(1), &DirOp::CreateUser { user: user("Bob", 1) }).is_err());
        let op = DirOp::UpdateUser { id: anna.id.clone(), display: None, admin: Some(true), disabled: None, outside: None };
        d.apply(OuId(1), &op).unwrap();
        assert!(d.by_name("anna").unwrap().admin);
    }

    #[test]
    fn passwords() {
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password(&h, "correct horse"));
        assert!(!verify_password(&h, "wrong horse"));
        assert!(!verify_password("garbage", "x"));
        let mut c = CredentialState::default();
        assert!(c.apply(&CredOp::SetPassword { user: "u".into(), hash: "plain".into() }).is_err());
        c.apply(&CredOp::SetPassword { user: "u".into(), hash: h }).unwrap();
    }

    #[test]
    fn folded_follows_the_log_and_its_cuts() {
        let id = directory_of(OuId(1));
        let log = Log::open(Arc::new(MemKv::new()), id).unwrap();
        let mut f: Folded<DirectoryState> = Folded::new(id);
        let fold = |s: &mut DirectoryState, e: &Entry| s.apply(OuId(1), &serde_json::from_slice(&e.payload).map_err(|e| e.to_string())?);
        for name in ["anna", "bob"] {
            let op = DirOp::CreateUser { user: user(name, 1) };
            log.append(1, 1, Draft { actor: "t".into(), module: DIRECTORY.into(), op: op.name().into(), payload: serde_json::to_vec(&op).unwrap() }).unwrap();
        }
        assert!(f.catch_up(&log, fold).unwrap());
        assert_eq!(f.state.users.len(), 2);
        assert!(!f.catch_up(&log, fold).unwrap());
        log.fence(2).unwrap();
        log.cut_after(1).unwrap();
        assert!(f.catch_up(&log, fold).unwrap());
        assert_eq!(f.state.users.len(), 1);
    }

    #[test]
    fn groups_and_grants() {
        use crate::rbac::Subject;
        let mut d = DirectoryState::default();
        let g = Group { id: "g1".into(), name: "helpdesk".into(), display: "Helpdesk".into(), ou: OuId(1), members: BTreeSet::new() };
        d.apply(OuId(1), &DirOp::CreateGroup { group: g.clone() }).unwrap();
        assert!(d.apply(OuId(1), &DirOp::CreateGroup { group: Group { id: "g2".into(), ..g.clone() } }).is_err());
        d.apply(OuId(1), &DirOp::SetMember { group: "g1".into(), user: "u1".into(), member: true }).unwrap();
        assert!(d.groups["g1"].members.contains("u1"));
        let grant = Grant { id: "r1".into(), role: "auditor".into(), subject: Subject::Group { id: "g1".into() } };
        d.apply(OuId(1), &DirOp::Grant { grant: grant.clone() }).unwrap();
        assert!(d.apply(OuId(1), &DirOp::Grant { grant: Grant { id: "r2".into(), ..grant.clone() } }).is_err());
        assert!(d.apply(OuId(1), &DirOp::Grant { grant: Grant { id: "r3".into(), role: "god".into(), ..grant } }).is_err());
        d.apply(OuId(1), &DirOp::Revoke { id: "r1".into() }).unwrap();
        assert!(d.grants.is_empty());
        // An account made before roles still reads.
        let old: User = serde_json::from_str(r#"{"id":"x","name":"x","display":"","ou":1,"disabled":false}"#).unwrap();
        assert!(!old.admin);
    }

    #[test]
    fn access_and_mass_updates() {
        let mut d = DirectoryState::default();
        let (anna, bob) = (user("anna", 1), user("bob", 1));
        for u in [&anna, &bob] {
            d.apply(OuId(1), &DirOp::CreateUser { user: u.clone() }).unwrap();
        }
        let g = Group { id: "g1".into(), name: "staff".into(), display: String::new(), ou: OuId(1), members: BTreeSet::new() };
        d.apply(OuId(1), &DirOp::CreateGroup { group: g }).unwrap();
        let ids = vec![anna.id.clone(), bob.id.clone()];
        d.apply(OuId(1), &DirOp::SetMembers { group: "g1".into(), users: ids.clone(), member: true }).unwrap();
        assert_eq!(d.groups["g1"].members.len(), 2);
        assert!(d.member_of[&bob.id].contains("g1"));
        d.apply(OuId(1), &DirOp::SetDisabled { ids: ids.clone(), disabled: true }).unwrap();
        assert!(d.users.values().all(|u| u.disabled));
        assert!(d.apply(OuId(1), &DirOp::SetDisabled { ids: vec!["elsewhere".into()], disabled: true }).is_err());

        let acc = |id: &str, module: &str, subject: Subject| Access { id: id.into(), module: module.into(), subject };
        let list = vec![acc("a1", "mail-eu", Subject::User { id: anna.id.clone() }), acc("a2", "wiki-main", Subject::Group { id: "g1".into() })];
        d.apply(OuId(1), &DirOp::GrantAccess { list }).unwrap();
        // The same module again for anna is passed over; another OU's user is refused.
        d.apply(OuId(1), &DirOp::GrantAccess { list: vec![acc("a3", "mail-eu", Subject::User { id: anna.id.clone() })] }).unwrap();
        assert_eq!(d.access_for(&anna.id).len(), 1);
        assert!(d.apply(OuId(1), &DirOp::GrantAccess { list: vec![acc("a4", "mail-eu", Subject::User { id: "x".into() })] }).is_err());
        d.apply(OuId(1), &DirOp::RevokeAccess { ids: vec!["a1".into(), "nope".into()] }).unwrap();
        assert!(d.access_for(&anna.id).is_empty());
        assert_eq!(d.access_for("g1")[0].module, "wiki-main");
        // check() changes nothing.
        let before = d.clone();
        d.check(OuId(1), &DirOp::SetMembers { group: "g1".into(), users: vec!["z".into()], member: true }).unwrap();
        assert_eq!(d, before);
    }
}
