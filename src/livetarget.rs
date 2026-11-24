//! Another node as a live backup's target (Round 14): the engine (osada1/livebackup.rs, on the
//! node with the files) writes through `NodeTarget`, a file backend over the link; the target
//! node's `server` keeps the files in `<its files folder>/live-backup/<backup>/` (a file is
//! written aside and renamed in when whole). Paths are relative, without `..` (vfs RelPath).

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt};
use serde_json::json;

use crate::{
    link::{self, Link, StreamHandler},
    placement::NodeId,
    osada1::vfs::{Backend, ByteStream, Entry, FsError, ReadSeek, RelPath, Space},
};

pub const KIND: &str = "live-backup";

pub struct NodeTarget {
    pub link: Arc<Link>,
    pub node: NodeId,
    pub backup: String,
}

fn io(e: impl std::fmt::Display) -> FsError {
    FsError::Remote(e.to_string())
}

impl NodeTarget {
    async fn ask(&self, head: serde_json::Value, data: Option<ByteStream>) -> Result<(), FsError> {
        let (mut send, mut recv) = self.link.open_stream(&self.node, KIND, head).await.map_err(|e| io(e.0))?;
        if let Some(mut data) = data {
            let _ = send.set_priority(-1);
            while let Some(chunk) = data.next().await {
                send.write_all(&chunk.map_err(io)?).await.map_err(io)?;
            }
        }
        let _ = send.finish();
        let a = link::read_head(&mut recv).await.map_err(|e| io(e.0))?;
        if a["ok"] == true { Ok(()) } else { Err(io(a["error"].as_str().unwrap_or("refused"))) }
    }
}

#[async_trait]
impl Backend for NodeTarget {
    async fn stat(&self, _p: &RelPath) -> Result<Entry, FsError> {
        Err(FsError::Invalid("not here".into()))
    }
    async fn list(&self, _p: &RelPath) -> Result<Vec<Entry>, FsError> {
        Err(FsError::Invalid("not here".into()))
    }
    async fn open(&self, _p: &RelPath) -> Result<(Entry, Box<dyn ReadSeek>), FsError> {
        Err(FsError::Invalid("not here".into()))
    }
    async fn write(&self, p: &RelPath, data: ByteStream) -> Result<Entry, FsError> {
        self.ask(json!({ "op": "put", "backup": self.backup, "path": p.to_string() }), Some(data)).await?;
        Ok(Entry { name: p.to_string(), is_dir: false, size: 0, mtime: 0, nanos: 0 })
    }
    async fn mkdir(&self, _p: &RelPath) -> Result<(), FsError> {
        // the target makes a file's folders with it
        Ok(())
    }
    async fn remove(&self, p: &RelPath) -> Result<(), FsError> {
        self.ask(json!({ "op": "delete", "backup": self.backup, "path": p.to_string() }), None).await
    }
    async fn rename(&self, _from: &RelPath, _to: &RelPath) -> Result<(), FsError> {
        Err(FsError::Invalid("not here".into()))
    }
    async fn space(&self, _p: &RelPath) -> Result<Option<Space>, FsError> {
        Ok(None)
    }
}

/// The target side of a live backup: `base()` is this node's files folder.
pub fn server(base: Arc<dyn Fn() -> PathBuf + Send + Sync>) -> StreamHandler {
    receiver(Arc::new(move |head: &serde_json::Value| {
        let backup = head["backup"].as_str().unwrap_or("");
        if !crate::storage::valid_name(backup) {
            return Err(format!("not a backup's name: {backup:?}"));
        }
        Ok(base().join("live-backup").join(backup))
    }))
}

/// The stream kind of a site's copy from the authoritative web node (Round 16).
pub const WEB_MIRROR: &str = "web-mirror";

/// The node a site runs on: what the authoritative web node sends of it goes to `<data>/web/<site>/`.
pub fn web_mirror_server(data: PathBuf) -> StreamHandler {
    receiver_with(true, Arc::new(move |head: &serde_json::Value| {
        let site = head["site"].as_str().unwrap_or("");
        let ok = !site.is_empty() && site.len() <= 63 && site.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && site.as_bytes()[0].is_ascii_lowercase();
        if !ok {
            return Err(format!("not a site's name: {site:?}"));
        }
        Ok(data.join("web").join(site))
    }))
}

