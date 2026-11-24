use std::{io::BufRead, sync::Arc};

use axum::{
    extract::{Extension, Path, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
    Json, Router,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::osada1::{
    auth::{self, Admin, User},
    crypto,
    db::{self, now},
    edge::{self, Remote},
    error::{AppError, Res},
    apptab, quota, tls, App,
};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/me/lang", put(set_lang))
        .route("/api/password", post(password))
        .route("/api/usernames", get(usernames))
        .route("/api/people", get(people))
        .route("/api/users", get(list).post(create))
        .route("/api/users/{id}", patch(update).delete(remove))
}

/// Someone of this instance (Round 21): the share window finds them by any of these.
#[derive(Clone, Debug)]
pub struct Local {
    pub username: String,
    pub display: String,
    pub upn: String,
    pub aliases: Vec<String>,
}

/// osada's public host name (`osada.example.org`), the domain of a UPN without a mailbox.
pub fn public_host(app: &App) -> String {
    app.cfg
        .public_url
        .as_deref()
        .and_then(|u| u.split("://").nth(1))
        .map(|h| h.split(['/', ':']).next().unwrap_or("").to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

/// The domain of every UPN (Round 22): the cluster's, else osada's public host.
pub fn upn_domain(app: &App) -> String {
    app.bridge.as_ref().and_then(|b| b.upn_domain()).filter(|d| !d.is_empty()).unwrap_or_else(|| public_host(app))
}

/// An account's UPN (Round 22): `<name>@<upn_domain>`, part of the account: its sign-in name besides
/// the bare name, what the share window and federation know it by, with a mailbox an address of it.
pub fn upn_of(app: &App, username: &str) -> String {
    format!("{}@{}", username.to_lowercase(), upn_domain(app))
}

/// The people of this instance (enabled accounts), with osada 0.2's directory over them; each by
/// their UPN, their mailbox's address and its aliases their other addresses.
pub fn people_index(app: &App) -> Vec<Local> {
    let host = upn_domain(app);
    let rows: Vec<(String, Option<String>, Option<String>)> = app
        .db
        .with(|c| {
            let mut st = c.prepare("SELECT username, sub, mail FROM users WHERE disabled = 0 ORDER BY username")?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>();
            v
        })
        .unwrap_or_default();
    let dir: std::collections::HashMap<String, auth::DirPerson> =
        app.bridge.as_ref().map(|b| b.directory().into_iter().map(|p| (p.sub.clone(), p)).collect()).unwrap_or_default();
    // people of the directory this side has no row for yet (sync_users makes it; sharing too)
    let known: std::collections::HashSet<String> = rows.iter().filter_map(|r| r.1.clone()).collect();
    let mut rows = rows;
    for d in dir.values().filter(|d| !d.disabled && !known.contains(&d.sub) && valid_username(&d.name)) {
        if !rows.iter().any(|r| r.0 == d.name) {
            rows.push((d.name.clone(), Some(d.sub.clone()), None));
        }
    }
    rows.into_iter()
        .filter_map(|(username, sub, mail)| {
            let d = sub.as_ref().and_then(|s| dir.get(s));
            if d.is_some_and(|d| d.disabled) {
                return None;
            }
            Some(Local {
                display: d.map(|d| d.display.clone()).filter(|x| !x.trim().is_empty()).unwrap_or_else(|| username.clone()),
                upn: format!("{username}@{host}").to_lowercase(),
                aliases: {
                    let mut a: Vec<String> = d.and_then(|d| d.mailbox.clone()).or(mail).into_iter().chain(d.map(|d| d.aliases.clone()).unwrap_or_default()).map(|a| a.to_lowercase()).collect();
                    a.sort();
                    a.dedup();
                    a
                },
                username,
            })
        })
        .collect()
}

/// The person `who` names: a user name, a UPN or one of their other addresses.
pub fn find_person(app: &App, who: &str) -> Option<Local> {
    let w = who.trim().trim_start_matches('<').trim_end_matches('>').to_lowercase();
    people_index(app).into_iter().find(|p| p.username == w || p.upn == w || p.aliases.contains(&w))
}

#[derive(Deserialize)]
struct PeopleQ {
    #[serde(default)]
    q: String,
}

/// The share window's search (Round 21), in this order: 1) people of this instance (name, display
/// name, UPN, other addresses); 2) federated instances (none yet: what federation is, is the
/// owner's to decide); 3) an address nobody here has: public email (a view-only link mailed to it).
async fn people(State(app): State<Arc<App>>, user: User, axum::extract::Query(q): axum::extract::Query<PeopleQ>) -> Res<Json<Value>> {
    let q = q.q.trim().trim_start_matches('<').trim_end_matches('>').to_lowercase();
    if q.chars().count() < 2 {
        return Ok(Json(json!({ "local": [], "federated": [], "federation": "none", "email": null })));
    }
    let all = people_index(&app);
    let score = |p: &Local| -> Option<u8> {
        let fields = || [p.username.as_str(), p.upn.as_str()].into_iter().chain(p.aliases.iter().map(String::as_str));
        if fields().any(|f| f == q) {
            Some(0)
        } else if fields().any(|f| f.starts_with(&q)) || p.display.to_lowercase().split_whitespace().any(|w| w.starts_with(&q)) {
            Some(1)
        } else if fields().any(|f| f.contains(&q)) || p.display.to_lowercase().contains(&q) {
            Some(2)
        } else {
            None
        }
    };
    let mut hits: Vec<(u8, &Local)> = all.iter().filter_map(|p| score(p).map(|s| (s, p))).collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.username.cmp(&b.1.username)));
    let exact = hits.iter().any(|(s, p)| *s == 0 && p.username != q);
    let local: Vec<Value> = hits
        .iter()
        .take(15)
        .map(|(s, p)| json!({ "user": p.username, "display": p.display, "upn": p.upn, "exact": *s == 0, "me": p.username == user.username }))
        .collect();
    // 2) federated instances (Round 22): an address at one's UPN domain, asked there
    let (federated, federation) = if exact { (Vec::new(), "on") } else { crate::osada1::federation::search(&app, &q).await };
    let email = crate::osada1::sendmail::address(&q).filter(|_| !exact && federated.is_empty()).map(|a| {
        let why_not = if crate::osada1::sendmail::base(&app).is_none() {
            Some("there is no mail server to send it through")
        } else if crate::osada1::sendmail::from_address(&app, &user).is_none() {
            Some("you have no mailbox to send it from")
        } else {
            None
        };
        json!({ "address": a, "can_send": why_not.is_none(), "why_not": why_not })
    });
    Ok(Json(json!({ "local": local, "federated": federated, "federation": federation, "email": email })))
}

/// Usernames double as folder names under `<data>/users`.
pub fn valid_username(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase() | b[0].is_ascii_digit()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(c))
}

