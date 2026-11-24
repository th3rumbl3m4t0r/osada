//! Second factors on the account page and the user card (§10.2): TOTP set
//! up (the secret shown once, kept when its first code is right), taken off
//! (with a code), reset by an administrator (password-set rights). All of it
//! in the auth log with the session.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{Actor, Node, Ticket, now_ms};
use crate::{
    audit::Verbosity,
    directory::random_hex,
    rbac::Perm,
    totp,
};

impl Node {
    /// Whether `user` has TOTP, as far as this node keeps their secrets.
    pub fn has_totp(&self, user: &crate::directory::User) -> Option<bool> {
        let inner = self.lock();
        inner.creds.get(&user.ou).map(|c| c.state.totp.contains_key(&user.id))
    }

    /// A new TOTP secret for the actor, to show; kept once confirmed.
    pub fn begin_totp(&self, actor: &Actor) -> Result<Value, String> {
        if self.has_totp(&actor.user).is_none() {
            return Err("this node does not keep your secrets".into());
        }
        let s = hex::decode(random_hex(totp::SECRET_LEN)).expect("hex");
        let uri = totp::uri("osada", &actor.user.name, &s);
        let show = json!({ "secret": totp::base32(&s), "uri": uri, "qr": totp::qr_svg(&uri) });
        let t = Ticket { user: actor.user.clone(), expires: now_ms() + 600_000, tries: 0, enrol: Some(s), challenge: None, fido2_enrol: false };
        self.lock().tickets.insert(format!("enrol:{}", actor.sid), t);
        Ok(show)
    }

    pub async fn confirm_totp(self: &Arc<Self>, actor: &Actor, typed: &str) -> Result<(), String> {
        let key = format!("enrol:{}", actor.sid);
        let (secret, last) = {
            let inner = self.lock();
            let t = inner.tickets.get(&key).filter(|t| t.expires > now_ms()).ok_or("set it up again: that ran out")?;
            (t.enrol.clone().unwrap_or_default(), inner.totp_used.get(&actor.user.id).copied())
        };
        let step = totp::check(&secret, typed, now_ms() / 1000, last).ok_or("wrong code")?;
        {
            let mut inner = self.lock();
            inner.tickets.remove(&key);
            inner.totp_used.insert(actor.user.id.clone(), step);
        }
        self.store_totp(&actor.user, Some(&secret), &actor.label()).await?;
        self.log_auth_as(actor, "totp_added", Verbosity::Minimal, json!({ "user": actor.user.id, "name": actor.user.name, "why": "set up by the user" }));
        Ok(())
    }

    /// The actor's TOTP taken off, given a code from it (unless their OU asks
    /// for a second factor: then it stays).
    pub async fn remove_totp(self: &Arc<Self>, actor: &Actor, typed: &str) -> Result<(), String> {
        let u = &actor.user;
        let (secret, last, needed) = {
            let inner = self.lock();
            let secret = inner.creds.get(&u.ou).and_then(|c| c.state.totp.get(&u.id)).and_then(|h| hex::decode(h).ok()).ok_or("you have no TOTP here")?;
            let needed = inner.cluster.state().placement.sign_in_rule(u.ou).unwrap_or_default().second_factor;
            (secret, inner.totp_used.get(&u.id).copied(), needed)
        };
        if let Some(ou) = needed {
            return Err(format!("{} asks for a second factor: your TOTP stays", self.cluster_state().placement.ou_path(ou)));
        }
        let step = totp::check(&secret, typed, now_ms() / 1000, last).ok_or("wrong code")?;
        self.lock().totp_used.insert(u.id.clone(), step);
        self.store_totp(u, None, &actor.label()).await?;
        self.log_auth_as(actor, "totp_removed", Verbosity::Minimal, json!({ "user": u.id, "name": u.name, "why": "taken off by the user" }));
        Ok(())
    }

    /// An administrator takes a user's TOTP off (a lost phone): they set it
    /// up again at their next sign-in where their OU asks for it.
    pub async fn reset_totp(self: &Arc<Self>, actor: &Actor, id: &str) -> Result<(), String> {
        let u = self.find_user(id).ok_or("no such user")?;
        self.require(actor, Perm::PasswordSet, u.ou)?;
        self.store_totp(&u, None, &actor.label()).await?;
        self.log_auth_as(actor, "totp_removed", Verbosity::Minimal, json!({ "user": u.id, "name": u.name, "why": "reset by an administrator" }));
        Ok(())
    }
}
