//! Other storage (T2): places outside the cluster's nodes that files can go
//! to, an SFTP server's folder or an S3 bucket. Kept in the cluster's
//! settings (the cluster log) by name; their secrets (a password, a private
//! key, an S3 secret key) are never shown back: the page gets what is set,
//! and an empty field keeps what was there (as log shipping's secret, T3).
//!
//! The nightly backup may go to an SFTP one. Files placed on them, mirrors
//! and stripes over nodes and other storage are A12's (chunked storage, the
//! next release).

use std::{collections::BTreeMap, time::Duration};

use ring::hmac;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// At most this many.
pub const MAX: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Storage {
    #[serde(flatten)]
    pub target: Target,
    #[serde(default)]
    pub secret: Secret,
    /// What it is for, for people.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Target {
    Sftp {
        host: String,
        #[serde(default = "port22")]
        port: u16,
        username: String,
        /// `password` or `key`
        #[serde(default = "by_password")]
        auth: String,
        /// The folder; empty: the login's home.
        #[serde(default)]
        root: String,
        /// The server's key (`SHA256:…`), pinned at the first test.
        #[serde(default)]
        host_key: Option<String>,
    },
    S3 {
        /// `https://host[:port]`
        endpoint: String,
        #[serde(default)]
        region: String,
        bucket: String,
        #[serde(default)]
        prefix: String,
        access_key: String,
        /// The bucket in the host name (AWS); else in the path (MinIO,
        /// Ceph, Garage, most others).
        #[serde(default)]
        virtual_host: bool,
    },
}

fn port22() -> u16 {
    22
}

fn by_password() -> String {
    "password".into()
}

/// The parts never shown back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub passphrase: String,
    /// S3's secret access key.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub secret_key: String,
}

impl Secret {
    /// The names of the parts that are set.
    pub fn set(&self) -> Vec<&'static str> {
        [("password", &self.password), ("key", &self.key), ("passphrase", &self.passphrase), ("secret_key", &self.secret_key)]
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(n, _)| n)
            .collect()
    }
}

/// What the page sends: the storage, and what to forget.
#[derive(Clone, Debug, Deserialize)]
pub struct Change {
    #[serde(flatten)]
    pub storage: Storage,
    /// Forget the secret's parts left empty (those typed are kept).
    #[serde(default)]
    pub clear_secret: bool,
    /// Forget the pinned host key (SFTP).
    #[serde(default)]
    pub reset_host_key: bool,
}

pub fn valid_name(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn plain(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(|c| c.is_control())
}

fn word(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// A host name or an address (IPv6 without brackets).
fn host_ok(h: &str) -> bool {
    h.parse::<std::net::IpAddr>().is_ok()
        || ((1..=253).contains(&h.len())
            && h.split('.').all(|l| {
                (1..=63).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !l.starts_with('-') && !l.ends_with('-')
            }))
}

fn local_host(h: &str) -> bool {
    matches!(h, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

/// `https://host[:port]` (http only to this machine): (https, host, port).
pub fn parse_endpoint(e: &str) -> Result<(bool, String, Option<u16>), String> {
    let e = e.trim_end_matches('/');
    let (tls, rest) = match (e.strip_prefix("https://"), e.strip_prefix("http://")) {
        (Some(r), _) => (true, r),
        (_, Some(r)) => (false, r),
        _ => return Err("the endpoint is https://host[:port]".into()),
    };
    if rest.is_empty() || rest.contains(['/', '?', '#', '@']) || rest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the endpoint is https://host[:port], nothing after".into());
    }
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (a, after) = v6.split_once(']').ok_or("the endpoint's address is not closed with ]")?;
        (format!("[{a}]"), after.strip_prefix(':'))
    } else {
        match rest.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (rest.to_string(), None),
        }
    };
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok().filter(|&p| p > 0).ok_or("the endpoint's port is not one")?),
        None => None,
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if !host_ok(bare) {
        return Err(format!("the endpoint's host {host:?} is not one"));
    }
    if !tls && !local_host(&host) {
        return Err("the endpoint is https (http only to this machine)".into());
    }
    Ok((tls, host.to_lowercase(), port))
}