pub fn create_user(app: &App, name: &str, pw: &str, admin: bool) -> Res<i64> {
    if !valid_username(name) {
        return Err(AppError::bad("username: 1-32 of a-z 0-9 . _ -, starting with a letter or digit"));
    }
    auth::check_password_rules(pw)?;
    let hash = auth::hash_password(pw);
    let id = app.db.with(|c| {
        c.execute(
            "INSERT INTO users (username, pw_hash, is_admin, created) VALUES (?1, ?2, ?3, ?4)",
            params![name, hash, admin, now()],
        )?;
        Ok(c.last_insert_rowid())
    });
    let id = match id {
        Err(e) if e.status.is_server_error() => {
            let taken = app.db.opt(|c| c.query_row("SELECT 1 FROM users WHERE username = ?1", [name], |_| Ok(())))?;
            return Err(if taken.is_some() { AppError::conflict("username taken") } else { e });
        }
        r => r?,
    };
    std::fs::create_dir_all(app.user_root(name)).map_err(AppError::internal)?;
    Ok(id)
}

fn user_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "username": r.get::<_, String>(1)?,
        "admin": r.get::<_, bool>(2)?, "disabled": r.get::<_, bool>(3)?, "created": r.get::<_, i64>(4)?,
        "remote": r.get::<_, String>(5)?, "quota": r.get::<_, Option<i64>>(6)?, "apptab": r.get::<_, bool>(7)?,
    }))
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

