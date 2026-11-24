//! Browsing and changing files. A location (`loc`) is `home`, `m<id>` (one of
//! the user's external mounts) or `s<id>` (something shared with the user).

use std::{collections::HashMap, collections::HashSet, io, sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use futures_util::{stream, StreamExt, TryStreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::osada1::{
    auth::User,
    db, edge,
    error::{AppError, Res},
    http_util, md, mounts, pictures, quota,
    vfs::{local::Local, Backend, ByteStream, FsError, RelPath},
    App,
};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/ls/{loc}", get(ls))
        .route("/api/ls/{loc}/{*path}", get(ls))
        .route("/api/raw/{loc}", get(raw_get).put(raw_put))
        .route("/api/raw/{loc}/{*path}", get(raw_get).put(raw_put))
        .route("/api/mkdir", post(mkdir))
        .route("/api/rm", post(rm))
        .route("/api/mv", post(mv))
        .route("/api/md", post(render_md))
        .route("/api/space/{loc}", get(space))
        .route("/api/du/{loc}", get(du))
        .route("/api/groups/mine", get(my_groups))
        .route("/api/du/{loc}/{*path}", get(du))
}

/// What a (loc, path) pair points at, and what the user may do there.
pub struct Target {
    pub backend: Arc<dyn Backend>,
    /// path within the owner's storage
    pub path: RelPath,
    /// the part of `path` the user cannot leave: `/` for own storage, the item for a share
    pub root: RelPath,
    pub write: bool,
    pub owner: i64,
    pub mount: Option<i64>,
    pub root_name: String,
    /// A group's folder (D-U29): its id.
    pub group: Option<String>,
}

impl Target {
    /// `path` as the user sees it, relative to the location
    pub fn rel(&self) -> RelPath {
        self.path.strip_prefix(&self.root).unwrap_or_default()
    }
    /// another path in the same location
    pub fn at(&self, sub: &str) -> Res<RelPath> {
        Ok(self.root.join(&RelPath::parse(sub)?))
    }
    pub fn name(&self, p: &RelPath) -> String {
        if p == &self.root && !self.root.is_root() {
            return self.root_name.clone();
        }
        p.name().unwrap_or(&self.root_name).to_string()
    }
}

/// Names a storage for the edge's cache keys: a user's home or a mount.
pub fn store_id(owner: i64, mount: Option<i64>) -> String {
    mount.map_or_else(|| format!("u{owner}"), |m| format!("m{m}"))
}

pub fn home_backend(app: &App, username: &str) -> Arc<dyn Backend> {
    Arc::new(Local::new(app.user_root(username), app.tmp_dir()))
}

pub async fn owner_backend(app: &App, owner_name: &str, mount: Option<i64>) -> Res<Arc<dyn Backend>> {
    match mount {
        None => Ok(home_backend(app, owner_name)),
        Some(id) => mounts::backend(app, id),
    }
}

pub async fn resolve(app: &App, user: &User, loc: &str, sub: &str) -> Res<Target> {
    let sub = RelPath::parse(sub)?;
    let id = |s: &str| s[1..].parse::<i64>().map_err(|_| AppError::bad("bad location"));
    if loc == "home" {
        return Ok(Target {
            backend: home_backend(app, &user.username),
            path: sub,
            root: RelPath::root(),
            write: true,
            owner: user.id,
            mount: None,
            root_name: "home".into(),
            group: None,
        });
    }
    // a group's folder (D-U29): its members read and write it; kept with osada's files, here
    if let Some(gid) = loc.strip_prefix('g') {
        if gid.len() != 32 || !gid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AppError::not_found());
        }
        let sub_of: Option<String> = app.db.opt(|c| c.query_row("SELECT sub FROM users WHERE id = ?1", [user.id], |r| r.get(0)))?.flatten();
        let groups = match (&app.bridge, sub_of) {
            (Some(b), Some(s)) => b.groups_of(&s),
            _ => Vec::new(),
        };
        let (_, name) = groups.into_iter().find(|(id, _)| id == gid).ok_or_else(AppError::not_found)?;
        let dir = app.cfg.data.join("groups").join(gid);
        std::fs::create_dir_all(&dir).map_err(|e| AppError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Target {
            backend: Arc::new(Local::new(dir, app.tmp_dir())),
            path: sub,
            root: RelPath::root(),
            write: true,
            owner: user.id,
            mount: None,
            root_name: name,
            group: Some(gid.to_string()),
        });
    }
    if loc.starts_with('m') {
        let mid = id(loc)?;
        let name: String = app
            .db
            .opt(|c| c.query_row("SELECT name FROM mounts WHERE id = ?1 AND user_id = ?2", [mid, user.id], |r| r.get(0)))?
            .ok_or_else(AppError::not_found)?;
        return Ok(Target {
            backend: mounts::backend(app, mid)?,
            path: sub,
            root: RelPath::root(),
            write: true,
            owner: user.id,
            mount: Some(mid),
            root_name: name,
            group: None,
        });
    }
    if loc.starts_with('s') {
        let sid = id(loc)?;
        if sid < 0 {
            // shared out of a group's folder (D-U29)
            let (gid, path, is_dir, write): (String, String, bool, bool) = app
                .db
                .opt(|c| {
                    c.query_row(
                        "SELECT s.group_id, s.path, s.is_dir, s.can_write FROM group_shares s WHERE s.id = ?1 AND s.recipient_id = ?2",
                        [-sid, user.id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                })?
                .ok_or_else(AppError::not_found)?;
            if gid.len() != 32 || !gid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(AppError::not_found());
            }
            let dir = app.cfg.data.join("groups").join(&gid);
            if !dir.is_dir() {
                return Err(AppError::not_found());
            }
            let root = RelPath::parse(&path)?;
            if !is_dir && !sub.is_root() {
                return Err(AppError::not_found());
            }
            return Ok(Target {
                backend: Arc::new(Local::new(dir, app.tmp_dir())),
                path: root.join(&sub),
                root_name: root.name().unwrap_or("shared").to_string(),
                root,
                write,
                owner: user.id,
                mount: None,
                group: Some(gid),
            });
        }
        let (owner, owner_name, mount, path, is_dir, write): (i64, String, Option<i64>, String, bool, bool) = app
            .db
            .opt(|c| {
                c.query_row(
                    "SELECT s.owner_id, o.username, s.mount_id, s.path, s.is_dir, s.can_write
                     FROM shares s JOIN users o ON o.id = s.owner_id
                     WHERE s.id = ?1 AND s.recipient_id = ?2 AND o.disabled = 0",
                    [sid, user.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
            })?
            .ok_or_else(AppError::not_found)?;
        let root = RelPath::parse(&path)?;
        if !is_dir && !sub.is_root() {
            return Err(AppError::not_found());
        }
        return Ok(Target {
            backend: owner_backend(app, &owner_name, mount).await?,
            path: root.join(&sub),
            root_name: root.name().unwrap_or("shared").to_string(),
            root,
            write,
            owner,
            mount,
            group: None,
        });
    }
    Err(AppError::bad("bad location"))
}

fn loc_sub(p: &HashMap<String, String>) -> (String, String) {
    (p.get("loc").cloned().unwrap_or_default(), p.get("path").cloned().unwrap_or_default())
}

/// Names in folder `dir` that the owner has shared with users / by link.
fn marks(app: &App, t: &Target, dir: &RelPath) -> Res<(HashSet<String>, HashSet<String>)> {
    let prefix = if dir.is_root() { "/".to_string() } else { format!("{dir}/") };
    let mut out = (HashSet::new(), HashSet::new());
    app.db.with(|c| {
        for (table, set) in [("shares", &mut out.0), ("links", &mut out.1)] {
            let mut st = c.prepare(&format!(
                "SELECT DISTINCT path FROM {table} WHERE owner_id = ?1 AND IFNULL(mount_id, 0) = ?2"
            ))?;
            for p in st.query_map(rusqlite::params![t.owner, t.mount.unwrap_or(0)], |r| r.get::<_, String>(0))? {
                let p = p?;
                if let Some(rest) = p.strip_prefix(&prefix) {
                    if !rest.is_empty() && !rest.contains('/') {
                        set.insert(rest.to_string());
                    }
                }
            }
        }
        Ok(())
    })?;
    Ok(out)
}

async fn ls(State(app): State<Arc<App>>, user: User, Path(p): Path<HashMap<String, String>>) -> Res<Json<Value>> {
    let (loc, sub) = loc_sub(&p);
    let t = resolve(&app, &user, &loc, &sub).await?;
    let me = t.backend.stat(&t.path).await?;
    let base = json!({
        "loc": loc, "path": t.rel().to_string(), "write": t.write,
        "root_name": t.root_name, "owner": t.owner == user.id,
    });
    if !me.is_dir {
        let mut e = me.json();
        e["name"] = json!(t.name(&t.path));
        return Ok(Json(json!({ "file": e, "loc": base["loc"], "path": base["path"], "write": t.write, "root_name": t.root_name })));
    }
    let entries = t.backend.list(&t.path).await?;
    let (shared, linked) = if t.owner == user.id { marks(&app, &t, &t.path)? } else { Default::default() };
    let list: Vec<Value> = entries
        .iter()
        .map(|e| {
            let mut v = e.json();
            v["shared"] = json!(shared.contains(&e.name));
            v["linked"] = json!(linked.contains(&e.name));
            v
        })
        .collect();
    let mut out = base;
    out["entries"] = json!(list);
    Ok(Json(out))
}

#[derive(Deserialize)]
struct RawQ {
    dl: Option<String>,
}

/// Room on the storage behind a location, for the places list: {size, used, free} in bytes,
/// null when the storage cannot tell. A home with a limit is as big as its limit.
async fn space(State(app): State<Arc<App>>, user: User, Path(loc): Path<String>) -> Res<Json<Value>> {
    let t = resolve(&app, &user, &loc, "").await?;
    if loc == "home" {
        if let Some(limit) = quota::limit(&app, user.id)? {
            let used = app.quotas.used(&app, user.id).await?;
            return Ok(Json(json!({ "size": limit, "used": used, "free": limit.saturating_sub(used), "limit": true })));
        }
    }
    let s = t.backend.space(&t.root).await?;
    Ok(Json(s.map_or(Value::Null, |s| json!({ "size": s.size, "used": s.used, "free": s.free }))))
}

/// Sizes of the folders in a folder, `{sizes: {name: bytes}}`; the file list asks
/// once it shows. A folder that takes over 20 s to add up is left out.
async fn du(State(app): State<Arc<App>>, user: User, Path(p): Path<HashMap<String, String>>) -> Res<Json<Value>> {
    let (loc, sub) = loc_sub(&p);
    let t = resolve(&app, &user, &loc, &sub).await?;
    let store = store_id(t.owner, t.mount);
    let dirs: Vec<RelPath> =
        t.backend.list(&t.path).await?.into_iter().filter(|e| e.is_dir).filter_map(|e| t.path.child(&e.name).ok()).collect();
    let (app, t, store) = (&app, &t, &store);
    let sizes: serde_json::Map<String, Value> = stream::iter(dirs)
        .map(|d| async move {
            let n = tokio::time::timeout(Duration::from_secs(20), app.sizes.get(store, t.backend.as_ref(), &d)).await;
            (d.name().unwrap_or_default().to_string(), n)
        })
        .buffer_unordered(4)
        .filter_map(|(name, n)| async move {
            match n {
                Ok(Ok(n)) => Some((name, json!(n))),
                _ => None,
            }
        })
        .collect()
        .await;
    Ok(Json(json!({ "sizes": sizes })))
}

async fn raw_get(
    State(app): State<Arc<App>>,
    user: User,
    Path(p): Path<HashMap<String, String>>,
    Query(q): Query<RawQ>,
    headers: HeaderMap,
) -> Res<Response> {
    let (loc, sub) = loc_sub(&p);
    let t = resolve(&app, &user, &loc, &sub).await?;
    let (e, r) = t.backend.open(&t.path).await?;
    note(3, "read", &user.username, t.owner, t.mount, &t.path, None, Some(e.size), "web");
    let probe = edge::probe(&app, &headers, &store_id(t.owner, t.mount), &t.path);
    Ok(http_util::serve(e, r, &headers, &t.name(&t.path), q.dl.is_some(), false, probe).await)
}

#[derive(Deserialize)]
struct PutQ {
    /// `new`: fail if the file exists
    mode: Option<String>,
    /// create missing parent folders (folder uploads)
    mkdirs: Option<String>,
}

pub async fn mkdirs(b: &dyn Backend, root: &RelPath, dir: &RelPath) -> Res<()> {
    let Some(rest) = dir.strip_prefix(root) else { return Err(AppError::forbidden()) };
    let mut cur = root.clone();
    for s in rest.segs() {
        cur = cur.child(s)?;
        match b.stat(&cur).await {
            Ok(e) if e.is_dir => {}
            Ok(_) => return Err(FsError::NotDir.into()),
            Err(FsError::NotFound) => match b.mkdir(&cur).await {
                Ok(()) | Err(FsError::Exists) => {}
                Err(e) => return Err(e.into()),
            },
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

async fn raw_put(
    State(app): State<Arc<App>>,
    user: User,
    Path(p): Path<HashMap<String, String>>,
    Query(q): Query<PutQ>,
    headers: HeaderMap,
    body: Body,
) -> Res<Json<Value>> {
    let (loc, sub) = loc_sub(&p);
    let t = resolve(&app, &user, &loc, &sub).await?;
    if !t.write {
        return Err(AppError::forbidden());
    }
    if t.path.is_root() {
        return Err(AppError::bad("no file name"));
    }
    let cur = t.backend.stat(&t.path).await;
    if cur.is_ok() && q.mode.as_deref() == Some("new") {
        return Err(AppError::conflict("already exists"));
    }
    // editors send the etag they loaded; someone else saving meanwhile is a conflict
    if let Some(want) = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()) {
        let changed = match &cur {
            Ok(e) => want != "*" && e.etag() != want,
            Err(_) => want != "*",
        };
        if changed {
            return Err(AppError::new(StatusCode::PRECONDITION_FAILED, "the file changed since it was opened"));
        }
    }
    let replaces = cur.as_ref().ok().filter(|e| !e.is_dir).map_or(0, |e| e.size);
    let w = quota::start(&app, t.owner, t.mount, replaces, quota::length(&headers)).await?;
    if q.mkdirs.is_some() {
        if let Some(parent) = t.path.parent() {
            mkdirs(t.backend.as_ref(), &t.root, &parent).await?;
        }
    }
    let mut data: ByteStream = Box::pin(body.into_data_stream().map_err(io::Error::other));
    if let Some(w) = &w {
        data = w.wrap(data);
    }
    let e = t.backend.write(&t.path, data).await.map_err(|e| quota::why(&w, e))?;
    if let Some(w) = w {
        w.stored();
    }
    app.sizes.touch(&store_id(t.owner, t.mount), &t.path);
    note(1, if cur.is_ok() { "write" } else { "create" }, &user.username, t.owner, t.mount, &t.path, None, Some(e.size), "web");
    Ok(Json(e.json()))
}

#[derive(Deserialize)]
struct MkdirReq {
    loc: String,
    path: String,
}

async fn mkdir(State(app): State<Arc<App>>, user: User, Json(req): Json<MkdirReq>) -> Res<Json<Value>> {
    let t = resolve(&app, &user, &req.loc, &req.path).await?;
    if !t.write || t.path == t.root {
        return Err(AppError::forbidden());
    }
    t.backend.mkdir(&t.path).await?;
    app.sizes.touch(&store_id(t.owner, t.mount), &t.path);
    note(1, "mkdir", &user.username, t.owner, t.mount, &t.path, None, None, "web");
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct RmReq {
    loc: String,
    paths: Vec<String>,
}

async fn rm(State(app): State<Arc<App>>, user: User, Json(req): Json<RmReq>) -> Res<Json<Value>> {
    let t = resolve(&app, &user, &req.loc, "").await?;
    if !t.write {
        return Err(AppError::forbidden());
    }
    for p in &req.paths {
        let at = t.at(p)?;
        remove_item(&app, &t, &at).await?;
        note(1, "delete", &user.username, t.owner, t.mount, &at, None, None, "web");
    }
    changed(&app, &t);
    Ok(Json(json!({ "ok": true })))
}

/// Deletes a file, or a folder with everything in it; shares, links and API
/// keys on it go too. Call [`changed`] after the last one.
pub async fn remove_item(app: &App, t: &Target, path: &RelPath) -> Res<()> {
    if *path == t.root {
        return Err(AppError::bad("cannot delete the root of this location"));
    }
    t.backend.remove(path).await?;
    app.sizes.touch(&store_id(t.owner, t.mount), path);
    if t.mount.is_none() {
        app.quotas.forget(t.owner);
    }
    let s = path.to_string();
    app.db.with(|c| db::unshare(c, t.owner, t.mount, &s))?;
    Ok(())
}

/// Renames or moves within one location; shares, links and API keys follow.
/// Call [`changed`] after the last one.
pub async fn move_item(app: &App, t: &Target, from: &RelPath, to: &RelPath) -> Res<()> {
    if *from == t.root || *to == t.root {
        return Err(AppError::bad("cannot move the root of this location"));
    }
    if to.starts_with(from) {
        return Err(AppError::bad("cannot move a folder into itself"));
    }
    t.backend.rename(from, to).await?;
    let store = store_id(t.owner, t.mount);
    app.sizes.touch(&store, from);
    app.sizes.touch(&store, to);
    let (f, n) = (from.to_string(), to.to_string());
    app.db.with(|c| db::repath(c, t.owner, t.mount, &f, &n))?;
    Ok(())
}

/// A line of the `files` log (D-U31): `op` on `path` in `owner`'s storage (or a mount of theirs)
/// by `user`, through `how`. Level 1: changes; 2: what a public link gives out; 3: reads.
#[allow(clippy::too_many_arguments)]
pub fn note(level: u8, op: &str, user: &str, owner: i64, mount: Option<i64>, path: &RelPath, to: Option<&RelPath>, bytes: Option<u64>, how: &str) {
    if !crate::logstore::on("files", level) {
        return;
    }
    let p = |x: &RelPath| format!("/{}", x.to_string().trim_start_matches('/'));
    crate::logstore::record(
        "files",
        level,
        json!({ "op": op, "user": user, "path": p(path), "to": to.map(p), "bytes": bytes, "how": how, "owner": owner, "mount": mount }),
    );
}

/// After removes and moves: the pictures page lists homes only.
pub fn changed(app: &Arc<App>, t: &Target) {
    if t.mount.is_none() {
        pictures::kick(app, Duration::ZERO);
    }
}

#[derive(Deserialize)]
struct Move {
    from: String,
    to: String,
}

#[derive(Deserialize)]
struct MvReq {
    loc: String,
    moves: Vec<Move>,
}

async fn mv(State(app): State<Arc<App>>, user: User, Json(req): Json<MvReq>) -> Res<Json<Value>> {
    let t = resolve(&app, &user, &req.loc, "").await?;
    if !t.write {
        return Err(AppError::forbidden());
    }
    for m in &req.moves {
        let (from, to) = (t.at(&m.from)?, t.at(&m.to)?);
        move_item(&app, &t, &from, &to).await?;
        note(1, "move", &user.username, t.owner, t.mount, &from, Some(&to), None, "web");
    }
    changed(&app, &t);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct MdReq {
    text: String,
    /// prefix for relative image urls (raw files) and links (the viewer)
    img_base: Option<String>,
    link_base: Option<String>,
}

async fn render_md(State(app): State<Arc<App>>, _user: User, Json(req): Json<MdReq>) -> Json<Value> {
    // references to wiki articles show whether the article is there
    let html = crate::osada1::wiki::mark(&app, md::render(&req.text, req.img_base.as_deref(), req.link_base.as_deref()));
    Json(json!({ "html": html }))
}

/// The groups the user is in: their folders (D-U29), for the places list.
async fn my_groups(State(app): State<Arc<App>>, user: User) -> Res<Json<Value>> {
    let sub: Option<String> = app.db.opt(|c| c.query_row("SELECT sub FROM users WHERE id = ?1", [user.id], |r| r.get(0)))?.flatten();
    let list = match (&app.bridge, sub) {
        (Some(b), Some(s)) => b.groups_of(&s),
        _ => Vec::new(),
    };
    Ok(Json(json!(list.into_iter().map(|(id, name)| json!({ "loc": format!("g{id}"), "id": id, "name": name })).collect::<Vec<_>>())))
}