/// A path inside `folder` for `path` (relative segments, none `..`), or why not.
fn inside(folder: &std::path::Path, path: &str) -> Result<PathBuf, String> {
    let rel = RelPath::parse(path).map_err(|e| format!("{e:?}"))?;
    // RelPath prints with a leading `/`, which Path::join would take as the root
    let segs: Vec<String> = rel.to_string().split('/').filter(|s| !s.is_empty()).map(str::to_string).collect();
    if segs.is_empty() || segs.iter().any(|s| s == ".." || s == "." || s.contains('\\')) {
        return Err(format!("not a path in the folder: {rel}"));
    }
    let target = segs.iter().fold(folder.to_path_buf(), |p, s| p.join(s));
    if !target.starts_with(folder) {
        return Err(format!("not a path in the folder: {rel}"));
    }
    Ok(target)
}

type FolderFor = Arc<dyn Fn(&serde_json::Value) -> Result<PathBuf, String> + Send + Sync>;

/// Puts (`op: put`, the body the file) and deletes (`op: delete`) under the folder `folder_for`
/// gives for the request; a file is written aside and renamed in when whole.
fn receiver(folder_for: FolderFor) -> StreamHandler {
    receiver_with(false, folder_for)
}

/// With `readable`, also `op: list` (every file under the folder: path, size, mtime) and `op: get`
/// (a file's bytes after the answer): the authoritative web node takes a site's files over.
fn receiver_with(readable: bool, folder_for: FolderFor) -> StreamHandler {
    Arc::new(move |peer: NodeId, _kind: String, head: serde_json::Value, mut send: quinn::SendStream, mut recv: quinn::RecvStream| {
        let folder_for = folder_for.clone();
        async move {
            if readable && matches!(head["op"].as_str(), Some("list") | Some("get")) {
                let r: Result<(serde_json::Value, Option<PathBuf>), String> = (|| {
                    let folder = folder_for(&head)?;
                    if head["op"] == "list" {
                        let mut out = Vec::new();
                        let mut todo = vec![folder.clone()];
                        while let Some(d) = todo.pop() {
                            let Ok(rd) = std::fs::read_dir(&d) else { continue };
                            for e in rd.flatten() {
                                let Ok(m) = e.metadata() else { continue };
                                if m.is_dir() {
                                    todo.push(e.path());
                                } else if m.is_file() {
                                    if let Ok(rel) = e.path().strip_prefix(&folder) {
                                        let rel = rel.to_string_lossy().to_string();
                                        if !rel.rsplit('/').next().unwrap_or("").starts_with('.') {
                                            out.push(json!([rel, m.len()]));
                                        }
                                    }
                                }
                            }
                        }
                        Ok((json!({ "ok": true, "files": out }), None))
                    } else {
                        let t = inside(&folder, head["path"].as_str().unwrap_or(""))?;
                        Ok((json!({ "ok": true }), Some(t)))
                    }
                })();
                match r {
                    Ok((answer, file)) => {
                        if link::write_head(&mut send, answer).await.is_err() {
                            return;
                        }
                        if let Some(p) = file {
                            if let Ok(mut f) = tokio::fs::File::open(&p).await {
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
                        }
                        let _ = send.finish();
                    }
                    Err(e) => {
                        let _ = link::write_head(&mut send, json!({ "ok": false, "error": e })).await;
                        let _ = send.finish();
                    }
                }
                return;
            }
            let r = async {
                let folder = folder_for(&head)?;
                let target = inside(&folder, head["path"].as_str().unwrap_or(""))?;
                match head["op"].as_str() {
                    Some("put") => {
                        let dir = target.parent().ok_or("no folder")?.to_path_buf();
                        tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
                        let tmp = dir.join(format!(".{}.part", target.file_name().and_then(|n| n.to_str()).unwrap_or("x")));
                        let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
                        let mut buf = vec![0u8; 256 * 1024];
                        loop {
                            match recv.read(&mut buf).await.map_err(|e| e.to_string())? {
                                Some(0) | None => break,
                                Some(n) => tokio::io::AsyncWriteExt::write_all(&mut f, &buf[..n]).await.map_err(|e| e.to_string())?,
                            }
                        }
                        tokio::io::AsyncWriteExt::flush(&mut f).await.map_err(|e| e.to_string())?;
                        drop(f);
                        tokio::fs::rename(&tmp, &target).await.map_err(|e| e.to_string())?;
                        Ok(())
                    }
                    Some("delete") => match tokio::fs::metadata(&target).await {
                        Ok(m) if m.is_dir() => tokio::fs::remove_dir_all(&target).await.map_err(|e| e.to_string()),
                        Ok(_) => tokio::fs::remove_file(&target).await.map_err(|e| e.to_string()),
                        Err(_) => Ok(()),
                    },
                    other => Err(format!("unknown op {other:?}")),
                }
            }
            .await;
            if let Err(e) = &r {
                eprintln!("a copy from {peer}: {e}");
            }
            let answer = match r {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "ok": false, "error": e }),
            };
            if link::write_head(&mut send, answer).await.is_ok() {
                let _ = send.finish();
            }
        }
        .boxed()
    })
}

