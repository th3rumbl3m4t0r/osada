//! The mail server's sync. osada only signs people in; a job on the mail
//! server (a timer, about once a minute) asks osada who the accounts are,
//! makes their mailboxes, and tells osada the address it gave each one. That
//! address is the `email` claim of sign-in tokens (oidc.rs). Which address an
//! account gets is the mail server's business; osada only keeps it.
//!
//! - `GET /api/mail/users`: every account, disabled ones too:
//!   `{"users":[{"sub","username","name","status":"active"|"disabled","outside","mail"}]}`
//! - `PUT /api/mail/users/{sub}` with `{"mail":"a@b.c"}` or `{"mail":null}`
//!
//! One token (`osada mail token`, kept here only as its sha256) in
//! `Authorization: Bearer`; over https only (the edge, or the home https
//! listener), never plain http.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::osada1::{
    crypto, db,
    edge::{ct_eq, Remote},
    error::{AppError, Res},
    oidc,
    tls::Https,
    App,
};

/// The settings key of the token's sha256.
const SETTING: &str = "mail_sync_token";
/// Tokens start with this, so they are easy to spot in configs.
const PREFIX: &str = "shm_";

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/mail/users", get(list)).route("/api/mail/users/{sub}", axum::routing::put(set_mail))
}

/// Over https and with the token, or the answer to give instead.
fn check(app: &App, remote: &Option<Extension<Remote>>, https: &Option<Extension<Https>>, headers: &HeaderMap) -> Result<(), Response> {
    if remote.is_none() && https.is_none() {
        let at = app.cfg.public_url.as_deref().map(|u| format!(" ({u})")).unwrap_or_default();
        return Err(AppError::new(StatusCode::FORBIDDEN, format!("https only{at}")).into_response());
    }
    let token = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim);
    let stored = db::get_setting(&app.db, SETTING).map_err(IntoResponse::into_response)?;
    match (token, stored) {
        (Some(t), Some(h)) if ct_eq(crypto::sha256_hex(t).as_bytes(), h.as_bytes()) => Ok(()),
        _ => {
            let mut r = (StatusCode::UNAUTHORIZED, Json(json!({ "error": "invalid_token" }))).into_response();
            r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer error=\"invalid_token\""));
            Err(r)
        }
    }
}

struct Row {
    id: i64,
    sub: Option<String>,
    username: String,
    disabled: bool,
    cidrs: String,
    mail: Option<String>,
}

const COLS: &str = "id, sub, username, disabled, remote_cidrs, mail";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row { id: r.get(0)?, sub: r.get(1)?, username: r.get(2)?, disabled: r.get(3)?, cidrs: r.get(4)?, mail: r.get(5)? })
}

fn entry(app: &App, r: Row) -> Res<Value> {
    let sub = match r.sub {
        Some(s) => s,
        None => oidc::sub(app, r.id)?,
    };
    Ok(json!({
        "sub": sub, "username": r.username, "name": r.username,
        "status": if r.disabled { "disabled" } else { "active" },
        // may be used from outside the home (through the edge)
        "outside": !r.cidrs.trim().is_empty(),
        "mail": r.mail,
    }))
}

async fn list(State(app): State<Arc<App>>, remote: Option<Extension<Remote>>, https: Option<Extension<Https>>, headers: HeaderMap) -> Response {
    if let Err(r) = check(&app, &remote, &https, &headers) {
        return r;
    }
    let rows = app.db.with(|c| {
        let mut st = c.prepare(&format!("SELECT {COLS} FROM users ORDER BY id"))?;
        let v = st.query_map([], row)?.collect::<rusqlite::Result<Vec<_>>>();
        v
    });
    let users = rows.and_then(|rows| rows.into_iter().map(|r| entry(&app, r)).collect::<Res<Vec<_>>>());
    // inside osada 0.2, once there is a mail instance: those with access to it (D-U16);
    // the mail server's sync turns off the mailboxes of the others (it deletes nothing)
    let only = app.bridge.as_ref().and_then(|b| b.mail_instance().map(|i| (b.clone(), i)));
    let users = users.map(|u| match &only {
        Some((b, i)) => u.into_iter().filter(|v| v["sub"].as_str().is_some_and(|s| b.may_use_mail(s, i))).collect(),
        None => u,
    });
    match users {
        Ok(u) => ([(header::CACHE_CONTROL, "no-store")], Json(json!({ "users": u }))).into_response(),
        Err(e) => e.into_response(),
    }
}

