//! Live backup (Round 14): a copy of the files kept up to date on a target, beside the nightly
//! backup's snapshots (which give earlier versions). Configured in the cluster's settings
//! (`Settings::backups`, admin → files → backups); run here, on the node that holds the files.
//! Every minute a pass walks the files (users/, groups/, wiki/), sends what is new or changed
//! since the last pass (by size and mtime, kept in a manifest) into `live/` on the target, and
//! removes there what is gone here. A pass that fails leaves the manifest as it was for what it
//! did not finish, so the next one takes it up.
//!
//! Targets: other storage of kind SFTP (its server key pinned by its test). S3 and another node
//! come next.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::io::ReaderStream;

use super::{
    App,
    backup::{Item, ensure_dir, walk},
    vfs::{Backend, FsError, RelPath, sftp::Sftp},
};

const EVERY: Duration = Duration::from_secs(60);

#[derive(Default, Serialize, Deserialize)]
pub struct Status {
    pub at: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub files: usize,
    pub bytes: u64,
    pub copied: usize,
    pub copied_bytes: u64,
    pub removed: usize,
    pub took_ms: u64,
}

fn dir(app: &App) -> PathBuf {
    app.cfg.data.join("live-backup")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

type Manifest = BTreeMap<String, (u64, i64, u32)>;

fn load(app: &App, name: &str) -> Manifest {
    std::fs::read(dir(app).join(format!("{name}.json"))).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(app: &App, name: &str, m: &Manifest) {
    let d = dir(app);
    let _ = std::fs::create_dir_all(&d);
    let tmp = d.join(format!("{name}.json.new"));
    if serde_json::to_vec(m).ok().and_then(|b| std::fs::write(&tmp, b).ok()).is_some() {
        let _ = std::fs::rename(&tmp, d.join(format!("{name}.json")));
    }
}

fn save_status(app: &App, name: &str, s: &Status) {
    let d = dir(app);
    let _ = std::fs::create_dir_all(&d);
    if let Ok(b) = serde_json::to_vec(s) {
        let _ = std::fs::write(d.join(format!("{name}.status.json")), b);
    }
}

/// Every live backup's last pass, by name (admin → files).
pub fn statuses(app: &App) -> Value {
    let mut out = serde_json::Map::new();
    if let Ok(rd) = std::fs::read_dir(dir(app)) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if let Some(name) = n.strip_suffix(".status.json") {
                if let Some(v) = std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
                    out.insert(name.to_string(), v);
                }
            }
        }
    }
    Value::Object(out)
}

/// The target of a live backup, connected: SFTP storage, or another node over the link.
fn target(app: &App, name: &str, target: &str) -> Result<Arc<dyn Backend>, String> {
    if let Some(n) = target.strip_prefix("node:") {
        let bridge = app.bridge.as_ref().ok_or("no cluster")?;
        return bridge.live_node_target(n, name).ok_or_else(|| format!("no link to node {n}"));
    }
    sftp_target(app, target).map(|s| s as Arc<dyn Backend>)
}

fn sftp_target(app: &App, target: &str) -> Result<Arc<Sftp>, String> {
    let bridge = app.bridge.as_ref().ok_or("no cluster")?;
    if let Some(name) = target.strip_prefix("storage:") {
        let s = bridge.storage(name).ok_or_else(|| format!("no storage {name}"))?;
        let cfg = s.sftp_cfg(true).map_err(|_| format!("storage {name} is S3: a live backup to S3 comes next"))?;
        if cfg.host_key.is_none() {
            return Err(format!("storage {name}: test it first (its server key is pinned then)"));
        }
        return Ok(Sftp::new(cfg, Arc::new(|_: &str| {})));
    }
    Err(format!("not a target: {target}"))
}

