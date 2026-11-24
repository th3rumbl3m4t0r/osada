//! An `idp` node's own signing key (§10.2, A15, D-U45): RSA 2048, RS256 as osada 0.1's OIDC signs,
//! made the first time in `<node folder>/idp.key` (PKCS#8 DER, 0600) and never leaving the node.
//! Its public half is in the cluster log (`Command::Key`), so every node publishes all of them
//! together (JWKS) and a token from any idp node is good everywhere; an administrator withdraws
//! one node's key on its own, and the node then makes a new one.

use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A node's public key as the cluster log keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKey {
    /// The RFC 7638 thumbprint of the key (what tokens name in `kid`).
    pub kid: String,
    /// RSA modulus and exponent, base64url (as in a JWK).
    pub n: String,
    pub e: String,
    /// When the node made it (ms since 1970).
    #[serde(default)]
    pub since: u64,
}

/// The RFC 7638 thumbprint: the required members, in order, no spaces.
pub fn thumbprint(n: &str, e: &str) -> String {
    B64URL.encode(Sha256::digest(format!(r#"{{"e":"{e}","kty":"RSA","n":"{n}"}}"#)))
}

impl PublicKey {
    /// Whether it is a key and its kid is its thumbprint.
    pub fn check(&self) -> Result<(), String> {
        let n = B64URL.decode(&self.n).map_err(|_| "a key's n is not base64url")?;
        if n.len() < 256 || B64URL.decode(&self.e).is_err() {
            return Err("a signing key is RSA of 2048 bits or more".into());
        }
        if self.kid != thumbprint(&self.n, &self.e) {
            return Err("a key's kid is its thumbprint (RFC 7638)".into());
        }
        Ok(())
    }

    /// As a JWK, for a JWKS.
    pub fn jwk(&self) -> Value {
        json!({ "kty": "RSA", "use": "sig", "alg": "RS256", "kid": self.kid, "n": self.n, "e": self.e })
    }
}

pub struct Key {
    pair: ring::signature::RsaKeyPair,
    pub public: PublicKey,
    file: PathBuf,
}

impl Key {
    fn from_der(der: &[u8], file: PathBuf, since: u64) -> Result<Key, String> {
        use rsa::{pkcs8::DecodePrivateKey, traits::PublicKeyParts};
        let k = rsa::RsaPrivateKey::from_pkcs8_der(der).map_err(|e| format!("{}: {e}", file.display()))?;
        let (n, e) = (B64URL.encode(k.n().to_bytes_be()), B64URL.encode(k.e().to_bytes_be()));
        let pair = ring::signature::RsaKeyPair::from_pkcs8(der).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(Key { pair, public: PublicKey { kid: thumbprint(&n, &e), n, e, since }, file })
    }

    /// The node's key in `dir` (`idp.key`), made the first time.
    pub fn load_or_make(dir: &Path) -> Result<Key, String> {
        let file = dir.join("idp.key");
        if !file.exists() {
            Self::make(&file)?;
        }
        let der = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let since = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        Self::from_der(&der, file, since)
    }

    fn make(file: &Path) -> Result<(), String> {
        use rsa::pkcs8::EncodePrivateKey;
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let der = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).and_then(|k| k.to_pkcs8_der().map_err(Into::into)).map_err(|e| e.to_string())?;
        let tmp = file.with_extension("key.new");
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        f.write_all(der.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
    }

    /// After its key was withdrawn: the old one kept aside (`idp.key.withdrawn-<kid>`), a new one.
    pub fn renew(self) -> Result<Key, String> {
        let aside = self.file.with_file_name(format!("idp.key.withdrawn-{}", &self.public.kid[..12.min(self.public.kid.len())]));
        std::fs::rename(&self.file, &aside).map_err(|e| format!("{}: {e}", aside.display()))?;
        let dir = self.file.parent().unwrap_or(Path::new(".")).to_path_buf();
        Key::load_or_make(&dir)
    }

    /// A JWT of `claims`, RS256, with `typ` and this key's kid.
    pub fn sign(&self, typ: &str, claims: &Value) -> Result<String, String> {
        let head = B64URL.encode(json!({ "alg": "RS256", "typ": typ, "kid": self.public.kid }).to_string());
        let input = format!("{head}.{}", B64URL.encode(claims.to_string()));
        let mut sig = vec![0; self.pair.public().modulus_len()];
        self.pair
            .sign(&ring::signature::RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), input.as_bytes(), &mut sig)
            .map_err(|_| "signing failed".to_string())?;
        Ok(format!("{input}.{}", B64URL.encode(sig)))
    }
}

/// Checks a JWT's RS256 signature with `key`; its claims.
pub fn verify(token: &str, key: &PublicKey) -> Result<Value, String> {
    let mut parts = token.split('.');
    let (Some(h), Some(c), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return Err("not a JWT".into()) };
    let head: Value = serde_json::from_slice(&B64URL.decode(h).map_err(|_| "not a JWT")?).map_err(|_| "not a JWT")?;
    if head["alg"] != "RS256" || head["kid"].as_str() != Some(&key.kid) {
        return Err("another key or algorithm".into());
    }
    let (n, e) = (B64URL.decode(&key.n).map_err(|_| "bad key")?, B64URL.decode(&key.e).map_err(|_| "bad key")?);
    let pk = ring::signature::RsaPublicKeyComponents { n: &n, e: &e };
    let sig = B64URL.decode(s).map_err(|_| "not a JWT")?;
    pk.verify(&ring::signature::RSA_PKCS1_2048_8192_SHA256, format!("{h}.{c}").as_bytes(), &sig).map_err(|_| "a bad signature".to_string())?;
    serde_json::from_slice(&B64URL.decode(c).map_err(|_| "not a JWT")?).map_err(|_| "not a JWT".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn a_key_signs_and_its_public_half_checks() {
        let dir = TempDir::new("idpkey");
        let k = Key::load_or_make(dir.path()).unwrap();
        k.public.check().unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.path().join("idp.key")).unwrap().permissions().mode() & 0o777, 0o600);
        // the same key the next time
        assert_eq!(Key::load_or_make(dir.path()).unwrap().public.kid, k.public.kid);
        let t = k.sign("JWT", &json!({ "sub": "anna" })).unwrap();
        assert_eq!(verify(&t, &k.public).unwrap()["sub"], "anna");
        let mut forged = t.clone();
        forged.replace_range(forged.len() - 6.., "AAAAAA");
        assert!(verify(&forged, &k.public).is_err());
        let mut wrong = k.public.clone();
        wrong.kid = "x".into();
        assert!(wrong.check().is_err());
        // withdrawn: kept aside, a new one
        let old = k.public.kid.clone();
        let k2 = k.renew().unwrap();
        assert_ne!(k2.public.kid, old);
        assert!(verify(&t, &k2.public).is_err());
        assert!(std::fs::read_dir(dir.path()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("idp.key.withdrawn-")));
    }
}
