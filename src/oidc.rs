//! Sign-in for other programs (OpenID Connect) in osada 0.2 (D-U45). The programs (clients) are
//! in the cluster log, so every idp node knows them; for now home publishes osada 0.1's table
//! there (dropin.rs), where the admin pages still change them.

use serde::{Deserialize, Serialize};

/// A program that signs people in with osada; its name is its client_id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    /// The sha256 (hex) of its secret; none: a public client (PKCE only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_hash: Option<String>,
    pub redirect_uris: Vec<String>,
    /// Who may sign in (user names); empty: everyone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<String>,
}

impl Client {
    pub fn check(&self, name: &str) -> Result<(), String> {
        if !crate::osada1::users::valid_username(name) {
            return Err(format!("{name:?} is not a program's name"));
        }
        if self.redirect_uris.is_empty() || self.redirect_uris.iter().any(|u| u.len() > 1000 || !u.bytes().all(|b| b.is_ascii_graphic()) || u.contains('#')) {
            return Err(format!("{name}: its redirect URIs"));
        }
        if self.secret_hash.as_ref().is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit())) {
            return Err(format!("{name}: its secret's hash"));
        }
        Ok(())
    }

    pub fn lets_in(&self, user: &str) -> bool {
        self.users.is_empty() || self.users.iter().any(|u| u == user)
    }
}

// ---- the provider of an idp node (D-U45, step 3) ------------------------------------------
//
// As osada 0.1's (osada1/oidc.rs), for the programs it knows: the authorization code flow with
// PKCE (S256; required for public clients), RS256 JWTs. What differs, so that any idp node can go
// on where another left off: the code and the refresh token are JWTs too, signed by the node that
// made them (any node checks them with the keys of the cluster log); a node signs only for the
// accounts whose secrets it keeps, and a token counts only if its key's node keeps the subject's
// secrets ("signing follows the secrets", §10.7). Codes are good once on each node (60 s).

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL}};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{directory::User, node::Node, placement::NodeId};

