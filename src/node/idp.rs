//! An idp node's own signing key (D-U45, crate::idpkey): made in the node folder, its public half
//! published in the cluster log by the node itself (and by no other), checked every minute; after
//! an administrator withdraws it, the node keeps it aside and publishes a new one.

use std::{sync::Arc, time::Duration};

use serde_json::{Value, json};

use super::{Actor, Node};
use crate::{cluster::Command, idpkey, placement::Role, rbac::Perm};

impl Node {
    /// This node's signing key, when it is an idp node and has one.
    pub fn idp_key(&self) -> Option<Arc<idpkey::Key>> {
        self.idp_key.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Every idp node's key, as a JWKS has them.
    pub fn idp_jwks(&self) -> Vec<Value> {
        self.cluster_state().keys.values().map(|k| k.jwk()).collect()
    }

    async fn keep_idp_key(self: &Arc<Self>) -> Result<(), String> {
        let state = self.cluster_state();
        if !state.placement.nodes.get(&self.me).is_some_and(|n| n.has(Role::Idp)) {
            return Ok(());
        }
        let Some(dir) = self.data.get().cloned() else { return Ok(()) };
        let mut key = match self.idp_key() {
            Some(k) => k,
            None => {
                let k = Arc::new(tokio::task::spawn_blocking(move || idpkey::Key::load_or_make(&dir)).await.map_err(|e| e.to_string())??);
                *self.idp_key.lock().unwrap_or_else(|e| e.into_inner()) = Some(k.clone());
                k
            }
        };
        if state.withdrawn.contains(&key.public.kid) {
            let old = self.idp_key.lock().unwrap_or_else(|e| e.into_inner()).take().ok_or("no key")?;
            drop(key);
            let owned = Arc::try_unwrap(old).map_err(|_| "the key is in use; again in a minute".to_string())?;
            let kid = owned.public.kid.clone();
            let k = Arc::new(tokio::task::spawn_blocking(move || owned.renew()).await.map_err(|e| e.to_string())??);
            println!("{}: signing key {kid} was withdrawn; a new one: {}", self.me, k.public.kid);
            *self.idp_key.lock().unwrap_or_else(|e| e.into_inner()) = Some(k.clone());
            key = k;
        }
        if state.keys.get(&self.me) != Some(&key.public) {
            self.commit_anywhere(&format!("node:{}", self.me), Command::Key { node: self.me.clone(), key: Some(key.public.clone()) }).await?;
            println!("{}: signing key {} published", self.me, key.public.kid);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) async fn keep_idp_key_now(self: &Arc<Self>) {
        let _ = self.keep_idp_key().await;
    }

    pub(super) fn spawn_idp_key(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            loop {
                let Some(n) = weak.upgrade() else { break };
                if let Err(e) = n.keep_idp_key().await {
                    eprintln!("{}: signing key: {e}", n.me);
                }
                drop(n);
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    /// admin → nodes → a node's card: its signing key withdrawn (it makes a new one).
    pub async fn withdraw_key(self: &Arc<Self>, actor: &Actor, node: &str) -> Result<Value, String> {
        self.require(actor, Perm::ClusterManage, self.root_ou())?;
        let id = self.cluster_state().placement.resolve_node(node).ok_or_else(|| format!("no node {node}"))?;
        let kid = self.cluster_state().keys.get(&id).map(|k| k.kid.clone()).ok_or("that node has no signing key")?;
        self.commit_anywhere(&actor.label(), Command::Key { node: id.clone(), key: None }).await?;
        self.log_admin(actor, "key_withdrawn", json!({ "node": id, "kid": kid }));
        Ok(json!({ "withdrawn": kid }))
    }
}
