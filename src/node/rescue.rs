//! A way back in for whoever runs the server: `osada rescue <user>` writes a
//! request next to the node's data (0600; writing there needs the server's
//! account, which is the authorization), with the argon2 hash of a new
//! password. The node picks it up within seconds when it is the authority of
//! that user's credentials: sets the password, enables the account, with
//! `--admin` grants the role admin over /, and with `--create` makes the
//! account first. Every rescue is in the admin and auth logs; the request
//! file is removed, or renamed `.failed` with the reason.

use std::{path::PathBuf, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Actor, Node};
use crate::{
    audit::Verbosity,
    directory::{self, CREDENTIALS, CredOp, DIRECTORY, DirOp, User, random_hex},
    rbac::Subject,
};

/// What `osada rescue` asks for.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rescue {
    pub name: String,
    /// The argon2id hash of the new password.
    pub hash: String,
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub create: bool,
}

impl Node {
    /// Looks for a rescue request at `file` every two seconds.
    pub fn watch_rescue(self: &Arc<Self>, file: PathBuf) {
        let node = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                let Ok(text) = std::fs::read_to_string(&file) else { continue };
                let res = match serde_json::from_str::<Rescue>(&text) {
                    Ok(r) => node.rescue(&r).await,
                    Err(e) => Err(format!("not a rescue request: {e}")),
                };
                match res {
                    Ok(None) => continue, // not the authority here (yet)
                    Ok(Some(msg)) => {
                        println!("{}: rescue: {msg}", node.me);
                        let _ = std::fs::remove_file(&file);
                    }
                    Err(e) => {
                        eprintln!("{}: rescue: {e}", node.me);
                        let failed = file.with_extension("failed");
                        let _ = std::fs::write(&failed, format!("{e}\n"));
                        let _ = std::fs::remove_file(&file);
                    }
                }
            }
        });
    }

    /// Does what `r` asks, when this node holds the user's credentials;
    /// None when it does not (another node may).
    async fn rescue(self: &Arc<Self>, r: &Rescue) -> Result<Option<String>, String> {
        if !r.hash.starts_with("$argon2id$") {
            return Err("the hash is not argon2id".into());
        }
        let actor = Actor::node(&self.me);
        let found = self.lock().dirs.values().find_map(|d| d.state.by_name(&r.name).cloned());
        let (user, made) = match found {
            Some(u) => (u, false),
            None if r.create => {
                if !directory::valid_user_name(&r.name) {
                    return Err(format!("{:?} is not a user name", r.name));
                }
                let Some(root) = self.root_ready() else { return Ok(None) };
                let u = User { id: random_hex(16), name: r.name.clone(), display: String::new(), ou: root, admin: false, disabled: false, outside: String::new(), mailbox: None };
                let op = serde_json::to_value(DirOp::CreateUser { user: u.clone() }).map_err(|e| e.to_string())?;
                self.submit(directory::directory_of(root), DIRECTORY, op, &actor.label()).await?;
                self.log_admin(&actor, "user_created", json!({ "user": u.id, "name": u.name, "ou": self.ou_path(root), "why": "osada rescue --create" }));
                (u, true)
            }
            None => return Err(format!("no user {} (--create makes one)", r.name)),
        };
        let creds = directory::credentials_of(user.ou);
        if !self.lock().replicas.get(&creds).is_some_and(|c| c.is_authority()) {
            return Ok(None);
        }
        let op = serde_json::to_value(CredOp::SetPassword { user: user.id.clone(), hash: r.hash.clone() }).map_err(|e| e.to_string())?;
        self.submit(creds, CREDENTIALS, op, &actor.label()).await?;
        self.log_auth_as(&actor, "password_changed", Verbosity::Minimal, json!({ "user": user.id, "name": user.name, "why": "osada rescue" }));
        if user.disabled {
            let op = DirOp::UpdateUser { id: user.id.clone(), display: None, admin: None, disabled: Some(false), outside: None };
            self.submit(directory::directory_of(user.ou), DIRECTORY, serde_json::to_value(op).map_err(|e| e.to_string())?, &actor.label()).await?;
        }
        if r.admin {
            let root = self.root_ou();
            if !self.runs_cluster(&user) {
                self.grant_unchecked(&actor, root, "admin", Subject::User { id: user.id.clone() }).await?;
            }
        }
        self.log_admin(&actor, "account_rescued", json!({ "user": user.id, "name": user.name, "created": made, "enabled": user.disabled, "admin": r.admin }));
        Ok(Some(format!("{}: new password{}{}{}", user.name, if made { ", new account" } else { "" }, if user.disabled { ", enabled" } else { "" }, if r.admin { ", admin over /" } else { "" })))
    }
}
