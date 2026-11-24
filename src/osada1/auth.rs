use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts, HeaderMap, StatusCode},
};
use rusqlite::params;

use crate::osada1::{
    crypto,
    db::now,
    edge::{self, Remote},
    error::{AppError, Res},
    App,
};

/// The session cookie's name: `osada`, or OSADA_COOKIE (a test instance on
/// the same address as the real one needs its own: cookies ignore ports).
pub fn cookie_name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::env::var("OSADA_COOKIE")
            .ok()
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            .unwrap_or_else(|| "shelf".into())
    })
}
pub const SESSION_SECS: i64 = 30 * 86400;

#[derive(Clone, Debug)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub admin: bool,
    /// networks and AS numbers the account may be used from through the edge (`edge::normalize`)
    pub cidrs: String,
}

/// A logged-in user who is also an admin.
pub struct Admin(pub User);

pub fn hash_password(pw: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(pw.as_bytes(), &salt).expect("argon2").to_string()
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

pub fn check_password_rules(pw: &str) -> Res<()> {
    if pw.chars().count() < 8 {
        return Err(AppError::bad("password needs at least 8 characters"));
    }
    Ok(())
}

pub fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == cookie_name() && !v.is_empty()).then(|| v.to_string())
        })
}

/// Who a osada 0.2 session stands for (docs/PLAN-0.2.0.md, D-U2).
#[derive(Clone, Debug)]
pub struct Person {
    /// The 0.2 user id: the `sub` of this module's users table.
    pub sub: String,
    pub name: String,
    /// Cluster rights: this module's admin pages.
    pub admin: bool,
    /// Holds some role: the admin portal (/admin) is for them.
    pub staff: bool,
    /// The networks the account may be used from through the edge (users.remote_cidrs follows).
    pub outside: String,
    pub disabled: bool,
    /// When the session started (seconds since 1970).
    pub started: i64,
}

/// A person of osada 0.2's directory (Round 21).
#[derive(Clone, Debug, Default)]
pub struct DirPerson {
    pub sub: String,
    pub name: String,
    pub display: String,
    pub mailbox: Option<String>,
    pub aliases: Vec<String>,
    pub disabled: bool,
}

/// osada 0.2's identity, which this module's sessions come from when it runs
/// inside 0.2. Without one (`App::bridge` None), it keeps its own sessions,
/// as osada 0.1 did.
pub trait Bridge: Send + Sync {
    /// The person a session token stands for.
    fn person(&self, token: &str) -> Option<Person>;
    /// The same, by the sha256 of the token (what a apptab pass keeps).
    fn person_by_hash(&self, hash: &str) -> Option<Person>;
    /// A sign-in with a password alone: the session's token, or what to tell.
    fn sign_in(&self, name: String, password: String, addr: String) -> futures_util::future::BoxFuture<'static, Result<String, String>>;
    fn sign_out(&self, token: &str, addr: &str);
    fn change_password(&self, token: String, old: String, new: String) -> futures_util::future::BoxFuture<'static, Result<(), String>>;
    /// Everyone in the directory, to keep this module's users table in step.
    fn people(&self) -> Vec<Person>;
    /// Whether the person may use the module instance `module` (a proxy).
    fn may_use(&self, sub: &str, module: &str) -> bool;
    /// The proxy modules of the node control.
    fn proxies(&self) -> Vec<crate::osada1::apptab::ProxyDef>;
    /// The mail instance whose access decides who has a mailbox (D-U16):
    /// once it is made and access was brought over; None before (then
    /// every account has one, as in osada 0.1).
    fn mail_instance(&self) -> Option<String> {
        None
    }
    /// Whether `sub` may use the webmail (D-U16): access to the mail instance `module`, or
    /// (inside osada 0.2, Round 18) to any mail instance.
    fn may_use_mail(&self, sub: &str, module: &str) -> bool {
        self.may_use(sub, module)
    }
    /// The office suite's page, `/web/<site>/`, when a site is one (Round 18).
    fn office(&self) -> Option<String> {
        None
    }
    /// Another node as a live backup's target, over the link (Round 14).
    fn live_node_target(&self, _node: &str, _backup: &str) -> Option<std::sync::Arc<dyn crate::osada1::vfs::Backend>> {
        None
    }
    /// The live backups of the files (Round 14).
    fn live_backups(&self) -> Vec<crate::audit::LiveBackup> {
        Vec::new()
    }
    /// The idp nodes' signing keys (D-U45), for the JWKS beside this module's own.
    fn jwks(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }
    /// The address of the person's mailbox, the one osada keeps (D-U43).
    fn mailbox_of(&self, _sub: &str) -> Option<String> {
        None
    }
    /// Everyone in osada 0.2's directory (Round 21, the share window's search): name, display
    /// name, mailbox address and its other addresses, by `sub`.
    fn directory(&self) -> Vec<DirPerson> {
        Vec::new()
    }
    /// The domain of the accounts' UPNs, `<name>@<domain>` (Round 22).
    fn upn_domain(&self) -> Option<String> {
        None
    }
    /// The mail instance's public name (Round 21: osada sends a person's mail through it).
    fn mail_host(&self) -> Option<String> {
        None
    }
    /// The groups a person is in: (id, name) (D-U29).
    fn groups_of(&self, _sub: &str) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The name of the node serving the page (T14).
    fn node_name(&self) -> Option<String> {
        None
    }