fn bucket_ok(b: &str) -> bool {
    (3..=63).contains(&b.len())
        && b.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'-')
        && b.as_bytes()[0].is_ascii_alphanumeric()
        && b.as_bytes()[b.len() - 1].is_ascii_alphanumeric()
        && !b.contains("..")
}

impl Storage {
    /// Trims what people type and fills the defaults.
    pub fn tidy(&mut self) {
        self.note = self.note.trim().to_string();
        match &mut self.target {
            Target::Sftp { host, port, username, auth, root, .. } => {
                *host = host.trim().to_lowercase();
                if *port == 0 {
                    *port = 22;
                }
                *username = username.trim().to_string();
                *auth = auth.trim().to_string();
                *root = root.trim().to_string();
            }
            Target::S3 { endpoint, region, bucket, prefix, access_key, .. } => {
                *endpoint = endpoint.trim().trim_end_matches('/').to_string();
                *region = region.trim().to_string();
                if region.is_empty() {
                    *region = "us-east-1".into();
                }
                *bucket = bucket.trim().to_string();
                *prefix = prefix.trim().trim_start_matches('/').to_string();
                *access_key = access_key.trim().to_string();
            }
        }
        if !self.secret.key.is_empty() {
            self.secret.key = self.secret.key.trim().to_string() + "\n";
        }
    }

    pub fn check(&self, name: &str) -> Result<(), String> {
        let bad = |what: &str| Err(format!("storage {name}: {what}"));
        if !valid_name(name) {
            return Err(format!("{name:?} is not a storage name: a-z first, then a-z, 0-9 and -, at most 40"));
        }
        if !plain(&self.note, 200) {
            return bad("the note is too long or has control characters");
        }
        let s = &self.secret;
        if !plain(&s.password, 1024) || !plain(&s.passphrase, 1024) || !plain(&s.secret_key, 1024) {
            return bad("a secret is too long or has control characters");
        }
        if s.key.len() > 16384 || s.key.chars().any(|c| c.is_control() && c != '\n' && c != '\r') {
            return bad("the private key is too long or has control characters");
        }
        match &self.target {
            Target::Sftp { host, port, username, auth, root, host_key } => {
                if !host_ok(host) {
                    return bad("the host is a name or an address");
                }
                if *port == 0 {
                    return bad("the port is 1 to 65535");
                }
                if !word(username, 128) {
                    return bad("the user is needed, without spaces");
                }
                match auth.as_str() {
                    "password" if s.password.is_empty() => return bad("the password is needed"),
                    "key" if s.key.is_empty() => return bad("the private key is needed"),
                    "password" | "key" => {}
                    _ => return bad("it logs in with a password or a key"),
                }
                if !plain(root, 1024) {
                    return bad("the folder is too long or has control characters");
                }
                if host_key.as_ref().is_some_and(|k| !word(k, 200)) {
                    return bad("the host key is not one");
                }
            }
            Target::S3 { endpoint, region, bucket, prefix, access_key, .. } => {
                if let Err(e) = parse_endpoint(endpoint) {
                    return bad(&e);
                }
                if !(1..=32).contains(&region.len()) || !region.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
                    return bad("the region is a-z, 0-9 and - (us-east-1 when empty)");
                }
                if !bucket_ok(bucket) {
                    return bad("the bucket is 3 to 63 of a-z, 0-9, . and -, a letter or digit at both ends");
                }
                if !plain(prefix, 512) {
                    return bad("the prefix is too long or has control characters");
                }
                if !word(access_key, 128) {
                    return bad("the access key is needed, without spaces");
                }
                if s.secret_key.is_empty() {
                    return bad("the secret key is needed");
                }
            }
        }
        Ok(())
    }

    /// Where it is, for people: `sftp://user@host:port/folder`, `s3://bucket/prefix at endpoint`.
    pub fn place(&self) -> String {
        match &self.target {
            Target::Sftp { host, port, username, root, .. } => {
                let p = if *port == 22 { String::new() } else { format!(":{port}") };
                let r = if root.is_empty() || root.starts_with('/') { root.clone() } else { format!("~/{root}") };
                format!("sftp://{username}@{host}{p}{r}")
            }
            Target::S3 { endpoint, bucket, prefix, .. } => format!("s3://{bucket}/{prefix} at {endpoint}"),
        }
    }

    /// An SFTP one's connection (the backup's target, the test).
    pub fn sftp_cfg(&self, private: bool) -> Result<crate::osada1::vfs::sftp::MountCfg, String> {
        use crate::osada1::vfs::sftp::{Auth, MountCfg};
        let Target::Sftp { host, port, username, auth, root, host_key } = &self.target else {
            return Err("not an SFTP storage".into());
        };
        let auth = match auth.as_str() {
            "key" => Auth::Key { pem: self.secret.key.clone(), passphrase: Some(self.secret.passphrase.clone()).filter(|p| !p.is_empty()) },
            _ => Auth::Password(self.secret.password.clone()),
        };
        Ok(MountCfg { id: 0, host: host.clone(), port: *port, user: username.clone(), auth, root: root.clone(), host_key: host_key.clone(), private })
    }

    pub fn kind(&self) -> &'static str {
        match self.target {
            Target::Sftp { .. } => "sftp",
            Target::S3 { .. } => "s3",
        }
    }

    /// As shown: without the secret, with which of its parts are set.
    pub fn shown(&self) -> serde_json::Value {
        let mut s = self.clone();
        s.secret = Secret::default();
        let mut v = serde_json::to_value(&s).unwrap_or_default();
        v["secret_set"] = serde_json::json!(self.secret.set());
        v["place"] = serde_json::json!(self.place());
        v
    }
}

