//! FIDO2 / WebAuthn (A16: tier 0 signs in with it): what the browser and
//! the security key hand over, checked here. Attestation is not asked for
//! ("none"): a key is trusted because its owner registered it while signed
//! in (or at their first sign-in, as TOTP), not for its make. Signatures:
//! ES256 (P-256) and EdDSA (Ed25519), which every current key does.
//!
//! - `register`: the answer to `navigator.credentials.create`: the client
//!   data (type `webauthn.create`, our challenge, our origin) and the
//!   attestation object (CBOR: its `authData` with the new credential).
//! - `verify`: the answer to `navigator.credentials.get`: client data (type
//!   `webauthn.get`), authenticator data (the relying party's hash, user
//!   present) and the signature over both.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::signature;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A registered key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    /// The credential id (base64url), as the browser names it.
    pub id: String,
    /// -7 (ES256) or -8 (EdDSA).
    pub alg: i64,
    /// ES256: the point, 65 bytes (0x04, x, y); EdDSA: 32 bytes. base64url.
    pub key: String,
    /// What its owner calls it.
    #[serde(default)]
    pub name: String,
    /// When it was registered (ms since 1970).
    #[serde(default)]
    pub added: u64,
}

pub const ES256: i64 = -7;
pub const EDDSA: i64 = -8;

// ---- CBOR, as far as WebAuthn needs it ------------------------------------

#[derive(Debug, PartialEq)]
enum Cbor {
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Other,
}

impl Cbor {
    fn get(&self, key: &Cbor) -> Option<&Cbor> {
        match self {
            Cbor::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn bytes(&self) -> Option<&[u8]> {
        if let Cbor::Bytes(b) = self { Some(b) } else { None }
    }

    fn int(&self) -> Option<i64> {
        if let Cbor::Int(i) = self { Some(*i) } else { None }
    }
}

/// One item from `b` at `*at`, moving `*at` past it.
fn cbor(b: &[u8], at: &mut usize, depth: u32) -> Result<Cbor, String> {
    if depth > 16 {
        return Err("CBOR nested too deep".into());
    }
    let first = *b.get(*at).ok_or("CBOR ends early")?;
    *at += 1;
    let (major, info) = (first >> 5, first & 31);
    let mut arg = |info: u8| -> Result<u64, String> {
        let n = match info {
            0..=23 => return Ok(info as u64),
            24 => 1,
            25 => 2,
            26 => 4,
            27 => 8,
            _ => return Err("CBOR: lengths of unknown size are not taken".into()),
        };
        let s = b.get(*at..*at + n).ok_or("CBOR ends early")?;
        *at += n;
        Ok(s.iter().fold(0u64, |a, &x| (a << 8) | x as u64))
    };
    let n = arg(info)?;
    let take = |at: &mut usize, n: u64| -> Result<Vec<u8>, String> {
        let n = usize::try_from(n).map_err(|_| "CBOR: too long")?;
        let s = b.get(*at..at.checked_add(n).ok_or("CBOR: too long")?).ok_or("CBOR ends early")?.to_vec();
        *at += n;
        Ok(s)
    };
    Ok(match major {
        0 => Cbor::Int(i64::try_from(n).map_err(|_| "CBOR: integer too large")?),
        1 => Cbor::Int(-1 - i64::try_from(n).map_err(|_| "CBOR: integer too large")?),
        2 => Cbor::Bytes(take(at, n)?),
        3 => Cbor::Text(String::from_utf8(take(at, n)?).map_err(|_| "CBOR: text not UTF-8")?),
        4 => {
            let mut v = Vec::new();
            for _ in 0..n.min(1024) {
                v.push(cbor(b, at, depth + 1)?);
            }
            Cbor::Array(v)
        }
        5 => {
            let mut v = Vec::new();
            for _ in 0..n.min(1024) {
                let k = cbor(b, at, depth + 1)?;
                v.push((k, cbor(b, at, depth + 1)?));
            }
            Cbor::Map(v)
        }
        6 => cbor(b, at, depth + 1)?, // a tag: its item
        _ => Cbor::Other,
    })
}

fn parse(b: &[u8]) -> Result<Cbor, String> {
    let mut at = 0;
    cbor(b, &mut at, 0)
}

// ---- the parts -------------------------------------------------------------

/// What the key signs besides the client data.
struct AuthData<'a> {
    rp_id_hash: &'a [u8],
    flags: u8,
    count: u32,
    /// With a registration: the credential id and its COSE key.
    attested: Option<(Vec<u8>, Cbor)>,
}

const UP: u8 = 0x01;
const UV: u8 = 0x04;
const AT: u8 = 0x40;

fn auth_data(b: &[u8]) -> Result<AuthData<'_>, String> {
    if b.len() < 37 {
        return Err("authenticator data too short".into());
    }
    let flags = b[32];
    let count = u32::from_be_bytes([b[33], b[34], b[35], b[36]]);
    let attested = if flags & AT != 0 {
        let rest = &b[37..];
        if rest.len() < 18 {
            return Err("attested credential data too short".into());
        }
        let len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
        let id = rest.get(18..18 + len).ok_or("credential id cut short")?.to_vec();
        let mut at = 18 + len;
        let key = cbor(rest, &mut at, 0)?;
        Some((id, key))
    } else {
        None
    };
    Ok(AuthData { rp_id_hash: &b[..32], flags, count, attested })
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
}

