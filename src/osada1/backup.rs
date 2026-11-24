//! Nightly backup to an SFTP server, taken while osada keeps serving: the
//! database is copied with `VACUUM INTO` on a connection of its own (WAL lets
//! it read a consistent snapshot next to live writes), and files are only
//! read. Osada replaces files by rename, so every read sees a whole version.
//!
//! Inside osada 0.2 (Round 17) there may be several nightly backups, each in
//! the cluster's settings (`Settings::backups`, kind `nightly`): a name, other
//! storage of kind SFTP it goes to, a time, and what it copies (the parts of
//! `audit::NIGHTLY_PARTS`): besides this node's files, wiki and database, every
//! node's data and mail (crate::nodebackup), so the whole cluster can be
//! rebuilt from one snapshot. The backup that osada 0.1 kept in its own
//! settings (`backup`, with its own SFTP target) is moved there once
//! ([`migrate`]); without a cluster it is used as it was.
//!
//! Every run writes a complete snapshot folder named by its local time, and
//! three are kept (see [`retain`]). A file unchanged since the newest snapshot,
//! or whose content any kept snapshot already has, is hard-linked instead of
//! sent, so each distinct content is stored once. Remote layout:
//!
//! ```text
//! RESTORE.md               how to rebuild the server, which snapshots there are
//! 2026-09-27T031500/       one snapshot:
//!   manifest.json          size, mtime and sha256 of every file in it
//!   shelf.db               database snapshot
//!   secret.key             unseals the SFTP credentials in the database
//!   users/…  deleted/…     user files, files of removed users
//!   wiki/…                 the wiki's pictures
//!   etc/…                  systemd unit and environment file
//!   bin/osada              the running binary
//!   nodes/<node>/…         a node's data: store.db, keys, certificates, sites
//!   mail/<node>/…          Stalwart's data/, etc/, bin/ where it runs
//! .partial-<time>/         a run in progress, or one that died
//! ```
//!
//! Nothing else in the target folder is touched. Mounted SFTP storage is not
//! backed up.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{self, Read, Seek},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{anyhow, Context};
use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

use crate::osada1::{
    auth::Admin,
    db::{self, now, Db},
    error::{AppError, Res},
    mounts::{self, SftpForm},
    vfs::{
        sftp::{self, MountCfg, Sftp},
        Backend, ByteStream, FsError, RelPath,
    },
    App,
};

/// Copied along if readable, so a rebuilt server starts the same way.
const EXTRA: &[(&str, &str)] = &[
    ("/etc/systemd/system/shelf.service", "etc/shelf.service"),
    ("/etc/shelf/shelf.env", "etc/shelf.env"),
    ("/etc/systemd/system/osada.service", "etc/osada.service"),
    ("/etc/osada/osada.env", "etc/osada.env"),
    ("/proc/self/exe", "bin/osada"),
];

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/backup", get(show).put(save))
        .route("/api/backup/live", get(live_status))
        .route("/api/backup/test", post(test))
        .route("/api/backup/run", post(run_now))
        .route("/api/backup/usage", get(usage))
}

