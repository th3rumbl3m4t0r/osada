//! Certificates (D-U26): one issuer per certificate, copies over the cluster,
//! a certificate per name at every edge.
//!
//! - Node control's `certs` (and one for each site whose front is `tls`):
//!   host names, the issuer node, the nodes that get a copy.
//! - The issuer runs ACME (http-01; Let's Encrypt, or `OSADA_ACME_DIRECTORY`
//!   with `OSADA_ACME_ROOT` for a test server) and keeps
//!   `<data>/certs/<cert>/{fullchain,privkey}.pem` and `meta.json`; it renews
//!   60 days after issuing (Let's Encrypt's last 90), or when the names change.
//!   The token is a file of `<data>/acme/.well-known/acme-challenge/`, which
//!   the edge's plain http serves; other edges ask the issuer for it.
//! - A node with a copy asks the issuer for it when its serial differs.
//! - `Store`: every certificate of `<data>/certs/*/` and `<data>/edge-tls/`,
//!   picked by the name a client asks for (SNI), read again when files change.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rustls::{
    crypto::ring::sign::any_supported_type,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::placement::{NodeId, Placement};

/// A certificate in node control (D-U26).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertSpec {
    /// The host names it is for (the first is its subject).
    pub names: Vec<String>,
    /// The node that runs ACME for it.
    pub issuer: NodeId,
    /// The nodes that get a copy (the issuer has it anyway).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub copies: Vec<NodeId>,
}

/// What is wrong with a certificate of node control, if anything.
pub fn check(name: &str, c: &CertSpec, p: &Placement) -> Option<String> {
    if !crate::placement::valid_module_name(name) {
        return Some(format!("{name:?}: a certificate's name is a-z, 0-9 and -"));
    }
    if c.names.is_empty() {
        return Some(format!("{name}: no host names"));
    }
    if let Some(n) = c.names.iter().find(|n| !crate::placement::valid_host(n)) {
        return Some(format!("{name}: {n:?} is not a host name"));
    }
    if let Some(n) = std::iter::once(&c.issuer).chain(&c.copies).find(|n| !p.nodes.contains_key(*n)) {
        return Some(format!("{name}: no node {n}"));
    }
    None
}

/// Every certificate: node control's, and one per site whose front is tls
/// (`site-<name>`: issuer the first node it is exposed on, copies the rest).
pub fn wanted(p: &Placement) -> BTreeMap<String, CertSpec> {
    let mut out = p.certs.clone();
    for (name, _, s) in crate::sites::sites(p) {
        if s.front != crate::sites::Front::Tls || s.exposed_on.is_empty() {
            continue;
        }
        // a site's names that node control's certificates cover already need none of their own
        if s.names.iter().all(|n| p.certs.values().any(|c| c.names.contains(n))) {
            continue;
        }
        out.insert(format!("site-{name}"), CertSpec { names: s.names.clone(), issuer: s.exposed_on[0].clone(), copies: s.exposed_on[1..].to_vec() });
    }
    out
}

/// What the issuer wrote about a certificate.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub names: Vec<String>,
    /// Seconds since 1970.
    pub issued: u64,
    /// The sha256 of fullchain.pem (copies compare it).
    pub serial: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub fn dir(data: &Path, cert: &str) -> PathBuf {
    data.join("certs").join(cert)
}

pub fn meta(data: &Path, cert: &str) -> Option<Meta> {
    serde_json::from_slice(&std::fs::read(dir(data, cert).join("meta.json")).ok()?).ok()
}

/// Writes a certificate atomically (key 0600), with its meta.
pub fn store(data: &Path, cert: &str, fullchain: &str, privkey: &str, m: &Meta) -> std::io::Result<()> {
    store_in(&dir(data, cert), fullchain, privkey, m)
}

/// The same, into the folder `d` (home's https folder).
pub fn store_in(d: &Path, fullchain: &str, privkey: &str, m: &Meta) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(d)?;
    let write = |name: &str, body: &[u8], mode: u32| -> std::io::Result<()> {
        let tmp = d.join(format!(".{name}.tmp"));
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
        std::io::Write::write_all(&mut f, body)?;
        std::fs::rename(&tmp, d.join(name))
    };
    write("privkey.pem", privkey.as_bytes(), 0o600)?;
    write("fullchain.pem", fullchain.as_bytes(), 0o644)?;
    write("meta.json", &serde_json::to_vec_pretty(m).unwrap_or_default(), 0o644)
}

pub fn serial(fullchain: &str) -> String {
    hex::encode(&Sha256::digest(fullchain.as_bytes())[..16])
}

/// Whether the issuer should (re)issue: no certificate, other names, or 60 days old.
pub fn due(data: &Path, cert: &str, spec: &CertSpec) -> bool {
    match meta(data, cert) {
        Some(m) => m.names != spec.names || now().saturating_sub(m.issued) > 60 * 86_400 || !dir(data, cert).join("privkey.pem").exists(),
        None => true,
    }
}

