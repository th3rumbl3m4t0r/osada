//! Copies of file bodies on local disk, named by osada's opaque cache key:
//! `objects/<ab>/<key>` holds the bytes, `<key>.meta` is "etag\nsize\n" and its
//! mtime says when the copy was last used. The least recently used copies go
//! when the total passes the limit.
//!
//! One fill per copy (D-U39): while a copy is being filled, another request for the same file
//! and version follows it (`follow`: the bytes as they arrive on disk) instead of fetching it
//! again (load test 2026-11-17: four first downloads at once made home send it four times).

use std::{
    collections::HashMap,
    fs,
    io::SeekFrom,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::watch,
};

pub struct Cache {
    dir: PathBuf,
    max: u64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    items: HashMap<String, Item>,
    total: u64,
    /// keys being filled right now
    filling: HashMap<String, Filling>,
}

struct Item {
    etag: String,
    size: u64,
    used: SystemTime,
}

/// How far a fill is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Filling,
    Done,
    Broken,
}

struct Filling {
    tmp: PathBuf,
    etag: String,
    size: u64,
    /// bytes written (and flushed), and the phase
    rx: watch::Receiver<(u64, Phase)>,
}

/// osada's keys are sha256 hex; anything else never becomes a file name
fn valid(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The bytes of a copy, whole or a range, as a stream for a body.
pub type Bytes = futures_util::stream::BoxStream<'static, std::io::Result<bytes::Bytes>>;

impl Cache {
    pub fn open(dir: PathBuf, max: u64) -> anyhow::Result<Arc<Cache>> {
        let tmp = dir.join("tmp");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp)?;
        fs::create_dir_all(dir.join("objects"))?;
        let mut st = State::default();
        for sub in fs::read_dir(dir.join("objects"))? {
            for f in fs::read_dir(sub?.path())? {
                let p = f?.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let (key, is_meta) = name.strip_suffix(".meta").map_or((name.as_str(), false), |k| (k, true));
                let (data, meta) = (p.with_file_name(key), p.with_file_name(format!("{key}.meta")));
                if is_meta {
                    if !data.exists() {
                        let _ = fs::remove_file(&meta);
                    }
                    continue;
                }
                let desc = fs::read_to_string(&meta).ok().and_then(|m| {
                    let (etag, size) = m.split_once('\n')?;
                    Some((etag.to_string(), size.trim().parse::<u64>().ok()?))
                });
                let len = fs::metadata(&data).map(|m| m.len()).ok();
                match desc {
                    Some((etag, size)) if valid(key) && Some(size) == len => {
                        let used = fs::metadata(&meta).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                        st.total += size;
                        st.items.insert(key.to_string(), Item { etag, size, used });
                    }
                    // cut off while being written
                    _ => {
                        let _ = fs::remove_file(&data);
                        let _ = fs::remove_file(&meta);
                    }
                }
            }
        }
        tracing::info!("cache {}: {} files, {} MB of {} MB", dir.display(), st.items.len(), st.total >> 20, max >> 20);
        Ok(Arc::new(Cache { dir, max, state: Mutex::new(st) }))
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join("objects").join(&key[..2]).join(key)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The copy, if it is still the file osada just described.
    pub async fn get(&self, key: &str, etag: &str, size: u64) -> Option<tokio::fs::File> {
        {
            let mut st = self.lock();
            let it = st.items.get_mut(key)?;
            if it.etag != etag || it.size != size {
                return None;
            }
            it.used = SystemTime::now();
        }
        let f = tokio::fs::File::open(self.path(key)).await.ok()?;
        let meta = self.path(key).with_extension("meta");
        tokio::task::spawn_blocking(move || {
            let _ = fs::File::options().append(true).open(meta).and_then(|f| f.set_modified(SystemTime::now()));
        });
        Some(f)
    }

    /// Whether a whole copy of this version is here.
    pub fn has(&self, key: &str, etag: &str, size: u64) -> bool {
        self.lock().items.get(key).is_some_and(|i| i.etag == etag && i.size == size)
    }

    /// How far a fill of this version is (bytes written), if one is under way.
    pub fn filling(&self, key: &str, etag: &str, size: u64) -> Option<u64> {
        let st = self.lock();
        let f = st.filling.get(key).filter(|f| f.etag == etag && f.size == size)?;
        let (written, phase) = *f.rx.borrow();
        (phase == Phase::Filling).then_some(written)
    }

    /// Start filling `key` (this version: `etag`, `size`), unless that is already under way or it
    /// is too big.
    pub fn claim(self: &Arc<Self>, key: &str, etag: &str, size: u64) -> Option<Fill> {
        if !valid(key) || size > self.max / 4 {
            return None;
        }
        let tmp = self.dir.join("tmp").join(format!("{key}-{}", std::process::id()));
        let (tx, rx) = watch::channel((0u64, Phase::Filling));
        let file = {
            let mut st = self.lock();
            // under way, or done a moment ago: no second fetch
            if st.filling.contains_key(key) || st.items.get(key).is_some_and(|i| i.etag == etag && i.size == size) {
                return None;
            }
            // there at once, for followers
            let file = fs::File::create(&tmp).ok()?;
            st.filling.insert(key.to_string(), Filling { tmp: tmp.clone(), etag: etag.to_string(), size, rx });
            file
        };
        Some(Fill { cache: self.clone(), key: key.to_string(), tmp, file: Some(tokio::fs::File::from_std(file)), written: 0, size, broken: false, tx })
    }

    /// The bytes `start..start+len` of a copy being filled (this version), as they arrive: a
    /// request for a file another is fetching right now does not fetch it again. None: no such
    /// fill (then the whole copy may be there: `get`).
    pub fn follow(&self, key: &str, etag: &str, size: u64, start: u64, len: u64) -> Option<Bytes> {
        if start.checked_add(len).is_none_or(|end| end > size) {
            return None;
        }
        let (file, mut rx) = {
            let st = self.lock();
            let f = st.filling.get(key).filter(|f| f.etag == etag && f.size == size)?;
            if f.rx.borrow().1 != Phase::Filling {
                return None;
            }
            (fs::File::open(&f.tmp).ok()?, f.rx.clone())
        };
        let (tx, out) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(4);
        tokio::spawn(async move {
            let mut f = tokio::fs::File::from_std(file);
            if f.seek(SeekFrom::Start(start)).await.is_err() {
                return;
            }
            let (mut pos, end) = (start, start + len);
            let mut buf = vec![0u8; 256 << 10];
            while pos < end {
                let (written, phase) = *rx.borrow_and_update();
                if pos < written {
                    let n = (written - pos).min(end - pos).min(buf.len() as u64) as usize;
                    match f.read(&mut buf[..n]).await {
                        Ok(0) => tokio::time::sleep(Duration::from_millis(5)).await,
                        Ok(k) => {
                            pos += k as u64;
                            if tx.send(Ok(bytes::Bytes::copy_from_slice(&buf[..k]))).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    }
                    continue;
                }
                if phase == Phase::Broken || (phase == Phase::Done && written < end) {
                    let _ = tx.send(Err(std::io::Error::other("the copy being fetched broke off"))).await;
                    return;
                }
                if rx.changed().await.is_err() && rx.borrow().0 <= pos {
                    let _ = tx.send(Err(std::io::Error::other("the copy being fetched broke off"))).await;
                    return;
                }
            }
        });
        Some(Box::pin(futures_util::stream::unfold(out, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) })))
    }

    fn evict(&self, st: &mut State) {
        while st.total > self.max {
            let Some(k) = st.items.iter().filter(|(k, _)| !st.filling.contains_key(*k)).min_by_key(|(_, i)| i.used).map(|(k, _)| k.clone())
            else {
                break;
            };
            let it = st.items.remove(&k).expect("listed");
            st.total -= it.size;
            let p = self.path(&k);
            let _ = fs::remove_file(p.with_extension("meta"));
            let _ = fs::remove_file(p);
            tracing::debug!("evicted {k} ({} bytes)", it.size);
        }
    }
}

