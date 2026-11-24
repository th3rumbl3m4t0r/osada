//! `osada serve` from osada 0.1's environment (/etc/osada/osada.env): the
//! drop-in upgrade (docs/PLAN-0.2.0.md, D-U2 to D-U5).
//!
//! - osada 0.1 runs whole, as the module `osada1`, on its data as it is.
//! - The node lives in `$OSADA_DATA/node/`. On the first start it is a
//!   cluster of one (every role) and osada 0.1's accounts are brought over:
//!   name, argon2 hash, disabled or not, the OIDC subject as the user id,
//!   administrators with the role admin over /.
//! - Sign-in, sessions and passwords are the node's (auth::Bridge); osada
//!   0.1's users table is kept in step, its password hashes too, so that osada
//!   0.1 could be started again on this data (the rollback).
//! - The node's pages and API are merged into osada 0.1's listeners (LAN,
//!   https, edge), with its cookie `osada`.
//!
//! Environment besides osada 0.1's: OSADA_NODE (the node's name; the host
//! name's first label), OSADA_LINK_BIND (UDP for links to other nodes,
//! 0.0.0.0:7441), OSADA_PEERS (`name@host:port`, comma-separated),
//! OSADA_COOKIE (the session cookie's name, `osada`); the mail server outside
//! osada (Stalwart) as a mail instance (D-U16): OSADA_MAIL_NODE (the node it
//! runs on: made once that node is in the cluster), OSADA_MAIL_HOST (its
//! name), OSADA_MAIL_UPSTREAM (its TLS on that node, `tls://127.0.0.1:8443`),
//! OSADA_MAIL_INSTANCE (`mail`).