async fn login(
    State(app): State<Arc<App>>,
    remote: Option<Extension<Remote>>,
    https: Option<Extension<tls::Https>>,
    peer: Option<Extension<axum::extract::ConnectInfo<edge::PeerAddr>>>,
    Json(req): Json<LoginReq>,
) -> Res<Response> {
    let name = req.username.trim().to_lowercase();
    let remote = remote.map(|Extension(Remote(ip))| ip);
    // strangers on the internet can only lock the account's outside logins
    let key = if remote.is_some() { format!("{name}@edge") } else { name.clone() };
    if !app.limiter.allowed(&key) {
        return Err(AppError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "too many failed logins, try again in 15 minutes",
        ));
    }
    if let Some(b) = app.bridge.clone() {
        // Inside 0.2: its sign-in (the pages have their own, with second
        // factors, at /signin); the outside networks still apply here.
        let addr = match (remote, peer) {
            (Some(ip), _) => ip.to_string(),
            (None, Some(Extension(axum::extract::ConnectInfo(edge::PeerAddr(a))))) => a.ip().to_canonical().to_string(),
            (None, None) => "lan".into(),
        };
        let token = match b.sign_in(name.clone(), req.password.clone(), addr.clone()).await {
            Ok(t) => t,
            Err(e) => {
                app.limiter.fail(&key);
                return Err(AppError::new(axum::http::StatusCode::UNAUTHORIZED, e));
            }
        };
        let user = auth::session_user(&app, &token)?.ok_or_else(AppError::unauthorized)?;
        if let Some(ip) = remote {
            if !edge::allowed(&app, &user.cidrs, ip) {
                b.sign_out(&token, &addr);
                app.limiter.fail(&key);
                return Err(AppError::new(axum::http::StatusCode::UNAUTHORIZED, format!("wrong username or password, or this account cannot be used from {ip}")));
            }
        }
        app.limiter.clear(&key);
        let cookie = auth::session_cookie(&app, &token, auth::SESSION_SECS, remote.is_some() || https.is_some());
        return Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response());
    }
    let row = app.db.opt(|c| {
        c.query_row(
            "SELECT id, pw_hash, remote_cidrs FROM users WHERE username = ?1 AND disabled = 0",
            [&name],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
    })?;
    // hash even for unknown names so both cases take as long
    let (id, hash, cidrs) =
        row.unwrap_or((0, "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$ybmUEjYvh7/XyCCs2FcxKtWYfy9pgh8OpeAGf6QgOYg".into(), String::new()));
    let pw = req.password;
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&pw, &hash)).await.unwrap_or(false);
    let from_ok = remote.is_none_or(|ip| edge::allowed(&app, &cidrs, ip));
    if !ok || id == 0 || !from_ok {
        app.limiter.fail(&key);
        // the same answer either way: no hint which names exist or are allowed out
        let (msg, via) = match remote {
            Some(ip) => (format!("wrong username or password, or this account cannot be used from {ip}"), format!(" from {ip} (edge)")),
            None => ("wrong username or password".into(), String::new()),
        };
        tracing::warn!("failed login for {name:?}{via}");
        return Err(AppError::new(axum::http::StatusCode::UNAUTHORIZED, msg));
    }
    app.limiter.clear(&key);
    if let Some(ip) = remote {
        tracing::info!("{name} logged in from {ip} (edge)");
    }
    let token = auth::new_session(&app, id)?;
    let cookie = auth::session_cookie(&app, &token, auth::SESSION_SECS, remote.is_some() || https.is_some());
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

async fn logout(State(app): State<Arc<App>>, remote: Option<Extension<Remote>>, https: Option<Extension<tls::Https>>, headers: HeaderMap) -> Res<Response> {
    if let Some(t) = auth::cookie_token(&headers) {
        match &app.bridge {
            Some(b) => b.sign_out(&t, if remote.is_some() { "edge" } else { "lan" }),
            None => {
                let h = crypto::sha256_hex(&t);
                app.db.with(|c| c.execute("DELETE FROM sessions WHERE token_hash = ?1", [h]))?;
            }
        }
    }
    // what the browser kept of osada's names (a /web/ site's pages too) goes with the session (Round 20)
    Ok(([(header::SET_COOKIE, auth::session_cookie(&app, "", 0, remote.is_some() || https.is_some())), (header::HeaderName::from_static("clear-site-data"), "\"cache\"".to_string())], Json(json!({ "ok": true }))).into_response())
}

