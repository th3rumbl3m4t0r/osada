//! Passwords for mail programs (Round 24). Once the mail server signs people in through osada
//! (osada-setup mail: Stalwart's OpenID Connect directory), the account's password is no mail
//! password any more: mail programs (IMAP, SMTP, CalDAV, CardDAV) need app passwords, and only the
//! account itself can make them at the mail server (Stalwart's x:AppPassword: the server chooses
//! the secret and shows it once; an administrator cannot set one). So the account page asks osada,
//! and osada speaks JMAP as the person (sendmail::Jmap: a short token for the webmail's client).
//! The token also carries the scope Stalwart 1.x asks for before it lets a token manage
//! credentials (`urn:stalwart:oauth:scope:security`, v1.0.3); 0.16 does not look at it.
//!
//! Under /api/account/ (not /api/mail/, whose sync API the X-Shelf check leaves out): the page's
//! session cookie and its X-Shelf header, like the rest of the account page.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::osada1::{
    App,
    auth::User,
    error::{AppError, Res},
    sendmail,
};

const SCOPE: &str = "openid email urn:stalwart:oauth:scope:security";
const USING: &[&str] = &["urn:ietf:params:jmap:core", "urn:stalwart:jmap"];
const MAX_NAME: usize = 80;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/account/mail-passwords", get(list).post(create))
        .route("/api/account/mail-passwords/{id}", delete(remove))
}

fn gateway(e: String) -> AppError {
    AppError::new(StatusCode::BAD_GATEWAY, e)
}

/// Why there are no app passwords for `user` here, if there are none.
fn why_not(app: &App, user: &User) -> Option<&'static str> {
    if sendmail::base(app).is_none() {
        return Some("there is no mail server");
    }
    if sendmail::from_address(app, user).is_none() {
        return Some("you have no mailbox");
    }
    None
}

async fn session(app: &App, user: &User) -> Res<(sendmail::Jmap, String)> {
    if let Some(w) = why_not(app, user) {
        return Err(AppError::bad(w));
    }
    let j = sendmail::Jmap::open(app, user, SCOPE).await.map_err(gateway)?;
    let account = j.account(&["urn:stalwart:jmap", "urn:ietf:params:jmap:mail"]).ok_or_else(|| gateway("no account of yours at the mail server".into()))?;
    Ok((j, account))
}

/// What a mail program needs besides the password: the user name and the server.
fn settings(app: &App, user: &User) -> Value {
    json!({
        "login": sendmail::from_address(app, user),
        "host": app.bridge.as_ref().and_then(|b| b.mail_host()),
    })
}

/// The account's app passwords (never their secrets), with the settings for a mail program;
/// `available: false` and why when there is no mail server or no mailbox.
async fn list(State(app): State<Arc<App>>, user: User) -> Res<Json<Value>> {
    if let Some(w) = why_not(&app, &user) {
        return Ok(Json(json!({ "available": false, "why": w })));
    }
    let (j, account) = session(&app, &user).await?;
    let q = j.call(USING, json!([["x:AppPassword/query", { "accountId": account }, "q"]])).await.map_err(gateway)?;
    let ids = q["methodResponses"][0][1]["ids"].as_array().cloned().unwrap_or_default();
    let mut list = Vec::new();
    if !ids.is_empty() {
        let g = j.call(USING, json!([["x:AppPassword/get", { "accountId": account, "ids": ids }, "g"]])).await.map_err(gateway)?;
        for p in g["methodResponses"][0][1]["list"].as_array().into_iter().flatten() {
            list.push(json!({ "id": p["id"], "description": p["description"], "created": p["createdAt"], "expires": p["expiresAt"] }));
        }
    }
    let mut out = settings(&app, &user);
    out["available"] = json!(true);
    out["list"] = json!(list);
    Ok(Json(out))
}

#[derive(Deserialize)]
struct New {
    #[serde(default)]
    description: String,
}

/// A new app password: the secret is in this answer only.
async fn create(State(app): State<Arc<App>>, user: User, Json(n): Json<New>) -> Res<Json<Value>> {
    let d = n.description.trim();
    let d = if d.is_empty() { "mail program" } else { d };
    if d.chars().count() > MAX_NAME || d.chars().any(char::is_control) {
        return Err(AppError::bad(format!("its name: one line, at most {MAX_NAME} characters")));
    }
    let (j, account) = session(&app, &user).await?;
    let v = j.call(USING, json!([["x:AppPassword/set", { "accountId": account, "create": { "p": { "description": d } } }, "s"]])).await.map_err(gateway)?;
    let c = &v["methodResponses"][0][1]["created"]["p"];
    let (Some(id), Some(secret)) = (c["id"].as_str(), c["secret"].as_str().filter(|s| !s.is_empty())) else {
        return Err(gateway("the mail server made no password".into()));
    };
    println!("mail app password {id} ({d:?}) made for {}", user.username);
    let mut out = settings(&app, &user);
    out["id"] = json!(id);
    out["description"] = json!(d);
    out["secret"] = json!(secret);
    Ok(Json(out))
}

/// An app password revoked: mail programs using it are refused from then on.
async fn remove(State(app): State<Arc<App>>, user: User, Path(id): Path<String>) -> Res<Json<Value>> {
    if id.is_empty() || id.len() > 40 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(AppError::bad("no such app password"));
    }
    let (j, account) = session(&app, &user).await?;
    let v = j.call(USING, json!([["x:AppPassword/set", { "accountId": account, "destroy": [id] }, "s"]])).await.map_err(gateway)?;
    let gone = v["methodResponses"][0][1]["destroyed"].as_array().is_some_and(|d| d.iter().any(|x| x == id.as_str()));
    if !gone {
        return Err(gateway("the mail server did not revoke it".into()));
    }
    println!("mail app password {id} revoked by {}", user.username);
    Ok(Json(json!({ "ok": true })))
}
