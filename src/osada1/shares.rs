//! Sharing with other users (`shares`) and public links (`links`, served by public.rs).

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    routing::{delete, get},
    Json, Router,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::osada1::{
    auth::User,
    crypto,
    db::now,
    error::{AppError, Res},
    files::{self, Target},
    App,
};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/shares", get(all_shares).post(create_share))
        .route("/api/shares/item", get(item))
        .route("/api/shares/{id}", delete(delete_share))
        .route("/api/links", get(all_links).post(create_link))
        .route("/api/links/{id}", delete(delete_link))
}

/// An item the user owns and may share: in their home or on one of their mounts.
async fn own_item(app: &App, user: &User, loc: &str, path: &str) -> Res<(Target, bool)> {
    if loc.starts_with('s') {
        return Err(AppError::bad("things shared with you cannot be shared on"));
    }
    // a group's folder (D-U29): any member shares on from it (resolve checks membership)
    let t = files::resolve(app, user, loc, path).await?;
    if t.path.is_root() {
        return Err(AppError::bad("share a folder or file, not the whole location"));
    }
    let e = t.backend.stat(&t.path).await?;
    Ok((t, e.is_dir))
}

fn loc_of(mount: Option<i64>) -> String {
    mount.map_or("home".into(), |m| format!("m{m}"))
}

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn link_url(app: &App, token: &str) -> String {
    format!("{}/s/{token}", app.cfg.public_url.as_deref().unwrap_or(""))
}

fn link_json(app: &App, r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let mount: Option<i64> = r.get(2)?;
    let path: String = r.get(3)?;
    let token: String = r.get(1)?;
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "token": token, "url": link_url(app, &token),
        "loc": loc_of(mount), "loc_name": r.get::<_, Option<String>>(4)?.unwrap_or("home".into()),
        "path": path, "name": name_of(&path), "dir": r.get::<_, bool>(5)?,
        "expires": r.get::<_, Option<i64>>(6)?, "created": r.get::<_, i64>(7)?, "hits": r.get::<_, i64>(8)?,
        "sent_to": r.get::<_, Option<String>>(9)?,
    }))
}

const LINK_COLS: &str = "l.id, l.token, l.mount_id, l.path, m.name, l.is_dir, l.expires, l.created, l.hits, l.sent_to";

fn share_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let mount: Option<i64> = r.get(1)?;
    let path: String = r.get(3)?;
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "loc": loc_of(mount),
        "loc_name": r.get::<_, Option<String>>(2)?.unwrap_or("home".into()),
        "path": path, "name": name_of(&path), "dir": r.get::<_, bool>(4)?,
        "user": r.get::<_, String>(5)?, "write": r.get::<_, bool>(6)?, "created": r.get::<_, i64>(7)?,
    }))
}

const SHARE_COLS: &str = "s.id, s.mount_id, m.name, s.path, s.is_dir, u.username, s.can_write, s.created";

/// Whether `user` is a member of group `gid` now (D-U29).
pub fn is_member(app: &App, user: &User, gid: &str) -> bool {
    let sub: Option<String> = app.db.opt(|c| c.query_row("SELECT sub FROM users WHERE id = ?1", [user.id], |r| r.get(0))).ok().flatten().flatten();
    match (&app.bridge, sub) {
        (Some(b), Some(s)) => b.groups_of(&s).iter().any(|(id, _)| id == gid),
        _ => false,
    }
}

/// The name of group `gid` as one of its members sees it.
fn group_name(app: &App, user: &User, gid: &str) -> String {
    let sub: Option<String> = app.db.opt(|c| c.query_row("SELECT sub FROM users WHERE id = ?1", [user.id], |r| r.get(0))).ok().flatten().flatten();
    match (&app.bridge, sub) {
        (Some(b), Some(s)) => b.groups_of(&s).into_iter().find(|(id, _)| id == gid).map(|(_, n)| n).unwrap_or_else(|| "a group".into()),
        _ => "a group".into(),
    }
}