/// The mail server's own pages for an account, on the webmail's server (Stalwart's /account/: app
/// passwords for mail programs, encryption at rest; T6).
/// The webmail: OSADA_MAILDOT_URL, else (Round 24) the one osada-setup mail installs in Stalwart,
/// at the mail instance's name.
fn maildot_url(app: &App) -> Option<String> {
    app.cfg.maildot_url.clone().or_else(|| app.bridge.as_ref().and_then(|b| b.mail_host()).map(|h| format!("https://{h}/maildot/")))
}

fn mail_account_url(maildot: &str) -> Option<String> {
    let (scheme, rest) = maildot.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())?;
    matches!(scheme, "https" | "http").then(|| format!("{scheme}://{host}/account/"))
}

async fn me(State(app): State<Arc<App>>, remote: Option<Extension<Remote>>, headers: axum::http::HeaderMap, user: User) -> Res<Json<Value>> {
    let (lang, mail, quota): (String, Option<String>, Option<i64>) =
        app.db.with(|c| c.query_row("SELECT lang, mail, quota FROM users WHERE id = ?1", [user.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))))?;
    let staff = app.bridge.as_ref().and_then(|b| auth::cookie_token(&headers).and_then(|t| b.person(&t))).is_some_and(|p| p.staff);
    // the webmail: for those with access to the mail instance once there is one (D-U16), else
    // for an account the mail server's sync gave an address
    let maildot = match app.bridge.as_ref().and_then(|b| b.mail_instance().map(|i| (b.clone(), i))) {
        Some((b, i)) => {
            let sub: Option<String> = app.db.with(|c| c.query_row("SELECT sub FROM users WHERE id = ?1", [user.id], |r| r.get(0)))?;
            sub.is_some_and(|s| b.may_use_mail(&s, &i)).then(|| maildot_url(&app)).flatten()
        }
        None => mail.and(maildot_url(&app)),
    };
    let proxies = apptab::usable(&app, user.id)?;
    // the apps of the header and the portal (D-U12, D-U13): name, title, where, a line on what it is
    let mut apps = vec![
        json!({ "name": "files", "title": "files", "href": "/files/home/", "about": "Your files at home: open, edit and share them; pictures, earlier versions, your own SFTP storage." }),
        json!({ "name": "shared", "title": "shared", "href": "/shared", "about": "What others share with you." }),
        json!({ "name": "links", "title": "links", "href": "/links", "about": "Public links you made, for people without an account." }),
        json!({ "name": "wiki", "title": "wiki", "href": "/wiki", "about": "The wiki: pages everyone here writes together." }),
    ];
    // chat, built into osada 0.2 (D-U42): its page is 0.2's
    if app.bridge.is_some() {
        apps.push(json!({ "name": "chat", "title": "chat", "href": "/chat", "about": "Rooms and direct messages with the people here." }));
        // contacts and calendar (Round 15): osada's pages over the mail server's
        apps.push(json!({ "name": "contacts", "title": "contacts", "href": "/contacts", "about": "Everyone: the people here, your address books, whom you mailed with; one search over all of them." }));
        apps.push(json!({ "name": "calendar", "title": "calendar", "href": "/calendar", "about": "Your calendars on the mail server: the month, the day, new events. Phones and maildot sync them by CalDAV." }));
    }
    for p in &proxies {
        let name = p["name"].as_str().unwrap_or_default();
        let href = if name == "apptab" { "/apptab".to_string() } else { format!("/proxy/{name}") };
        let host = app.proxies.by_name(name).map(|m| m.host.clone()).unwrap_or_default();
        apps.push(json!({ "name": name, "title": p["title"], "href": href, "kind": "proxy", "about": format!("Another program, at {host}, in a tab of osada.") }));
    }
    if let Some(m) = &maildot {
        apps.push(json!({ "name": "mail", "title": "mail", "href": m, "external": true, "about": "Your mail (maildot), in a tab of its own.",
            "account": mail_account_url(m) }));
    }
    Ok(Json(json!({
        "id": user.id, "username": user.username, "upn": upn_of(&app, &user.username), "admin": user.admin, "lang": lang,
        "host": app.cfg.host, "version": env!("CARGO_PKG_VERSION"),
        // the node serving this page, as node control names it (T14)
        "node_name": app.bridge.as_ref().and_then(|b| b.node_name()),
        "public_url": app.cfg.public_url, "remote": remote.map(|Extension(Remote(ip))| ip.to_string()),
        "apptab": apptab::on_for(&app, user.id, "apptab")?,
        // inside osada 0.2: the proxy modules this account may use, a tab each
        "proxies": proxies,
        // the webmail, for an account the mail server's sync gave an address
        "maildot": maildot,
        "apps": apps,
        // the admin page: osada 0.1's admins, and inside osada 0.2 everyone with a role
        "admin_link": user.admin || staff,
        // the home's size is counted at most every few minutes (quota.rs)
        "storage": { "limit": super::quota::limit(&app, user.id).ok().flatten().map(|l| l as i64).or(quota), "used": app.quotas.used(&app, user.id).await.ok() },
        // earlier versions come from the backup
        "versions": db::get_setting(&app.db, "backup")?.is_some(),
        // inside osada 0.2: the office suite's page, when a site is one (Round 18)
        "office": app.bridge.as_ref().and_then(|b| b.office()),
        // inside osada 0.2: its account page, and the admin portal for those with a role
        "portal": app.bridge.is_some(),
        // the header's "osada // <instance>" (Round 24)
        "instance": upn_domain(&app),
        "staff": staff,
    })))
}

