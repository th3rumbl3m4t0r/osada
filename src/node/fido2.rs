//! FIDO2 keys (webauthn.rs) on the account page and at sign-in: registered
//! while signed in, or the first one at the first sign-in of a tier-0
//! account (as TOTP is set up where an OU asks for it); taken off by their
//! owner (not the last one in tier 0) or all of them by an administrator
//! (password-set rights), after which the owner registers one at their next
//! sign-in. Everything in the auth log with the session.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Actor, Node, Session, Ticket, now_ms};
use crate::{
    audit::Verbosity,
    directory::{self, CREDENTIALS, CredOp, User, random_hex},
    rbac::Perm,
    signin::Factor,
    webauthn::{self, Credential, Rp},
};

/// What the browser hands over: for a sign-in the key's id, the
/// authenticator data and the signature; for a registration the
/// attestation object. All base64url.
#[derive(Debug, Default, Deserialize)]
pub struct Fido2Answer {
    #[serde(default)]
    pub id: String,
    pub client: String,
    #[serde(default)]
    pub authenticator: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub attestation: String,
    /// A name for a new key.
    #[serde(default)]
    pub name: String,
}

impl Node {
    /// The user's keys, as far as this node keeps their secrets.
    pub fn fido2_keys(&self, user: &User) -> Option<Vec<Credential>> {
        let inner = self.lock();
        inner.creds.get(&user.ou).map(|c| c.state.fido2.get(&user.id).cloned().unwrap_or_default())
    }

    async fn store_cred(self: &Arc<Self>, user: &User, op: CredOp, actor: &str) -> Result<(), String> {
        let op = serde_json::to_value(op).map_err(|e| e.to_string())?;
        self.submit(directory::credentials_of(user.ou), CREDENTIALS, op, actor).await.map(|_| ())
    }

    /// A registration for the actor: what `navigator.credentials.create` needs.
    pub fn begin_fido2(&self, actor: &Actor) -> Result<Value, String> {
        let keys = self.fido2_keys(&actor.user).ok_or("this node does not keep your secrets")?;
        let challenge = hex::decode(random_hex(32)).expect("hex");
        let show = json!({
            "challenge": webauthn::b64(&challenge),
            "user": { "id": actor.user.id, "name": actor.user.name },
            "exclude": keys.iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
        });
        let t = Ticket { user: actor.user.clone(), expires: now_ms() + 300_000, tries: 0, enrol: None, challenge: Some(challenge), fido2_enrol: true };
        self.lock().tickets.insert(format!("fido2:{}", actor.sid), t);
        Ok(show)
    }

    pub async fn confirm_fido2(self: &Arc<Self>, actor: &Actor, rp: &Rp, a: &Fido2Answer) -> Result<(), String> {
        let key = format!("fido2:{}", actor.sid);
        let challenge = {
            let mut inner = self.lock();
            let t = inner.tickets.remove(&key).filter(|t| t.expires > now_ms()).ok_or("start again: that ran out")?;
            t.challenge.unwrap_or_default()
        };
        let name = if a.name.trim().is_empty() { "security key".to_string() } else { a.name.trim().chars().take(60).collect() };
        let cred = webauthn::register(&rp.id, &rp.origins, &challenge, &webauthn::unb64(&a.client)?, &webauthn::unb64(&a.attestation)?, &name, now_ms())?;
        let id = cred.id.clone();
        self.store_cred(&actor.user, CredOp::AddFido2 { user: actor.user.id.clone(), cred }, &actor.label()).await?;
        self.log_auth_as(actor, "fido2_added", Verbosity::Minimal, json!({ "user": actor.user.id, "name": actor.user.name, "key": id, "key_name": name, "why": "registered by the user" }));
        Ok(())
    }