/// Checks the client data: its type, our challenge, an origin we take.
fn client_data(json: &[u8], kind: &str, challenge: &[u8], origins: &[String]) -> Result<(), String> {
    let c: ClientData = serde_json::from_slice(json).map_err(|e| format!("client data: {e}"))?;
    if c.kind != kind {
        return Err(format!("client data of type {}, not {kind}", c.kind));
    }
    if B64.decode(c.challenge.trim_end_matches('=')).ok().as_deref() != Some(challenge) {
        return Err("not the challenge of this sign-in".into());
    }
    if !origins.iter().any(|o| *o == c.origin) {
        return Err(format!("from {}, which is not this site", c.origin));
    }
    Ok(())
}

fn cose_key(k: &Cbor) -> Result<(i64, Vec<u8>), String> {
    let alg = k.get(&Cbor::Int(3)).and_then(Cbor::int).ok_or("the key names no algorithm")?;
    match alg {
        ES256 => {
            let (x, y) = (k.get(&Cbor::Int(-2)).and_then(Cbor::bytes), k.get(&Cbor::Int(-3)).and_then(Cbor::bytes));
            match (x, y, k.get(&Cbor::Int(-1)).and_then(Cbor::int)) {
                (Some(x), Some(y), Some(1)) if x.len() == 32 && y.len() == 32 => Ok((ES256, [&[4u8][..], x, y].concat())),
                _ => Err("not a P-256 key".into()),
            }
        }
        EDDSA => match (k.get(&Cbor::Int(-2)).and_then(Cbor::bytes), k.get(&Cbor::Int(-1)).and_then(Cbor::int)) {
            (Some(x), Some(6)) if x.len() == 32 => Ok((EDDSA, x.to_vec())),
            _ => Err("not an Ed25519 key".into()),
        },
        a => Err(format!("algorithm {a} is not taken (ES256 or EdDSA)")),
    }
}

/// A new key, from `navigator.credentials.create`'s answer.
pub fn register(rp_id: &str, origins: &[String], challenge: &[u8], client_json: &[u8], attestation: &[u8], name: &str, now: u64) -> Result<Credential, String> {
    client_data(client_json, "webauthn.create", challenge, origins)?;
    let att = parse(attestation)?;
    let ad = att.get(&Cbor::Text("authData".into())).and_then(Cbor::bytes).ok_or("no authenticator data")?;
    let ad = auth_data(ad)?;
    if ad.rp_id_hash != Sha256::digest(rp_id.as_bytes()).as_slice() {
        return Err("made for another site".into());
    }
    if ad.flags & UP == 0 {
        return Err("nobody touched the key".into());
    }
    let (id, key) = ad.attested.ok_or("no new credential in the answer")?;
    let (alg, key) = cose_key(&key)?;
    Ok(Credential { id: B64.encode(id), alg, key: B64.encode(key), name: name.into(), added: now })
}

