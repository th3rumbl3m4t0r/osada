//! HTTPS on the LAN (`OSADA_TLS_BIND`, e.g. `0.0.0.0:443`) with the public
//! name's certificate: `<data>/tls/fullchain.pem` and `privkey.pem`, copied
//! here from the edge host, which renews them (deploy/osada-cert-pull.sh).
//! The home network's DNS points the public name at this server, so at home
//! the public address works without the edge, over https: the service worker,
//! secure cookies. A renewed certificate is picked up without a restart: the
//! files are looked at again at most once a minute, on a handshake.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use axum::{extract::Request, middleware::Next, response::Response};
use tokio_rustls::rustls::{
    self,
    crypto::ring::{default_provider, sign::any_supported_type},
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};

/// Set on requests that came in over the HTTPS listener.
#[derive(Clone, Copy, Debug)]
pub struct Https;

pub async fn mark(mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(Https);
    next.run(req).await
}

const LOOK_AGAIN: Duration = Duration::from_secs(60);

struct Loaded {
    key: Arc<CertifiedKey>,
    /// the two files' times when read
    stamp: (SystemTime, SystemTime),
    looked: Instant,
}

/// The certificate in `dir`, read again when its files change.
#[derive(Debug)]
struct Files {
    dir: PathBuf,
    now: Mutex<Option<Loaded>>,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded").field("stamp", &self.stamp).finish()
    }
}

fn stamp(dir: &Path) -> std::io::Result<(SystemTime, SystemTime)> {
    Ok((fs::metadata(dir.join("fullchain.pem"))?.modified()?, fs::metadata(dir.join("privkey.pem"))?.modified()?))
}

fn read(dir: &Path) -> anyhow::Result<Loaded> {
    let stamp = stamp(dir)?;
    let certs = CertificateDer::pem_file_iter(dir.join("fullchain.pem"))?.collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(!certs.is_empty(), "{}: no certificate", dir.join("fullchain.pem").display());
    let key = CertifiedKey::new(certs, any_supported_type(&PrivateKeyDer::from_pem_file(dir.join("privkey.pem"))?)?);
    key.keys_match().map_err(|e| anyhow::anyhow!("the key does not go with the certificate: {e}"))?;
    Ok(Loaded { key: Arc::new(key), stamp, looked: Instant::now() })
}

impl ResolvesServerCert for Files {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let mut now = self.now.lock().unwrap_or_else(|e| e.into_inner());
        let cur = now.as_mut()?;
        if cur.looked.elapsed() >= LOOK_AGAIN {
            cur.looked = Instant::now();
            if stamp(&self.dir).is_ok_and(|s| s != cur.stamp) {
                // half copied, or a key that does not fit: the one in use stays
                match read(&self.dir) {
                    Ok(new) => {
                        tracing::info!("https: the certificate in {} changed, now in use", self.dir.display());
                        *cur = new;
                    }
                    Err(e) => tracing::warn!("https: {}: {e}; the previous certificate stays", self.dir.display()),
                }
            }
        }
        Some(cur.key.clone())
    }
}

/// TLS for the HTTPS listener: the certificate in `dir` (there has to be one to start).
pub fn config(dir: &Path) -> anyhow::Result<Arc<rustls::ServerConfig>> {
    let first = read(dir).map_err(|e| anyhow::anyhow!("https: {}: {e}", dir.display()))?;
    let files = Files { dir: dir.to_path_buf(), now: Mutex::new(Some(first)) };
    let mut c = rustls::ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(files));
    // http/1.1 only: the server is built without http/2
    c.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(c))
}