/// The key authorization for an http-01 token this node published, if any.
pub fn token(data: &Path, token: &str) -> Option<String> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    std::fs::read_to_string(data.join("acme/.well-known/acme-challenge").join(token)).ok()
}

/// Gets a certificate for `spec.names` by ACME as `cert`, and stores it.
pub async fn issue(data: &Path, cert: &str, spec: &CertSpec) -> Result<Meta, String> {
    use instant_acme::{Account, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder, OrderStatus, RetryPolicy};
    let directory = std::env::var("OSADA_ACME_DIRECTORY").unwrap_or_else(|_| LetsEncrypt::Production.url().to_string());
    let builder = match std::env::var("OSADA_ACME_ROOT") {
        Ok(root) => Account::builder_with_root(root),
        Err(_) => Account::builder(),
    }
    .map_err(|e| format!("acme: {e}"))?;
    // the account, kept per directory
    let acct_file = data.join(format!("acme/account-{}.json", &hex::encode(Sha256::digest(directory.as_bytes()))[..12]));
    let account = match std::fs::read(&acct_file).ok().and_then(|b| serde_json::from_slice::<instant_acme::AccountCredentials>(&b).ok()) {
        Some(c) => builder.from_credentials(c).await.map_err(|e| format!("acme account: {e}"))?,
        None => {
            let (a, creds) = builder
                .create(&NewAccount { contact: &[], terms_of_service_agreed: true, only_return_existing: false }, directory.clone(), None)
                .await
                .map_err(|e| format!("acme account: {e}"))?;
            std::fs::create_dir_all(data.join("acme")).map_err(|e| e.to_string())?;
            store_private(&acct_file, &serde_json::to_vec(&creds).unwrap_or_default()).map_err(|e| e.to_string())?;
            a
        }
    };
    let ids: Vec<Identifier> = spec.names.iter().map(|n| Identifier::Dns(n.clone())).collect();
    let mut order = account.new_order(&NewOrder::new(&ids)).await.map_err(|e| format!("acme order: {e}"))?;
    let chal_dir = data.join("acme/.well-known/acme-challenge");
    std::fs::create_dir_all(&chal_dir).map_err(|e| e.to_string())?;
    let mut tokens = Vec::new();
    {
        let mut auths = order.authorizations();
        while let Some(a) = auths.next().await {
            let mut a = a.map_err(|e| format!("acme authorization: {e}"))?;
            match a.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                other => return Err(format!("acme authorization: {other:?}")),
            }
            let mut ch = a.challenge(ChallengeType::Http01).ok_or("acme: no http-01 challenge")?;
            let token = ch.token.clone();
            std::fs::write(chal_dir.join(&token), ch.key_authorization().as_str()).map_err(|e| e.to_string())?;
            tokens.push(token);
            ch.set_ready().await.map_err(|e| format!("acme challenge: {e}"))?;
        }
    }
    let res = async {
        let status = order.poll_ready(&RetryPolicy::default()).await.map_err(|e| format!("acme: {e}"))?;
        if status != OrderStatus::Ready {
            // why: the challenges' problems, as the CA says them
            let mut why = Vec::new();
            let mut auths = order.authorizations();
            while let Some(Ok(a)) = auths.next().await {
                for c in a.challenges.iter().filter_map(|c| c.error.as_ref()) {
                    why.push(c.detail.clone().or_else(|| c.r#type.clone()).unwrap_or_default());
                }
            }
            return Err(format!("acme: the order is {status:?}{}", if why.is_empty() { String::new() } else { format!(": {}", why.join("; ")) }));
        }
        let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
        let mut params = rcgen::CertificateParams::new(spec.names.clone()).map_err(|e| e.to_string())?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        let csr = params.serialize_request(&key).map_err(|e| e.to_string())?;
        order.finalize_csr(csr.der()).await.map_err(|e| format!("acme finalize: {e}"))?;
        let chain = order.poll_certificate(&RetryPolicy::default()).await.map_err(|e| format!("acme certificate: {e}"))?;
        let m = Meta { names: spec.names.clone(), issued: now(), serial: serial(&chain) };
        store(data, cert, &chain, &key.serialize_pem(), &m).map_err(|e| e.to_string())?;
        Ok(m)
    }
    .await;
    for t in tokens {
        let _ = std::fs::remove_file(chal_dir.join(t));
    }
    res
}

fn store_private(file: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = file.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    std::io::Write::write_all(&mut f, body)?;
    std::fs::rename(&tmp, file)
}

/// A certificate as the issuer hands it to a copy.
pub fn bundle(data: &Path, cert: &str) -> Option<serde_json::Value> {
    let d = dir(data, cert);
    let m = meta(data, cert)?;
    Some(json!({ "meta": m, "fullchain": std::fs::read_to_string(d.join("fullchain.pem")).ok()?, "privkey": std::fs::read_to_string(d.join("privkey.pem")).ok()? }))
}

// ---- the certificates an edge answers with ---------------------------------

struct One {
    der: Vec<u8>,
    key: Arc<CertifiedKey>,
}

/// Every certificate of `<data>/certs/*/` and `<data>/edge-tls/`, by name.
pub struct Store {
    data: PathBuf,
    now: Mutex<(Instant, u64, Vec<One>)>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("data", &self.data).finish()
    }
}