    /// This node's id (the files' usage says where it is).
    fn node_id(&self) -> Option<String> {
        None
    }

    /// Other storage `name` (T2): the nightly backup's target when it names one.
    /// The storage limit of the OU of the account with subject `sub`, for an account without
    /// its own (D-U30).
    fn default_limit(&self, _sub: &str) -> Option<u64> {
        None
    }

    /// The cluster's nodes (id, name) for the nightly backup (Round 17).
    fn backup_nodes(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// A node's part of the nightly backup (`node`, `mail`): made ready and listed; `None`: it
    /// has no such part.
    fn backup_list(&self, _node: String, _part: String) -> futures_util::future::BoxFuture<'static, Result<Option<Vec<crate::nodebackup::Entry>>, String>> {
        Box::pin(async { Err("no cluster".to_string()) })
    }
    /// One file of a part a node listed.
    fn backup_get(&self, _node: String, _part: String, _path: String) -> futures_util::future::BoxFuture<'static, Result<crate::osada1::vfs::ByteStream, String>> {
        Box::pin(async { Err("no cluster".to_string()) })
    }
    /// Round 17, once: the nightly backup into the cluster's settings (its own target, if given,
    /// as other storage); its name there.
    fn adopt_nightly(&self, _storage: Option<crate::storage::Storage>, _entry: crate::audit::LiveBackup) -> futures_util::future::BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Err("no cluster".to_string()) })
    }
    fn storage(&self, _name: &str) -> Option<crate::storage::Storage> {
        None
    }
}