/// UI languages (static/i18n.js has the texts); "" goes back to the browser's.
pub const LANGS: &[&str] = &["en", "cs", "sk", "hu", "fr", "de"];

#[derive(Deserialize)]
struct LangReq {
    lang: String,
}

async fn set_lang(State(app): State<Arc<App>>, user: User, Json(req): Json<LangReq>) -> Res<Json<Value>> {
    if !req.lang.is_empty() && !LANGS.contains(&req.lang.as_str()) {
        return Err(AppError::bad("unknown language"));
    }
    app.db.with(|c| c.execute("UPDATE users SET lang = ?1 WHERE id = ?2", params![req.lang, user.id]))?;
    Ok(Json(json!({ "lang": req.lang })))
}

#[derive(Deserialize)]
struct PasswordReq {
    old: String,
    new: String,
}

async fn password(State(app): State<Arc<App>>, user: User, headers: HeaderMap, Json(req): Json<PasswordReq>) -> Res<Json<Value>> {
    auth::check_password_rules(&req.new)?;
    if let Some(b) = app.bridge.clone() {
        // Inside 0.2: its password; this table's too, so that osada 0.1 could
        // still be started on this data.
        let token = auth::cookie_token(&headers).ok_or_else(AppError::unauthorized)?;
        b.change_password(token, req.old.clone(), req.new.clone()).await.map_err(AppError::bad)?;
        let new_hash = tokio::task::spawn_blocking(move || auth::hash_password(&req.new)).await.map_err(AppError::internal)?;
        app.db.with(|c| {
            c.execute("UPDATE users SET pw_hash = ?1 WHERE id = ?2", params![new_hash, user.id])?;
            c.execute("DELETE FROM oidc_tokens WHERE user_id = ?1", [user.id])
        })?;
        return Ok(Json(json!({ "ok": true })));
    }
    let hash: String = app.db.with(|c| c.query_row("SELECT pw_hash FROM users WHERE id = ?1", [user.id], |r| r.get(0)))?;
    let old = req.old;
    if !tokio::task::spawn_blocking(move || auth::verify_password(&old, &hash)).await.unwrap_or(false) {
        return Err(AppError::bad("current password is wrong"));
    }
    let new_hash = tokio::task::spawn_blocking(move || auth::hash_password(&req.new)).await.map_err(AppError::internal)?;
    let keep = auth::cookie_token(&headers).map(|t| crypto::sha256_hex(&t)).unwrap_or_default();
    app.db.with(|c| {
        c.execute("UPDATE users SET pw_hash = ?1 WHERE id = ?2", params![new_hash, user.id])?;
        // other devices have to log in again, and so do the programs signed in to
        c.execute("DELETE FROM sessions WHERE user_id = ?1 AND token_hash != ?2", params![user.id, keep])?;
        c.execute("DELETE FROM oidc_tokens WHERE user_id = ?1", [user.id])
    })?;
    Ok(Json(json!({ "ok": true })))
}