/// A sign-in with `cred`: the counter the key reports, when it is right.
pub fn verify(
    cred: &Credential,
    rp_id: &str,
    origins: &[String],
    challenge: &[u8],
    client_json: &[u8],
    authenticator: &[u8],
    sig: &[u8],
    need_uv: bool,
) -> Result<u32, String> {
    client_data(client_json, "webauthn.get", challenge, origins)?;
    let ad = auth_data(authenticator)?;
    if ad.rp_id_hash != Sha256::digest(rp_id.as_bytes()).as_slice() {
        return Err("signed for another site".into());
    }
    if ad.flags & UP == 0 {
        return Err("nobody touched the key".into());
    }
    if need_uv && ad.flags & UV == 0 {
        return Err("the key did not check who holds it (PIN or fingerprint)".into());
    }
    let signed = [authenticator, Sha256::digest(client_json).as_slice()].concat();
    let key = B64.decode(&cred.key).map_err(|_| "a stored key that does not read")?;
    let alg: &dyn signature::VerificationAlgorithm = match cred.alg {
        ES256 => &signature::ECDSA_P256_SHA256_ASN1,
        EDDSA => &signature::ED25519,
        _ => return Err("a stored key of an algorithm not taken".into()),
    };
    signature::UnparsedPublicKey::new(alg, key).verify(&signed, sig).map_err(|_| "the signature is wrong")?;
    Ok(ad.count)
}

/// base64url, as the browser's side wants it.
pub fn b64(b: &[u8]) -> String {
    B64.encode(b)
}

/// base64url read back.
pub fn unb64(s: &str) -> Result<Vec<u8>, String> {
    B64.decode(s.trim_end_matches('=')).map_err(|_| "not base64url".to_string())
}

/// Where a ceremony happens: the relying party's id (the host name) and the
/// origins its pages have.
#[derive(Clone, Debug)]
pub struct Rp {
    pub id: String,
    pub origins: Vec<String>,
}