/// The row of this module's users table for a person of 0.2: found by its
/// sub, else the row of that name from before subs, else a new one. Its name,
/// admin flag and disabled flag follow 0.2.
pub fn ensure_user(app: &App, p: &Person) -> Res<User> {
    use rusqlite::OptionalExtension;
    app.db.with(|c| {
        let found: Option<(i64, String, bool, bool, String)> = c
            .query_row("SELECT id, username, is_admin, disabled, remote_cidrs FROM users WHERE sub = ?1", [&p.sub], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .optional()?;
        let id: i64 = match found {
            Some((id, name, admin, disabled, outside)) => {
                if name != p.name || admin != p.admin || disabled != p.disabled || outside != p.outside {
                    c.execute(
                        "UPDATE users SET username = ?1, is_admin = ?2, disabled = ?3, remote_cidrs = ?4 WHERE id = ?5",
                        params![p.name, p.admin, p.disabled, p.outside, id],
                    )?;
                }
                id
            }
            None => {
                let n = c.execute(
                    "UPDATE users SET sub = ?1, is_admin = ?2, disabled = ?3, remote_cidrs = ?4 WHERE username = ?5 AND (sub IS NULL OR sub = '')",
                    params![p.sub, p.admin, p.disabled, p.outside, p.name],
                )?;
                if n == 0 {
                    c.execute(
                        "INSERT INTO users (username, pw_hash, is_admin, disabled, created, sub, remote_cidrs) VALUES (?1, '!', ?2, ?3, ?4, ?5, ?6)",
                        params![p.name, p.admin, p.disabled, now(), p.sub, p.outside],
                    )?;
                }
                // the home folder, as osada 0.1 makes it with the account
                if let Err(e) = std::fs::create_dir_all(app.user_root(&p.name)) {
                    tracing::warn!("home folder of {}: {e}", p.name);
                }
                c.query_row("SELECT id FROM users WHERE sub = ?1", [&p.sub], |r| r.get(0))?
            }
        };
        c.query_row("SELECT id, username, is_admin, remote_cidrs FROM users WHERE id = ?1", [id], |r| {
            Ok(User { id: r.get(0)?, username: r.get(1)?, admin: r.get(2)?, cidrs: r.get(3)? })
        })
    })
}

/// Every person of 0.2 in the users table (new accounts, new names, rights).
pub fn sync_users(app: &App) {
    let Some(b) = &app.bridge else { return };
    for p in b.people() {
        if let Err(e) = ensure_user(app, &p) {
            tracing::warn!("users table: {}: {}", p.name, e.msg);
        }
        // every account has its home (rows made before homes were, too)
        let home = app.user_root(&p.name);
        if !home.exists() {
            if let Err(e) = std::fs::create_dir_all(&home) {
                tracing::warn!("home folder of {}: {e}", p.name);
            }
        }
    }
}

/// When the session of `token` started.
pub fn session_started(app: &App, token: &str) -> Res<Option<i64>> {
    if let Some(b) = &app.bridge {
        return Ok(b.person(token).map(|p| p.started));
    }
    let h = crypto::sha256_hex(token);
    app.db.opt(|c| c.query_row("SELECT created FROM sessions WHERE token_hash = ?1", [h], |r| r.get(0)))
}

pub fn session_user(app: &App, token: &str) -> Res<Option<User>> {
    if let Some(b) = &app.bridge {
        return match b.person(token) {
            Some(p) if !p.disabled => ensure_user(app, &p).map(Some),
            _ => Ok(None),
        };
    }
    let h = crypto::sha256_hex(token);
    let t = now();
    app.db.opt(|c| {
        let (user, last): (User, i64) = c.query_row(
            "SELECT u.id, u.username, u.is_admin, u.remote_cidrs, s.last_seen FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = ?1 AND s.expires > ?2 AND u.disabled = 0",
            params![h, t],
            |r| Ok((User { id: r.get(0)?, username: r.get(1)?, admin: r.get(2)?, cidrs: r.get(3)? }, r.get(4)?)),
        )?;
        // sliding expiry, written at most every 10 minutes
        if t - last > 600 {
            c.execute(
                "UPDATE sessions SET last_seen = ?1, expires = ?2 WHERE token_hash = ?3",
                params![t, t + SESSION_SECS, h],
            )?;
        }
        Ok(user)
    })
}

pub fn new_session(app: &App, user_id: i64) -> Res<String> {
    let token = crypto::random_token(32);
    let t = now();
    app.db.with(|c| {
        c.execute(
            "INSERT INTO sessions (token_hash, user_id, created, last_seen, expires) VALUES (?1, ?2, ?3, ?3, ?4)",
            params![crypto::sha256_hex(&token), user_id, t, t + SESSION_SECS],
        )
    })?;
    Ok(token)
}

/// `https`: the request came in over https (through the edge, or the LAN's https listener)
pub fn session_cookie(app: &App, token: &str, max_age: i64, https: bool) -> String {
    let secure = if app.cfg.secure_cookie || https { "; Secure" } else { "" };
    format!("{}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}", cookie_name())
}

impl FromRequestParts<Arc<App>> for User {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let token = cookie_token(&parts.headers).ok_or_else(AppError::unauthorized)?;
        let user = session_user(app, &token)?.ok_or_else(AppError::unauthorized)?;
        if let Some(Remote(ip)) = parts.extensions.get::<Remote>() {
            if !edge::allowed(app, &user.cidrs, *ip) {
                return Err(AppError::new(StatusCode::FORBIDDEN, format!("{} cannot be used from {ip}", user.username)));
            }
        }
        Ok(user)
    }
}

impl FromRequestParts<Arc<App>> for Admin {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let u = User::from_request_parts(parts, app).await?;
        if !u.admin {
            return Err(AppError::forbidden());
        }
        Ok(Admin(u))
    }
}

/// Failed logins per username: 10 within 15 minutes and that name is locked
/// until the window has passed.
#[derive(Default)]
pub struct Limiter(Mutex<HashMap<String, (u32, Instant)>>);

const WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_FAILS: u32 = 10;

impl Limiter {
    pub fn allowed(&self, key: &str) -> bool {
        let mut m = self.0.lock().unwrap();
        match m.get(key) {
            Some((_, since)) if since.elapsed() > WINDOW => {
                m.remove(key);
                true
            }
            Some((n, _)) => *n < MAX_FAILS,
            None => true,
        }
    }

    pub fn fail(&self, key: &str) {
        let mut m = self.0.lock().unwrap();
        let e = m.entry(key.to_string()).or_insert((0, Instant::now()));
        if e.1.elapsed() > WINDOW {
            *e = (0, Instant::now());
        }
        e.0 += 1;
    }

    pub fn clear(&self, key: &str) {
        self.0.lock().unwrap().remove(key);
    }
}