    /// One of the actor's keys taken off; in tier 0 not the last one.
    pub async fn remove_fido2(self: &Arc<Self>, actor: &Actor, id: &str) -> Result<(), String> {
        let u = &actor.user;
        let keys = self.fido2_keys(u).ok_or("this node does not keep your secrets")?;
        if !keys.iter().any(|k| k.id == id) {
            return Err("no such key of yours".into());
        }
        let tier0 = self.cluster_state().placement.sign_in_rule(u.ou).unwrap_or_default().tier0;
        if tier0.is_some() && keys.len() == 1 {
            return Err("your account is in tier 0: it keeps at least one key".into());
        }
        self.store_cred(u, CredOp::RemoveFido2 { user: u.id.clone(), id: id.into() }, &actor.label()).await?;
        self.log_auth_as(actor, "fido2_removed", Verbosity::Minimal, json!({ "user": u.id, "name": u.name, "key": id, "why": "taken off by the user" }));
        Ok(())
    }

    /// An administrator takes every key of a user's off (lost keys): they
    /// register one at their next sign-in where FIDO2 is asked for.
    pub async fn reset_fido2(self: &Arc<Self>, actor: &Actor, id: &str) -> Result<(), String> {
        let u = self.find_user(id).ok_or("no such user")?;
        self.require(actor, Perm::PasswordSet, u.ou)?;
        self.store_cred(&u, CredOp::ClearFido2 { user: u.id.clone() }, &actor.label()).await?;
        self.log_auth_as(actor, "fido2_removed", Verbosity::Minimal, json!({ "user": u.id, "name": u.name, "key": "all", "why": "reset by an administrator" }));
        Ok(())
    }

    /// The second step with a key: its signature over the ticket's challenge,
    /// or the first key's registration (`enrol_fido2`).
    pub async fn sign_in_fido2(self: &Arc<Self>, ticket: &str, rp: &Rp, a: &Fido2Answer, addr: &str) -> Result<(String, Session), String> {
        let t = {
            let mut inner = self.lock();
            let now = now_ms();
            inner.tickets.retain(|_, t| t.expires > now);
            // one try per ticket: a new challenge for the next
            inner.tickets.remove(ticket).filter(|t| t.challenge.is_some()).ok_or("this sign-in ran out; start again")?
        };
        let challenge = t.challenge.clone().unwrap_or_default();
        let u = t.user.clone();
        let failed = |why: String| -> String {
            let ev = json!({ "event": "sign_in_failed", "name": u.name, "user": u.id, "why": format!("FIDO2: {why}"), "addr": addr });
            self.log_event(&mut self.lock(), crate::audit::LogKind::Auth, Verbosity::Minimal, ev);
            format!("{why}; start again")
        };
        let client = webauthn::unb64(&a.client)?;
        let key_id = if t.fido2_enrol {
            let cred = webauthn::register(&rp.id, &rp.origins, &challenge, &client, &webauthn::unb64(&a.attestation)?, "security key", now_ms()).map_err(&failed)?;
            let id = cred.id.clone();
            self.store_cred(&u, CredOp::AddFido2 { user: u.id.clone(), cred }, &format!("user:{}", u.name)).await?;
            id
        } else {
            let cred = self.fido2_keys(&u).unwrap_or_default().into_iter().find(|k| k.id == a.id).ok_or_else(|| failed("not a key of this account".into()))?;
            webauthn::verify(&cred, &rp.id, &rp.origins, &challenge, &client, &webauthn::unb64(&a.authenticator)?, &webauthn::unb64(&a.signature)?, false)
                .map_err(&failed)?;
            cred.id
        };
        // what the OU asks for is met: password and key
        let rule = self.cluster_state().placement.sign_in_rule(u.ou).unwrap_or_default();
        rule.check(&[Factor::Password, Factor::Fido2 { verified: false }]).map_err(|m| failed(format!("{m:?}")))?;
        let (token, session) = self.open_session(&u, addr, &["pwd", "fido2"], None);
        if t.fido2_enrol {
            let actor = Actor { user: u.clone(), sid: session.sid.clone(), addr: addr.into() };
            self.log_auth_as(&actor, "fido2_added", Verbosity::Minimal, json!({ "user": u.id, "name": u.name, "key": key_id, "why": "registered at sign-in" }));
        }
        Ok((token, session))
    }
}