/// A mail address as the mail server gave it, lowercase; None if it is not one.
fn address(s: &str) -> Option<String> {
    let (local, domain) = s.split_once('@')?;
    let label = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let ok = s.len() <= 254
        && !local.is_empty()
        && local.len() <= 64
        && local.chars().all(|c| c.is_ascii_graphic() && c != '@')
        && domain.contains('.')
        && domain.split('.').all(label);
    ok.then(|| s.to_ascii_lowercase())
}

async fn set_mail(
    State(app): State<Arc<App>>,
    remote: Option<Extension<Remote>>,
    https: Option<Extension<Https>>,
    headers: HeaderMap,
    Path(sub): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = check(&app, &remote, &https, &headers) {
        return r;
    }
    let body: Option<Value> = serde_json::from_slice(&body).ok();
    let mail = match body.as_ref().and_then(|b| b.get("mail")) {
        Some(Value::Null) => None,
        Some(Value::String(s)) => match address(s) {
            Some(a) => Some(a),
            None => return AppError::bad(format!("not a mail address: {s:?}")).into_response(),
        },
        _ => return AppError::bad(r#"expected {"mail": "<address>"} or {"mail": null}"#).into_response(),
    };
    match put(&app, &sub, mail) {
        Ok(v) => ([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

fn put(app: &App, sub: &str, mail: Option<String>) -> Res<Value> {
    let q = format!("SELECT {COLS} FROM users WHERE sub = ?1");
    let r = app.db.opt(|c| c.query_row(&q, [sub], row))?.ok_or_else(AppError::not_found)?;
    if r.mail != mail {
        let taken: Option<String> = app.db.with(|c| {
            if let Some(m) = &mail {
                if let Some(other) = c.query_row("SELECT username FROM users WHERE mail = ?1 AND id != ?2", params![m, r.id], |x| x.get(0)).optional()? {
                    return Ok(Some(other));
                }
            }
            c.execute("UPDATE users SET mail = ?1 WHERE id = ?2", params![mail, r.id])?;
            Ok(None)
        })?;
        if let Some(other) = taken {
            return Err(AppError::conflict(format!("that is {other}'s address")));
        }
        match &mail {
            Some(m) => tracing::info!("mail sync: {}'s address is now {m}", r.username),
            None => tracing::info!("mail sync: {} has no address now", r.username),
        }
    }
    entry(app, Row { mail, ..r })
}

/// A new token, printed once; the one before stops working.
fn new_token(app: &App) -> Res<String> {
    let t = format!("{PREFIX}{}", crypto::random_token(32));
    db::set_setting(&app.db, SETTING, &crypto::sha256_hex(&t))?;
    Ok(t)
}

pub fn cli(app: &App, args: &[String]) -> anyhow::Result<()> {
    let e = |e: AppError| anyhow::anyhow!(e.msg);
    match args.first().map(String::as_str) {
        Some("token") => {
            let had = db::get_setting(&app.db, SETTING).map_err(e)?.is_some();
            println!("{}", new_token(app).map_err(e)?);
            eprintln!("the mail sync token{}; it is shown only now", if had { " is new, the old one no longer works" } else { "" });
        }
        Some("off") => {
            app.db.with(|c| c.execute("DELETE FROM settings WHERE key = ?1", [SETTING])).map_err(e)?;
            eprintln!("no mail sync token now: /api/mail answers 401 until `osada mail token`");
        }
        Some("list") => {
            let rows = app
                .db
                .with(|c| {
                    let mut st = c.prepare(&format!("SELECT {COLS} FROM users ORDER BY id"))?;
                    let v = st.query_map([], row)?.collect::<rusqlite::Result<Vec<_>>>();
                    v
                })
                .map_err(e)?;
            for r in rows {
                println!("{:<16} {:<9} {}", r.username, if r.disabled { "disabled" } else { "active" }, r.mail.as_deref().unwrap_or("-"));
            }
        }
        _ => anyhow::bail!("usage: osada mail token | off | list"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::address;

    #[test]
    fn addresses() {
        assert_eq!(address("Vera@Example.org").as_deref(), Some("vera@example.org"));
        assert_eq!(address("alice@alice.ca.example.org").as_deref(), Some("alice@alice.ca.example.org"));
        assert_eq!(address("a.b+c@example.net").as_deref(), Some("a.b+c@example.net"));
        for bad in ["", "alice", "@x.com", "alice@", "a@b@c.com", "a b@c.com", "a@c .com", "a@localhost", "a@-x.com", "a@x..com", "a\n@x.com", "ä@x.com"] {
            assert_eq!(address(bad), None, "{bad:?}");
        }
        assert_eq!(address(&format!("{}@x.com", "a".repeat(65))), None);
    }
}