/// `new` (from the page) over `old`: secret parts left empty keep the old
/// ones of the same kind unless `clear`; a pinned host key stays while the
/// host and port do, unless `reset`.
pub fn merged(mut new: Storage, old: Option<&Storage>, clear: bool, reset: bool) -> Storage {
    new.tidy();
    let same_kind = old.is_some_and(|o| o.kind() == new.kind());
    if let (Some(old), true, false) = (old, same_kind, clear) {
        let (n, o) = (&mut new.secret, &old.secret);
        for (nv, ov) in [(&mut n.password, &o.password), (&mut n.key, &o.key), (&mut n.passphrase, &o.passphrase), (&mut n.secret_key, &o.secret_key)] {
            if nv.is_empty() {
                nv.clone_from(ov);
            }
        }
    }
    let old_key = match old.map(|o| &o.target) {
        Some(Target::Sftp { host, port, host_key, .. }) => Some((host.clone(), *port, host_key.clone())),
        _ => None,
    };
    if let Target::Sftp { host, port, host_key, .. } = &mut new.target {
        *host_key = match old_key {
            Some((h, p, k)) if !reset && h == *host && p == *port => k,
            _ => None,
        };
    }
    new
}

/// Checks all of them.
pub fn check_all(all: &BTreeMap<String, Storage>) -> Result<(), String> {
    if all.len() > MAX {
        return Err(format!("at most {MAX} other storages"));
    }
    all.iter().try_for_each(|(n, s)| s.check(n))
}

// ---- S3: a signed ListObjectsV2 (signature version 4) --------------------

fn hmac256(key: &[u8], data: &str) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data.as_bytes()).as_ref().to_vec()
}

fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// RFC 3986 unreserved kept, the rest %XX (S3's URI encoding).
fn uri_encode(s: &str, slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') || (b == b'/' && !slash) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `20130524T000000Z` of unix seconds.
pub fn amz_date(secs: u64) -> String {
    let iso = crate::node::ship::iso_utc(secs * 1000);
    let d: String = iso[..19].chars().filter(|c| *c != '-' && *c != ':').collect();
    format!("{d}Z")
}

/// The headers for a GET of `uri` (encoded) with `query` (name, value
/// pairs, unencoded) at `host` (with its port when not the scheme's):
/// `Authorization`, `x-amz-content-sha256`, `x-amz-date`.
#[allow(clippy::too_many_arguments)]
pub fn sign_get(host: &str, uri: &str, query: &[(&str, &str)], date: &str, region: &str, access: &str, secret: &str) -> [(String, String); 3] {
    let empty = sha_hex(b"");
    let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (uri_encode(k, true), uri_encode(v, true))).collect();
    q.sort();
    let cq = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let canonical = format!(
        "GET\n{uri}\n{cq}\nhost:{host}\nx-amz-content-sha256:{empty}\nx-amz-date:{date}\n\nhost;x-amz-content-sha256;x-amz-date\n{empty}"
    );
    let day = &date[..8];
    let scope = format!("{day}/{region}/s3/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{}", sha_hex(canonical.as_bytes()));
    let mut k = hmac256(format!("AWS4{secret}").as_bytes(), day);
    for part in [region, "s3", "aws4_request"] {
        k = hmac256(&k, part);
    }
    let sig = hex::encode(hmac256(&k, &to_sign));
    [
        ("authorization".into(), format!("AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={sig}")),
        ("x-amz-content-sha256".into(), empty),
        ("x-amz-date".into(), date.to_string()),
    ]
}

fn xml_tag<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let i = body.find(&open)? + open.len();
    let j = body[i..].find(&format!("</{tag}>"))?;
    Some(&body[i..i + j])
}