/// A copy being written; only becomes visible on a complete `commit`, followed meanwhile.
pub struct Fill {
    cache: Arc<Cache>,
    key: String,
    tmp: PathBuf,
    file: Option<tokio::fs::File>,
    written: u64,
    size: u64,
    broken: bool,
    tx: watch::Sender<(u64, Phase)>,
}

impl Fill {
    pub fn broken(&self) -> bool {
        self.broken
    }

    pub async fn write(&mut self, b: &[u8]) {
        if self.broken {
            return;
        }
        if self.file.is_none() {
            match tokio::fs::File::create(&self.tmp).await {
                Ok(f) => self.file = Some(f),
                Err(e) => return self.fail(e),
            }
        }
        let f = self.file.as_mut().expect("opened");
        // flushed before it is said: a follower reads the file from disk
        match async {
            f.write_all(b).await?;
            f.flush().await
        }
        .await
        {
            Ok(()) => {
                self.written += b.len() as u64;
                self.tx.send_replace((self.written, Phase::Filling));
            }
            Err(e) => self.fail(e),
        }
    }

    fn fail(&mut self, e: std::io::Error) {
        tracing::warn!("cache write: {e}");
        self.broken = true;
        self.file = None;
        self.tx.send_replace((self.written, Phase::Broken));
    }