async fn usernames(State(app): State<Arc<App>>, _user: User) -> Res<Json<Value>> {
    let names = app.db.with(|c| {
        let mut st = c.prepare("SELECT username FROM users WHERE disabled = 0 ORDER BY username")?;
        let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>();
        v
    })?;
    Ok(Json(json!(names)))
}

async fn list(State(app): State<Arc<App>>, _a: Admin) -> Res<Json<Value>> {
    let users = app.db.with(|c| {
        let mut st = c.prepare("SELECT id, username, is_admin, disabled, created, remote_cidrs, quota, apptab FROM users ORDER BY username")?;
        let v = st.query_map([], user_json)?.collect::<rusqlite::Result<Vec<_>>>();
        v
    })?;
    // how full the homes with a limit are; apptab only where it is set up (inside osada 0.2 it is
    // access to the proxy module, in the admin portal)
    let mut users = users;
    if app.proxies.by_name("apptab").is_none() || app.bridge.is_some() {
        for u in users.iter_mut() {
            u["apptab"] = Value::Null;
        }
    }
    // the bytes used, for everyone (with a limit or not; counted at most every few minutes)
    for u in users.iter_mut() {
        if let Some(id) = u["id"].as_i64() {
            u["used"] = json!(app.quotas.used(&app, id).await.ok());
        }
    }
    Ok(Json(json!(users)))
}

#[derive(Deserialize)]
struct CreateReq {
    username: String,
    password: String,
    #[serde(default)]
    admin: bool,
}