/// A site's files on `node` (path, size), for taking it over.
pub async fn list(link: &Link, node: &NodeId, site: &str) -> Result<Vec<(String, u64)>, String> {
    let (mut tx, mut rx) = link.open_stream(node, WEB_MIRROR, json!({ "op": "list", "site": site })).await.map_err(|e| e.0)?;
    let _ = tx.finish();
    let a = link::read_head(&mut rx).await.map_err(|e| e.0)?;
    if a["ok"] != true {
        return Err(a["error"].as_str().unwrap_or("refused").to_string());
    }
    Ok(a["files"].as_array().map(|l| l.iter().filter_map(|x| Some((x[0].as_str()?.to_string(), x[1].as_u64()?))).collect()).unwrap_or_default())
}

/// One of a site's files on `node`, into `to` (written aside, renamed in when whole).
pub async fn get(link: &Link, node: &NodeId, site: &str, path: &str, folder: &std::path::Path) -> Result<(), String> {
    let to = inside(folder, path)?;
    let (mut tx, mut rx) = link.open_stream(node, WEB_MIRROR, json!({ "op": "get", "site": site, "path": path })).await.map_err(|e| e.0)?;
    let _ = tx.finish();
    let a = link::read_head(&mut rx).await.map_err(|e| e.0)?;
    if a["ok"] != true {
        return Err(a["error"].as_str().unwrap_or("refused").to_string());
    }
    let dir = to.parent().ok_or("no folder")?.to_path_buf();
    tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{}.part", to.file_name().and_then(|n| n.to_str()).unwrap_or("x")));
    let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match rx.read(&mut buf).await.map_err(|e| e.to_string())? {
            Some(0) | None => break,
            Some(n) => tokio::io::AsyncWriteExt::write_all(&mut f, &buf[..n]).await.map_err(|e| e.to_string())?,
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut f).await.map_err(|e| e.to_string())?;
    drop(f);
    tokio::fs::rename(&tmp, &to).await.map_err(|e| e.to_string())
}

/// Sends a put (with `file`'s bytes) or a delete to `node`; its answer.
pub async fn send(link: &Link, node: &NodeId, kind: &str, head: serde_json::Value, file: Option<&std::path::Path>) -> Result<(), String> {
    let (mut tx, mut rx) = link.open_stream(node, kind, head).await.map_err(|e| e.0)?;
    if let Some(p) = file {
        let _ = tx.set_priority(-1);
        let mut f = tokio::fs::File::open(p).await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut f, &mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            tx.write_all(&buf[..n]).await.map_err(|e| e.to_string())?;
        }
    }
    let _ = tx.finish();
    let a = link::read_head(&mut rx).await.map_err(|e| e.0)?;
    if a["ok"] == true { Ok(()) } else { Err(a["error"].as_str().unwrap_or("refused").to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug found by tests/live_backup_node.py: RelPath's leading `/` made Path::join write at
    /// the root. Every path lands inside the backup's folder now.
    #[test]
    fn paths_stay_in_the_folder() {
        let folder = PathBuf::from("/data/live-backup/b");
        for p in ["live/users/a.txt", "/live/users/a.txt"] {
            assert_eq!(inside(&folder, p).unwrap(), PathBuf::from("/data/live-backup/b/live/users/a.txt"));
        }
        for bad in ["", "/", "../etc/passwd", "a/../../b"] {
            assert!(inside(&folder, bad).map_or(true, |t| t.starts_with(&folder) && t != folder), "{bad}");
        }
    }
}