use std::{
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use futures_util::{FutureExt, future::BoxFuture};
use serde_json::json;

use crate::{
    cluster::Cluster,
    directory::User,
    kv::{Kv, sqlite::SqliteKv},
    link::Link,
    node::{Actor, Imported, Node, Session},
    pki::{Ca, NodeCert},
    placement::NodeId,
    osada1::{
        self, App,
        auth::{self, Bridge, Person},
        crypto, edge,
    },
    web,
};

const LINK_BIND: &str = "0.0.0.0:7441";

fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn s(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The node's folder in osada 0.1's data.
pub fn node_dir(data: &Path) -> PathBuf {
    data.join("node")
}

/// OSADA_NODE, else the host name's first label.
fn node_name() -> Result<String, String> {
    let name = var("OSADA_NODE").unwrap_or_else(|| {
        let h = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
        h.trim().split('.').next().unwrap_or_default().to_lowercase()
    });
    if !crate::placement::valid_node_label(&name) {
        return Err(format!("{name:?} is not a node name: set OSADA_NODE (letters, digits, '-', '_' and '.')"));
    }
    Ok(name)
}

/// A cluster of one: this node with every role, its CA and certificate.
fn init(dir: &Path, name: &str) -> Result<(), String> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let kv: Arc<dyn Kv> = Arc::new(SqliteKv::open(&dir.join("store.db")).map_err(s)?);
    // the id is a GUID; `name` (OSADA_NODE or the host's) is what people see, and editable (D-U19)
    let id = NodeId::fresh();
    Cluster::init_named(kv, id.clone(), Some(name.into()), now_ms()).map_err(s)?;
    let ca = Ca::new().map_err(s)?;
    ca.save(dir).map_err(s)?;
    ca.issue(&id.0).and_then(|cert| cert.save(dir)).map_err(s)?;
    println!("{name} ({id}): a new cluster in {}, CA {}", dir.display(), ca.fingerprint().map_err(s)?);
    Ok(())
}

/// osada 0.1's accounts, with an OIDC subject each (made now where none was).
fn legacy_accounts(app: &App) -> Result<Vec<Imported>, String> {
    let e = |e: osada1::error::AppError| e.msg;
    app.db
        .with(|c| {
            let fresh: Vec<i64> = {
                let mut st = c.prepare("SELECT id FROM users WHERE sub IS NULL OR sub = ''")?;
                let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
                v
            };
            for id in fresh {
                c.execute("UPDATE users SET sub = ?1 WHERE id = ?2", rusqlite::params![crypto::random_token(16), id])?;
            }
            let mut st = c.prepare("SELECT sub, username, pw_hash, is_admin, disabled, remote_cidrs FROM users ORDER BY id")?;
            let v = st
                .query_map([], |r| {
                    Ok(Imported { id: r.get(0)?, name: r.get(1)?, hash: Some(r.get(2)?), admin: r.get(3)?, disabled: r.get(4)?, outside: r.get(5)? })
                })?
                .collect::<rusqlite::Result<Vec<_>>>();
            v
        })
        .map_err(e)
}

/// Brings osada 0.1's accounts over, once the root OU is ready here; notes
/// it in `<node>/imported`.
async fn import(node: &Arc<Node>, dir: &Path, accounts: Vec<Imported>) -> Result<(), String> {
    let mut waited = 0;
    while node.root_ready().is_none() {
        if waited > 600 {
            return Err("the root OU did not become ready in 2 minutes".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        waited += 1;
    }
    let mut report = String::new();
    let (mut done, mut failed) = (0, 0);
    for a in &accounts {
        match node.import_user(a).await {
            Ok(true) => {
                done += 1;
                report += &format!("imported {} ({}){}\n", a.name, a.id, if a.admin { ", admin over /" } else { "" });
            }
            Ok(false) => report += &format!("there already: {}\n", a.name),
            Err(e) => {
                failed += 1;
                report += &format!("NOT imported {}: {e}\n", a.name);
                eprintln!("import of {}: {e}", a.name);
            }
        }
    }
    println!("{}: {done} accounts of osada 0.1 imported, {failed} not", node.me());
    std::fs::write(dir.join("imported"), report).map_err(|e| e.to_string())
}

/// osada 0.1's outside networks into the directory, for accounts imported
/// before they were the directory's (D-U15); once, before the users table
/// follows the directory (it would empty them), noted in `<node>/outside-imported`.
async fn import_outside(node: &Arc<Node>, dir: &Path, app: &App) {
    let marker = dir.join("outside-imported");
    if marker.exists() {
        return;
    }
    let list: Vec<(String, String)> = app
        .db
        .with(|c| {
            let mut st = c.prepare("SELECT sub, remote_cidrs FROM users WHERE sub IS NOT NULL AND sub != '' AND remote_cidrs != ''")?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>();
            v
        })
        .unwrap_or_default();
    for _ in 0..600 {
        if node.root_ready().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    match node.import_outside(&list).await {
        Ok(n) => {
            let _ = std::fs::write(&marker, format!("{n} accounts' outside networks from osada 0.1\n"));
        }
        Err(e) => eprintln!("{}: outside networks from osada 0.1: {e} (tried again at the next start)", node.me()),
    }
}

/// osada 0.1's apptab (OSADA_APPTAB_*) as the proxy module `apptab`, with access for the
/// accounts it was on for (D-U7); once, noted in `<node>/proxy-imported`. From then on the node
/// control has it: the environment is not read for it again.
async fn import_apptab(node: &Arc<Node>, dir: &Path, app: &App) {
    let marker = dir.join("proxy-imported");
    if marker.exists() {
        return;
    }
    let (Some(host), Some(upstream)) = (app.cfg.apptab_host.clone(), app.cfg.apptab_upstream.clone()) else {
        let _ = std::fs::write(&marker, "no apptab in the environment\n");
        return;
    };
    for _ in 0..600 {
        if node.root_ready().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let users: Vec<String> = app
        .db
        .with(|c| {
            let mut st = c.prepare("SELECT sub FROM users WHERE apptab = 1 AND sub IS NOT NULL AND sub != ''")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>();
            v
        })
        .unwrap_or_default();
    match node.import_proxy("apptab", &host, &upstream, &users).await {
        Ok(made) => {
            let note = if made { format!("apptab: {host} → {upstream}, access for {} accounts\n", users.len()) } else { "apptab was there already\n".into() };
            print!("{}: {note}", node.me());
            let _ = std::fs::write(&marker, note);
        }
        Err(e) => eprintln!("{}: apptab as a proxy module: {e} (tried again at the next start)", node.me()),
    }
}

/// The mail instance in force (D-U16): made, and access brought over.
static MAIL_INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Stalwart as a mail instance (D-U16), from OSADA_MAIL_*: made once the node
/// it runs on has joined, with access for the accounts the mail server's
/// sync gave an address (those that have a mailbox now). Until then the mail
/// sync API lists everyone, as before. `<node>/mail-imported` holds its name.
/// The programs that sign people in (osada 0.1's OIDC clients, changed in the admin pages) into
/// the cluster log, so every idp node knows them (D-U45): at start and every minute, what differs.
async fn publish_oidc_clients(node: Arc<Node>, db: Arc<osada1::db::Db>) {
    let mut first = true;
    loop {
        tokio::time::sleep(Duration::from_secs(if first { 20 } else { 60 })).await;
        first = false;
        let rows = db.with(|c| {
            let mut st = c.prepare("SELECT name, secret_hash, redirect_uris, users FROM oidc_clients")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    crate::oidc::Client {
                        secret_hash: r.get(1)?,
                        redirect_uris: r.get::<_, String>(2)?.lines().map(str::to_string).collect(),
                        users: r.get::<_, String>(3)?.split_whitespace().map(str::to_string).collect(),
                    },
                ))
            })?;
            rows.collect::<Result<std::collections::BTreeMap<_, _>, _>>()
        });
        let Ok(want) = rows else { continue };
        let have = node.cluster_state().oidc_clients;
        let mut cmds: Vec<crate::cluster::Command> = want
            .iter()
            .filter(|(n, c)| have.get(*n) != Some(c))
            .map(|(n, c)| crate::cluster::Command::OidcClient { name: n.clone(), client: Some(c.clone()) })
            .collect();
        cmds.extend(have.keys().filter(|n| !want.contains_key(*n)).map(|n| crate::cluster::Command::OidcClient { name: n.clone(), client: None }));
        for cmd in cmds {
            match node.commit_anywhere("osada 0.1's OIDC clients", cmd.clone()).await {
                Ok(()) => println!("{}: OIDC client published: {:?}", node.me(), match &cmd { crate::cluster::Command::OidcClient { name, client } => (name.clone(), client.is_some()), _ => (String::new(), false) }),
                Err(e) => eprintln!("{}: OIDC client: {e}", node.me()),
            }
        }
    }
}

async fn import_mail(node: Arc<Node>, dir: PathBuf, db: Arc<osada1::db::Db>) {
    let marker = dir.join("mail-imported");
    if let Ok(s) = std::fs::read_to_string(&marker) {
        if let Some(name) = s.lines().next().filter(|n| !n.is_empty()) {
            let _ = MAIL_INSTANCE.set(name.to_string());
        }
        return;
    }
    let Some(on) = var("OSADA_MAIL_NODE") else { return };
    let Some(host) = var("OSADA_MAIL_HOST") else {
        return eprintln!("{}: OSADA_MAIL_NODE without OSADA_MAIL_HOST: no mail instance", node.me());
    };
    let name = var("OSADA_MAIL_INSTANCE").unwrap_or_else(|| "mail".into());
    let upstream = var("OSADA_MAIL_UPSTREAM").unwrap_or_else(|| "tls://127.0.0.1:8443".into());
    // the node by its name or id (D-U19), once it has joined
    let on_name = on;
    let mut said = false;
    let on = loop {
        if node.root_ready().is_some() {
            if let Some(id) = node.cluster_state().placement.resolve_node(&on_name) {
                break id;
            }
        }
        if !said {
            println!("{}: the mail instance {name} waits for the node {on_name} to join", node.me());
            said = true;
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    };
    let users: Vec<String> = db
        .with(|c| {
            let mut st = c.prepare("SELECT sub FROM users WHERE mail IS NOT NULL AND mail != '' AND sub IS NOT NULL AND sub != ''")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>();
            v
        })
        .unwrap_or_default();
    match node.import_mail(&name, &on, &host, &upstream, &users).await {
        Ok(n) => {
            let note = format!("{name}\nmail: {host} on {on_name} ({on}) → {upstream}, access for {n} of {} accounts with an address\n", users.len());
            print!("{}: {}", node.me(), note.split_once('\n').map_or("", |x| x.1));
            let _ = std::fs::write(&marker, note);
            let _ = MAIL_INSTANCE.set(name);
        }
        Err(e) => eprintln!("{}: the mail instance {name}: {e} (tried again at the next start)", node.me()),
    }
}

/// The node, as osada 0.1's module sees it.
struct NodeBridge(Arc<Node>);

impl NodeBridge {
    fn person(&self, s: &Session, u: &User) -> Person {
        Person {
            sub: u.id.clone(),
            name: u.name.clone(),
            admin: self.0.runs_cluster(u),
            staff: self.0.has_any_role(u),
            outside: u.outside.clone(),
            disabled: u.disabled,
            started: (s.started / 1000) as i64,
        }
    }
}

impl Bridge for NodeBridge {
    fn person(&self, token: &str) -> Option<Person> {
        let (s, u) = self.0.session(token)?;
        Some(NodeBridge::person(self, &s, &u))
    }

    fn person_by_hash(&self, hash: &str) -> Option<Person> {
        let (s, u) = self.0.session_by_hash(hash)?;
        Some(NodeBridge::person(self, &s, &u))
    }

    fn sign_in(&self, name: String, password: String, addr: String) -> BoxFuture<'static, Result<String, String>> {
        let node = self.0.clone();
        async move {
            match node.sign_in(&name, &password, &addr).await {
                Ok((token, _)) => Ok(token),
                Err(e) if e.starts_with("a second factor") => Err("this account signs in with a second factor: on the sign-in page, /signin".into()),
                Err(e) => Err(e),
            }
        }
        .boxed()
    }

    fn sign_out(&self, token: &str, addr: &str) {
        self.0.sign_out(token, addr);
    }

    fn change_password(&self, token: String, old: String, new: String) -> BoxFuture<'static, Result<(), String>> {
        let node = self.0.clone();
        async move {
            let (s, u) = node.session(&token).ok_or("not signed in")?;
            let actor = Actor { user: u, sid: s.sid, addr: s.addr };
            node.change_password(&actor, &old, &new).await
        }
        .boxed()
    }

    fn may_use(&self, sub: &str, module: &str) -> bool {
        self.0.may_use(sub, module)
    }

    fn groups_of(&self, sub: &str) -> Vec<(String, String)> {
        self.0.groups_of(sub)
    }

    fn node_name(&self) -> Option<String> {
        Some(self.0.cluster_state().placement.node_label(self.0.me()))
    }

    fn node_id(&self) -> Option<String> {
        Some(self.0.me().0.clone())
    }

    fn default_limit(&self, sub: &str) -> Option<u64> {
        let u = self.0.find_user(sub)?;
        self.0.cluster_state().placement.storage_limit(u.ou).map(|(l, _)| l)
    }

    fn storage(&self, name: &str) -> Option<crate::storage::Storage> {
        self.0.cluster_state().settings.storages.get(name).cloned()
    }

    fn live_node_target(&self, node: &str, backup: &str) -> Option<Arc<dyn crate::osada1::vfs::Backend>> {
        let link = self.0.link_handle();
        Some(Arc::new(crate::livetarget::NodeTarget { link, node: crate::placement::NodeId(node.into()), backup: backup.into() }))
    }

    fn live_backups(&self) -> Vec<crate::audit::LiveBackup> {
        self.0.cluster_state().settings.backups
    }

    fn backup_nodes(&self) -> Vec<(String, String)> {
        // this node first: what others hold copies of (static sites) is then linked, not fetched
        let p = self.0.cluster_state().placement;
        let me = self.0.me().clone();
        let mut l: Vec<(String, String)> = p.nodes.keys().map(|id| (id.0.clone(), p.node_label(id))).collect();
        l.sort_by_key(|(id, _)| *id != me.0);
        l
    }

    fn backup_list(&self, node: String, part: String) -> futures_util::future::BoxFuture<'static, Result<Option<Vec<crate::nodebackup::Entry>>, String>> {
        let n = self.0.clone();
        Box::pin(async move {
            if node == n.me().0 {
                let dir = n.data_dir().ok_or("no data folder")?;
                crate::nodebackup::list_local(&dir, &part).await
            } else {
                crate::nodebackup::list(&n.link_handle(), &crate::placement::NodeId(node), &part).await
            }
        })
    }

    fn backup_get(&self, node: String, part: String, path: String) -> futures_util::future::BoxFuture<'static, Result<crate::osada1::vfs::ByteStream, String>> {
        let n = self.0.clone();
        Box::pin(async move {
            if node == n.me().0 {
                crate::nodebackup::open_local(&part, &path).await
            } else {
                crate::nodebackup::get(&n.link_handle(), &crate::placement::NodeId(node), &part, &path).await
            }
        })
    }

    fn adopt_nightly(&self, storage: Option<crate::storage::Storage>, entry: crate::audit::LiveBackup) -> futures_util::future::BoxFuture<'static, Result<String, String>> {
        let n = self.0.clone();
        Box::pin(async move { n.adopt_nightly(storage, entry).await })
    }

    fn jwks(&self) -> Vec<serde_json::Value> {
        self.0.idp_jwks()
    }

    fn mailbox_of(&self, sub: &str) -> Option<String> {
        self.0.find_user(sub)?.mailbox.map(|m| m.address)
    }

    fn directory(&self) -> Vec<osada1::auth::DirPerson> {
        self.0
            .users()
            .into_iter()
            .map(|u| osada1::auth::DirPerson {
                aliases: u.mailbox.as_ref().and_then(|m| m.aliases.clone()).unwrap_or_default().into_iter().filter(|a| !a.starts_with("*@")).collect(),
                mailbox: u.mailbox.map(|m| m.address),
                sub: u.id,
                name: u.name,
                display: u.display,
                disabled: u.disabled,
            })
            .collect()
    }

    fn upn_domain(&self) -> Option<String> {
        self.0.upn_domain()
    }

    fn mail_host(&self) -> Option<String> {
        let name = MAIL_INSTANCE.get()?;
        self.0.cluster_state().placement.modules.get(name)?.host.clone()
    }

    fn may_use_mail(&self, sub: &str, _module: &str) -> bool {
        self.0.may_use_mail(sub)
    }

    fn office(&self) -> Option<String> {
        crate::sites::office(&self.0.cluster_state().placement)
    }

    fn mail_instance(&self) -> Option<String> {
        let name = MAIL_INSTANCE.get()?;
        let p = self.0.cluster_state().placement;
        p.modules.get(name).is_some_and(|m| m.kind == crate::placement::ModuleKind::Data(crate::placement::Domain::Mail)).then(|| name.clone())
    }

    fn proxies(&self) -> Vec<osada1::apptab::ProxyDef> {
        self.0
            .proxies()
            .into_iter()
            .map(|(name, m)| osada1::apptab::ProxyDef {
                title: if m.title.is_empty() { name.clone() } else { m.title },
                name,
                host: m.host.unwrap_or_default(),
                upstream: m.upstream.unwrap_or_default(),
            })
            .collect()
    }

    fn people(&self) -> Vec<Person> {
        self.0
            .users()
            .into_iter()
            .map(|u| Person {
                admin: self.0.runs_cluster(&u),
                staff: self.0.has_any_role(&u),
                outside: u.outside,
                sub: u.id,
                name: u.name,
                disabled: u.disabled,
                started: 0,
            })
            .collect()
    }
}