/// A share out of a group's folder, as share_json has it; its id is negative (D-U29).
fn group_share_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let path: String = r.get(2)?;
    let gid: String = r.get(1)?;
    Ok(json!({
        "id": -r.get::<_, i64>(0)?, "loc": format!("g{gid}"), "loc_name": Value::Null, "group": gid,
        "path": path, "name": name_of(&path), "dir": r.get::<_, bool>(3)?,
        "user": r.get::<_, String>(4)?, "write": r.get::<_, bool>(5)?, "created": r.get::<_, i64>(6)?,
    }))
}

const GSHARE_COLS: &str = "s.id, s.group_id, s.path, s.is_dir, u.username, s.can_write, s.created";

fn group_link_json(app: &App, r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let path: String = r.get(3)?;
    let token: String = r.get(1)?;
    let gid: String = r.get(2)?;
    Ok(json!({
        "id": -r.get::<_, i64>(0)?, "token": token, "url": link_url(app, &token), "loc": format!("g{gid}"), "loc_name": Value::Null, "group": gid,
        "path": path, "name": name_of(&path), "dir": r.get::<_, bool>(4)?,
        "expires": r.get::<_, Option<i64>>(5)?, "created": r.get::<_, i64>(6)?, "hits": r.get::<_, i64>(7)?,
        "sent_to": r.get::<_, Option<String>>(8)?,
    }))
}

const GLINK_COLS: &str = "l.id, l.token, l.group_id, l.path, l.is_dir, l.expires, l.created, l.hits, l.sent_to";

/// Each share's recipient with their UPN and display name (Round 21).
fn with_people(app: &App, shares: &mut [Value]) {
    let people = crate::osada1::users::people_index(app);
    for s in shares.iter_mut() {
        if let Some(p) = s["user"].as_str().and_then(|u| people.iter().find(|p| p.username == u)) {
            s["upn"] = json!(p.upn);
            s["display"] = json!(p.display);
        }
    }
}