/// Lists one key under the prefix: whether the bucket answers to these
/// credentials. Says how many keys it saw (0 or 1) or why not.
pub async fn test_s3(s: &Storage) -> Result<String, String> {
    let Target::S3 { endpoint, region, bucket, prefix, access_key, virtual_host } = &s.target else {
        return Err("not an S3 storage".into());
    };
    let (tls, host, port) = parse_endpoint(endpoint)?;
    let host = if *virtual_host { format!("{bucket}.{host}") } else { host };
    let authority = match port {
        Some(p) if p != if tls { 443 } else { 80 } => format!("{host}:{p}"),
        _ => host.clone(),
    };
    let uri = if *virtual_host { "/".to_string() } else { format!("/{}", uri_encode(bucket, true)) };
    let mut query = vec![("list-type", "2"), ("max-keys", "1")];
    if !prefix.is_empty() {
        query.push(("prefix", prefix.as_str()));
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let headers = sign_get(&authority, &uri, &query, &amz_date(now), region, access_key, &s.secret.secret_key);
    let qs = query.iter().map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true))).collect::<Vec<_>>().join("&");
    let url = format!("{}://{authority}{uri}?{qs}", if tls { "https" } else { "http" });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(format!("osada/{}", crate::VERSION))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(&url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.send().await.map_err(|e| {
        let mut m = e.to_string();
        let mut src = std::error::Error::source(&e);
        while let Some(c) = src {
            m.push_str(": ");
            m.push_str(&c.to_string());
            src = c.source();
        }
        m
    })?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let code = xml_tag(&body, "Code").unwrap_or("");
        let msg = xml_tag(&body, "Message").unwrap_or("");
        return Err(format!("the bucket answered {}: {code} {msg}", status.as_u16()).trim().to_string());
    }
    let n = xml_tag(&body, "KeyCount").unwrap_or("?");
    Ok(format!("the bucket answers; {n} key(s) seen under the prefix (at most 1 asked)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sftp() -> Storage {
        Storage {
            target: Target::Sftp { host: "nas.lan".into(), port: 22, username: "osada".into(), auth: "password".into(), root: "/tank/x".into(), host_key: Some("SHA256:abc".into()) },
            secret: Secret { password: "pw".into(), ..Default::default() },
            note: String::new(),
        }
    }

    fn s3() -> Storage {
        Storage {
            target: Target::S3 {
                endpoint: "https://s3.example.com".into(),
                region: "eu-1".into(),
                bucket: "my-bucket".into(),
                prefix: "osada/".into(),
                access_key: "AK".into(),
                virtual_host: false,
            },
            secret: Secret { secret_key: "SK".into(), ..Default::default() },
            note: "off-site".into(),
        }
    }

    // AWS's own example (GET Bucket, list objects; signature version 4 docs)
    #[test]
    fn signature_v4_example() {
        let h = sign_get(
            "examplebucket.s3.amazonaws.com",
            "/",
            &[("max-keys", "2"), ("prefix", "J")],
            "20130524T000000Z",
            "us-east-1",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        );
        assert!(h[0].1.ends_with("Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"), "{}", h[0].1);
        assert!(h[0].1.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, "));
    }

    #[test]
    fn amz_dates() {
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
    }

    #[test]
    fn checks() {
        assert!(sftp().check("nas").is_ok());
        assert!(s3().check("offsite").is_ok());
        assert!(sftp().check("Nas").is_err());
        let mut x = sftp();
        x.secret.password.clear();
        assert!(x.check("nas").is_err());
        let mut x = s3();
        if let Target::S3 { endpoint, .. } = &mut x.target {
            *endpoint = "http://s3.example.com".into();
        }
        assert!(x.check("o").is_err());
        if let Target::S3 { endpoint, .. } = &mut x.target {
            *endpoint = "http://127.0.0.1:9000".into();
        }
        assert!(x.check("o").is_ok());
        if let Target::S3 { bucket, .. } = &mut x.target {
            *bucket = "Bad_Bucket".into();
        }
        assert!(x.check("o").is_err());
        assert_eq!(parse_endpoint("https://[::1]:9000/").unwrap(), (true, "[::1]".into(), Some(9000)));
        assert!(parse_endpoint("https://a.b/x").is_err());
    }

    #[test]
    fn secrets_kept_and_cleared() {
        let old = sftp();
        let mut page = old.clone();
        page.secret = Secret::default();
        let m = merged(page.clone(), Some(&old), false, false);
        assert_eq!(m.secret.password, "pw");
        assert_eq!(m, old);
        let m = merged(page.clone(), Some(&old), true, false);
        assert!(m.secret.password.is_empty());
        let m = merged(page.clone(), Some(&old), false, true);
        assert!(matches!(m.target, Target::Sftp { host_key: None, .. }));
        if let Target::Sftp { host, .. } = &mut page.target {
            *host = "other.lan".into();
        }
        let m = merged(page, Some(&old), false, false);
        assert!(matches!(m.target, Target::Sftp { host_key: None, .. }));
        // another kind keeps nothing
        let m = merged(s3(), Some(&old), false, false);
        assert!(m.secret.password.is_empty());
    }

    #[test]
    fn json_round_trip_and_shown() {
        let all: BTreeMap<String, Storage> = [("nas".to_string(), sftp()), ("offsite".to_string(), s3())].into();
        let j = serde_json::to_string(&all).unwrap();
        let back: BTreeMap<String, Storage> = serde_json::from_str(&j).unwrap();
        assert_eq!(back, all);
        let shown = sftp().shown();
        assert!(!shown.to_string().contains("\"pw\""));
        assert_eq!(shown["secret_set"], serde_json::json!(["password"]));
        assert_eq!(shown["kind"], "sftp");
        assert_eq!(s3().place(), "s3://my-bucket/osada/ at https://s3.example.com");
    }
}
