//! The cluster's certificates (§6.2). `init` makes a certificate authority;
//! every node has a certificate from it that names the node (its DNS SAN is
//! the node name) and serves for both ends of a node link (link.rs), since
//! either side may dial. The CA's key stays on the node that made it (later
//! in its keyholder, §4.2) and signs the certificates of nodes that join.
//!
//! Files in `<data>/tls/`: `ca.crt`, `node.crt`, `node.key` (0600),
//! `node.name`, and on the CA's node `ca.key` (0600).

use std::{
    fmt,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use sha2::{Digest, Sha256};

const CA_NAME: &str = "osada cluster";
const CA_YEARS: i32 = 30;
/// Node certificates are renewed over the link before they run out (later).
const NODE_YEARS: i32 = 2;

#[derive(Debug)]
pub struct PkiError(pub String);

impl fmt::Display for PkiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "certificates: {}", self.0)
    }
}

impl std::error::Error for PkiError {}

fn err(e: impl fmt::Display) -> PkiError {
    PkiError(e.to_string())
}

/// The cluster's certificate authority, with its key.
pub struct Ca {
    pub cert_pem: String,
    key: KeyPair,
}

impl Ca {
    pub fn new() -> Result<Ca, PkiError> {
        let key = KeyPair::generate().map_err(err)?;
        let cert = ca_params()?.self_signed(&key).map_err(err)?;
        Ok(Ca { cert_pem: cert.pem(), key })
    }

    pub fn load(dir: &Path) -> Result<Ca, PkiError> {
        let tls = dir.join("tls");
        let cert_pem = fs::read_to_string(tls.join("ca.crt")).map_err(err)?;
        let key = KeyPair::from_pem(&fs::read_to_string(tls.join("ca.key")).map_err(err)?).map_err(err)?;
        Ok(Ca { cert_pem, key })
    }

    /// Writes `ca.crt` and the key, `ca.key` (0600); never over existing files.
    pub fn save(&self, dir: &Path) -> Result<(), PkiError> {
        let tls = dir.join("tls");
        fs::create_dir_all(&tls).map_err(err)?;
        write_new(&tls.join("ca.crt"), &self.cert_pem, 0o644)?;
        write_new(&tls.join("ca.key"), &self.key.serialize_pem(), 0o600)
    }

    /// The sha256 of the CA certificate, which a joining node checks (§6.2).
    pub fn fingerprint(&self) -> Result<String, PkiError> {
        fingerprint(&self.cert_pem)
    }

    /// A certificate for `node`, with a new key.
    pub fn issue(&self, node: &str) -> Result<NodeCert, PkiError> {
        let key = KeyPair::generate().map_err(err)?;
        Ok(NodeCert {
            node: node.to_string(),
            cert_pem: self.issue_for(node, &key)?,
            key_pem: key.serialize_pem(),
            ca_pem: self.cert_pem.clone(),
        })
    }

    /// A certificate for `node`'s public key, which it keeps the private
    /// half of (a joining node, §6.2). PEM.
    pub fn issue_for(&self, node: &str, public_key: &impl rcgen::PublicKeyData) -> Result<String, PkiError> {
        if !valid_node_name(node) {
            return Err(PkiError(format!("{node:?} is not a node name")));
        }
        // rcgen signs with an issuer certificate made from the same name and
        // key; the subject name and key identifier it takes from it are those
        // of the stored certificate.
        let issuer = ca_params()?.self_signed(&self.key).map_err(err)?;
        let mut p = CertificateParams::new(vec![node.to_string()]).map_err(err)?;
        p.distinguished_name = DistinguishedName::new();
        p.distinguished_name.push(DnType::CommonName, node);
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth, ExtendedKeyUsagePurpose::ClientAuth];
        set_validity(&mut p, NODE_YEARS);
        Ok(p.signed_by(public_key, &issuer, &self.key).map_err(err)?.pem())
    }
}

/// A node's public key as it travels in a join: P-256, the uncompressed
/// point, as rcgen's `KeyPair::public_key_raw` gives it.
pub struct P256PublicKey(Vec<u8>);

impl P256PublicKey {
    pub fn from_hex(s: &str) -> Result<P256PublicKey, PkiError> {
        let v = hex::decode(s).map_err(err)?;
        if v.len() != 65 || v[0] != 4 {
            return Err(PkiError("not a P-256 public key".into()));
        }
        Ok(P256PublicKey(v))
    }
}

impl rcgen::PublicKeyData for P256PublicKey {
    fn der_bytes(&self) -> &[u8] {
        &self.0
    }

    fn algorithm(&self) -> &rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

/// Letters, digits, '-' and '.': a DNS name, which the certificate carries.
pub fn valid_node_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !s.starts_with(['-', '.'])
        && !s.ends_with(['-', '.'])
}