/// One pass of the live backup `name` to `to`.
async fn pass(app: &Arc<App>, name: &str, to: &str) -> Status {
    let started = Instant::now();
    let mut st = Status { at: now_ms(), ..Default::default() };
    // an SFTP target's connection is closed after the pass
    let (t, sftp): (Arc<dyn Backend>, Option<Arc<Sftp>>) = match to.strip_prefix("storage:") {
        Some(_) => match sftp_target(app, to) {
            Ok(s) => (s.clone(), Some(s)),
            Err(e) => {
                st.error = Some(e);
                return st;
            }
        },
        None => match target(app, name, to) {
            Ok(t) => (t, None),
            Err(e) => {
                st.error = Some(e);
                return st;
            }
        },
    };
    let data = app.cfg.data.clone();
    let walked = tokio::task::spawn_blocking(move || -> std::io::Result<(Vec<Item>, Vec<String>)> {
        let (mut items, mut dirs) = (Vec::new(), Vec::new());
        for part in ["users", "groups", "wiki"] {
            walk(&data.join(part), part, &mut items, &mut dirs)?;
        }
        Ok((items, dirs))
    })
    .await;
    let (items, dirs) = match walked {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            st.error = Some(format!("reading the files: {e}"));
            return st;
        }
        Err(e) => {
            st.error = Some(e.to_string());
            return st;
        }
    };
    let mut manifest = load(app, name);
    let res = sync(&*t, &items, &dirs, &mut manifest, &mut st, |m| save(app, name, m)).await;
    save(app, name, &manifest);
    if let Some(s) = sftp {
        s.close().await;
    }
    st.files = items.len();
    st.bytes = items.iter().map(|i| i.seen.0).sum();
    st.took_ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(()) => st.ok = true,
        Err(e) => st.error = Some(e),
    }
    st
}

fn live(p: &str) -> Result<RelPath, String> {
    RelPath::parse(&format!("live/{p}")).map_err(|e| format!("{p}: {e:?}"))
}

/// Brings `t`'s `live/` in line with `items`, as far as `manifest` says it is not yet.
async fn sync(t: &dyn Backend, items: &[Item], dirs: &[String], manifest: &mut Manifest, st: &mut Status, checkpoint: impl Fn(&Manifest)) -> Result<(), String> {
    let mut made: HashSet<String> = HashSet::new();
    let changed: Vec<&Item> = items.iter().filter(|i| manifest.get(&i.remote) != Some(&i.seen)).collect();
    if !changed.is_empty() {
        ensure_dir(t, &mut made, &RelPath::parse("live").map_err(|e| format!("{e:?}"))?).await.map_err(|e| format!("the target: {e:?}"))?;
        let wanted: BTreeSet<&str> = changed.iter().filter_map(|i| i.remote.rsplit_once('/').map(|(d, _)| d)).collect();
        for d in dirs.iter().filter(|d| wanted.contains(d.as_str())) {
            ensure_dir(t, &mut made, &live(d)?).await.map_err(|e| format!("{d} on the target: {e:?}"))?;
        }
    }
    for (n, i) in changed.iter().enumerate() {
        let f = match tokio::fs::File::open(&i.src).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", i.remote)),
        };
        t.write(&live(&i.remote)?, Box::pin(ReaderStream::with_capacity(f, 256 * 1024))).await.map_err(|e| format!("{} to the target: {e:?}", i.remote))?;
        manifest.insert(i.remote.clone(), i.seen);
        st.copied += 1;
        st.copied_bytes += i.seen.0;
        if n % 200 == 199 {
            checkpoint(manifest);
        }
    }
    let here: HashSet<&str> = items.iter().map(|i| i.remote.as_str()).collect();
    let gone: Vec<String> = manifest.keys().filter(|k| !here.contains(k.as_str())).cloned().collect();
    for k in gone {
        match t.remove(&live(&k)?).await {
            Ok(()) | Err(FsError::NotFound) => {}
            Err(e) => return Err(format!("removing {k} on the target: {e:?}")),
        }
        manifest.remove(&k);
        st.removed += 1;
    }
    Ok(())
}

/// The live backups of this node, a pass a minute each.
pub async fn run(app: Arc<App>) {
    // a minute; shorter for tests
    let every = std::env::var("OSADA_LIVE_BACKUP_SECS").ok().and_then(|v| v.parse().ok()).map_or(EVERY, Duration::from_secs);
    loop {
        tokio::time::sleep(every).await;
        let Some(bridge) = app.bridge.clone() else { continue };
        let me = bridge.node_id();
        for b in bridge.live_backups() {
            if b.off || b.nightly() || !(b.source == "all" || Some(&b.source) == me.as_ref()) {
                continue;
            }
            let st = pass(&app, &b.name, &b.target).await;
            if let Some(e) = &st.error {
                tracing::warn!("live backup {}: {e}", b.name);
            } else if st.copied + st.removed > 0 {
                tracing::info!("live backup {}: {} sent ({} bytes), {} removed", b.name, st.copied, st.copied_bytes, st.removed);
            }
            save_status(&app, &b.name, &st);
        }
    }
}

pub fn status_json(app: &App) -> Value {
    json!({ "live": statuses(app) })
}