fn dirs(data: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(data.join("certs")).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    v.sort();
    v.push(data.join("edge-tls"));
    v
}

/// A fingerprint of the files' times, to see a change.
fn stamp(data: &Path) -> u64 {
    let mut h = Sha256::new();
    for d in dirs(data) {
        for f in ["fullchain.pem", "privkey.pem"] {
            if let Ok(t) = std::fs::metadata(d.join(f)).and_then(|m| m.modified()) {
                h.update(d.to_string_lossy().as_bytes());
                h.update(t.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos().to_le_bytes());
            }
        }
    }
    u64::from_le_bytes(h.finalize()[..8].try_into().unwrap_or_default())
}

fn load(data: &Path) -> Vec<One> {
    dirs(data)
        .into_iter()
        .filter_map(|d| {
            let certs = CertificateDer::pem_file_iter(d.join("fullchain.pem")).ok()?.collect::<Result<Vec<_>, _>>().ok()?;
            let key = any_supported_type(&PrivateKeyDer::from_pem_file(d.join("privkey.pem")).ok()?).ok()?;
            let der = certs.first()?.to_vec();
            let ck = CertifiedKey::new(certs, key);
            if ck.keys_match().is_err() {
                tracing::warn!("certificates: {}: the key does not go with the certificate", d.display());
                return None;
            }
            Some(One { der, key: Arc::new(ck) })
        })
        .collect()
}

impl Store {
    pub fn new(data: &Path) -> Arc<Store> {
        Arc::new(Store { data: data.to_path_buf(), now: Mutex::new((Instant::now(), stamp(data), load(data))) })
    }

    fn with<T>(&self, f: impl FnOnce(&[One]) -> T) -> T {
        let mut s = self.now.lock().unwrap_or_else(|e| e.into_inner());
        if s.0.elapsed() > Duration::from_secs(20) {
            s.0 = Instant::now();
            let st = stamp(&self.data);
            if st != s.1 {
                s.1 = st;
                s.2 = load(&self.data);
                tracing::info!("certificates: read again ({})", s.2.len());
            }
        }
        f(&s.2)
    }

    fn valid_for(der: &[u8], name: &str) -> bool {
        let Ok(sn) = ServerName::try_from(name.to_string()) else { return false };
        let c = CertificateDer::from(der);
        webpki::EndEntityCert::try_from(&c).is_ok_and(|ee| ee.verify_is_valid_for_subject_name(&sn).is_ok())
    }

    /// Whether some certificate here is for `name`.
    pub fn covers(&self, name: &str) -> bool {
        self.with(|all| all.iter().any(|o| Store::valid_for(&o.der, name)))
    }

    /// TLS with these certificates (http/1.1, as osada 0.1's).
    pub fn config(self: &Arc<Self>) -> Arc<rustls::ServerConfig> {
        let mut c = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("tls versions")
            .with_no_client_auth()
            .with_cert_resolver(self.clone());
        c.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(c)
    }
}

impl ResolvesServerCert for Store {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = hello.server_name().map(str::to_ascii_lowercase);
        self.with(|all| {
            name.as_deref().and_then(|n| all.iter().find(|o| Store::valid_for(&o.der, n))).or_else(|| all.last()).map(|o| o.key.clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selfsigned(names: &[&str]) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let c = rcgen::CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap().self_signed(&key).unwrap();
        (c.pem(), key.serialize_pem())
    }

    #[test]
    fn a_certificate_per_name() {
        let d = std::env::temp_dir().join(format!("osada-acme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (c1, k1) = selfsigned(&["osada.example.org", "apptab.example.org"]);
        let (c2, k2) = selfsigned(&["site.example.net"]);
        store(&d, "osada", &c1, &k1, &Meta { names: vec!["osada.example.org".into()], issued: now(), serial: serial(&c1) }).unwrap();
        store(&d, "site-x", &c2, &k2, &Meta { names: vec!["site.example.net".into()], issued: now() - 61 * 86_400, serial: serial(&c2) }).unwrap();
        let s = Store::new(&d);
        assert!(s.covers("apptab.example.org") && s.covers("site.example.net") && !s.covers("other.example.com"));
        assert_eq!(s.with(|a| a.len()), 2);
        let spec = |n: &[&str]| CertSpec { names: n.iter().map(|x| x.to_string()).collect(), issuer: NodeId("a".into()), copies: vec![] };
        assert!(!due(&d, "osada", &spec(&["osada.example.org"])));
        assert!(due(&d, "osada", &spec(&["osada.example.org", "new.example.org"])), "other names");
        assert!(due(&d, "site-x", &spec(&["site.example.net"])), "60 days old");
        assert!(due(&d, "none", &spec(&["x.example.org"])));
        assert_eq!(bundle(&d, "osada").unwrap()["meta"]["serial"], serial(&c1));
        // the key's mode
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir(&d, "osada").join("privkey.pem")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(token(&d, "../x"), None);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