const CODE_SECS: u64 = 60;
const PENDING_SECS: u64 = 600;
const ACCESS_SECS: u64 = 3600;
const REFRESH_SECS: u64 = 30 * 86400;
const MAX_HELD: usize = 10_000;
const SCOPES: &[&str] = &["openid", "profile", "email", "offline_access"];

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn random() -> String {
    crate::directory::random_hex(16)
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct AuthReq {
    pub response_type: Option<String>,
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    pub scope: Option<String>,
    pub state: Option<String>,
    pub nonce: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub prompt: Option<String>,
    pub max_age: Option<String>,
    pub response_mode: Option<String>,
    pub request: Option<String>,
    pub request_uri: Option<String>,
    pub login_hint: Option<String>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct TokenReq {
    pub grant_type: Option<String>,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
    pub scope: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

/// What the authorization endpoint does: back to the program (a URL), to the sign-in page (the
/// pending sign-in's id), or a page saying what is wrong (never sent to an unchecked URI).
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Redirect(String),
    Login { id: String, client: String, hint: Option<String> },
    Error(String),
}

/// An error of the token endpoint (RFC 6749 §5.2): status, code, description.
pub type TokenError = (u16, &'static str, String);

struct Pending {
    req: AuthReq,
    since: u64,
    relogin: bool,
}

#[derive(Default)]
pub struct Provider {
    pending: Mutex<HashMap<String, Pending>>,
    used: Mutex<HashMap<String, u64>>,
}

fn has_scope(scope: &str, s: &str) -> bool {
    scope.split_whitespace().any(|x| x == s)
}

fn grant_scope(asked: &str) -> String {
    let mut v: Vec<&str> = asked.split_whitespace().filter(|s| SCOPES.contains(s)).collect();
    v.dedup();
    v.join(" ")
}

fn pkce_ok(s: &str) -> bool {
    (43..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

fn back(uri: &str, kv: &[(&str, &str)], state: Option<&str>, iss: &str) -> String {
    let mut u = uri.to_string();
    let mut sep = if u.contains('?') { '&' } else { '?' };
    // all but the unreserved characters (RFC 3986 §2.3), as osada 0.1 does
    const QUERY: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');
    let enc = |s: &str| percent_encoding::utf8_percent_encode(s, QUERY).to_string();
    for (k, v) in kv.iter().copied().chain(state.map(|s| ("state", s))).chain([("iss", iss)]) {
        u.push(sep);
        u.push_str(k);
        u.push('=');
        u.push_str(&enc(v));
        sep = '&';
    }
    u
}

/// Whether `node` signs for `user`: it keeps the secrets of the user's OU.
pub fn signs_for(node: &Node, who: &NodeId, user: &User) -> bool {
    node.cluster_state().placement.secret_holders(user.ou).is_ok_and(|s| s.nodes.contains(who))
}

/// A JWT checked with the key its `kid` names in the cluster log: the node of the key, the claims.
pub fn verify_any(node: &Node, token: &str, typ: &str) -> Option<(NodeId, Value)> {
    let head = token.split('.').next()?;
    let head: Value = serde_json::from_slice(&B64URL.decode(head).ok()?).ok()?;
    if head["typ"] != typ {
        return None;
    }
    let kid = head["kid"].as_str()?;
    let st = node.cluster_state();
    let (who, key) = st.keys.iter().find(|(_, k)| k.kid == kid)?;
    let claims = crate::idpkey::verify(token, key).ok()?;
    (claims["exp"].as_u64()? > now()).then(|| (who.clone(), claims))
}

fn profile(claims: &mut Value, u: &User) {
    claims["preferred_username"] = json!(u.name);
    claims["name"] = json!(u.name);
}

fn email(claims: &mut Value, u: &User) {
    if let Some(m) = &u.mailbox {
        claims["email"] = json!(m.address);
        claims["email_verified"] = json!(true);
    }
}

impl Provider {
    fn hold_pending(&self, p: Pending) -> Option<String> {
        let t = now();
        let mut m = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, x| t - x.since < PENDING_SECS);
        if m.len() >= MAX_HELD {
            return None;
        }
        let id = random();
        m.insert(id.clone(), p);
        Some(id)
    }

    /// The authorization endpoint, for whoever is signed in at this node (`session`: the user and
    /// when the session started, ms).
    pub fn authorize(&self, node: &Node, iss: &str, req: AuthReq, session: Option<(User, u64)>) -> Outcome {
        let st = node.cluster_state();
        let name = req.client_id.clone().unwrap_or_default();
        let Some(client) = st.oidc_clients.get(&name) else { return Outcome::Error("no program with this client_id".into()) };
        let Some(uri) = req.redirect_uri.clone().filter(|u| client.redirect_uris.contains(u)) else {
            return Outcome::Error(format!("the redirect_uri is not one registered for {name}"));
        };
        let fail = |code: &str, desc: &str| Outcome::Redirect(back(&uri, &[("error", code), ("error_description", desc)], req.state.as_deref(), iss));
        if req.request.is_some() {
            return fail("request_not_supported", "request objects are not supported");
        }
        if req.request_uri.is_some() {
            return fail("request_uri_not_supported", "request_uri is not supported");
        }
        if req.response_type.as_deref() != Some("code") {
            return fail("unsupported_response_type", "only response_type=code");
        }
        if req.response_mode.as_deref().is_some_and(|m| m != "query") {
            return fail("invalid_request", "only response_mode=query");
        }
        if !has_scope(req.scope.as_deref().unwrap_or(""), "openid") {
            return fail("invalid_scope", "the scope must include openid");
        }
        match (req.code_challenge.as_deref(), req.code_challenge_method.as_deref()) {
            (None, None) if client.secret_hash.is_some() => {}
            (None, None) => return fail("invalid_request", "a public client must use PKCE (code_challenge, S256)"),
            (Some(c), Some("S256")) if pkce_ok(c) => {}
            _ => return fail("invalid_request", "code_challenge: only S256"),
        }
        let prompt: Vec<&str> = req.prompt.as_deref().unwrap_or("").split_whitespace().collect();
        if prompt.contains(&"none") && prompt.len() > 1 {
            return fail("invalid_request", "prompt=none goes alone");
        }
        let max_age = match req.max_age.as_deref().map(str::parse::<u64>) {
            None => None,
            Some(Ok(m)) => Some(m),
            _ => return fail("invalid_request", "max_age: seconds"),
        };
        let t = now();
        let relogin = prompt.contains(&"login") || matches!((max_age, &session), (Some(m), Some((_, at))) if t.saturating_sub(at / 1000) > m);
        match session {
            Some((user, at)) if !relogin => self.finish(node, iss, &req, &uri, &user, at / 1000),
            _ if prompt.contains(&"none") => fail("login_required", "not signed in"),
            _ => match self.hold_pending(Pending { req: req.clone(), since: t, relogin }) {
                Some(id) => Outcome::Login { id, client: name, hint: req.login_hint.clone() },
                None => fail("temporarily_unavailable", "too many sign-ins at once"),
            },
        }
    }

    /// After the sign-in page: on where it was left.
    pub fn resume(&self, node: &Node, iss: &str, id: &str, session: Option<(User, u64)>) -> Outcome {
        let t = now();
        let held = {
            let m = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            m.get(id).filter(|p| t - p.since < PENDING_SECS).map(|p| (p.req.clone(), p.since, p.relogin))
        };
        let Some((req, since, relogin)) = held else { return Outcome::Error("this sign-in is done or took too long; start again from the program".into()) };
        match session {
            Some((user, at)) if !relogin || at / 1000 >= since => {
                self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
                let uri = req.redirect_uri.clone().unwrap_or_default();
                let ok = node.cluster_state().oidc_clients.get(req.client_id.as_deref().unwrap_or("")).is_some_and(|c| c.redirect_uris.contains(&uri));
                if !ok {
                    return Outcome::Error("the program or its redirect URI was changed".into());
                }
                self.finish(node, iss, &req, &uri, &user, at / 1000)
            }
            _ => Outcome::Login { id: id.to_string(), client: req.client_id.clone().unwrap_or_default(), hint: req.login_hint.clone() },
        }
    }

    fn finish(&self, node: &Node, iss: &str, req: &AuthReq, uri: &str, user: &User, auth_time: u64) -> Outcome {
        let state = req.state.as_deref();
        let name = req.client_id.clone().unwrap_or_default();
        let st = node.cluster_state();
        let client = st.oidc_clients.get(&name).cloned();
        if !client.as_ref().is_some_and(|c| c.lets_in(&user.name)) {
            let desc = format!("{} may not use {name}", user.name);
            return Outcome::Redirect(back(uri, &[("error", "access_denied"), ("error_description", &desc)], state, iss));
        }
        let Some(key) = node.idp_key().filter(|_| signs_for(node, node.me(), user)) else {
            return Outcome::Redirect(back(uri, &[("error", "temporarily_unavailable"), ("error_description", "this node does not sign for your account")], state, iss));
        };
        let t = now();
        let code = json!({
            "cid": name, "sub": user.id, "uri": uri, "scope": grant_scope(req.scope.as_deref().unwrap_or("")), "nonce": req.nonce,
            "ch": req.code_challenge, "at": auth_time, "g": random(), "jti": random(), "exp": t + CODE_SECS,
        });
        match key.sign("code+jwt", &code) {
            Ok(c) => Outcome::Redirect(back(uri, &[("code", &c)], state, iss)),
            Err(e) => Outcome::Error(e),
        }
    }

    /// The token endpoint: client_secret_basic, client_secret_post, or none for a public client.
    pub fn token(&self, node: &Node, iss: &str, basic: Option<&str>, req: &TokenReq) -> Result<Value, TokenError> {
        let bad = || (401, "invalid_client", "unknown client or wrong secret".to_string());
        let unform = |s: &str| percent_encoding::percent_decode_str(&s.replace('+', " ")).decode_utf8_lossy().into_owned();
        let (id, secret) = match basic {
            Some(b) => {
                if req.client_secret.is_some() {
                    return Err((400, "invalid_request", "client secret both in the header and in the body".into()));
                }
                let raw = B64.decode(b.trim()).ok().and_then(|r| String::from_utf8(r).ok()).ok_or_else(bad)?;
                let (i, s) = raw.split_once(':').ok_or_else(bad)?;
                (unform(i), Some(unform(s)))
            }
            None => (req.client_id.clone().ok_or_else(bad)?, req.client_secret.clone()),
        };
        let st = node.cluster_state();
        let client = st.oidc_clients.get(&id).cloned().ok_or_else(bad)?;
        let ok = match (&client.secret_hash, &secret) {
            (Some(h), Some(s)) => crate::osada1::edge::ct_eq(hex::encode(Sha256::digest(s.as_bytes())).as_bytes(), h.as_bytes()),
            (None, None) => true,
            _ => false,
        };
        if !ok {
            return Err(bad());
        }
        let grant = |d: &str| (400, "invalid_grant", d.to_string());
        let (user, scope, nonce, auth_time, g) = match req.grant_type.as_deref() {
            Some("authorization_code") => {
                let (_, c) = req.code.as_deref().and_then(|c| verify_any(node, c, "code+jwt")).ok_or_else(|| grant("unknown or expired code"))?;
                if c["cid"] != id.as_str() {
                    return Err(grant("the code is another program's"));
                }
                if req.redirect_uri.as_deref() != c["uri"].as_str() {
                    return Err(grant("redirect_uri is not the one of the authorization request"));
                }
                match (c["ch"].as_str(), req.code_verifier.as_deref()) {
                    (None, None) => {}
                    (Some(ch), Some(v)) if pkce_ok(v) && B64URL.encode(Sha256::digest(v)) == ch => {}
                    _ => return Err(grant("code_verifier does not match the code_challenge")),
                }
                let jti = c["jti"].as_str().unwrap_or("").to_string();
                {
                    let t = now();
                    let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
                    used.retain(|_, exp| *exp > t);
                    if used.insert(jti, t + CODE_SECS).is_some() {
                        return Err(grant("the code was used already"));
                    }
                }
                let user = node.find_user(c["sub"].as_str().unwrap_or("")).ok_or_else(|| grant("no such account"))?;
                (user, c["scope"].as_str().unwrap_or("").to_string(), c["nonce"].as_str().map(String::from), c["at"].as_u64().unwrap_or(0), c["g"].as_str().unwrap_or("").to_string())
            }
            Some("refresh_token") => {
                let rt = req.refresh_token.as_deref().ok_or((400, "invalid_request", "refresh_token missing".to_string()))?;
                let (by, c) = verify_any(node, rt, "rt+jwt").ok_or_else(|| grant("unknown or expired refresh token"))?;
                if c["cid"] != id.as_str() {
                    return Err(grant("the refresh token is another program's"));
                }
                let user = node.find_user(c["sub"].as_str().unwrap_or("")).ok_or_else(|| grant("no such account"))?;
                // signed by a node that keeps the account's secrets, and not before they last changed
                if !signs_for(node, &by, &user) {
                    return Err(grant("that node does not sign for the account"));
                }
                let scope = c["scope"].as_str().unwrap_or("").to_string();
                let scope = match req.scope.as_deref() {
                    None => scope,
                    Some(s) if s.split_whitespace().all(|x| has_scope(&scope, x)) => grant_scope(s),
                    Some(_) => return Err((400, "invalid_scope", "more than the original grant".into())),
                };
                (user, scope, None, c["at"].as_u64().unwrap_or(0), c["g"].as_str().unwrap_or("").to_string())
            }
            _ => return Err((400, "unsupported_grant_type", "authorization_code or refresh_token".into())),
        };
        if user.disabled {
            return Err(grant("the account is disabled"));
        }
        if !client.lets_in(&user.name) {
            return Err(grant(&format!("{} may not use {id}", user.name)));
        }
        if node.revoked(&user.id, auth_time) {
            return Err(grant("signed out since; sign in again"));
        }
        self.issue(node, iss, &id, &user, &scope, nonce.as_deref(), auth_time, &g).map_err(|e| (503, "temporarily_unavailable", e))
    }

    #[allow(clippy::too_many_arguments)]
    fn issue(&self, node: &Node, iss: &str, client: &str, user: &User, scope: &str, nonce: Option<&str>, auth_time: u64, g: &str) -> Result<Value, String> {
        let key = node.idp_key().filter(|_| signs_for(node, node.me(), user)).ok_or("this node does not sign for the account")?;
        let t = now();
        let mut at = json!({
            "iss": iss, "sub": user.id, "aud": client, "client_id": client, "scope": scope,
            "iat": t, "exp": t + ACCESS_SECS, "auth_time": auth_time, "jti": random(),
        });
        if has_scope(scope, "profile") {
            profile(&mut at, user);
        }
        if has_scope(scope, "email") {
            email(&mut at, user);
        }
        let access = key.sign("at+jwt", &at)?;
        let refresh = key.sign("rt+jwt", &json!({ "cid": client, "sub": user.id, "scope": scope, "at": auth_time, "g": g, "iat": t, "exp": t + REFRESH_SECS, "jti": random() }))?;
        let mut out = json!({ "access_token": access, "token_type": "Bearer", "expires_in": ACCESS_SECS, "refresh_token": refresh, "scope": scope });
        if has_scope(scope, "openid") {
            let mut c = json!({
                "iss": iss, "sub": user.id, "aud": client, "azp": client, "iat": t, "exp": t + ACCESS_SECS, "auth_time": auth_time,
                "at_hash": B64URL.encode(&Sha256::digest(access.as_bytes())[..16]),
            });
            if let Some(n) = nonce {
                c["nonce"] = json!(n);
            }
            if has_scope(scope, "profile") {
                profile(&mut c, user);
            }
            if has_scope(scope, "email") {
                email(&mut c, user);
            }
            out["id_token"] = json!(key.sign("JWT", &c)?);
        }
        Ok(out)
    }

    /// The userinfo endpoint: the claims of a good access token's account, as they are now.
    pub fn userinfo(&self, node: &Node, bearer: &str) -> Option<Value> {
        let (by, c) = verify_any(node, bearer.trim(), "at+jwt")?;
        let user = node.find_user(c["sub"].as_str()?)?;
        if user.disabled || !signs_for(node, &by, &user) {
            return None;
        }
        let scope = c["scope"].as_str().unwrap_or("");
        let mut v = json!({ "sub": user.id });
        if has_scope(scope, "profile") {
            profile(&mut v, &user);
        }
        if has_scope(scope, "email") {
            email(&mut v, &user);
        }
        Some(v)
    }
}

pub fn discovery(iss: &str) -> Value {
    json!({
        "issuer": iss,
        "authorization_endpoint": format!("{iss}/oidc/authorize"),
        "token_endpoint": format!("{iss}/oidc/token"),
        "userinfo_endpoint": format!("{iss}/oidc/userinfo"),
        "jwks_uri": format!("{iss}/oidc/jwks"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "code_challenge_methods_supported": ["S256"],
        "scopes_supported": SCOPES,
        "claims_supported": ["iss", "sub", "aud", "azp", "exp", "iat", "auth_time", "nonce", "at_hash", "preferred_username", "name", "email", "email_verified"],
        "authorization_response_iss_parameter_supported": true,
    })
}