pub fn fingerprint(cert_pem: &str) -> Result<String, PkiError> {
    let der = CertificateDer::from_pem_slice(cert_pem.as_bytes()).map_err(err)?;
    Ok(hex::encode(Sha256::digest(der.as_ref())))
}

fn ca_params() -> Result<CertificateParams, PkiError> {
    let mut p = CertificateParams::new(Vec::<String>::new()).map_err(err)?;
    p.distinguished_name = DistinguishedName::new();
    p.distinguished_name.push(DnType::CommonName, CA_NAME);
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    set_validity(&mut p, CA_YEARS);
    Ok(p)
}

/// Valid from yesterday (clocks differ a little) for `years`.
fn set_validity(p: &mut CertificateParams, years: i32) {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    let (y, m, d) = civil(days - 1);
    p.not_before = rcgen::date_time_ymd(y, m, d);
    p.not_after = rcgen::date_time_ymd(y + years, m, d.min(28));
}

/// The calendar date `days` after 1970-01-01 (H. Hinnant's civil_from_days).
fn civil(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y as i32, m as u8, d as u8)
}

fn write_new(path: &Path, text: &str, mode: u32) -> Result<(), PkiError> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|e| PkiError(format!("{}: {e}", path.display())))?;
    f.write_all(text.as_bytes()).map_err(err)?;
    f.sync_all().map_err(err)
}

/// A node's certificate and key, and the CA's certificate, as PEM.
pub struct NodeCert {
    pub node: String,
    pub cert_pem: String,
    pub key_pem: String,
    pub ca_pem: String,
}

impl NodeCert {
    pub fn save(&self, dir: &Path) -> Result<(), PkiError> {
        let tls = dir.join("tls");
        fs::create_dir_all(&tls).map_err(err)?;
        if !tls.join("ca.crt").exists() {
            write_new(&tls.join("ca.crt"), &self.ca_pem, 0o644)?;
        }
        write_new(&tls.join("node.crt"), &self.cert_pem, 0o644)?;
        write_new(&tls.join("node.key"), &self.key_pem, 0o600)?;
        write_new(&tls.join("node.name"), &format!("{}\n", self.node), 0o644)
    }

    pub fn load(dir: &Path) -> Result<NodeCert, PkiError> {
        let tls = dir.join("tls");
        let read = |f: &str| fs::read_to_string(tls.join(f)).map_err(|e| PkiError(format!("{f}: {e}")));
        Ok(NodeCert { node: read("node.name")?.trim().to_string(), cert_pem: read("node.crt")?, key_pem: read("node.key")?, ca_pem: read("ca.crt")? })
    }

    /// The same in the form rustls takes.
    pub fn tls(&self) -> Result<NodeTls, PkiError> {
        Ok(NodeTls {
            node: self.node.clone(),
            cert: CertificateDer::from_pem_slice(self.cert_pem.as_bytes()).map_err(err)?,
            key: PrivateKeyDer::from_pem_slice(self.key_pem.as_bytes()).map_err(err)?,
            ca: CertificateDer::from_pem_slice(self.ca_pem.as_bytes()).map_err(err)?,
        })
    }
}

pub struct NodeTls {
    pub node: String,
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
    pub ca: CertificateDer<'static>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_407), (2025, 11, 15));
        assert_eq!(civil(11_016), (2000, 2, 29));
        assert_eq!(civil(-1), (1969, 12, 31));
    }

    #[test]
    fn node_names() {
        for good in ["home", "edge", "mail-vm", "a.b"] {
            assert!(valid_node_name(good), "{good}");
        }
        for bad in ["", "-a", "a.", "a b", "home/x", "č"] {
            assert!(!valid_node_name(bad), "{bad}");
        }
    }

    #[test]
    fn issue_save_load() {
        let dir = TempDir::new("pki");
        let ca = Ca::new().unwrap();
        ca.save(dir.path()).unwrap();
        assert!(ca.save(dir.path()).is_err(), "never over an existing CA");
        let cert = ca.issue("home").unwrap();
        cert.save(dir.path()).unwrap();
        let back = NodeCert::load(dir.path()).unwrap();
        assert_eq!((back.node.as_str(), &back.cert_pem), ("home", &cert.cert_pem));
        back.tls().unwrap();
        let again = Ca::load(dir.path()).unwrap();
        assert_eq!(again.fingerprint().unwrap(), ca.fingerprint().unwrap());
        assert_eq!(ca.fingerprint().unwrap().len(), 64);
        for (f, mode) in [("ca.key", 0o600), ("node.key", 0o600), ("ca.crt", 0o644), ("node.crt", 0o644)] {
            let m = fs::metadata(dir.path().join("tls").join(f)).unwrap().permissions().mode() & 0o777;
            assert_eq!(m, mode, "{f}");
        }
        assert!(ca.issue("not a name").is_err());
    }
}
