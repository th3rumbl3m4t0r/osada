//! The nightly backup's parts from the cluster's nodes (Round 17): every node's own data (`node`:
//! its cluster store, keys, certificates, static sites) and mail (`mail`: Stalwart's data,
//! settings and program, where it runs). The node that runs the nightly backup (home, the one with
//! the files) asks each node over the link, and itself directly:
//! - `list` makes a consistent copy first (the store with `VACUUM INTO`; Stalwart's data through
//!   the snapshot helper, deploy/stalwart-snapshot.sh, which stops Stalwart for the seconds it
//!   takes to hard-link its files) and answers every file: path, size, mtime, sha256. A node
//!   without mail answers `none`.
//! - `get` sends one of the files the last `list` gave, nothing else.
//!
//! Only the cluster's home may ask.

use std::{
    collections::{BTreeMap, HashMap},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use futures_util::FutureExt;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    link::{self, Link, StreamHandler},
    placement::NodeId,
    osada1::vfs::ByteStream,
};

pub const KIND: &str = "node-backup";

/// A file of a part: its path in the part, size, mtime (seconds, nanoseconds), sha256.
pub type Entry = (String, u64, i64, u32, String);

/// What a node's data leaves out: what is made anew (caches, logs, what home sends again) and
/// the backup's own working copies.
const SKIP: &[&str] = &["cache", "keep", "logs", "web-mirror", "backup-out", "mail-snapshot", "rescue.json"];

/// The files the last `list` of each part gave, by their path in the part.
static LISTED: Mutex<Option<HashMap<String, BTreeMap<String, PathBuf>>>> = Mutex::new(None);

/// sha256 by file, while size, mtime and inode stay.
static SUMS: Mutex<Option<HashMap<PathBuf, ((u64, i64, u32, u64), String)>>> = Mutex::new(None);

fn sum(p: &Path) -> std::io::Result<(u64, i64, u32, String)> {
    let m = std::fs::metadata(p)?;
    let key = (m.len(), m.mtime(), m.mtime_nsec() as u32, m.ino());
    if let Some((k, s)) = SUMS.lock().unwrap().get_or_insert_with(HashMap::new).get(p) {
        if *k == key {
            return Ok((key.0, key.1, key.2, s.clone()));
        }
    }
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut n_total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        n_total += n as u64;
        h.update(&buf[..n]);
    }
    let s = hex::encode(h.finalize());
    // /proc/self/exe and the like say 0 as their size
    let size = if key.0 == 0 { n_total } else { key.0 };
    SUMS.lock().unwrap().get_or_insert_with(HashMap::new).insert(p.to_path_buf(), (key, s.clone()));
    Ok((size, key.1, key.2, s))
}

/// Every regular file under `dir` as (path under `prefix`, file); dot files and `*.part` left out.
fn walk(dir: &Path, prefix: &str, skip_top: &[&str], out: &mut BTreeMap<String, PathBuf>) {
    let mut todo = vec![(dir.to_path_buf(), prefix.to_string(), true)];
    while let Some((d, pre, top)) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.ends_with(".part") || (top && (skip_top.contains(&name.as_str()) || name.starts_with("store.db"))) {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let rel = if pre.is_empty() { name.clone() } else { format!("{pre}/{name}") };
            if ft.is_dir() {
                todo.push((e.path(), rel, false));
            } else if ft.is_file() {
                out.insert(rel, e.path());
            }
        }
    }
}

/// The systemd unit this process runs in, from its cgroup.
fn own_unit() -> Option<String> {
    let cg = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let last = cg.lines().find_map(|l| l.strip_prefix("0::"))?.rsplit('/').next()?.to_string();
    (last.ends_with(".service") && !last.contains("..")).then_some(last)
}

/// The node's data: a consistent copy of its store, the rest of its folder but [`SKIP`], its
/// systemd unit and the program.
fn prepare_node(dir: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let out = dir.join("backup-out");
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut files = BTreeMap::new();
    let store = dir.join("store.db");
    if store.exists() {
        let snap = out.join("store.db");
        let _ = std::fs::remove_file(&snap);
        let c = rusqlite::Connection::open(&store).map_err(|e| format!("{}: {e}", store.display()))?;
        c.busy_timeout(Duration::from_secs(30)).map_err(|e| e.to_string())?;
        c.execute("VACUUM INTO ?1", [snap.to_string_lossy()]).map_err(|e| format!("copying the store: {e}"))?;
        files.insert("store.db".to_string(), snap);
    }
    walk(dir, "", SKIP, &mut files);
    if let Some(unit) = own_unit() {
        for base in ["/etc/systemd/system", "/run/systemd/system"] {
            let p = Path::new(base).join(&unit);
            if std::fs::File::open(&p).is_ok() {
                files.insert(format!("etc/{unit}"), p);
                walk(&Path::new(base).join(format!("{unit}.d")), &format!("etc/{unit}.d"), &[], &mut files);
                break;
            }
        }
    }
    files.insert("bin/osada".into(), PathBuf::from("/proc/self/exe"));
    Ok(files)
}