/// What this node keeps of files (T2, admin → files): the parts of the data
/// folder, the database included.
async fn usage(State(app): State<Arc<App>>, _a: Admin) -> Res<Json<Value>> {
    let data = app.cfg.data.clone();
    let parts = tokio::task::spawn_blocking(move || {
        ["users", "groups", "wiki", "deleted", "thumbs", "shelf.db"]
            .iter()
            .filter_map(|p| crate::nodeinfo::du(&data.join(p)).map(|(bytes, n)| json!({ "name": p, "bytes": bytes, "files": n })))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(AppError::internal)?;
    let total: u64 = parts.iter().filter_map(|p| p["bytes"].as_u64()).sum();
    let node = app.bridge.as_ref().and_then(|b| b.node_id());
    Ok(Json(json!({ "node": node, "parts": parts, "total": total })))
}

/// The `backup` setting.
#[derive(Serialize, Deserialize, Clone, Default)]
struct Conf {
    enabled: bool,
    /// local time, `HH:MM`
    time: String,
    host: String,
    port: u16,
    username: String,
    auth: String,
    root: String,
    /// sealed `mounts::Secret`
    secret: String,
    host_key: Option<String>,
    /// Other storage (T2) the backup goes to instead of its own target,
    /// which is kept for going back to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<String>,
}

fn load(db: &Db) -> Res<Option<Conf>> {
    match db::get_setting(db, "backup")? {
        Some(s) => Ok(Some(serde_json::from_str(&s).map_err(AppError::internal)?)),
        None => Ok(None),
    }
}

fn store(db: &Db, c: &Conf) -> Res<()> {
    db::set_setting(db, "backup", &serde_json::to_string(c).expect("json"))
}

/// Other storage `name` as a backup target: SFTP, its server's key pinned.
fn storage_of(app: &App, name: &str) -> Res<crate::storage::Storage> {
    let st = app
        .bridge
        .as_ref()
        .and_then(|b| b.storage(name))
        .ok_or_else(|| AppError::bad(format!("no other storage {name} (admin → files → other storage)")))?;
    match &st.target {
        crate::storage::Target::Sftp { host_key: None, .. } => Err(AppError::bad(format!("test storage {name} first: that pins its server's key"))),
        crate::storage::Target::Sftp { .. } => Ok(st),
        _ => Err(AppError::bad(format!("storage {name} is S3: the nightly backup goes to SFTP for now (S3 comes with A12)"))),
    }
}

/// The target a run uses: its own, or the other storage it names, as if it
/// were its own (the connection, RESTORE.md).
fn resolve(app: &App, c: Conf) -> Res<Conf> {
    let Some(name) = c.storage.clone() else { return Ok(c) };
    let st = storage_of(app, &name)?;
    let crate::storage::Target::Sftp { host, port, username, auth, root, host_key } = st.target else {
        return Err(AppError::bad(format!("storage {name} is not SFTP")));
    };
    let some = |s: &str| Some(s.to_string()).filter(|s| !s.is_empty());
    let secret = mounts::Secret { password: some(&st.secret.password), key: some(&st.secret.key), passphrase: some(&st.secret.passphrase) };
    Ok(Conf { host, port, username, auth, root, host_key, secret: mounts::seal(app, &secret), ..c })
}

fn target_cfg(app: &App, c: &Conf) -> Res<MountCfg> {
    let form = SftpForm {
        host: c.host.clone(),
        port: Some(c.port),
        username: c.username.clone(),
        auth: c.auth.clone(),
        root: Some(c.root.clone()),
        ..Default::default()
    };
    let mut cfg = form.cfg(0, &mounts::unseal(app, &c.secret)?, c.host_key.clone())?;
    cfg.private = true;
    Ok(cfg)
}

/// The name osada 0.1's one nightly backup has (Round 17).
const LEGACY: &str = "nightly";

/// A nightly backup as a run sees it.
#[derive(Clone)]
struct Job {
    name: String,
    time: String,
    enabled: bool,
    what: Vec<String>,
    /// "storage:<name>", or "own" (osada 0.1's own target, without a cluster)
    target: String,
}

impl Job {
    fn has(&self, part: &str) -> bool {
        self.what.iter().any(|w| w == part)
    }
}

/// Whether the nightly backup moved into the cluster's settings (Round 17).
fn migrated(app: &App) -> bool {
    app.bridge.is_some() && db::get_setting(&app.db, "backup_migrated").ok().flatten().is_some()
}

/// The nightly backups: the cluster's, once moved there; else osada 0.1's one.
fn jobs(app: &App) -> Res<Vec<Job>> {
    if let (Some(b), true) = (&app.bridge, migrated(app)) {
        return Ok(b
            .live_backups()
            .into_iter()
            .filter(|x| x.nightly())
            .map(|x| Job { time: x.at(), enabled: !x.off, what: x.parts(), target: x.target.clone(), name: x.name })
            .collect());
    }
    Ok(load(&app.db)?
        .map(|c| {
            let target = c.storage.as_ref().map_or("own".to_string(), |s| format!("storage:{s}"));
            vec![Job { name: LEGACY.into(), time: c.time, enabled: c.enabled, what: vec!["database".into(), "files".into(), "wiki".into()], target }]
        })
        .unwrap_or_default())
}

fn job(app: &App, name: Option<&str>) -> Res<Job> {
    let all = jobs(app)?;
    match name.filter(|n| !n.is_empty()) {
        Some(n) => all.into_iter().find(|j| j.name == n).ok_or_else(|| AppError::bad(format!("no nightly backup {n}"))),
        None => all.into_iter().next().ok_or_else(|| AppError::bad("no nightly backup set up (admin → files → backups)")),
    }
}

/// The connection a job's run uses.
fn conf_of(app: &App, j: &Job) -> Res<Conf> {
    match j.target.strip_prefix("storage:") {
        Some(name) => resolve(app, Conf { storage: Some(name.into()), time: j.time.clone(), enabled: j.enabled, ..Default::default() }),
        None => resolve(app, load(&app.db)?.ok_or_else(|| AppError::bad("no backup target configured"))?),
    }
}

/// A setting of one nightly backup's (`backup_snapshots:<name>`, …).
fn key(base: &str, name: &str) -> String {
    format!("{base}:{name}")
}

/// Once: the settings of osada 0.1's one backup become those of the backup named [`LEGACY`].
fn migrate_keys(app: &App) {
    if db::get_setting(&app.db, "backup_keys_named").ok().flatten().is_some() {
        return;
    }
    for base in ["backup_snapshots", "backup_last_day", "backup_prune_pending"] {
        if let Ok(Some(v)) = db::get_setting(&app.db, base) {
            db::set_setting(&app.db, &key(base, LEGACY), &v).ok();
        }
    }
    db::set_setting(&app.db, "backup_keys_named", "1").ok();
}

/// Round 17, once, inside osada 0.2: osada 0.1's nightly backup becomes a nightly backup in the
/// cluster's settings, its own SFTP target other storage (secrets and all; never shown). Until
/// that is done (the cluster may not take changes yet) it runs as before.
pub async fn migrate(app: Arc<App>) {
    let Some(bridge) = app.bridge.clone() else { return };
    let mut said = false;
    loop {
        if migrated(&app) {
            return;
        }
        let res: Result<String, String> = async {
            let Some(c) = load(&app.db).map_err(|e| e.msg)? else { return Ok(String::new()) };
            let storage = match &c.storage {
                Some(_) => None,
                None => {
                    let s = mounts::unseal(&app, &c.secret).map_err(|e| e.msg)?;
                    Some(crate::storage::Storage {
                        target: crate::storage::Target::Sftp {
                            host: c.host.clone(),
                            port: c.port,
                            username: c.username.clone(),
                            auth: c.auth.clone(),
                            root: c.root.clone(),
                            host_key: c.host_key.clone(),
                        },
                        secret: crate::storage::Secret {
                            password: s.password.unwrap_or_default(),
                            key: s.key.unwrap_or_default(),
                            passphrase: s.passphrase.unwrap_or_default(),
                            secret_key: String::new(),
                        },
                        note: "the nightly backup's own target until 0.2.0".into(),
                    })
                }
            };
            let entry = crate::audit::LiveBackup {
                name: LEGACY.into(),
                kind: "nightly".into(),
                source: "all".into(),
                target: c.storage.as_ref().map_or(String::new(), |s| format!("storage:{s}")),
                off: !c.enabled,
                time: c.time.clone(),
                what: Vec::new(),
            };
            bridge.adopt_nightly(storage, entry).await
        }
        .await;
        match res {
            Ok(name) => {
                migrate_keys(&app);
                if !name.is_empty() && name != LEGACY {
                    // the backup took another name: its settings follow it
                    for base in ["backup_snapshots", "backup_last_day", "backup_prune_pending"] {
                        if let Ok(Some(v)) = db::get_setting(&app.db, &key(base, LEGACY)) {
                            db::set_setting(&app.db, &key(base, &name), &v).ok();
                        }
                    }
                    app.db.with(|c| c.execute("UPDATE backup_runs SET name = ?1 WHERE name = ?2", [&name, LEGACY])).ok();
                }
                db::set_setting(&app.db, "backup_migrated", if name.is_empty() { "-" } else { &name }).ok();
                if !name.is_empty() {
                    tracing::info!("backup: the nightly backup is {name} in the cluster's settings now (admin → files → backups)");
                }
                return;
            }
            Err(e) => {
                if !said {
                    tracing::warn!("backup: moving the nightly backup into the cluster's settings: {e}; trying again every minute");
                    said = true;
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }
}

/// Where earlier versions of files come from (versions.rs): the first nightly backup with the
/// files; what it is (to know when it changed) and its target.
pub fn versions_key(app: &App) -> Res<String> {
    let j = jobs(app)?.into_iter().find(|j| j.has("files")).ok_or_else(|| AppError::bad("there is no backup to take versions from"))?;
    Ok(match j.target.strip_prefix("storage:") {
        Some(n) => {
            let st = storage_of(app, n)?;
            format!("{}|{}", j.name, hex::encode(Sha256::digest(serde_json::to_vec(&st).unwrap_or_default())))
        }
        None => format!("{}|{}", j.name, db::get_setting(&app.db, "backup")?.unwrap_or_default()),
    })
}

pub fn versions_reader(app: &App) -> Res<Arc<Sftp>> {
    let j = jobs(app)?.into_iter().find(|j| j.has("files")).ok_or_else(|| AppError::bad("there is no backup to take versions from"))?;
    let conf = conf_of(app, &j)?;
    Ok(Sftp::new(target_cfg(app, &conf)?, pin(app.db.clone())))
}

/// Whether a run is going on (one at a time), of which backup, and how far it got.
#[derive(Default)]
pub struct Runner {
    running: AtomicBool,
    status: Mutex<String>,
    current: Mutex<String>,
}

impl Runner {
    fn set(&self, s: impl Into<String>) {
        *self.status.lock().unwrap() = s.into();
    }
}

fn local_time(ts: i64) -> libc::tm {
    let t = ts as libc::time_t;
    // SAFETY: localtime_r only writes the tm it is given
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

/// `2026-09-27T03:15:00+02:00`, for people
fn iso_time(ts: i64) -> String {
    let t = local_time(ts);
    let off = t.tm_gmtoff / 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
        t.tm_year + 1900,
        t.tm_mon + 1,
        t.tm_mday,
        t.tm_hour,
        t.tm_min,
        t.tm_sec,
        if off < 0 { '-' } else { '+' },
        off.abs() / 60,
        off.abs() % 60
    )
}

/// `2026-09-27T031500`: names a snapshot and sorts by time.
fn stamp(ts: i64) -> String {
    let t = local_time(ts);
    format!(
        "{:04}-{:02}-{:02}T{:02}{:02}{:02}",
        t.tm_year + 1900,
        t.tm_mon + 1,
        t.tm_mday,
        t.tm_hour,
        t.tm_min,
        t.tm_sec
    )
}

/// Year, month, day, hour, minute and second of a [`stamp`]. None for
/// anything that is not a stamp.
fn stamp_fields(s: &str) -> Option<[i64; 6]> {
    let b = s.as_bytes();
    if b.len() != 17 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        if !b[r.clone()].iter().all(u8::is_ascii_digit) {
            return None;
        }
        s[r].parse().ok()
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(13..15)?, num(15..17)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    Some([y, mo, d, h, mi, se])
}

/// Seconds of a [`stamp`], reading local time as if it were UTC; only
/// differences are used. None for anything that is not a stamp.
pub fn stamp_secs(s: &str) -> Option<i64> {
    let [y, mo, d, h, mi, se] = stamp_fields(s)?;
    // days from the civil date, after Howard Hinnant
    let y = if mo <= 2 { y - 1 } else { y };
    let doy = (153 * ((mo + 9) % 12) + 2) / 5 + d - 1;
    let yoe = y % 400;
    let days = y / 400 * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + se)
}

/// When the snapshot named by a [`stamp`] was started, read back as local time.
pub fn stamp_time(s: &str) -> Option<i64> {
    let [y, mo, d, h, mi, se] = stamp_fields(s)?;
    // SAFETY: mktime only reads and normalises the tm it is given
    let t = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = (y - 1900) as i32;
        tm.tm_mon = (mo - 1) as i32;
        tm.tm_mday = d as i32;
        tm.tm_hour = h as i32;
        tm.tm_min = mi as i32;
        tm.tm_sec = se as i32;
        tm.tm_isdst = -1;
        libc::mktime(&mut tm)
    };
    (t != -1).then_some(t as i64)
}

/// Which snapshots to keep, given their times newest first; returns indices.
/// The newest stays, plus one from the week before it and one from one to four
/// weeks before it. With one run a night the second is 1–7 days older than the
/// newest and the third 8–29: once the second is more than a week old it
/// either replaces the third (if that would pass 30 days before the next such
/// turn) or goes, and the run before the newest takes its place.
fn retain(times: &[i64]) -> Vec<usize> {
    if times.len() <= 3 {
        return (0..times.len()).collect();
    }
    // whole days before the newest, rounded
    let age = |i: usize| (times[0] - times[i] + 43200).div_euclid(86400);
    let (prev, week, month) = (1, 2, times.len() - 1);
    if age(week) <= 7 {
        vec![0, week, month]
    } else if age(month) + 7 > 30 {
        vec![0, prev, week]
    } else {
        vec![0, prev, month]
    }
}

fn check_time(s: &str) -> Res<String> {
    let bad = || AppError::bad("time must be HH:MM");
    let (h, m) = s.trim().split_once(':').ok_or_else(bad)?;
    let (h, m): (u32, u32) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok(format!("{h:02}:{m:02}"))
}

/// Checks twice a minute. A run missed while the server was down happens as
/// soon as it is back. A nightly backup new today runs at its time, or from
/// tomorrow if that has passed. Runs go one at a time: one that is due while
/// another runs starts after it.
pub async fn scheduler(app: Arc<App>) {
    app.db
        .with(|c| c.execute("UPDATE backup_runs SET finished = started, ok = 0, message = 'interrupted' WHERE finished IS NULL", []))
        .ok();
    migrate_keys(&app);
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        let Ok(all) = jobs(&app) else { continue };
        let day = stamp(now())[..10].to_string();
        let t = local_time(now());
        let hm = format!("{:02}:{:02}", t.tm_hour, t.tm_min);
        for j in all.iter().filter(|j| j.enabled) {
            let k = key("backup_last_day", &j.name);
            let last = db::get_setting(&app.db, &k).ok().flatten();
            if last.is_none() {
                db::set_setting(&app.db, &k, if hm < j.time { "new" } else { day.as_str() }).ok();
                continue;
            }
            if hm < j.time || last.as_deref() == Some(day.as_str()) {
                continue;
            }
            match start(&app, vec![j.name.clone()]) {
                Ok(()) => {
                    db::set_setting(&app.db, &k, &day).ok();
                }
                // another runs: after it
                Err(e) if e.status == axum::http::StatusCode::CONFLICT => {}
                Err(e) => {
                    db::set_setting(&app.db, &k, &day).ok();
                    tracing::warn!("backup {}: not started: {}", j.name, e.msg);
                    failed_run(&app, &j.name, &format!("not started: {}", e.msg));
                }
            }
        }
    }
}

/// A run that could not start, so the page says so.
fn failed_run(app: &App, name: &str, msg: &str) {
    let t = now();
    app.db
        .with(|c| {
            c.execute(
                "INSERT INTO backup_runs (started, finished, ok, message, name) VALUES (?1, ?1, 0, ?2, ?3)",
                rusqlite::params![t, msg, name],
            )
        })
        .ok();
}

/// How long the UPS page may be unreadable before pruning goes ahead without it.
const UPS_GRACE: i64 = 600;

/// Reads the UPS page: Some(true) on line power, Some(false) on anything else,
/// None if the page cannot be read. When it went unreadable is kept in the
/// settings (`ups_down_since`), so `osada backup run` knows it too.
async fn ups_read(app: &App, url: &str) -> Option<bool> {
    let read = async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("osada/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let body = client.get(url).send().await?.error_for_status()?.bytes().await?;
        let v: Value = serde_json::from_slice(&body)?;
        anyhow::Ok(v["status"]["flags"].as_object().is_some_and(|f| f.values().any(|x| x == "on line power")))
    };
    match read.await {
        Ok(line) => {
            app.db.with(|c| c.execute("DELETE FROM settings WHERE key = 'ups_down_since'", [])).ok();
            Some(line)
        }
        Err(e) => {
            if db::get_setting(&app.db, "ups_down_since").ok().flatten().is_none() {
                tracing::warn!("ups: cannot read {url}: {e:#}");
                db::set_setting(&app.db, "ups_down_since", &now().to_string()).ok();
            }
            None
        }
    }
}

/// The UPS page has been unreadable for [`UPS_GRACE`]; then backups go on as
/// if there were no UPS.
fn ups_gone(app: &App) -> bool {
    let since = db::get_setting(&app.db, "ups_down_since").ok().flatten().and_then(|s| s.parse::<i64>().ok());
    since.is_some_and(|t| now() - t >= UPS_GRACE)
}

/// Old snapshots are removed only while the UPS reads "on line power" (or its
/// page has been gone a while, or there is no UPS); a power cut during the
/// removal could leave a half-removed snapshot.
async fn may_prune(app: &App) -> bool {
    let Some(url) = &app.cfg.ups_url else { return true };
    match ups_read(app, url).await {
        Some(line) => line,
        None => ups_gone(app),
    }
}

fn prune_pending(app: &App, name: &str) -> bool {
    db::get_setting(&app.db, &key("backup_prune_pending", name)).ok().flatten().is_some()
}

fn any_prune_pending(app: &App) -> bool {
    jobs(app).is_ok_and(|l| l.iter().any(|j| prune_pending(app, &j.name)))
}

/// Looks at the UPS page once in every five minutes, at a random second. A
/// backup starts when it stops reading line power; a pruning put off for want
/// of line power happens once it is back.
pub async fn ups_watch(app: Arc<App>) {
    let Some(url) = app.cfg.ups_url.clone() else { return };
    let mut on_battery = false;
    loop {
        let t = now();
        let next = (t / 300 + 1) * 300 + rand::random::<i64>().rem_euclid(300);
        tokio::time::sleep(Duration::from_secs((next - t) as u64)).await;
        let line = ups_read(&app, &url).await;
        match line {
            Some(false) if !on_battery => {
                on_battery = true;
                tracing::warn!("ups: not on line power; starting the nightly backups");
                let names = jobs(&app).map(|l| l.into_iter().filter(|j| j.enabled).map(|j| j.name).collect()).unwrap_or_default();
                if let Err(e) = start(&app, names) {
                    tracing::warn!("backup: not started: {}", e.msg);
                }
            }
            Some(true) if on_battery => {
                on_battery = false;
                tracing::info!("ups: on line power again");
            }
            _ => {}
        }
        if any_prune_pending(&app) && (line == Some(true) || line.is_none() && ups_gone(&app)) {
            start_prune(&app);
        }
    }
}

/// Held for a run, so the server and `osada backup run` never overlap.
fn lock(app: &App) -> Res<fs::File> {
    let dir = app.cfg.data.join("backup");
    fs::create_dir_all(&dir).map_err(AppError::internal)?;
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))
        .map_err(AppError::internal)?;
    // SAFETY: flock only looks at the descriptor, which f keeps open
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(AppError::conflict("a backup is running already"));
    }
    Ok(f)
}