/// A password set at the node goes into osada 0.1's table too (the
/// rollback), and the account's OIDC tokens end, as osada 0.1 did.
struct Hooks(Arc<osada1::db::Db>, Option<String>);

impl web::Hooks for Hooks {
    fn issuer(&self) -> Option<String> {
        self.1.clone()
    }

    fn mail_users(&self) -> Vec<(String, String)> {
        self.0
            .with(|c| {
                let mut st = c.prepare("SELECT sub, mail FROM users WHERE mail IS NOT NULL AND mail != '' AND sub IS NOT NULL")?;
                let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                rows.collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default()
    }

    fn password_set(&self, user: &str, password: &str) {
        let (db, user, password) = (self.0.clone(), user.to_string(), password.to_string());
        tokio::task::spawn_blocking(move || {
            let hash = auth::hash_password(&password);
            let r = db.with(|c| {
                c.execute("UPDATE users SET pw_hash = ?1 WHERE sub = ?2", rusqlite::params![hash, user])?;
                c.execute("DELETE FROM oidc_tokens WHERE user_id IN (SELECT id FROM users WHERE sub = ?1)", [&user])
            });
            if let Err(e) = r {
                eprintln!("osada 0.1's copy of a password: {}", e.msg);
            }
        });
    }
}

/// Through the edge: the account's outside networks (osada 0.1's per-user
/// list) apply to the node's pages too, for a session and at sign-in.
async fn outside(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let Some(edge::Remote(ip)) = req.extensions().get::<edge::Remote>().copied() else {
        return next.run(req).await;
    };
    let Some(bridge) = app.bridge.clone() else { return next.run(req).await };
    let refuse = |status: StatusCode, msg: String| (status, [(header::CACHE_CONTROL, "no-store")], Json(json!({ "error": msg }))).into_response();
    if let Some(t) = auth::cookie_token(req.headers()) {
        if let Some(p) = bridge.person(&t) {
            if !allowed(&app, &p, ip) {
                return refuse(StatusCode::FORBIDDEN, format!("this account cannot be used from {ip}"));
            }
        }
    }
    let signin = matches!(req.uri().path(), "/api/signin" | "/api/signin/totp");
    let res = next.run(req).await;
    if !signin {
        return res;
    }
    let token = res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| c.strip_prefix(&format!("{}=", auth::cookie_name())).map(|v| v.split(';').next().unwrap_or("").to_string()))
        .filter(|t| !t.is_empty());
    if let Some(token) = token {
        if let Some(p) = bridge.person(&token) {
            if !allowed(&app, &p, ip) {
                bridge.sign_out(&token, &ip.to_string());
                return refuse(StatusCode::UNAUTHORIZED, format!("wrong user name or password, or this account cannot be used from {ip}"));
            }
        }
    }
    res
}

fn allowed(app: &App, p: &Person, ip: IpAddr) -> bool {
    auth::ensure_user(app, p).is_ok_and(|u| edge::allowed(app, &u.cidrs, ip))
}

fn address(s: &str) -> Result<SocketAddr, String> {
    s.to_socket_addrs().map_err(|e| format!("{s}: {e}"))?.next().ok_or_else(|| format!("{s}: no address"))
}

/// Runs osada 0.1 with the node, until SIGTERM.
pub async fn serve(cfg: osada1::Config) -> Result<(), String> {
    let dir = node_dir(&cfg.data);
    if !dir.join("store.db").exists() {
        init(&dir, &node_name()?)?;
    }
    let tls = NodeCert::load(&dir).and_then(|c| c.tls()).map_err(s)?;
    let kv: Arc<dyn Kv> = Arc::new(SqliteKv::open(&dir.join("store.db")).map_err(s)?);
    let listen = address(&var("OSADA_LINK_BIND").unwrap_or_else(|| LINK_BIND.into()))?;
    let link = Link::bind(&tls, listen).map_err(s)?;
    let ca = if dir.join("tls/ca.key").exists() { Some(Arc::new(Ca::load(&dir).map_err(s)?)) } else { None };
    let mut app = osada1::open_app(cfg).map_err(|e| e.to_string())?;
    // the guide this version ships, when it is not the one last imported (T9)
    if let Err(e) = osada1::wiki::guide(&app) {
        eprintln!("wiki: the guide: {e}");
    }
    let accounts = if dir.join("imported").exists() { Vec::new() } else { legacy_accounts(&app)? };
    // a osada without accounts gets a first administrator, as a new cluster does
    // (only made while the directory has nobody: a restart before it is there makes it then)
    let first_admin = accounts.is_empty().then(|| dir.join("first-admin"));
    let node = Node::start(kv, link.clone(), ca, first_admin)?;
    node.watch_rescue(dir.join("rescue.json"));
    node.set_data(dir.clone());
    node.watch_web_import(dir.join("web-import.json"));
    // certificates (D-U26): a copy for the public name goes where home's https reads it
    let lan = app.cfg.public_url.as_deref().and_then(|u| u.split("://").nth(1)).map(|h| h.split(['/', ':']).next().unwrap_or(h).to_string()).map(|h| (app.cfg.data.join("tls"), h));
    // the UPNs' domain (Round 22): the public host, until the cluster has its own
    if let Some(host) = app.cfg.public_url.as_deref().and_then(|u| u.split("://").nth(1)).map(|h| h.split(['/', ':']).next().unwrap_or(h).to_ascii_lowercase()).filter(|h| h.contains('.')) {
        let _ = crate::node::UPN_FALLBACK.set(host.clone());
        let n = node.clone();
        tokio::spawn(async move {
            for _ in 0..60 {
                match n.default_upn_domain(&host).await {
                    Ok(_) => return,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_secs(10)).await,
                }
            }
        });
    }
    node.spawn_certs(lan);
    node.spawn_ship();
    node.spawn_logs();
    link.spawn_accept();
    for p in var("OSADA_PEERS").unwrap_or_default().split(',').map(str::trim).filter(|p| !p.is_empty()) {
        // `[<name or id>@]<host:port>`; a name not known (yet): whichever node of the cluster is there (D-U19)
        let (name, addr) = match p.split_once('@') {
            Some((n, a)) => (Some(n.trim()), a),
            None => (None, p),
        };
        let id = name.and_then(|n| node.cluster_state().placement.resolve_node(n));
        link.spawn_keep_dialing_at(id, address(addr)?);
    }
    if !accounts.is_empty() {
        import(&node, &dir, accounts).await?;
    } else if !dir.join("imported").exists() {
        let _ = std::fs::write(dir.join("imported"), "no accounts in osada 0.1\n");
    }
    import_apptab(&node, &dir, &app).await;
    import_outside(&node, &dir, &app).await;
    tokio::spawn(import_mail(node.clone(), dir.clone(), app.db.clone()));
    tokio::spawn(publish_oidc_clients(node.clone(), app.db.clone()));
    if !dir.join("outside-imported").exists() {
        return Err("outside networks of osada 0.1 not brought over yet; not starting (it would empty them)".into());
    }
    println!("{} node of the cluster, links on {} (udp)", tls.node, link.local_addr().map_err(s)?);
    app.bridge = Some(Arc::new(NodeBridge(node.clone())));
    // requests an edge node relays (relay.rs), from nodes with the role edge-http
    let relayed = Arc::new(std::sync::OnceLock::new());
    app.relayed = Some(relayed.clone());
    let (roles, names) = (node.clone(), node.clone());
    // a osada folder (D-U22, D-U29, Round 18): a user's (name or id), a group's, or a public link's,
    // where it is on this node's disk; for sites' folders and containers' osada folders
    let (ldb, lroot, lgroups, lnode) = (app.db.clone(), app.cfg.data.join("users"), app.cfg.data.join("groups"), node.clone());
    let resolve: Arc<dyn Fn(&crate::sites::Link) -> Result<PathBuf, String> + Send + Sync> = Arc::new(move |l: &crate::sites::Link| {
        let mut p = if let Some(token) = l.share.as_deref().filter(|t| !t.is_empty()) {
            type Row = (String, Option<i64>, String, i64, Option<i64>);
            let row: Row = ldb
                .with(|c| {
                    c.query_row(
                        "SELECT u.username, l.mount_id, l.path, l.is_dir, l.expires FROM links l JOIN users u ON u.id = l.owner_id WHERE l.token = ?1",
                        [token],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                })
                .map_err(|_| "no such link (removed?)".to_string())?;
            let (owner, mount, path, is_dir, expires) = row;
            if expires.is_some_and(|e| e < osada1::db::now()) {
                return Err("the link has expired".into());
            }
            if mount.is_some() {
                return Err("the link is to other storage of its owner's, not on this node's disk".into());
            }
            if is_dir == 0 {
                return Err("the link is to a file, not a folder".into());
            }
            let mut p = lroot.join(&owner);
            for part in path.split('/').filter(|x| !x.is_empty() && *x != ".") {
                if part == ".." {
                    return Err("not a path".into());
                }
                p.push(part);
            }
            p
        } else if let Some(g) = l.group.as_deref().filter(|g| !g.is_empty()) {
            lgroups.join(lnode.group_id(g).ok_or_else(|| format!("no group {g}"))?)
        } else {
            let name: String = ldb
                .with(|c| c.query_row("SELECT username FROM users WHERE username = ?1 OR sub = ?1", [&l.user], |r| r.get(0)))
                .map_err(|_| format!("no user {}", l.user))?;
            lroot.join(&name)
        };
        for part in l.path.split('/').filter(|x| !x.is_empty() && *x != ".") {
            if part == ".." {
                return Err("not a path".into());
            }
            p.push(part);
        }
        Ok(p)
    });
    let lr = resolve.clone();
    let links: crate::websrv::Links = Arc::new(move |l: &crate::sites::Link| lr(l).ok());
    node.set_links(links.clone());
    // the containers of the sites that run here, their osada folders found here (D-U23, Round 18)
    {
        let (wn, wd, wme, wr) = (node.clone(), dir.clone(), node.me().clone(), resolve.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                let (wme2, wr2, p) = (wme.clone(), wr.clone(), wn.cluster_state().placement);
                let w = tokio::task::spawn_blocking(move || crate::containers::wanted(&p, &wme2, Some(&|l: &crate::sites::Link| wr2(l).and_then(|f| if f.is_dir() { Ok(f) } else { Err("not a folder here".into()) })))).await;
                let Ok(w) = w else { continue };
                if !w.is_empty() || wd.join("web/containers.json").exists() {
                    if let Err(e) = crate::containers::write_wanted(&wd, &w) {
                        eprintln!("{wme}: web/containers.json: {e}");
                    }
                }
                // the jobs that run here (Round 18b)
                let j = crate::containers::jobs_wanted(&wn.cluster_state().placement, &wme);
                if !j.is_empty() || wd.join("web/jobs.json").exists() {
                    if let Err(e) = crate::containers::write_jobs(&wd, &j) {
                        eprintln!("{wme}: web/jobs.json: {e}");
                    }
                }
            }
        });
    }
    // streams: osada's requests from edges, and the sites that run here (D-U27)
    let (rn, mn, rme) = (node.clone(), node.clone(), node.me().clone());
    let site_of: Arc<dyn Fn(&str) -> Option<crate::placement::Module> + Send + Sync> = Arc::new(move |name: &str| {
        let p = rn.cluster_state().placement;
        crate::sites::sites(&p).find(|(n, m, s)| n.as_str() == name && crate::sites::served_by(&p, m, s, &rme)).map(|(_, m, _)| m.clone())
    });
    let may_web: Arc<dyn Fn(&NodeId) -> bool + Send + Sync> = Arc::new(move |peer: &NodeId| {
        let p = mn.cluster_state().placement;
        p.nodes.get(peer).is_some_and(|n| n.has(crate::placement::Role::EdgeHttp)) || p.home_node().as_ref() == Some(peer)
    });
    crate::linkstats::init(Some(&dir));
    crate::linkstats::spawn_tests(link.clone());
    crate::linkstats::spawn_rates(link.clone());
    // egress (Round 19): an exit for the containers and jobs that say so, and the way out for
    // this node's own (from the root agent's local socket)
    let egress_placement: crate::egress::PlacementFn = { let n = node.clone(); Arc::new(move || n.cluster_state().placement) };
    crate::egress::spawn_local(&dir, link.clone(), egress_placement.clone());
    link.take_streams(crate::relay::dispatch(vec![
        (crate::egress::KIND, crate::egress::server(egress_placement, node.me().clone())),
        (crate::linkstats::BW_KIND, crate::linkstats::server()),
        // a live backup kept here (Round 14), under the node folder
        (crate::livetarget::KIND, crate::livetarget::server({ let d = dir.clone(); Arc::new(move || d.clone()) })),
        (crate::livetarget::WEB_MIRROR, crate::livetarget::web_mirror_server(dir.clone())),
        (crate::relay::KIND, crate::relay::home(
        relayed,
        Arc::new(move |peer: &NodeId| roles.cluster_state().placement.nodes.get(peer).is_some_and(|n| n.has(crate::placement::Role::EdgeHttp))),
        Arc::new(move |host: &str| names.proxies().iter().any(|(_, m)| m.host.as_deref() == Some(host))),
    )),
        (crate::relay::WEB_KIND, crate::relay::web_runner(site_of, crate::websrv::Web::with_links(&dir, links.clone()), may_web)),
    ]));
    let opts = web::Options { cookie: auth::cookie_name().into(), files: true, hooks: Some(Arc::new(Hooks(app.db.clone(), app.cfg.public_url.clone()))), oidc: None };
    let pages = node.clone();
    let extra: osada1::Extra = Box::new(move |app: &Arc<App>| {
        web::router::<Arc<App>>(pages, opts).layer(middleware::from_fn_with_state(app.clone(), outside))
    });
    let res = osada1::run(app, Some(extra)).await.map_err(|e| e.to_string());
    link.close_and_wait().await;
    res
}
