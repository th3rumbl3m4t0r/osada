//! What the edge keeps for when osada at home does not answer:
//! - the two pages (the app's, a public link's) as home last sent them; they are
//!   the same for every address of their kind;
//! - every /static/ file by version (a version never changes), fetched as soon
//!   as a page names it, dropped when no kept page does;
//! - for public links: their JSON answers (listings, info, markdown), and for
//!   their files which copy in the file cache (cache.rs) is that file, and the
//!   headers to send it with.
//!
//! A link's answers go out only for links on the list osada gave last
//! (`/_edge/links`: removed and expired ones are not on it), and for KEEP_FOR
//! after home last gave them. Logged-in answers are never kept: nobody could
//! check the login.
//!
//! On disk `keep/<ab>/<sha256 of what>`: "what\nstatus\nname: value\n…\n\n" and
//! the body, written whole and renamed into place; `keep/links` is the list.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use sha2::{Digest, Sha256};

/// a link's answers are given for this long after home last gave them
pub const KEEP_FOR: Duration = Duration::from_secs(30 * 86400);
/// bigger answers are not kept here (file bodies go to the file cache)
pub const KEEP_MAX: u64 = 8 << 20;
/// a file's headers are written again after this long, so that KEEP_FOR counts from the last use
const REFRESH: Duration = Duration::from_secs(86400);
/// where a file of a link is in the file cache
const OBJECT: &str = "x-edge-object";

pub struct Keep {
    dir: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// what -> when home last gave it, and for a file the ETag of its copy
    items: HashMap<String, (SystemTime, String)>,
    /// the public links that work: token -> expires (0: never); None until osada first said
    links: Option<HashMap<String, i64>>,
}

pub struct Kept {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    /// a file: its copy in the file cache (key, etag, size)
    pub object: Option<(String, String, u64)>,
    pub at: SystemTime,
}

/// headers a kept answer goes out with; the rest is about one connection or one moment
fn keepable(name: &HeaderName) -> bool {
    let n = name.as_str();
    !(super::dropped(n) || n == "content-length" || n == "date" || n == "set-cookie" || n == "content-range" || n == OBJECT)
}

fn parse(raw: &[u8]) -> Option<(String, Kept)> {
    let end = raw.windows(2).position(|w| w == b"\n\n")?;
    let head = std::str::from_utf8(&raw[..end]).ok()?;
    let mut lines = head.split('\n');
    let what = lines.next()?.to_string();
    let status = StatusCode::from_u16(lines.next()?.parse().ok()?).ok()?;
    let (mut headers, mut object) = (HeaderMap::new(), None);
    for l in lines {
        let (k, v) = l.split_once(": ")?;
        if k == OBJECT {
            // "<key> <etag> <size>"; an ETag has no spaces in it
            let mut p = v.split(' ');
            object = Some((p.next()?.to_string(), p.next()?.to_string(), p.next()?.parse().ok()?));
            continue;
        }
        headers.append(HeaderName::from_bytes(k.as_bytes()).ok()?, HeaderValue::from_str(v).ok()?);
    }
    Some((what, Kept { status, headers, body: raw[end + 2..].to_vec(), object, at: SystemTime::UNIX_EPOCH }))
}

/// the token of a link's address: /s/<token>/…
pub fn token_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/s/")?;
    let t = rest.split('/').next()?;
    (!t.is_empty()).then_some(t)
}

fn token_in(what: &str) -> Option<&str> {
    token_of(what.split_once(':')?.1)
}