/// Starts the nightly backups `names`, one after the other.
pub fn start(app: &Arc<App>, names: Vec<String>) -> Res<()> {
    let mut todo = Vec::new();
    for n in &names {
        let j = job(app, Some(n))?;
        let conf = conf_of(app, &j)?;
        todo.push((j, conf));
    }
    if todo.is_empty() {
        return Err(AppError::bad("no nightly backup to run"));
    }
    if app.backup.running.swap(true, Ordering::SeqCst) {
        return Err(AppError::conflict("a backup is running already"));
    }
    let held = match lock(app) {
        Ok(f) => f,
        Err(e) => {
            app.backup.running.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    let app = app.clone();
    tokio::spawn(async move {
        for (j, conf) in todo {
            *app.backup.current.lock().unwrap() = j.name.clone();
            let _ = execute(&app, &j, &conf).await;
        }
        app.backup.current.lock().unwrap().clear();
        drop(held);
        app.backup.running.store(false, Ordering::SeqCst);
    });
    Ok(())
}

/// The prunings runs had to leave, on their own. Quietly skipped while a run
/// is going on; that run prunes, or the next look at the UPS tries again.
fn start_prune(app: &Arc<App>) {
    let Ok(all) = jobs(app) else { return };
    let todo: Vec<(Job, Conf)> = all
        .into_iter()
        .filter(|j| prune_pending(app, &j.name))
        .filter_map(|j| conf_of(app, &j).ok().map(|c| (j, c)))
        .collect();
    if todo.is_empty() || app.backup.running.swap(true, Ordering::SeqCst) {
        return;
    }
    let Ok(held) = lock(app) else {
        app.backup.running.store(false, Ordering::SeqCst);
        return;
    };
    let app = app.clone();
    tokio::spawn(async move {
        for (j, conf) in todo {
            tracing::info!("backup {}: pruning that was put off", j.name);
            *app.backup.current.lock().unwrap() = j.name.clone();
            let res = async {
                let target = connect(&app, &conf)?;
                let res = prune(&app, &j, &conf, &target, true).await;
                target.close().await;
                res
            }
            .await;
            match res {
                Ok(keep) => tracing::info!("backup {}: pruned; snapshots {}", j.name, keep.join(", ")),
                Err(e) => tracing::error!("backup {}: pruning failed: {e:#}", j.name),
            }
        }
        app.backup.set("");
        app.backup.current.lock().unwrap().clear();
        drop(held);
        app.backup.running.store(false, Ordering::SeqCst);
    });
}

/// One run, recorded in `backup_runs`.
async fn execute(app: &Arc<App>, j: &Job, conf: &Conf) -> anyhow::Result<()> {
    let started = now();
    let id = app.db.with(|c| {
        c.execute("INSERT INTO backup_runs (started, name) VALUES (?1, ?2)", rusqlite::params![started, j.name])?;
        Ok(c.last_insert_rowid())
    });
    tracing::info!("backup {}: started", j.name);
    let mut st = Stats::default();
    let res = run(app, j, conf, &mut st).await;
    let msg = match &res {
        Ok(()) => {
            let mut m = format!("{} files; snapshots {}", st.checked, st.kept.join(", "));
            if st.no_links {
                m += "; the target cannot hard-link, so every file was sent";
            }
            if st.prune_put_off {
                m += "; old snapshots stay until the UPS is on line power";
            }
            if !st.missing.is_empty() {
                m = format!("incomplete: {}; {m}", st.missing.join("; "));
            }
            m
        }
        Err(e) => format!("{e:#}"),
    };
    if res.is_ok() {
        tracing::info!("backup {}: done, {} sent ({}), {} linked; {msg}", j.name, st.uploaded, human(st.bytes), st.linked);
    } else {
        tracing::error!("backup {}: failed: {msg}", j.name);
    }
    if let Ok(id) = id {
        app.db
            .with(|c| {
                c.execute(
                    "UPDATE backup_runs SET finished = ?1, ok = ?2, uploaded = ?3, bytes = ?4, linked = ?5, message = ?6
                     WHERE id = ?7",
                    rusqlite::params![
                        now(),
                        res.is_ok(),
                        st.uploaded as i64,
                        st.bytes as i64,
                        st.linked as i64,
                        msg,
                        id
                    ],
                )
            })
            .ok();
    }
    app.backup.set("");
    res
}

#[derive(Default)]
struct Stats {
    checked: u64,
    uploaded: u64,
    bytes: u64,
    linked: u64,
    /// the target has no `hardlink@openssh.com`
    no_links: bool,
    /// the UPS was not on line power, so nothing was removed
    prune_put_off: bool,
    kept: Vec<String>,
    /// what of the nodes' parts could not be had
    missing: Vec<String>,
}

/// size, mtime seconds, mtime nanoseconds, sha256
pub type Rec = (u64, i64, u32, String);

/// `manifest.json` of a snapshot; versions.rs reads it too.
#[derive(Serialize, Deserialize, Default)]
pub struct Manifest {
    pub finished: i64,
    /// by path inside the snapshot (`users/<name>/…`)
    pub files: BTreeMap<String, Rec>,
}

pub(crate) struct Item {
    /// path inside the snapshot
    pub(crate) remote: String,
    pub(crate) src: PathBuf,
    /// size, mtime seconds and nanoseconds when the walk saw it
    pub(crate) seen: (u64, i64, u32),
}

pub(crate) fn seen(m: &fs::Metadata) -> (u64, i64, u32) {
    (m.len(), m.mtime(), m.mtime_nsec() as u32)
}

/// Regular files and folders below `src`; symlinks and the like are skipped,
/// as is whatever disappears while we look.
pub(crate) fn walk(src: &Path, remote: &str, items: &mut Vec<Item>, dirs: &mut Vec<String>) -> io::Result<()> {
    let rd = match fs::read_dir(src) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        r => r?,
    };
    dirs.push(remote.to_string());
    for e in rd {
        let e = e?;
        let Ok(name) = e.file_name().into_string() else { continue };
        let Ok(ft) = e.file_type() else { continue };
        let r = format!("{remote}/{name}");
        if ft.is_dir() {
            walk(&e.path(), &r, items, dirs)?;
        } else if ft.is_file() {
            if let Ok(m) = e.metadata() {
                items.push(Item { remote: r, src: e.path(), seen: seen(&m) });
            }
        }
    }
    Ok(())
}

pub(crate) async fn ensure_dir(b: &dyn Backend, made: &mut HashSet<String>, dir: &RelPath) -> Result<(), FsError> {
    let mut cur = RelPath::root();
    for s in dir.segs() {
        cur = cur.child(s)?;
        let k = cur.to_string();
        if made.contains(&k) {
            continue;
        }
        match b.mkdir(&cur).await {
            Ok(()) | Err(FsError::Exists) => {}
            Err(e) => return Err(e),
        }
        made.insert(k);
    }
    Ok(())
}

fn once(data: Vec<u8>) -> ByteStream {
    Box::pin(futures_util::stream::once(async move { Ok(Bytes::from(data)) }))
}

async fn read_remote(b: &dyn Backend, p: &str) -> anyhow::Result<Option<Vec<u8>>> {
    match b.open(&RelPath::parse(p)?).await {
        Ok((_, mut r)) => {
            let mut v = Vec::new();
            r.read_to_end(&mut v).await?;
            Ok(Some(v))
        }
        Err(FsError::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Opens and hashes `src`. The file comes back rewound, so what is sent is
/// what was hashed (osada never rewrites a file in place). None if it is gone.
async fn hash(src: PathBuf) -> anyhow::Result<Option<(fs::File, Rec)>> {
    let res = tokio::task::spawn_blocking(move || -> io::Result<(fs::File, Rec)> {
        let mut f = fs::File::open(&src)?;
        let m = f.metadata()?;
        let mut h = Sha256::new();
        let mut buf = vec![0; 256 << 10];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        f.rewind()?;
        let (size, secs, nanos) = seen(&m);
        Ok((f, (size, secs, nanos, hex::encode(h.finalize()))))
    })
    .await?;
    match res {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Hard-links `from` (a path on the target) as `to`. False means send the
/// file instead: the target cannot link, or `from` is not there any more.
async fn link(t: &Sftp, st: &mut Stats, from: &str, to: &RelPath) -> anyhow::Result<bool> {
    if st.no_links {
        return Ok(false);
    }
    match t.hardlink(&RelPath::parse(from)?, to).await {
        Ok(true) => {
            st.linked += 1;
            Ok(true)
        }
        Ok(false) => {
            st.no_links = true;
            Ok(false)
        }
        Err(e) => {
            tracing::warn!("backup: linking {from}: {e}; sending it instead");
            Ok(false)
        }
    }
}

fn human(n: u64) -> String {
    let mut f = n as f64;
    for u in ["B", "KiB", "MiB", "GiB"] {
        if f < 1024.0 {
            return format!("{f:.1} {u}");
        }
        f /= 1024.0;
    }
    format!("{f:.1} TiB")
}

async fn run(app: &Arc<App>, j: &Job, conf: &Conf, st: &mut Stats) -> anyhow::Result<()> {
    let label = stamp(now());
    let work = app.cfg.data.join("backup");
    fs::create_dir_all(&work)?;
    let snap = work.join("shelf.db");
    if j.has("database") {
        app.backup.set("copying the database");
        let (src, snap) = (app.cfg.data.join("shelf.db"), snap.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let _ = fs::remove_file(&snap);
            let c = rusqlite::Connection::open(&src)?;
            c.busy_timeout(Duration::from_secs(30))?;
            c.execute("VACUUM INTO ?1", [snap.to_string_lossy()])?;
            Ok(())
        })
        .await?
        .context("database snapshot")?;
    }
    let res = sync(app, j, conf, st, &label, j.has("database").then_some(snap.as_path())).await;
    let _ = fs::remove_file(&snap);
    res
}

/// A file of another part (a node's data, mail) as one of the nodes listed it.
pub(crate) struct Remote {
    /// path inside the snapshot
    remote: String,
    node: String,
    part: &'static str,
    /// path in the part
    path: String,
    rec: Rec,
}

async fn sync(app: &Arc<App>, j: &Job, conf: &Conf, st: &mut Stats, label: &str, snap: Option<&Path>) -> anyhow::Result<()> {
    app.backup.set("looking for files");
    let (items, dirs) = {
        let (data, snap) = (app.cfg.data.clone(), snap.map(Path::to_path_buf));
        let (files, wiki) = (j.has("files"), j.has("wiki"));
        tokio::task::spawn_blocking(move || -> io::Result<(Vec<Item>, Vec<String>)> {
            let (mut items, mut dirs) = (Vec::new(), Vec::new());
            let mut add = |src: PathBuf, remote: &str| {
                if let Ok(m) = fs::metadata(&src) {
                    items.push(Item { remote: remote.into(), src, seen: seen(&m) });
                }
            };
            if let Some(snap) = snap {
                add(snap, "shelf.db");
                add(data.join("secret.key"), "secret.key");
                for (src, remote) in EXTRA {
                    if fs::File::open(src).is_ok() {
                        add(src.into(), remote);
                    }
                }
                // the unit's drop-ins (osada 0.2's among them)
                if let Ok(rd) = fs::read_dir("/etc/systemd/system/shelf.service.d") {
                    for e in rd.flatten() {
                        let n = e.file_name().to_string_lossy().to_string();
                        if n.ends_with(".conf") && fs::File::open(e.path()).is_ok() {
                            add(e.path(), &format!("etc/shelf.service.d/{n}"));
                        }
                    }
                }
            }
            if files {
                walk(&data.join("users"), "users", &mut items, &mut dirs)?;
                // the groups' folders (D-U29)
                if data.join("groups").is_dir() {
                    walk(&data.join("groups"), "groups", &mut items, &mut dirs)?;
                }
                walk(&data.join("deleted"), "deleted", &mut items, &mut dirs)?;
            }
            if wiki {
                walk(&data.join("wiki"), "wiki", &mut items, &mut dirs)?;
            }
            Ok((items, dirs))
        })
        .await??
    };

    // the nodes' data and mail (Round 17), each node asked in turn
    let mut remote = Vec::new();
    let parts: Vec<(&str, &'static str)> = [("nodes", "node"), ("mail", "mail")].into_iter().filter(|(p, _)| j.has(p)).collect();
    if !parts.is_empty() {
        match &app.bridge {
            None => st.missing.push("without a cluster there are no nodes' data or mail".into()),
            Some(b) => {
                for (id, name) in b.backup_nodes() {
                    for (folder, part) in &parts {
                        app.backup.set(if *part == "mail" {
                            format!("asking {name} for a snapshot of mail (Stalwart stops for seconds where it runs)")
                        } else {
                            format!("asking {name} for its data")
                        });
                        match b.backup_list(id.clone(), part.to_string()).await {
                            Ok(None) => {}
                            Ok(Some(l)) => {
                                for e in l {
                                    remote.push(Remote { remote: format!("{folder}/{name}/{}", e.0), node: id.clone(), part, path: e.0, rec: (e.1, e.2, e.3, e.4) });
                                }
                            }
                            Err(e) => {
                                tracing::warn!("backup {}: {folder} of {name}: {e}", j.name);
                                st.missing.push(format!("{folder} of {name}: {e}"));
                            }
                        }
                    }
                }
            }
        }
    }

    let target = connect(app, conf)?;
    let res = snapshot(app, j, conf, &target, st, label, &items, &dirs, &remote).await;
    target.close().await;
    res
}

fn connect(app: &App, conf: &Conf) -> anyhow::Result<Arc<Sftp>> {
    app.backup.set("connecting");
    let cfg = target_cfg(app, conf).map_err(|e| anyhow!(e.msg))?;
    Ok(Sftp::new(cfg, pin(app.db.clone())))
}

/// Keeps the target's host key the first time it is seen (its own target;
/// other storage is pinned by its test).
fn pin(db: Arc<Db>) -> sftp::PinFn {
    Arc::new(move |fp: &str| {
        if let Ok(Some(mut c)) = load(&db) {
            if c.host_key.is_none() && c.storage.is_none() {
                c.host_key = Some(fp.into());
                store(&db, &c).ok();
            }
        }
    })
}

/// Writes `.partial-<label>`, renames it to `<label>` once complete, then
/// removes the snapshots [`retain`] lets go if [`may_prune`].
#[allow(clippy::too_many_arguments)]
async fn snapshot(
    app: &App,
    j: &Job,
    conf: &Conf,
    t: &Sftp,
    st: &mut Stats,
    label: &str,
    items: &[Item],
    dirs: &[String],
    remote: &[Remote],
) -> anyhow::Result<()> {
    let b: &dyn Backend = t;
    let top = b.list(&RelPath::root()).await.context("listing the target folder")?;
    for e in &top {
        if e.is_dir && e.name.strip_prefix(".partial-").is_some_and(|s| stamp_secs(s).is_some()) {
            app.backup.set(format!("removing the unfinished {}", e.name));
            b.remove(&RelPath::parse(&e.name)?).await.with_context(|| format!("removing {}", e.name))?;
        }
    }
    let mut snaps: Vec<String> =
        top.into_iter().filter(|e| e.is_dir && stamp_secs(&e.name).is_some()).map(|e| e.name).collect();
    snaps.sort_by(|x, y| y.cmp(x));

    // content already on the target by sha256, newest snapshot first
    let mut known: HashMap<String, String> = HashMap::new();
    let mut last: Option<(String, Manifest)> = None;
    for s in &snaps {
        app.backup.set(format!("reading {s}/manifest.json"));
        let raw = read_remote(b, &format!("{s}/manifest.json")).await.with_context(|| format!("reading {s}/manifest.json"))?;
        let m: Manifest = match raw.map(|r| serde_json::from_slice(&r)) {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                tracing::warn!("backup: {s}/manifest.json is damaged: {e}");
                continue;
            }
            None => continue,
        };
        for (p, r) in &m.files {
            known.entry(r.3.clone()).or_insert_with(|| format!("{s}/{p}"));
        }
        if last.is_none() {
            last = Some((s.clone(), m));
        }
    }

    let part = format!(".partial-{label}");
    let at = |p: &str| RelPath::parse(&format!("{part}/{p}"));
    let mut made = HashSet::new();
    ensure_dir(b, &mut made, &RelPath::parse(&part)?).await.with_context(|| format!("creating {part}"))?;
    for d in dirs {
        ensure_dir(b, &mut made, &at(d)?).await.with_context(|| format!("creating {d}"))?;
    }
    let mut man = Manifest::default();
    let total = items.len() + remote.len();
    for (i, it) in items.iter().enumerate() {
        st.checked += 1;
        app.backup.set(format!(
            "{} of {total} files, {} sent ({}), {} linked: {}",
            i + 1,
            st.uploaded,
            human(st.bytes),
            st.linked,
            it.remote
        ));
        let dst = at(&it.remote)?;
        ensure_dir(b, &mut made, &dst.parent().unwrap_or_default()).await?;
        // unchanged since the newest snapshot
        if let Some((s, m)) = &last {
            if let Some(r) = m.files.get(&it.remote).filter(|r| (r.0, r.1, r.2) == it.seen) {
                if link(t, st, &format!("{s}/{}", it.remote), &dst).await? {
                    man.files.insert(it.remote.clone(), r.clone());
                    continue;
                }
            }
        }
        let Some((f, rec)) = hash(it.src.clone()).await.with_context(|| format!("reading {}", it.src.display()))? else {
            continue; // deleted since the walk
        };
        // the same content under another name or in an older snapshot
        if let Some(from) = known.get(&rec.3) {
            if link(t, st, from, &dst).await? {
                man.files.insert(it.remote.clone(), rec);
                continue;
            }
        }
        b.write(&dst, Box::pin(ReaderStream::with_capacity(tokio::fs::File::from_std(f), 256 * 1024)))
            .await
            .with_context(|| format!("uploading {}", it.remote))?;
        st.uploaded += 1;
        st.bytes += rec.0;
        known.insert(rec.3.clone(), format!("{part}/{}", it.remote));
        man.files.insert(it.remote.clone(), rec);
    }
    // the nodes' parts: what the target has is linked, the rest comes over the link
    for (i, it) in remote.iter().enumerate() {
        st.checked += 1;
        app.backup.set(format!(
            "{} of {total} files, {} sent ({}), {} linked: {}",
            items.len() + i + 1,
            st.uploaded,
            human(st.bytes),
            st.linked,
            it.remote
        ));
        let dst = at(&it.remote)?;
        ensure_dir(b, &mut made, &dst.parent().unwrap_or_default()).await?;
        if let Some((s, m)) = &last {
            if m.files.get(&it.remote) == Some(&it.rec) && link(t, st, &format!("{s}/{}", it.remote), &dst).await? {
                man.files.insert(it.remote.clone(), it.rec.clone());
                continue;
            }
        }
        if let Some(from) = known.get(&it.rec.3) {
            if link(t, st, from, &dst).await? {
                man.files.insert(it.remote.clone(), it.rec.clone());
                continue;
            }
        }
        let bridge = app.bridge.as_ref().ok_or_else(|| anyhow!("no cluster"))?;
        let stream = match bridge.backup_get(it.node.clone(), it.part.to_string(), it.path.clone()).await {
            Ok(s) => s,
            Err(e) => {
                st.missing.push(format!("{}: {e}", it.remote));
                continue;
            }
        };
        // hashed on the way: what arrives is what the manifest says
        let seen = Arc::new(Mutex::new((Sha256::new(), 0u64, false)));
        let s2 = seen.clone();
        let stream: ByteStream = Box::pin(futures_util::StreamExt::map(stream, move |r| {
            let mut g = s2.lock().unwrap();
            match &r {
                Ok(bytes) => {
                    g.0.update(bytes);
                    g.1 += bytes.len() as u64;
                }
                Err(_) => g.2 = true,
            }
            r
        }));
        if let Err(e) = b.write(&dst, stream).await {
            if !seen.lock().unwrap().2 {
                return Err(anyhow!("uploading {}: {e}", it.remote));
            }
            // the node went away in the middle: noted, the rest goes on
            st.missing.push(format!("{}: the link broke", it.remote));
            let _ = b.remove(&dst).await;
            continue;
        }
        let (h, n, _) = std::mem::take(&mut *seen.lock().unwrap());
        let rec = (n, it.rec.1, it.rec.2, hex::encode(h.finalize()));
        st.uploaded += 1;
        st.bytes += n;
        known.insert(rec.3.clone(), format!("{part}/{}", it.remote));
        man.files.insert(it.remote.clone(), rec);
    }
    man.finished = now();
    b.write(&at("manifest.json")?, once(serde_json::to_vec(&man)?)).await.context("writing manifest.json")?;
    b.rename(&RelPath::parse(&part)?, &RelPath::parse(label)?)
        .await
        .with_context(|| format!("renaming {part} to {label}"))?;

    app.backup.set("asking the UPS");
    let may = may_prune(app).await;
    st.prune_put_off = !may;
    st.kept = prune(app, j, conf, t, may).await?;
    Ok(())
}

/// Removes the snapshots [`retain`] lets go, or with `may` false only notes
/// that this is still to do (`backup_prune_pending`). Rewrites RESTORE.md
/// either way; returns the snapshots left.
async fn prune(app: &App, j: &Job, conf: &Conf, t: &Sftp, may: bool) -> anyhow::Result<Vec<String>> {
    let b: &dyn Backend = t;
    let mut snaps: Vec<String> = b
        .list(&RelPath::root())
        .await
        .context("listing the target folder")?
        .into_iter()
        .filter(|e| e.is_dir && stamp_secs(&e.name).is_some())
        .map(|e| e.name)
        .collect();
    snaps.sort_by(|x, y| y.cmp(x));
    let keep: Vec<String> = if may {
        let times: Vec<i64> = snaps.iter().map(|s| stamp_secs(s).unwrap_or_default()).collect();
        let keep: Vec<String> = retain(&times).into_iter().map(|i| snaps[i].clone()).collect();
        for s in snaps.iter().filter(|s| !keep.contains(s)) {
            app.backup.set(format!("removing {s}"));
            b.remove(&RelPath::parse(s)?).await.with_context(|| format!("removing {s}"))?;
        }
        keep
    } else {
        snaps
    };
    // which nodes' data and mail the newest has, for RESTORE.md
    let has = |sub: &'static str| {
        let p = keep.first().map(|n| format!("{n}/{sub}"));
        async move {
            match p {
                Some(p) => b.list(&RelPath::parse(&p).ok()?).await.ok().map(|l| l.into_iter().filter(|e| e.is_dir).map(|e| e.name).collect::<Vec<_>>()),
                None => None,
            }
        }
    };
    let nodes = has("nodes").await.unwrap_or_default();
    let mail = has("mail").await.unwrap_or_default();
    b.write(&RelPath::parse("RESTORE.md")?, once(restore_md(app, j, conf, &keep, &nodes, &mail).into_bytes()))
        .await
        .context("writing RESTORE.md")?;
    db::set_setting(&app.db, &key("backup_snapshots", &j.name), &serde_json::to_string(&keep)?).map_err(|e| anyhow!(e.msg))?;
    if may {
        app.db
            .with(|c| c.execute("DELETE FROM settings WHERE key = ?1", [key("backup_prune_pending", &j.name)]))
            .map_err(|e| anyhow!(e.msg))?;
    } else {
        db::set_setting(&app.db, &key("backup_prune_pending", &j.name), &now().to_string()).map_err(|e| anyhow!(e.msg))?;
    }
    Ok(keep)
}

fn restore_md(app: &App, j: &Job, conf: &Conf, keep: &[String], nodes: &[String], mail: &[String]) -> String {
    let list: String = keep.iter().map(|s| format!("    {s}/\n")).collect();
    let parts: String = crate::audit::NIGHTLY_PARTS
        .iter()
        .map(|(p, about)| format!("- {about}{}\n", if j.has(p) { "" } else { ": not in this backup" }))
        .collect();
    let home = app.bridge.as_ref().and_then(|b| b.node_name()).unwrap_or_else(|| app.cfg.host.clone());
    let others: Vec<&String> = nodes.iter().filter(|n| **n != home).collect();
    let fetch = format!("    scp -r {}@{}:{}/{} /root/osada-backup", conf.username, conf.host, conf.root, keep.first().map_or("<time>", String::as_str));
    let mut md = format!(
        r#"# Restoring osada

Nightly backup `{name}` of the osada cluster whose home is {host} ({home}); this
file was last written {time}. It holds:

{parts}
Snapshots:

{list}
Each folder, named by home's local time, is a complete copy. A file that
did not change between runs is a hard link shared by the folders, so it takes
space once. Kept are the newest, one from the week before it and one from one
to four weeks before it; the others are removed after each run. A
`.partial-<time>` folder is a run that did not finish; the next run removes it.

    shelf.db         home's database: users, shares, links, mounts, settings, the wiki
    secret.key       unseals the SFTP credentials stored in the database
    users/<name>/    everyone's files
    groups/<name>/   the groups' folders
    deleted/         files of deleted users
    wiki/            the wiki's pictures (older backups may not have it)
    etc/             home's systemd unit, its drop-ins and environment file
    bin/osada        the program (x86_64 Linux; every node runs the same one)
    nodes/<node>/    each node's data folder: store.db (the cluster: its log,
                     directory, sign-in, chat, settings; a consistent copy),
                     tls/ (the node's identity in the cluster), certs/, acme/,
                     edge-tls/, idp.key, links.json, web/ (static sites), and
                     etc/<unit> (its systemd unit) and bin/osada
    mail/<node>/     where Stalwart runs: data/ (its RocksDB, copied with
                     Stalwart stopped for seconds), etc/ (config.json, its
                     environment), bin/stalwart
    manifest.json    size, mtime and sha256 of every file above

Not in here: files on external SFTP mounts (they live on their own servers),
the nodes' logs (log shipping sends them elsewhere), the edges' caches, and
what containers keep of their own.

## Rebuild the whole cluster

Home first, then the other nodes, then mail. On each new server, as root, take
the newest folder (or an older one):

{fetch}
    B=/root/osada-backup

### Home ({home})

    useradd -r -m -d /home/osada -s /sbin/nologin osada
    install -m 600 $B/shelf.db $B/secret.key /home/osada/
    for d in users groups deleted wiki; do [ -d $B/$d ] && cp -a $B/$d /home/osada/; done
    mkdir -p /home/osada/node && cp -a $B/nodes/{home}/. /home/osada/node/
    rm -rf /home/osada/node/etc /home/osada/node/bin
    chown -R osada:osada /home/osada
    install -D -m 755 $B/bin/osada /opt/osada-0.2/osada
    install -D -m 644 $B/etc/shelf.service /etc/systemd/system/shelf.service
    [ -d $B/etc/shelf.service.d ] && cp -a $B/etc/shelf.service.d /etc/systemd/system/
    install -D -m 644 $B/etc/shelf.env /etc/shelf/shelf.env
    systemctl daemon-reload && systemctl enable --now osada

The program goes where ExecStart in the unit (or its drop-in) says; the node
folder is /home/osada/node unless the environment file says otherwise. Then
allow port 80/tcp (and 443, 7441/udp for the cluster's link) in the firewall.
"#,
        name = j.name,
        host = app.cfg.host,
        time = iso_time(now()),
    );
    if !others.is_empty() {
        md += &format!(
            r#"
### The other nodes ({})

For each, with N its name and U the unit in $B/nodes/N/etc/:

    D=$(grep -o -- '--data [^ ]*' $B/nodes/N/etc/U | cut -d' ' -f2)
    S=$(sed -n 's/^User=//p' $B/nodes/N/etc/U)
    useradd -r -s /sbin/nologin $S
    mkdir -p $D && cp -a $B/nodes/N/. $D/ && rm -rf $D/etc $D/bin
    chown -R $S:$S $(dirname $D)
    install -D -m 755 $B/nodes/N/bin/osada /usr/local/bin/osada
    install -D -m 644 $B/nodes/N/etc/U /etc/systemd/system/U
    systemctl daemon-reload && systemctl enable --now U

(the program goes where ExecStart says.) A node keeps its identity (tls/) and
finds the others at the addresses in links.json; at the same addresses (or DNS
names) the cluster is whole again. A node at a new address joins anew from
node control and gets its sites and roles from home.
"#,
            others.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        );
    }
    if !mail.is_empty() {
        md += &format!(
            r#"
### Mail ({})

On the node in mail/<node>/ (Stalwart, the same version or newer):

    useradd -r -s /sbin/nologin stalwart
    mkdir -p /opt/stalwart/bin /opt/stalwart/etc /opt/stalwart/data /opt/stalwart/logs
    cp -a $B/mail/<node>/data/. /opt/stalwart/data/
    cp -a $B/mail/<node>/etc/. /opt/stalwart/etc/
    install -m 755 $B/mail/<node>/bin/stalwart /opt/stalwart/bin/stalwart
    chown -R stalwart:stalwart /opt/stalwart

Its unit (stalwart.service): User=stalwart, ExecStart=/opt/stalwart/bin/stalwart
--config=/opt/stalwart/etc/config.json, EnvironmentFile=-/opt/stalwart/etc/stalwart.env,
AmbientCapabilities=CAP_NET_BIND_SERVICE; then `systemctl enable --now stalwart`.
Should a newer Stalwart not read the data, Stalwart's own export makes a copy
any version takes: `stalwart --config=<a config whose store is these files>
--export <folder>` with that Stalwart stopped, then `--import` on the new one.
For the next nights' snapshots install the snapshot helper again
(deploy/stalwart-snapshot.sh in osada's sources says how).
"#,
            mail.join(", ")
        );
    }
    md += r#"
If the program does not run on a new server, build it from osada's sources
(`cargo build --release`). Users sign in with their old passwords and public
links keep working. The sha256 of every file must match manifest.json.
"#;
    md
}

fn overview(app: &App) -> Res<Value> {
    // osada 0.1's own target, while it is in use (without a cluster, or before the move)
    let config = match (migrated(app), load(&app.db)?) {
        (false, Some(c)) => {
            let s = mounts::unseal(app, &c.secret)?;
            json!({
                "enabled": c.enabled, "time": c.time, "host": c.host, "port": c.port,
                "username": c.username, "auth": c.auth, "root": c.root, "host_key": c.host_key,
                "has_password": s.password.is_some(), "has_key": s.key.is_some(),
                "storage": c.storage,
            })
        }
        _ => Value::Null,
    };
    let snaps = |name: &str| -> Vec<String> {
        db::get_setting(&app.db, &key("backup_snapshots", name)).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    };
    let nightly: Vec<Value> = jobs(app)?
        .iter()
        .map(|j| {
            json!({
                "name": j.name, "time": j.time, "enabled": j.enabled, "what": j.what, "target": j.target,
                "snapshots": snaps(&j.name), "prune_pending": prune_pending(app, &j.name),
            })
        })
        .collect();
    let runs = app.db.with(|c| {
        let mut st = c.prepare(
            "SELECT id, started, finished, ok, uploaded, bytes, linked, message, name FROM backup_runs ORDER BY id DESC LIMIT 60",
        )?;
        let v = st
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "started": r.get::<_, i64>(1)?, "finished": r.get::<_, Option<i64>>(2)?,
                    "ok": r.get::<_, Option<bool>>(3)?, "uploaded": r.get::<_, i64>(4)?, "bytes": r.get::<_, i64>(5)?,
                    "linked": r.get::<_, i64>(6)?, "message": r.get::<_, Option<String>>(7)?, "name": r.get::<_, String>(8)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>();
        v
    })?;
    let first = nightly.first().cloned().unwrap_or(Value::Null);
    Ok(json!({
        "config": config,
        "migrated": migrated(app),
        "nightly": nightly,
        "parts": crate::audit::NIGHTLY_PARTS.iter().map(|(p, about)| json!([p, about])).collect::<Vec<_>>(),
        "running": app.backup.running.load(Ordering::SeqCst),
        "current": app.backup.current.lock().unwrap().clone(),
        "status": app.backup.status.lock().unwrap().clone(),
        "snapshots": first["snapshots"].clone(),
        "prune_pending": any_prune_pending(app),
        "runs": runs,
    }))
}

async fn show(State(app): State<Arc<App>>, _a: Admin) -> Res<Json<Value>> {
    Ok(Json(overview(&app)?))
}

#[derive(Deserialize)]
struct ConfReq {
    #[serde(default)]
    enabled: bool,
    time: String,
    #[serde(flatten)]
    sftp: SftpForm,
    #[serde(default)]
    reset_host_key: bool,
    /// Other storage to go to instead (T2); empty: its own target.
    #[serde(default)]
    storage: Option<String>,
}

/// Connects first; the host key is pinned from then on. Returns the host key
/// and how many entries the target folder has.
async fn configure(app: &App, req: ConfReq) -> Res<(String, usize)> {
    if migrated(app) {
        return Err(AppError::bad("the nightly backups are in the cluster's settings now: admin → files → backups; where they go: other storage"));
    }
    let time = check_time(&req.time)?;
    let old = load(&app.db)?;
    if let Some(name) = req.storage.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let st = storage_of(app, name)?;
        let (fp, n) = sftp::probe(&st.sftp_cfg(true).map_err(AppError::bad)?).await?;
        // its own target stays as it was, for going back to it
        let mut c = old.unwrap_or_else(|| Conf { secret: mounts::seal(app, &mounts::Secret::default()), ..Default::default() });
        c.enabled = req.enabled;
        c.time = time;
        c.storage = Some(name.to_string());
        store(&app.db, &c)?;
        return Ok((fp, n));
    }
    let old_secret = match &old {
        Some(o) => Some(mounts::unseal(app, &o.secret)?),
        None => None,
    };
    let secret = req.sftp.secret(old_secret);
    let port = req.sftp.port.unwrap_or(22);
    let pinned = old
        .filter(|o| !req.reset_host_key && o.host == req.sftp.host.trim() && o.port == port)
        .and_then(|o| o.host_key);
    let cfg = req.sftp.cfg(0, &secret, pinned)?;
    let (fp, n) = sftp::probe(&cfg).await?;
    store(
        &app.db,
        &Conf {
            enabled: req.enabled,
            time,
            host: cfg.host,
            port: cfg.port,
            username: cfg.user,
            auth: req.sftp.auth,
            root: cfg.root,
            secret: mounts::seal(app, &secret),
            host_key: Some(fp.clone()),
            storage: None,
        },
    )?;
    Ok((fp, n))
}

async fn save(State(app): State<Arc<App>>, _a: Admin, Json(req): Json<ConfReq>) -> Res<Json<Value>> {
    let (fp, n) = configure(&app, req).await?;
    Ok(Json(json!({ "ok": true, "host_key": fp, "entries": n })))
}

#[derive(Deserialize, Default)]
struct Which {
    #[serde(default)]
    name: Option<String>,
}

/// `?name=`: which nightly backup (else the first).
async fn test(State(app): State<Arc<App>>, _a: Admin, Query(w): Query<Which>) -> Res<Json<Value>> {
    let j = job(&app, w.name.as_deref())?;
    let conf = conf_of(&app, &j)?;
    let (fp, n) = sftp::probe(&target_cfg(&app, &conf)?).await?;
    Ok(Json(json!({ "ok": true, "name": j.name, "host_key": fp, "entries": n })))
}

async fn run_now(State(app): State<Arc<App>>, _a: Admin, Query(w): Query<Which>) -> Res<Json<Value>> {
    let j = job(&app, w.name.as_deref())?;
    start(&app, vec![j.name.clone()])?;
    Ok(Json(json!({ "started": true, "name": j.name })))
}

/// `osada backup set|run|show`, as the osada user. `set` reads the private key
/// (`--key`) or password (`--password`) from stdin, so neither shows up in the
/// process list or shell history.
pub fn cli(app: App, args: &[String]) -> anyhow::Result<()> {
    let app = Arc::new(app);
    let opt = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).map(String::as_str);
    let flag = |f: &str| args.iter().any(|a| a == f);
    let e = |e: AppError| anyhow!(e.msg);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    match args.first().map(String::as_str) {
        Some("set") => {
            let old = load(&app.db).map_err(e)?;
            let target = match (opt("--target"), &old) {
                (Some(t), _) => t.to_string(),
                (None, Some(o)) => format!("{}@{}:{}", o.username, o.host, o.root),
                (None, None) => anyhow::bail!("--target user@host:/folder is required"),
            };
            let bad = || anyhow!("--target is user@host:/folder");
            let (user, rest) = target.split_once('@').ok_or_else(bad)?;
            let (host, root) = rest.split_once(':').ok_or_else(bad)?;
            let auth = match (flag("--key"), flag("--password")) {
                (true, _) => "key",
                (_, true) => "password",
                _ => old.as_ref().map_or("key", |o| o.auth.as_str()),
            };
            let mut secret = String::new();
            if flag("--key") || flag("--password") {
                eprintln!("{} on stdin:", if auth == "key" { "private key" } else { "password (one line)" });
                io::stdin().read_to_string(&mut secret)?;
                if auth == "password" {
                    secret = secret.lines().next().unwrap_or_default().to_string();
                }
            }
            let req = ConfReq {
                enabled: !flag("--off"),
                time: opt("--time")
                    .map(str::to_string)
                    .or_else(|| old.as_ref().map(|o| o.time.clone()))
                    .unwrap_or_else(|| "03:15".into()),
                sftp: SftpForm {
                    host: host.into(),
                    port: match opt("--port") {
                        Some(p) => Some(p.parse().context("--port")?),
                        None => old.as_ref().map(|o| o.port),
                    },
                    username: user.into(),
                    auth: auth.into(),
                    password: (auth == "password").then(|| secret.clone()),
                    key: (auth == "key").then(|| secret.clone()),
                    passphrase: None,
                    root: Some(root.into()),
                },
                reset_host_key: flag("--reset-host-key"),
                storage: None,
            };
            let (fp, n) = rt.block_on(configure(&app, req)).map_err(e)?;
            eprintln!("saved; target reachable ({n} entries), host key {fp}");
        }
        Some("run") => {
            let conf = load(&app.db).map_err(e)?.context("no backup target configured; see osada backup set")?;
            if let Some(s) = &conf.storage {
                anyhow::bail!("the backup goes to other storage {s}, which the running osada knows: admin → files → backups → run now");
            }
            if db::get_setting(&app.db, "backup_migrated").map_err(e)?.is_some() {
                anyhow::bail!("the nightly backups are in the cluster's settings now: admin → files → backups → run now");
            }
            let j = Job { name: LEGACY.into(), time: conf.time.clone(), enabled: conf.enabled, what: vec!["database".into(), "files".into(), "wiki".into()], target: "own".into() };
            migrate_keys(&app);
            let _held = lock(&app).map_err(e)?;
            rt.block_on(async {
                let a = app.clone();
                let progress = tokio::spawn(async move {
                    let mut last = String::new();
                    loop {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        let s = a.backup.status.lock().unwrap().clone();
                        if !s.is_empty() && s != last {
                            eprintln!("{s}");
                            last = s;
                        }
                    }
                });
                let res = execute(&app, &j, &conf).await;
                progress.abort();
                res
            })?;
        }
        Some("show") => println!("{}", serde_json::to_string_pretty(&overview(&app).map_err(e)?)?),
        _ => anyhow::bail!(
            "usage: osada backup set [--target user@host:/folder] [--port N] [--time HH:MM] [--key|--password] [--off] [--reset-host-key] | run | show"
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86400;

    /// Feeds runs at times `runs` (ascending) through [`retain`] as the backup
    /// does, handing each run's time and the kept set (newest first) to `check`.
    fn simulate(runs: &[i64], mut check: impl FnMut(i64, &[i64])) {
        let mut kept: Vec<i64> = Vec::new();
        for &t in runs {
            kept.insert(0, t);
            kept = retain(&kept).into_iter().map(|i| kept[i]).collect();
            check(t, &kept);
        }
    }

    #[test]
    fn stamps() {
        assert_eq!(stamp_secs("1970-01-01T000000"), Some(0));
        assert_eq!(stamp_secs("2026-09-27T031500"), Some(1_790_478_900));
        assert_eq!(stamp_secs("2000-03-01T000000"), Some(951_868_800));
        for bad in ["test", "2026-09-27T0315", "2026-13-01T000000", "2026-09-27 031500", "2026-09-27T03150x", ".partial-2026"] {
            assert_eq!(stamp_secs(bad), None, "{bad}");
        }
        let t = 1_790_478_900;
        assert_eq!(stamp_secs(&stamp(t + DAY)).unwrap() - stamp_secs(&stamp(t)).unwrap(), DAY);
        assert_eq!(stamp_time(&stamp(t)), Some(t));
        assert_eq!(stamp_time("2026-09-27T0315"), None);
    }

    #[test]
    fn nightly() {
        let runs: Vec<i64> = (0..200).map(|d| d * DAY + 3 * 3600 + 900).collect();
        simulate(&runs, |t, kept| {
            let (day, age) = (t / DAY, |k: i64| (t - k) / DAY);
            assert_eq!(kept[0], t);
            assert_eq!(kept.len(), (day as usize + 1).min(3), "day {day}");
            if day >= 30 {
                assert!((1..=7).contains(&age(kept[1])), "day {day}: {kept:?}");
                assert!((8..=30).contains(&age(kept[2])), "day {day}: {kept:?}");
            }
        });
    }

    #[test]
    fn extra_runs_and_gaps() {
        // nightly, with runs by hand the same day and the server off for days
        let (mut runs, mut t, mut r) = (Vec::new(), 0, 7u64);
        for _ in 0..600 {
            r = r.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            t += match r >> 59 {
                0 => 5 * DAY,
                1..=4 => 3600,
                _ => DAY,
            };
            runs.push(t);
        }
        simulate(&runs, |t, kept| {
            assert_eq!(kept[0], t);
            assert!(kept.len() == 3 || t < runs[2], "{kept:?}");
            assert!(kept.windows(2).all(|w| w[0] > w[1]), "{kept:?}");
            assert!(t - kept[kept.len() - 1] <= 40 * DAY, "{kept:?}");
        });
    }
}

/// The live backups' last passes (Round 14; livebackup.rs).
async fn live_status(State(app): State<Arc<App>>, _a: Admin) -> Res<Json<Value>> {
    Ok(Json(super::livebackup::status_json(&app)))
}