async fn all_shares(State(app): State<Arc<App>>, user: User) -> Res<Json<Value>> {
    let (mut by_me, mut with_me) = app.db.with(|c| {
        let mut st = c.prepare(&format!(
            "SELECT {SHARE_COLS} FROM shares s JOIN users u ON u.id = s.recipient_id
             LEFT JOIN mounts m ON m.id = s.mount_id WHERE s.owner_id = ?1 ORDER BY s.path, u.username"
        ))?;
        let by_me = st.query_map([user.id], share_json)?.collect::<rusqlite::Result<Vec<_>>>()?;
        // for the recipient, "user" is the owner, and loc is the share itself
        let mut st = c.prepare(&format!(
            "SELECT {SHARE_COLS} FROM shares s JOIN users u ON u.id = s.owner_id
             LEFT JOIN mounts m ON m.id = s.mount_id WHERE s.recipient_id = ?1 AND u.disabled = 0
             ORDER BY s.created DESC"
        ))?;
        let with_me = st
            .query_map([user.id], |r| {
                let mut v = share_json(r)?;
                v["loc"] = json!(format!("s{}", v["id"]));
                v["loc_name"] = Value::Null;
                Ok(v)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((by_me, with_me))
    })?;
    // out of group folders (D-U29): those this user made, and those made for them
    let (gby, gwith) = app.db.with(|c| {
        let mut st = c.prepare(&format!("SELECT {GSHARE_COLS} FROM group_shares s JOIN users u ON u.id = s.recipient_id WHERE s.by_id = ?1 ORDER BY s.path, u.username"))?;
        let a = st.query_map([user.id], group_share_json)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut st = c.prepare(&format!(
            "SELECT {GSHARE_COLS} FROM group_shares s JOIN users u ON u.id = s.by_id WHERE s.recipient_id = ?1 AND u.disabled = 0 ORDER BY s.created DESC"
        ))?;
        let b = st.query_map([user.id], group_share_json)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((a, b))
    })?;
    for mut v in gby {
        v["loc_name"] = json!(group_name(&app, &user, v["group"].as_str().unwrap_or("")));
        by_me.push(v);
    }
    for mut v in gwith {
        v["loc"] = json!(format!("s{}", v["id"]));
        with_me.push(v);
    }
    Ok(Json(json!({ "by_me": by_me, "with_me": with_me })))
}

#[derive(Deserialize)]
struct ItemQ {
    loc: String,
    path: String,
}

/// Shares and links of one item, for the share dialog.
async fn item(State(app): State<Arc<App>>, user: User, Query(q): Query<ItemQ>) -> Res<Json<Value>> {
    let (t, is_dir) = own_item(&app, &user, &q.loc, &q.path).await?;
    let path = t.path.to_string();
    if let Some(gid) = &t.group {
        let (shares, links) = app.db.with(|c| {
            let mut st = c.prepare(&format!("SELECT {GSHARE_COLS} FROM group_shares s JOIN users u ON u.id = s.recipient_id WHERE s.group_id = ?1 AND s.path = ?2 ORDER BY u.username"))?;
            let shares = st.query_map(params![gid, path], group_share_json)?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut st = c.prepare(&format!("SELECT {GLINK_COLS} FROM group_links l WHERE l.group_id = ?1 AND l.path = ?2 ORDER BY l.created"))?;
            let links = st.query_map(params![gid, path], |r| group_link_json(&app, r))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((shares, links))
        })?;
        let mut shares = shares;
        with_people(&app, &mut shares);
        return Ok(Json(json!({ "shares": shares, "links": links, "dir": is_dir, "path": path })));
    }
    let (mut shares, links) = app.db.with(|c| {
        let mut st = c.prepare(&format!(
            "SELECT {SHARE_COLS} FROM shares s JOIN users u ON u.id = s.recipient_id
             LEFT JOIN mounts m ON m.id = s.mount_id
             WHERE s.owner_id = ?1 AND IFNULL(s.mount_id, 0) = ?2 AND s.path = ?3 ORDER BY u.username"
        ))?;
        let shares = st.query_map(params![user.id, t.mount.unwrap_or(0), path], share_json)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut st = c.prepare(&format!(
            "SELECT {LINK_COLS} FROM links l LEFT JOIN mounts m ON m.id = l.mount_id
             WHERE l.owner_id = ?1 AND IFNULL(l.mount_id, 0) = ?2 AND l.path = ?3 ORDER BY l.created"
        ))?;
        let links = st
            .query_map(params![user.id, t.mount.unwrap_or(0), path], |r| link_json(&app, r))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((shares, links))
    })?;
    with_people(&app, &mut shares);
    let federated = crate::osada1::federation::out_for_item(&app, user.id, t.mount, &path)?;
    Ok(Json(json!({ "shares": shares, "links": links, "federated": federated, "dir": is_dir, "path": path })))
}

#[derive(Deserialize)]
struct ShareReq {
    loc: String,
    path: String,
    user: String,
    #[serde(default)]
    write: bool,
}

async fn create_share(State(app): State<Arc<App>>, user: User, Json(req): Json<ShareReq>) -> Res<Json<Value>> {
    let (t, is_dir) = own_item(&app, &user, &req.loc, &req.path).await?;
    // someone of a federated instance (Round 22): view-only, by their instance
    if crate::osada1::users::find_person(&app, &req.user).is_none() && crate::osada1::federation::peer_for(&app, req.user.trim()).await.is_some() {
        return Ok(Json(crate::osada1::federation::offer(&app, &user, &t, is_dir, &req.user).await?));
    }
    // a user name, or (Round 21) a UPN or another address of theirs
    let name = crate::osada1::users::find_person(&app, &req.user).map(|p| p.username).unwrap_or_else(|| req.user.trim().to_string());
    // someone of 0.2's directory this side has no row for yet: made now, as sync_users would
    if let Some(b) = &app.bridge {
        let has: bool = app.db.opt(|c| c.query_row("SELECT 1 FROM users WHERE username = ?1", [&name], |_| Ok(true)))?.unwrap_or(false);
        if !has {
            if let Some(p) = b.people().into_iter().find(|p| p.name == name && !p.disabled) {
                crate::osada1::auth::ensure_user(&app, &p)?;
            }
        }
    }
    let to: i64 = app
        .db
        .opt(|c| c.query_row("SELECT id FROM users WHERE username = ?1 AND disabled = 0", [name], |r| r.get(0)))?
        .ok_or_else(|| AppError::bad("no such user"))?;
    if to == user.id {
        return Err(AppError::bad("that is you"));
    }
    let path = t.path.to_string();
    if let Some(gid) = &t.group {
        let id = app.db.with(|c| {
            c.execute(
                "INSERT INTO group_shares (group_id, path, is_dir, recipient_id, can_write, by_id, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (group_id, path, recipient_id) DO UPDATE SET can_write = excluded.can_write, is_dir = excluded.is_dir",
                params![gid, path, is_dir, to, req.write, user.id, now()],
            )?;
            c.query_row("SELECT id FROM group_shares WHERE group_id = ?1 AND path = ?2 AND recipient_id = ?3", params![gid, path, to], |r| r.get::<_, i64>(0))
        })?;
        return Ok(Json(json!({ "id": -id })));
    }
    let id = app.db.with(|c| {
        let n = c.execute(
            "UPDATE shares SET can_write = ?5, is_dir = ?6
             WHERE owner_id = ?1 AND IFNULL(mount_id, 0) = IFNULL(?2, 0) AND path = ?3 AND recipient_id = ?4",
            params![user.id, t.mount, path, to, req.write, is_dir],
        )?;
        if n == 0 {
            c.execute(
                "INSERT INTO shares (owner_id, mount_id, path, is_dir, recipient_id, can_write, created)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![user.id, t.mount, path, is_dir, to, req.write, now()],
            )?;
        }
        c.query_row(
            "SELECT id FROM shares WHERE owner_id = ?1 AND IFNULL(mount_id, 0) = IFNULL(?2, 0) AND path = ?3 AND recipient_id = ?4",
            params![user.id, t.mount, path, to],
            |r| r.get::<_, i64>(0),
        )
    })?;
    Ok(Json(json!({ "id": id })))
}

/// The owner revokes a share, or the recipient drops it.
async fn delete_share(State(app): State<Arc<App>>, user: User, Path(id): Path<i64>) -> Res<Json<Value>> {
    if id < 0 {
        // out of a group's folder: its recipient, who made it, or a member now
        let row: Option<(String, i64, Option<i64>)> = app.db.opt(|c| c.query_row("SELECT group_id, recipient_id, by_id FROM group_shares WHERE id = ?1", [-id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))))?;
        let (gid, to, by) = row.ok_or_else(AppError::not_found)?;
        if to != user.id && by != Some(user.id) && !is_member(&app, &user, &gid) {
            return Err(AppError::not_found());
        }
        app.db.with(|c| c.execute("DELETE FROM group_shares WHERE id = ?1", [-id]))?;
        return Ok(Json(json!({ "ok": true })));
    }
    let n = app.db.with(|c| {
        c.execute("DELETE FROM shares WHERE id = ?1 AND (owner_id = ?2 OR recipient_id = ?2)", [id, user.id])
    })?;
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(Json(json!({ "ok": true })))
}

async fn all_links(State(app): State<Arc<App>>, user: User) -> Res<Json<Value>> {
    let links = app.db.with(|c| {
        let mut st = c.prepare(&format!(
            "SELECT {LINK_COLS} FROM links l LEFT JOIN mounts m ON m.id = l.mount_id
             WHERE l.owner_id = ?1 ORDER BY l.created DESC"
        ))?;
        let v = st.query_map([user.id], |r| link_json(&app, r))?.collect::<rusqlite::Result<Vec<_>>>();
        v
    })?;
    let mut links = links;
    let glinks = app.db.with(|c| {
        let mut st = c.prepare(&format!("SELECT {GLINK_COLS} FROM group_links l WHERE l.by_id = ?1 ORDER BY l.created DESC"))?;
        let v = st.query_map([user.id], |r| group_link_json(&app, r))?.collect::<rusqlite::Result<Vec<_>>>();
        v
    })?;
    for mut v in glinks {
        v["loc_name"] = json!(group_name(&app, &user, v["group"].as_str().unwrap_or("")));
        links.push(v);
    }
    Ok(Json(json!(links)))
}

#[derive(Deserialize)]
struct LinkReq {
    loc: String,
    path: String,
    /// valid for this many days; none = until removed
    days: Option<i64>,
    /// Round 21: mail the link to this address (public email: nobody of this instance), from the
    /// person's own mailbox; the link says to whom it went
    #[serde(default)]
    send_to: Option<String>,
}

async fn create_link(State(app): State<Arc<App>>, user: User, Json(req): Json<LinkReq>) -> Res<Json<Value>> {
    let (t, is_dir) = own_item(&app, &user, &req.loc, &req.path).await?;
    let send_to = match req.send_to.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(a) => {
            let a = crate::osada1::sendmail::address(a).ok_or_else(|| AppError::bad("that is not an e-mail address"))?;
            if let Some(p) = crate::osada1::users::find_person(&app, &a) {
                return Err(AppError::bad(format!("{a} is {} here: share with them instead", p.username)));
            }
            Some(a)
        }
    };
    let token = crypto::random_token(16);
    let expires = req.days.filter(|d| *d > 0).map(|d| now() + d * 86400);
    let path = t.path.to_string();
    if let Some(gid) = &t.group {
        let id = app.db.with(|c| {
            c.execute(
                "INSERT INTO group_links (token, group_id, path, is_dir, expires, by_id, created, sent_to) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![token, gid, path, is_dir, expires, user.id, now(), send_to],
            )?;
            Ok(c.last_insert_rowid())
        })?;
        if let Some(a) = &send_to {
            if let Err(why) = mail_link(&app, &user, a, &path, &token, expires).await {
                let _ = app.db.with(|c| c.execute("DELETE FROM group_links WHERE id = ?1", [id]));
                return Err(AppError::bad(format!("not sent to {a}: {why}")));
            }
        }
        return Ok(Json(json!({ "id": -id, "token": token, "url": link_url(&app, &token), "sent_to": send_to })));
    }
    let id = app.db.with(|c| {
        c.execute(
            "INSERT INTO links (token, owner_id, mount_id, path, is_dir, expires, created, sent_to) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![token, user.id, t.mount, path, is_dir, expires, now(), send_to],
        )?;
        Ok(c.last_insert_rowid())
    })?;
    if let Some(a) = &send_to {
        if let Err(why) = mail_link(&app, &user, a, &path, &token, expires).await {
            let _ = app.db.with(|c| c.execute("DELETE FROM links WHERE id = ?1", [id]));
            return Err(AppError::bad(format!("not sent to {a}: {why}")));
        }
    }
    Ok(Json(json!({ "id": id, "token": token, "url": link_url(&app, &token), "sent_to": send_to })))
}