    pub async fn commit(mut self, etag: &str) {
        if self.broken || self.written != self.size || etag.is_empty() || etag.contains('\n') {
            return;
        }
        if self.size == 0 && self.file.is_none() {
            let _ = tokio::fs::File::create(&self.tmp).await;
        }
        if let Some(mut f) = self.file.take() {
            if f.flush().await.is_err() {
                return;
            }
        }
        let dest = self.cache.path(&self.key);
        let meta = dest.with_extension("meta");
        let done = async {
            tokio::fs::create_dir_all(dest.parent().expect("in objects/")).await?;
            tokio::fs::rename(&self.tmp, &dest).await?;
            tokio::fs::write(&meta, format!("{etag}\n{}\n", self.size)).await
        };
        if let Err(e) = done.await {
            tracing::warn!("cache commit: {e}");
            let _ = tokio::fs::remove_file(&dest).await;
            return;
        }
        let mut st = self.cache.lock();
        let item = Item { etag: etag.to_string(), size: self.size, used: SystemTime::now() };
        if let Some(old) = st.items.insert(self.key.clone(), item) {
            st.total -= old.size;
        }
        st.total += self.size;
        st.filling.remove(&self.key);
        self.tx.send_replace((self.written, Phase::Done));
        self.cache.evict(&mut st);
        tracing::debug!("cached {} ({} bytes)", self.key, self.size);
    }
}

impl Drop for Fill {
    fn drop(&mut self) {
        if self.tx.borrow().1 != Phase::Done {
            self.tx.send_replace((self.written, Phase::Broken));
        }
        let mut st = self.cache.lock();
        if st.filling.get(&self.key).is_some_and(|f| f.tmp == self.tmp) {
            st.filling.remove(&self.key);
        }
        drop(st);
        let _ = fs::remove_file(&self.tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn a_second_request_follows_the_fill() {
        let dir = crate::testutil::TempDir::new("cache");
        let c = Cache::open(dir.path().to_path_buf(), 1 << 30).unwrap();
        let key = "ab".repeat(32);
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut fill = c.claim(&key, "\"v1\"", body.len() as u64).unwrap();
        assert!(c.claim(&key, "\"v1\"", body.len() as u64).is_none(), "one fill at a time");
        assert!(c.follow(&key, "\"v2\"", body.len() as u64, 0, body.len() as u64).is_none(), "another version");
        let whole = c.follow(&key, "\"v1\"", body.len() as u64, 0, body.len() as u64).unwrap();
        let part = c.follow(&key, "\"v1\"", body.len() as u64, 1000, 5000).unwrap();
        let writer = tokio::spawn(async move {
            for chunk in body.chunks(7000) {
                fill.write(chunk).await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            fill.commit("\"v1\"").await;
            body
        });
        let got: Vec<u8> = whole.map(|b| b.unwrap().to_vec()).concat().await;
        let got_part: Vec<u8> = part.map(|b| b.unwrap().to_vec()).concat().await;
        let body = writer.await.unwrap();
        assert_eq!(got, body);
        assert_eq!(got_part, body[1000..6000]);
        assert!(c.has(&key, "\"v1\"", body.len() as u64));
        assert!(c.follow(&key, "\"v1\"", body.len() as u64, 0, 1).is_none(), "done: from the copy now");
        // a fill that breaks off: its follower says so
        let key2 = "cd".repeat(32);
        let mut fill = c.claim(&key2, "\"v1\"", 100).unwrap();
        let follower = c.follow(&key2, "\"v1\"", 100, 0, 100).unwrap();
        fill.write(&[1u8; 40]).await;
        drop(fill);
        let r: Vec<std::io::Result<bytes::Bytes>> = follower.collect().await;
        assert!(r.last().unwrap().is_err());
    }
}