/// How long the snapshot helper may take.
const MAIL_WAIT: Duration = Duration::from_secs(300);

/// Mail: `<dir>/mail-snapshot/` is there where the snapshot helper is installed. A request with a
/// token, then the helper's answer in `done`: the token, or the token and what went wrong.
async fn prepare_mail(dir: &Path) -> Result<Option<BTreeMap<String, PathBuf>>, String> {
    let d = dir.join("mail-snapshot");
    if !d.is_dir() {
        return Ok(None);
    }
    let token = format!("{:016x}", rand::random::<u64>());
    std::fs::write(d.join("request"), format!("{token}\n")).map_err(|e| format!("asking for a snapshot of mail: {e}"))?;
    let until = tokio::time::Instant::now() + MAIL_WAIT;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let done = std::fs::read_to_string(d.join("done")).unwrap_or_default();
        let done = done.trim();
        if let Some(rest) = done.strip_prefix(&token) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Err(format!("the snapshot of mail: {rest}"));
            }
            break;
        }
        if tokio::time::Instant::now() > until {
            return Err("the snapshot helper did not answer in 5 minutes (is osada-mail-snapshot.path active? deploy/stalwart-snapshot.sh)".into());
        }
    }
    let mut files = BTreeMap::new();
    walk(&d.join("snap"), "", &[], &mut files);
    if files.is_empty() {
        return Err("the snapshot of mail is empty".into());
    }
    Ok(Some(files))
}

/// Makes the part ready and lists it (sha256 too); `None`: this node has no such part.
pub async fn list_local(dir: &Path, part: &str) -> Result<Option<Vec<Entry>>, String> {
    let files = match part {
        "node" => {
            let d = dir.to_path_buf();
            Some(tokio::task::spawn_blocking(move || prepare_node(&d)).await.map_err(|e| e.to_string())??)
        }
        "mail" => prepare_mail(dir).await?,
        _ => return Err(format!("no part {part:?}")),
    };
    let Some(files) = files else { return Ok(None) };
    let (entries, files) = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let mut kept = BTreeMap::new();
        for (rel, p) in files {
            if let Ok((size, s, ns, sha)) = sum(&p) {
                out.push((rel.clone(), size, s, ns, sha));
                kept.insert(rel, p);
            }
        }
        (out, kept)
    })
    .await
    .map_err(|e| e.to_string())?;
    LISTED.lock().unwrap().get_or_insert_with(HashMap::new).insert(part.to_string(), files);
    Ok(Some(entries))
}

/// One of the files the last list of `part` gave.
pub fn listed(part: &str, path: &str) -> Option<PathBuf> {
    LISTED.lock().unwrap().as_ref()?.get(part)?.get(path).cloned()
}

/// A local file as a stream.
pub async fn open_local(part: &str, path: &str) -> Result<ByteStream, String> {
    let p = listed(part, path).ok_or_else(|| format!("{part}/{path} was not listed"))?;
    let f = tokio::fs::File::open(&p).await.map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(Box::pin(tokio_util::io::ReaderStream::with_capacity(f, 256 * 1024)))
}