/// The origins a request at `host` (no port) may come from.
pub fn origins(host: &str, port: Option<u16>) -> Vec<String> {
    let mut v = vec![format!("https://{host}")];
    if let Some(p) = port {
        v.push(format!("https://{host}:{p}"));
    }
    // http only where browsers allow WebAuthn over it
    if host == "localhost" {
        v.push("http://localhost".into());
        if let Some(p) = port {
            v.push(format!("http://localhost:{p}"));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, Ed25519KeyPair, KeyPair},
    };

    fn cbor_bytes(b: &[u8]) -> Vec<u8> {
        let mut v = vec![0x58, b.len() as u8];
        v.extend_from_slice(b);
        v
    }

    fn cbor_text(s: &str) -> Vec<u8> {
        let mut v = vec![0x60 | s.len() as u8];
        v.extend_from_slice(s.as_bytes());
        v
    }

    /// A key as an authenticator would make it: the COSE key.
    fn cose_es256(point: &[u8]) -> Vec<u8> {
        // {1: 2, 3: -7, -1: 1, -2: x, -3: y}
        let mut v = vec![0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21];
        v.extend(cbor_bytes(&point[1..33]));
        v.push(0x22);
        v.extend(cbor_bytes(&point[33..65]));
        v
    }

    fn cose_ed25519(x: &[u8]) -> Vec<u8> {
        // {1: 1, 3: -8, -1: 6, -2: x}
        let mut v = vec![0xa4, 0x01, 0x01, 0x03, 0x27, 0x20, 0x06, 0x21];
        v.extend(cbor_bytes(x));
        v
    }

    fn auth(rp: &str, flags: u8, count: u32, cred: Option<(&[u8], &[u8])>) -> Vec<u8> {
        let mut v = Sha256::digest(rp.as_bytes()).to_vec();
        v.push(flags);
        v.extend_from_slice(&count.to_be_bytes());
        if let Some((id, key)) = cred {
            v.extend_from_slice(&[0u8; 16]);
            v.extend_from_slice(&(id.len() as u16).to_be_bytes());
            v.extend_from_slice(id);
            v.extend_from_slice(key);
        }
        v
    }

    fn attestation(ad: &[u8]) -> Vec<u8> {
        // {"fmt": "none", "attStmt": {}, "authData": ad}
        let mut v = vec![0xa3];
        v.extend(cbor_text("fmt"));
        v.extend(cbor_text("none"));
        v.extend(cbor_text("attStmt"));
        v.push(0xa0);
        v.extend(cbor_text("authData"));
        let mut b = vec![0x59];
        b.extend_from_slice(&(ad.len() as u16).to_be_bytes());
        b.extend_from_slice(ad);
        v.extend(b);
        v
    }

    fn client(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
        format!(r#"{{"type":"{kind}","challenge":"{}","origin":"{origin}","crossOrigin":false}}"#, B64.encode(challenge)).into_bytes()
    }

    const RP: &str = "osada.example.org";
    const ORIGIN: &str = "https://osada.example.org";

    #[test]
    fn es256_registers_and_signs_in() {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let origins = origins(RP, None);
        let ch = b"0123456789abcdef0123456789abcdef";
        let ad = auth(RP, UP | UV | AT, 0, Some((b"cred-1", &cose_es256(kp.public_key().as_ref()))));
        let cred = register(RP, &origins, ch, &client("webauthn.create", ch, ORIGIN), &attestation(&ad), "yubikey", 1).unwrap();
        assert_eq!((cred.alg, cred.id.as_str()), (ES256, B64.encode(b"cred-1").as_str()));
        // the wrong challenge, origin, type, or site
        assert!(register(RP, &origins, b"other", &client("webauthn.create", ch, ORIGIN), &attestation(&ad), "", 1).is_err());
        assert!(register(RP, &origins, ch, &client("webauthn.create", ch, "https://evil.example"), &attestation(&ad), "", 1).is_err());
        assert!(register(RP, &origins, ch, &client("webauthn.get", ch, ORIGIN), &attestation(&ad), "", 1).is_err());
        assert!(register("other.example", &super::origins(RP, None), ch, &client("webauthn.create", ch, ORIGIN), &attestation(&ad), "", 1).is_err());

        let ch2 = b"fedcba9876543210fedcba9876543210";
        let ad2 = auth(RP, UP | UV, 7, None);
        let cj = client("webauthn.get", ch2, ORIGIN);
        let sig = kp.sign(&rng, &[ad2.as_slice(), Sha256::digest(&cj).as_slice()].concat()).unwrap();
        assert_eq!(verify(&cred, RP, &origins, ch2, &cj, &ad2, sig.as_ref(), true), Ok(7));
        // a signature over something else, a replayed challenge, a key not touched, no UV where needed
        assert!(verify(&cred, RP, &origins, ch2, &cj, &auth(RP, UP | UV, 8, None), sig.as_ref(), true).is_err());
        assert!(verify(&cred, RP, &origins, ch, &cj, &ad2, sig.as_ref(), true).is_err());
        let ad3 = auth(RP, UP, 9, None);
        let sig3 = kp.sign(&rng, &[ad3.as_slice(), Sha256::digest(&cj).as_slice()].concat()).unwrap();
        assert!(verify(&cred, RP, &origins, ch2, &cj, &ad3, sig3.as_ref(), true).is_err());
        assert_eq!(verify(&cred, RP, &origins, ch2, &cj, &ad3, sig3.as_ref(), false), Ok(9));
        let ad4 = auth(RP, 0, 10, None);
        let sig4 = kp.sign(&rng, &[ad4.as_slice(), Sha256::digest(&cj).as_slice()].concat()).unwrap();
        assert!(verify(&cred, RP, &origins, ch2, &cj, &ad4, sig4.as_ref(), false).is_err());
    }

    #[test]
    fn ed25519_too() {
        let rng = SystemRandom::new();
        let kp = Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref()).unwrap();
        let origins = origins("localhost", Some(8080));
        let ch = b"challenge-challenge-challenge-32";
        let ad = auth("localhost", UP | AT, 0, Some((b"k2", &cose_ed25519(kp.public_key().as_ref()))));
        let cred = register("localhost", &origins, ch, &client("webauthn.create", ch, "http://localhost:8080"), &attestation(&ad), "", 1).unwrap();
        assert_eq!(cred.alg, EDDSA);
        let ad2 = auth("localhost", UP, 1, None);
        let cj = client("webauthn.get", ch, "http://localhost:8080");
        let sig = kp.sign(&[ad2.as_slice(), Sha256::digest(&cj).as_slice()].concat());
        assert_eq!(verify(&cred, "localhost", &origins, ch, &cj, &ad2, sig.as_ref(), false), Ok(1));
    }

    #[test]
    fn cbor_that_does_not_read_is_refused() {
        assert!(parse(&[0x5f]).is_err());
        assert!(parse(&[0x59, 0xff]).is_err());
        assert!(parse(&[0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).is_err());
        let deep = vec![0x81; 40];
        assert!(parse(&deep).is_err());
    }
}