async fn create(State(app): State<Arc<App>>, Admin(a): Admin, Json(req): Json<CreateReq>) -> Res<Json<Value>> {
    if app.bridge.is_some() {
        return Err(AppError::bad("users are made in the admin portal: /admin/users"));
    }
    let name = req.username.trim().to_lowercase();
    let id = tokio::task::block_in_place(|| create_user(&app, &name, &req.password, req.admin))?;
    tracing::info!("{} created user {name} (admin: {})", a.username, req.admin);
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
struct UpdateReq {
    admin: Option<bool>,
    disabled: Option<bool>,
    password: Option<String>,
    /// networks (CIDR, AS numbers) for outside access, `""`: none
    remote: Option<String>,
    /// room in the home, as `quota::parse` takes it; `""`: no limit
    quota: Option<String>,
    /// apps1's apps in a tab (apptab.rs)
    apptab: Option<bool>,
}

async fn update(State(app): State<Arc<App>>, Admin(a): Admin, Path(id): Path<i64>, Json(req): Json<UpdateReq>) -> Res<Json<Value>> {
    if app.bridge.is_some() && (req.admin.is_some() || req.disabled.is_some() || req.password.is_some() || req.apptab.is_some() || req.remote.is_some()) {
        return Err(AppError::bad("rights, passwords, disabling, outside networks and access to modules are in the admin page: /admin/users"));
    }
    if id == a.id && (req.admin == Some(false) || req.disabled == Some(true)) {
        return Err(AppError::bad("you cannot demote or disable yourself"));
    }
    let hash = match req.password {
        Some(pw) => {
            auth::check_password_rules(&pw)?;
            Some(tokio::task::spawn_blocking(move || auth::hash_password(&pw)).await.map_err(AppError::internal)?)
        }
        None => None,
    };
    let remote = req.remote.as_deref().map(edge::normalize).transpose().map_err(AppError::bad)?;
    let quota = req.quota.as_deref().map(quota::parse).transpose()?;
    let n = app.db.with(|c| {
        let mut n = 0;
        if let Some(v) = req.admin {
            n += c.execute("UPDATE users SET is_admin = ?1 WHERE id = ?2", params![v, id])?;
        }
        if let Some(v) = req.disabled {
            n += c.execute("UPDATE users SET disabled = ?1 WHERE id = ?2", params![v, id])?;
            if v {
                c.execute("DELETE FROM sessions WHERE user_id = ?1", [id])?;
                c.execute("DELETE FROM oidc_tokens WHERE user_id = ?1", [id])?;
            }
        }
        if let Some(h) = &hash {
            n += c.execute("UPDATE users SET pw_hash = ?1 WHERE id = ?2", params![h, id])?;
            c.execute("DELETE FROM sessions WHERE user_id = ?1", [id])?;
            c.execute("DELETE FROM oidc_tokens WHERE user_id = ?1", [id])?;
        }
        if let Some(r) = &remote {
            n += c.execute("UPDATE users SET remote_cidrs = ?1 WHERE id = ?2", params![r, id])?;
        }
        if let Some(q) = quota {
            n += c.execute("UPDATE users SET quota = ?1 WHERE id = ?2", params![q.map(|q| q as i64), id])?;
        }
        if let Some(v) = req.apptab {
            n += c.execute("UPDATE users SET apptab = ?1 WHERE id = ?2", params![v, id])?;
        }
        Ok(n)
    })?;
    if n == 0 {
        return Err(AppError::not_found());
    }
    if let Some(q) = quota {
        tracing::info!("{} set the storage limit of user {id} to {}", a.username, q.map_or("none".into(), |q| format!("{q} bytes")));
    }
    if let Some(v) = req.apptab {
        tracing::info!("{} turned apptab {} for user {id}", a.username, if v { "on" } else { "off" });
    }
    let mut note = None;
    if let Some(r) = &remote {
        tracing::info!("{} set outside access of user {id} to {:?}", a.username, r);
        note = asn_note(&app, r).await;
    }
    Ok(Json(json!({ "ok": true, "note": note })))
}

/// Fetches the networks of the AS numbers in a new list right away, so a
/// typo shows up now rather than as a refused login later.
async fn asn_note(app: &App, list: &str) -> Option<String> {
    let asns = edge::asns_in(list);
    if asns.is_empty() {
        return None;
    }
    let got = app.asns.refresh(&app.db, &asns, true).await;
    let parts: Vec<String> = got
        .into_iter()
        .map(|(n, r)| match r {
            Ok(k) => format!("AS{n}: {k} networks"),
            Err(e) => format!("AS{n}: not fetched ({e}); retried hourly"),
        })
        .collect();
    Some(parts.join("\n"))
}

/// Deletes the account with its sessions, mounts, shares and links. The files
/// are kept: they move to `<data>/deleted/<name>-<time>`.
async fn remove(State(app): State<Arc<App>>, Admin(a): Admin, Path(id): Path<i64>) -> Res<Json<Value>> {
    if app.bridge.is_some() {
        return Err(AppError::bad("users are kept: disable them in the admin portal (/admin/users)"));
    }
    if id == a.id {
        return Err(AppError::bad("you cannot delete yourself"));
    }
    let (name, mounts) = app
        .db
        .opt(|c| {
            let name: String = c.query_row("SELECT username FROM users WHERE id = ?1", [id], |r| r.get(0))?;
            let mut st = c.prepare("SELECT id FROM mounts WHERE user_id = ?1")?;
            let mounts = st.query_map([id], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            c.execute("DELETE FROM users WHERE id = ?1", [id])?;
            Ok((name, mounts))
        })?
        .ok_or_else(AppError::not_found)?;
    for m in mounts {
        app.sftp.forget(m);
    }
    let kept = app.cfg.data.join("deleted").join(format!("{name}-{}", now()));
    let moved = std::fs::rename(app.user_root(&name), &kept).is_ok();
    tracing::info!("{} deleted user {name}; files {}", a.username, if moved { kept.display().to_string() } else { "none".into() });
    Ok(Json(json!({ "ok": true, "files_kept_at": moved.then(|| kept.display().to_string()) })))
}

/// `osada user add|passwd|remote|limit|list …` for the first admin and for lockouts.
pub fn cli(app: &App, args: &[String]) -> anyhow::Result<()> {
    let flag = |f: &str| args.iter().any(|a| a == f);
    let read_pw = || -> anyhow::Result<String> {
        if flag("--random") {
            let pw = crypto::random_token(12);
            println!("{pw}");
            return Ok(pw);
        }
        eprintln!("password (one line on stdin):");
        let mut s = String::new();
        std::io::stdin().lock().read_line(&mut s)?;
        Ok(s.trim_end_matches(['\r', '\n']).to_string())
    };
    let e = |e: AppError| anyhow::anyhow!(e.msg);
    // inside osada 0.2 (its node next to this data): accounts and passwords are its
    if app.cfg.data.join("node").exists() && matches!(args.first().map(String::as_str), Some("add" | "passwd" | "admin" | "disable" | "enable" | "delete" | "remote" | "apptab")) {
        anyhow::bail!("accounts, passwords, outside networks and access are osada 0.2's now: the admin page (/admin/users), or osada rescue");
    }
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("add"), Some(name)) => {
            let pw = read_pw()?;
            create_user(app, name, &pw, flag("--admin")).map_err(e)?;
            eprintln!("created {name}{}", if flag("--admin") { " (admin)" } else { "" });
        }
        (Some("passwd"), Some(name)) => {
            let pw = read_pw()?;
            auth::check_password_rules(&pw).map_err(e)?;
            let h = auth::hash_password(&pw);
            let n = app.db.with(|c| c.execute("UPDATE users SET pw_hash = ?1, disabled = 0 WHERE username = ?2", params![h, name])).map_err(e)?;
            anyhow::ensure!(n == 1, "no user {name}");
            eprintln!("password of {name} set");
        }
        (Some("remote"), Some(name)) => {
            let list = edge::normalize(&args[2..].join(" ")).map_err(|m| anyhow::anyhow!(m))?;
            let n = app.db.with(|c| c.execute("UPDATE users SET remote_cidrs = ?1 WHERE username = ?2", params![list, name])).map_err(e)?;
            anyhow::ensure!(n == 1, "no user {name}");
            eprintln!("{name} from outside: {}", if list.is_empty() { "from anywhere" } else { &list });
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            if let Some(note) = rt.block_on(asn_note(app, &list)) {
                eprintln!("{note}");
            }
        }
        (Some("apptab"), Some(name)) => {
            let on = match args.get(2).map(String::as_str) {
                Some("on") => true,
                Some("off") => false,
                _ => anyhow::bail!("usage: osada user apptab <name> on|off"),
            };
            let n = app.db.with(|c| c.execute("UPDATE users SET apptab = ?1 WHERE username = ?2", params![on, name])).map_err(e)?;
            anyhow::ensure!(n == 1, "no user {name}");
            eprintln!("{name}: apptab {}", if on { "on" } else { "off" });
        }
        (Some("limit"), Some(name)) => {
            let q = quota::parse(args.get(2).map_or("", String::as_str)).map_err(e)?;
            let n = app.db.with(|c| c.execute("UPDATE users SET quota = ?1 WHERE username = ?2", params![q.map(|q| q as i64), name])).map_err(e)?;
            anyhow::ensure!(n == 1, "no user {name}");
            eprintln!("{name}: {}", q.map_or("no storage limit".into(), |q| format!("storage limit {q} bytes")));
        }
        (Some("list"), _) => {
            app.db
                .with(|c| {
                    let mut st = c.prepare("SELECT id, username, is_admin, disabled, created, remote_cidrs, quota, apptab FROM users ORDER BY username")?;
                    for u in st.query_map([], user_json)? {
                        println!("{}", u?);
                    }
                    Ok(())
                })
                .map_err(e)?;
        }
        _ => anyhow::bail!("usage: osada user add <name> [--admin] [--random] | passwd <name> [--random] | remote <name> [cidr|AS<n> …] | limit <name> [50G|1.5T|…] | apptab <name> on|off | list"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_account_pages() {
        assert_eq!(mail_account_url("https://mail.example.org/maildot/").as_deref(), Some("https://mail.example.org/account/"));
        assert_eq!(mail_account_url("http://127.0.0.1:8080").as_deref(), Some("http://127.0.0.1:8080/account/"));
        for bad in ["mail.example.org/maildot/", "https:///x", "javascript://x/", ""] {
            assert_eq!(mail_account_url(bad), None, "{bad}");
        }
    }
}