/// The answering side: `dir` is this node's data folder, `home()` the node that may ask.
pub fn server(dir: PathBuf, home: Arc<dyn Fn() -> Option<NodeId> + Send + Sync>) -> StreamHandler {
    Arc::new(move |peer: NodeId, _kind: String, head: serde_json::Value, mut send: quinn::SendStream, _recv: quinn::RecvStream| {
        let (dir, home) = (dir.clone(), home.clone());
        async move {
            let part = head["part"].as_str().unwrap_or("").to_string();
            if home().as_ref() != Some(&peer) {
                let _ = link::write_head(&mut send, json!({ "ok": false, "error": "only the cluster's home takes the nightly backup" })).await;
                let _ = send.finish();
                return;
            }
            match head["op"].as_str() {
                Some("list") => {
                    let a = match list_local(&dir, &part).await {
                        Ok(None) => json!({ "ok": true, "none": true }),
                        Ok(Some(l)) => json!({ "ok": true, "files": l }),
                        Err(e) => json!({ "ok": false, "error": e }),
                    };
                    let _ = link::write_head(&mut send, a).await;
                }
                Some("get") => {
                    let path = head["path"].as_str().unwrap_or("");
                    match listed(&part, path) {
                        None => {
                            let _ = link::write_head(&mut send, json!({ "ok": false, "error": format!("{part}/{path} was not listed") })).await;
                        }
                        Some(p) => match tokio::fs::File::open(&p).await {
                            Err(e) => {
                                let _ = link::write_head(&mut send, json!({ "ok": false, "error": format!("{path}: {e}") })).await;
                            }
                            Ok(mut f) => {
                                if link::write_head(&mut send, json!({ "ok": true })).await.is_err() {
                                    return;
                                }
                                let _ = send.set_priority(-1);
                                let mut buf = vec![0u8; 256 * 1024];
                                loop {
                                    match tokio::io::AsyncReadExt::read(&mut f, &mut buf).await {
                                        Ok(0) | Err(_) => break,
                                        Ok(n) => {
                                            if send.write_all(&buf[..n]).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        },
                    }
                }
                other => {
                    let _ = link::write_head(&mut send, json!({ "ok": false, "error": format!("unknown op {other:?}") })).await;
                }
            }
            let _ = send.finish();
        }
        .boxed()
    })
}

/// Asks `node` for a part's list; `None`: it has none.
pub async fn list(link: &Link, node: &NodeId, part: &str) -> Result<Option<Vec<Entry>>, String> {
    let (mut tx, mut rx) = link.open_stream(node, KIND, json!({ "op": "list", "part": part })).await.map_err(|e| e.0)?;
    let _ = tx.finish();
    // the mail snapshot may take minutes
    let a = tokio::time::timeout(MAIL_WAIT + Duration::from_secs(120), link::read_head(&mut rx))
        .await
        .map_err(|_| "no answer in 7 minutes".to_string())?
        .map_err(|e| e.0)?;
    if a["ok"] != true {
        return Err(a["error"].as_str().unwrap_or("refused").to_string());
    }
    if a["none"] == true {
        return Ok(None);
    }
    let l = a["files"]
        .as_array()
        .map(|l| {
            l.iter()
                .filter_map(|x| Some((x[0].as_str()?.to_string(), x[1].as_u64()?, x[2].as_i64()?, x[3].as_u64()? as u32, x[4].as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(l))
}

/// One file of a part `node` listed, as a stream.
pub async fn get(link: &Link, node: &NodeId, part: &str, path: &str) -> Result<ByteStream, String> {
    let (mut tx, mut rx) = link.open_stream(node, KIND, json!({ "op": "get", "part": part, "path": path })).await.map_err(|e| e.0)?;
    let _ = tx.finish();
    let a = link::read_head(&mut rx).await.map_err(|e| e.0)?;
    if a["ok"] != true {
        return Err(a["error"].as_str().unwrap_or("refused").to_string());
    }
    let s = futures_util::stream::unfold(Some(rx), |rx| async move {
        let mut rx = rx?;
        let mut buf = vec![0u8; 256 * 1024];
        match rx.read(&mut buf).await {
            Ok(Some(n)) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), Some(rx)))
            }
            Ok(None) => None,
            Err(e) => Some((Err(std::io::Error::other(e.to_string())), None)),
        }
    });
    Ok(Box::pin(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nodes_part() {
        let d = std::env::temp_dir().join(format!("nodebackup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (p, body) in [("idp.key", "k"), ("tls/node.pem", "t"), ("web/site/index.html", "w"), ("logs/x.log", "l"), ("cache/a", "c"), ("store.db-wal", "x"), (".hidden", "h"), ("web/site/.f.part", "p")] {
            let f = d.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, body).unwrap();
        }
        let c = rusqlite::Connection::open(d.join("store.db")).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE kv (k TEXT, v TEXT); INSERT INTO kv VALUES ('a', 'b');").unwrap();
        let files = prepare_node(&d).unwrap();
        let names: Vec<&str> = files.keys().map(String::as_str).filter(|n| !n.starts_with("etc/")).collect();
        assert_eq!(names, ["bin/osada", "idp.key", "store.db", "tls/node.pem", "web/site/index.html"]);
        let copy = rusqlite::Connection::open(&files["store.db"]).unwrap();
        let v: String = copy.query_row("SELECT v FROM kv WHERE k = 'a'", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "b");
        let (size, _, _, sha) = sum(&d.join("idp.key")).unwrap();
        assert_eq!((size, sha.len()), (1, 64));
        assert!(sum(Path::new("/proc/self/exe")).unwrap().0 > 0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