impl Keep {
    pub fn open(dir: PathBuf) -> anyhow::Result<Keep> {
        fs::create_dir_all(dir.join("tmp"))?;
        let mut st = State::default();
        for sub in fs::read_dir(&dir)? {
            let sub = sub?.path();
            if !sub.is_dir() || sub.file_name().is_some_and(|n| n == "tmp") {
                continue;
            }
            for f in fs::read_dir(sub)? {
                let p = f?.path();
                let at = fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                match fs::read(&p).ok().as_deref().and_then(parse) {
                    Some((what, k)) => {
                        st.items.insert(what, (at, k.object.map(|o| o.1).unwrap_or_default()));
                    }
                    None => {
                        let _ = fs::remove_file(&p);
                    }
                }
            }
        }
        if let Ok(t) = fs::read_to_string(dir.join("links")) {
            st.links = Some(links(&t));
        }
        tracing::info!("kept for when home is away: {} answers, {} links", st.items.len(), st.links.as_ref().map_or(0, |l| l.len()));
        Ok(Keep { dir, state: Mutex::new(st) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn path(&self, what: &str) -> PathBuf {
        let k = hex::encode(Sha256::digest(what));
        self.dir.join(&k[..2]).join(k)
    }

    pub fn has(&self, what: &str) -> bool {
        self.lock().items.contains_key(what)
    }

    /// a file's copy known with this ETag, and not long ago
    pub fn knows(&self, what: &str, etag: &str) -> bool {
        self.lock().items.get(what).is_some_and(|(at, e)| e == etag && at.elapsed().is_ok_and(|d| d < REFRESH))
    }

    pub async fn put(&self, what: &str, status: StatusCode, headers: &HeaderMap, body: &[u8], object: Option<(&str, &str, u64)>) {
        let mut raw = format!("{what}\n{}\n", status.as_u16());
        for (k, v) in headers.iter().filter(|(k, _)| keepable(k)) {
            if let Ok(v) = v.to_str() {
                raw += &format!("{k}: {v}\n");
            }
        }
        if let Some((key, etag, size)) = object {
            if etag.contains([' ', '\n']) {
                return;
            }
            raw += &format!("{OBJECT}: {key} {etag} {size}\n");
        }
        raw += "\n";
        let mut raw = raw.into_bytes();
        raw.extend_from_slice(body);
        let (dest, tmp) = (self.path(what), self.dir.join("tmp").join(format!("{}-{}", hex::encode(&Sha256::digest(what)[..8]), std::process::id())));
        let done = async {
            tokio::fs::create_dir_all(dest.parent().expect("in keep/")).await?;
            tokio::fs::write(&tmp, &raw).await?;
            tokio::fs::rename(&tmp, &dest).await
        };
        if let Err(e) = done.await {
            tracing::warn!("keep {what}: {e}");
            let _ = tokio::fs::remove_file(&tmp).await;
            return;
        }
        self.lock().items.insert(what.to_string(), (SystemTime::now(), object.map(|o| o.1.to_string()).unwrap_or_default()));
    }

    pub async fn get(&self, what: &str) -> Option<Kept> {
        let at = self.lock().items.get(what)?.0;
        let raw = tokio::fs::read(self.path(what)).await.ok()?;
        let (w, mut k) = parse(&raw)?;
        (w == what).then(|| {
            k.at = at;
            k
        })
    }

    pub async fn forget(&self, what: &str) {
        if self.lock().items.remove(what).is_some() {
            let _ = tokio::fs::remove_file(self.path(what)).await;
        }
    }

    pub async fn forget_where(&self, f: impl Fn(&str) -> bool) {
        let gone: Vec<String> = {
            let mut st = self.lock();
            let gone: Vec<String> = st.items.keys().filter(|w| f(w)).cloned().collect();
            for w in &gone {
                st.items.remove(w);
            }
            gone
        };
        for w in gone {
            let _ = tokio::fs::remove_file(self.path(&w)).await;
        }
    }

    /// osada's list of the links that work; what is kept of the others goes
    pub async fn set_links(&self, text: &str) {
        let l = links(text);
        let changed = self.lock().links.as_ref() != Some(&l);
        if !changed {
            return;
        }
        let tmp = self.dir.join("tmp").join("links");
        if tokio::fs::write(&tmp, text).await.is_ok() {
            let _ = tokio::fs::rename(&tmp, self.dir.join("links")).await;
        }
        let ok: HashSet<String> = l.keys().cloned().collect();
        self.lock().links = Some(l);
        self.forget_where(|w| token_in(w).is_some_and(|t| !ok.contains(t))).await;
    }

    /// Some(true): a link that works; Some(false): one that does not; None: osada never said
    pub fn link(&self, token: &str) -> Option<bool> {
        let st = self.lock();
        let exp = st.links.as_ref()?.get(token).copied();
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        Some(exp.is_some_and(|e| e == 0 || e > now))
    }
}

/// "<token> <expires>" lines; anything else is skipped
fn links(text: &str) -> HashMap<String, i64> {
    text.lines()
        .filter_map(|l| {
            let (t, e) = l.trim().split_once(' ')?;
            let ok = !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
            ok.then(|| Some((t.to_string(), e.trim().parse().ok()?))).flatten()
        })
        .collect()
}

/// a kept answer that is gzipped
pub fn is_gzip(h: &HeaderMap) -> bool {
    h.get(header::CONTENT_ENCODING).is_some_and(|v| v == "gzip")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kept_answers_and_links() {
        let dir = std::env::temp_dir().join(format!("keep-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let k = Keep::open(dir.clone()).unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("2"));
        h.insert("x-shelf-cache", HeaderValue::from_static("secret"));
        k.put("link:/s/Tok1/ls", StatusCode::OK, &h, b"{}\n\nx", None).await;
        k.put("file:/s/Tok2/raw/a.txt", StatusCode::OK, &h, b"", Some(("ab".repeat(32).as_str(), "\"5-6-7\"", 5))).await;
        let g = k.get("link:/s/Tok1/ls").await.unwrap();
        assert_eq!((g.status, g.body.as_slice()), (StatusCode::OK, &b"{}\n\nx"[..]));
        assert!(g.headers.get("content-type").is_some() && g.headers.get("content-length").is_none() && g.headers.get("x-shelf-cache").is_none());
        assert!(k.knows("file:/s/Tok2/raw/a.txt", "\"5-6-7\"") && !k.knows("file:/s/Tok2/raw/a.txt", "\"5-6-8\""));
        // read back from disk
        let k = Keep::open(dir.clone()).unwrap();
        assert_eq!(k.get("file:/s/Tok2/raw/a.txt").await.unwrap().object, Some(("ab".repeat(32), "\"5-6-7\"".into(), 5)));
        assert_eq!(k.link("Tok1"), None);
        k.set_links("Tok1 0\nTok3 1\nbad token 5\n").await;
        assert_eq!((k.link("Tok1"), k.link("Tok2"), k.link("Tok3")), (Some(true), Some(false), Some(false)));
        assert!(k.has("link:/s/Tok1/ls") && !k.has("file:/s/Tok2/raw/a.txt"));
        assert_eq!(Keep::open(dir.clone()).unwrap().link("Tok1"), Some(true));
        assert_eq!(token_of("/s/abc/raw/x"), Some("abc"));
        assert_eq!(token_of("/s/"), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