/// The link, mailed from the person's own mailbox (Round 21).
async fn mail_link(app: &App, user: &User, to: &str, path: &str, token: &str, expires: Option<i64>) -> Result<(), String> {
    let who = crate::osada1::users::people_index(app).into_iter().find(|p| p.username == user.username).map(|p| p.display).unwrap_or_else(|| user.username.clone());
    let l = crate::osada1::sendmail::link_letter(to, &who, name_of(path), &link_url(app, token), expires, &crate::osada1::users::public_host(app));
    crate::osada1::sendmail::send_as(app, user, &who, &l).await
}

async fn delete_link(State(app): State<Arc<App>>, user: User, Path(id): Path<i64>) -> Res<Json<Value>> {
    if id < 0 {
        let row: Option<(String, Option<i64>)> = app.db.opt(|c| c.query_row("SELECT group_id, by_id FROM group_links WHERE id = ?1", [-id], |r| Ok((r.get(0)?, r.get(1)?))))?;
        let (gid, by) = row.ok_or_else(AppError::not_found)?;
        if by != Some(user.id) && !is_member(&app, &user, &gid) {
            return Err(AppError::not_found());
        }
        app.db.with(|c| c.execute("DELETE FROM group_links WHERE id = ?1", [-id]))?;
        return Ok(Json(json!({ "ok": true })));
    }
    let n = app.db.with(|c| c.execute("DELETE FROM links WHERE id = ?1 AND owner_id = ?2", [id, user.id]))?;
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(Json(json!({ "ok": true })))
}
