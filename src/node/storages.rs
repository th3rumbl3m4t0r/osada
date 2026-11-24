//! Other storage (T2) in node control: added, changed, removed and tested
//! by the cluster's administrators; see crate::storage.

use std::sync::Arc;

use serde_json::{json, Value};

use super::{Actor, Node};
use crate::{
    cluster::Command,
    rbac::Perm,
    storage::{self, Change},
};

impl Node {
    /// Adds or changes other storage `name` (`None` removes it).
    pub async fn set_storage(self: &Arc<Self>, actor: &Actor, name: &str, change: Option<Change>) -> Result<(), String> {
        self.require(actor, Perm::ClusterManage, self.root_ou())?;
        let mut s = self.cluster_state().settings;
        let old = s.storages.get(name).cloned();
        let what = match change {
            None => {
                if s.storages.remove(name).is_none() {
                    return Err(format!("no storage {name}"));
                }
                format!("storage {name}: removed")
            }
            Some(c) => {
                let new = storage::merged(c.storage, old.as_ref(), c.clear_secret, c.reset_host_key);
                new.check(name)?;
                if old.as_ref() == Some(&new) {
                    return Ok(());
                }
                let mut what = format!("storage {name}: {} {}", if old.is_some() { "changed," } else { "added," }, new.place());
                if old.as_ref().map(|o| &o.secret) != Some(&new.secret) {
                    what.push_str(if old.is_some() { "; secret replaced" } else { "; secret set" });
                }
                s.storages.insert(name.to_string(), new);
                what
            }
        };
        self.commit_anywhere(&actor.label(), Command::Settings(s)).await?;
        self.log_admin(actor, "storage_changed", json!({ "storage": name, "changes": [what] }));
        Ok(())
    }

    /// Connects to other storage `name` from this node: SFTP lists its
    /// folder (and pins the server's key the first time), S3 lists a key.
    pub async fn test_storage(self: &Arc<Self>, actor: &Actor, name: &str) -> Result<Value, String> {
        self.require(actor, Perm::ClusterManage, self.root_ou())?;
        let st = self.cluster_state().settings.storages.get(name).cloned().ok_or_else(|| format!("no storage {name}"))?;
        let at = self.cluster_state().placement.node_label(self.me());
        match st.kind() {
            "sftp" => {
                let cfg = st.sftp_cfg(false)?;
                let pinned = cfg.host_key.is_some();
                let (fp, n) = crate::osada1::vfs::sftp::probe(&cfg).await.map_err(|e| format!("{name} from {at}: {e}"))?;
                if !pinned {
                    // pin it, unless the storage changed meanwhile
                    let mut s = self.cluster_state().settings;
                    if let Some(cur) = s.storages.get_mut(name).filter(|c| **c == st) {
                        if let storage::Target::Sftp { host_key, .. } = &mut cur.target {
                            *host_key = Some(fp.clone());
                        }
                        self.commit_anywhere(&actor.label(), Command::Settings(s)).await?;
                        self.log_admin(actor, "storage_changed", json!({ "storage": name, "changes": [format!("storage {name}: host key pinned, {fp}")] }));
                    }
                }
                Ok(json!({ "ok": true, "from": at, "message": format!("{name} answers from {at}: {n} entries in its folder; host key {fp}{}", if pinned { "" } else { " (pinned now)" }), "host_key": fp }))
            }
            _ => {
                let m = storage::test_s3(&st).await.map_err(|e| format!("{name} from {at}: {e}"))?;
                Ok(json!({ "ok": true, "from": at, "message": format!("{name} from {at}: {m}") }))
            }
        }
    }
}
