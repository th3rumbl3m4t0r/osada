//! Mail (D-U38): the mail instance's Stalwart, managed from admin → mail. The node that runs the
//! instance talks to its Stalwart's management API (JMAP with Stalwart's `x:` objects, 0.16) with
//! a credential of its own, `<data>/mail/stalwart.auth` (an API key, or `Basic <base64>`), at
//! OSADA_STALWART_URL (http://127.0.0.1:8080); home asks that node over the link (Call::Mail).
//!
//! Operations: `state` (domains with their DNS records, mailboxes with their size and use),
//! `domain_add`, `domain_enable`, `domain_remove`, `quota` (a mailbox's size; none: no limit);
//! for the mailboxes osada keeps (D-U43, node/mailadmin.rs): `account_create` (in its domain, made
//! first when asked, with the mailbox as its catch-all, as the old sync did), `account_update`
//! (size, on or off), `account_destroy` (with its mail).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The credential file in a node's data folder.
pub fn auth_file(data: &Path) -> PathBuf {
    data.join("mail/stalwart.auth")
}

pub struct Stalwart {
    http: reqwest::Client,
    base: String,
    auth: String,
}

fn e(x: impl std::fmt::Display) -> String {
    format!("Stalwart: {x}")
}

impl Stalwart {
    pub fn from_data(data: &Path) -> Result<Stalwart, String> {
        let file = auth_file(data);
        let raw = std::fs::read_to_string(&file)
            .map_err(|_| format!("no credential for Stalwart on this node ({}: an API key of Stalwart's, or Basic …)", file.display()))?;
        let raw = raw.trim();
        let auth = if raw.starts_with("Basic ") || raw.starts_with("Bearer ") { raw.to_string() } else { format!("Bearer {raw}") };
        let base = std::env::var("OSADA_STALWART_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into()).trim_end_matches('/').to_string();
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(8)).build().map_err(e)?;
        Ok(Stalwart { http, base, auth })
    }

    async fn json(&self, req: reqwest::RequestBuilder) -> Result<Value, String> {
        let r = req.header("authorization", &self.auth).send().await.map_err(e)?;
        let status = r.status();
        let body = r.bytes().await.map_err(e)?;
        if !status.is_success() {
            return Err(e(format!("HTTP {status}: {}", String::from_utf8_lossy(&body[..body.len().min(200)]))));
        }
        serde_json::from_slice(&body).map_err(e)
    }

    /// One request of method calls; their answers, or the first error.
    async fn call(&self, calls: Vec<(&str, Value)>) -> Result<Vec<Value>, String> {
        let s = self.json(self.http.get(format!("{}/jmap/session", self.base))).await?;
        let acct = s["primaryAccounts"]["urn:stalwart:jmap"]
            .as_str()
            .map(str::to_string)
            .or_else(|| s["accounts"].as_object().and_then(|a| a.keys().next().cloned()))
            .ok_or_else(|| e("no account in the session"))?;
        let path = s["apiUrl"].as_str().and_then(|u| u.split("://").nth(1)).and_then(|r| r.split_once('/')).map_or("jmap/", |(_, p)| p);
        let mc: Vec<Value> = calls
            .into_iter()
            .enumerate()
            .map(|(i, (m, mut a))| {
                a["accountId"] = json!(acct);
                json!([m, a, i.to_string()])
            })
            .collect();
        let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:stalwart:jmap"], "methodCalls": mc });
        let v = self.json(self.http.post(format!("{}/{path}", self.base)).header("content-type", "application/json").body(body.to_string())).await?;
        let mut out = Vec::new();
        for r in v["methodResponses"].as_array().cloned().unwrap_or_default() {
            let (name, res) = (r[0].as_str().unwrap_or(""), r[1].clone());
            if name == "error" {
                return Err(e(format!("{res}")));
            }
            for k in ["notCreated", "notUpdated", "notDestroyed"] {
                if let Some(m) = res[k].as_object().filter(|m| !m.is_empty()) {
                    let why: Vec<String> = m
                        .values()
                        .map(|x| match x["type"].as_str() {
                            Some("objectIsLinked") => {
                                let linked: Vec<&str> = x["linkedObjects"].as_array().map(|l| l.iter().filter_map(|o| o["object"].as_str()).collect()).unwrap_or_default();
                                format!("still linked to: {}", linked.join(", "))
                            }
                            _ => {
                                let why = x["description"].as_str().or(x["type"].as_str()).unwrap_or("refused").to_string();
                                match x["properties"].as_array().filter(|p| !p.is_empty()) {
                                    Some(p) => format!("{why} ({})", p.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")),
                                    None => why,
                                }
                            }
                        })
                        .collect();
                    return Err(e(why.join("; ")));
                }
            }
            out.push(res);
        }
        Ok(out)
    }

    /// The ids of the domains `names`; one Stalwart lacks is made when `make` (DKIM by hand, as
    /// the old sync made them), else refused. Also the names made.
    async fn domain_ids(&self, names: &[&str], make: bool) -> Result<(std::collections::BTreeMap<String, Value>, Vec<String>), String> {
        let r = self.call(vec![("x:Domain/get", json!({ "properties": ["name"] }))]).await?;
        let mut ids: std::collections::BTreeMap<String, Value> = r[0]["list"]
            .as_array()
            .map(|l| l.iter().filter_map(|d| Some((d["name"].as_str()?.to_ascii_lowercase(), d["id"].clone()))).collect())
            .unwrap_or_default();
        let mut made = Vec::new();
        for n in names {
            let n = n.to_ascii_lowercase();
            if ids.contains_key(&n) || made.contains(&n) {
                continue;
            }
            if !make {
                return Err(format!("Stalwart has no domain {n}: add it (admin → mail → domains), or let osada make domains"));
            }
            if !crate::placement::valid_host(&n) || !n.contains('.') {
                return Err(format!("{n:?} is not a domain name"));
            }
            let d = json!({ "name": n, "dkimManagement": { "@type": "Manual" }, "description": "made by osada" });
            let r = self.call(vec![("x:Domain/set", json!({ "create": { "d": d } }))]).await?;
            ids.insert(n.clone(), r[0]["created"]["d"]["id"].clone());
            made.push(n);
        }
        Ok((ids, made))
    }

    /// Domains and mailboxes.
    pub async fn state(&self) -> Result<Value, String> {
        let r = self
            .call(vec![
                ("x:Domain/get", json!({ "properties": ["name", "isEnabled", "catchAllAddress", "dnsZoneFile", "description"] })),
                ("x:Account/get", json!({ "properties": ["name", "emailAddress", "description", "quotas", "usedDiskQuota", "permissions", "domainId", "aliases"] })),
            ])
            .await?;
        let domains: Vec<Value> = r[0]["list"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|d| json!({ "id": d["id"], "name": d["name"], "enabled": d["isEnabled"].as_bool().unwrap_or(true), "catch_all": d["catchAllAddress"], "description": d["description"], "dns": d["dnsZoneFile"] }))
            .collect();
        let names: std::collections::BTreeMap<String, String> = r[0]["list"]
            .as_array()
            .map(|l| l.iter().filter_map(|d| Some((d["id"].as_str()?.to_string(), d["name"].as_str()?.to_string()))).collect())
            .unwrap_or_default();
        let accounts: Vec<Value> = r[1]["list"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a["@type"].as_str().unwrap_or("User") == "User")
            .map(|a| {
                let off = a["permissions"]["disabledPermissions"]["authenticate"].as_bool().unwrap_or(false);
                // the aliases as addresses (Stalwart keeps a map of {name, domainId})
                let mut aliases: Vec<String> = a["aliases"]
                    .as_object()
                    .map(|m| m.values().filter_map(|x| Some(format!("{}@{}", x["name"].as_str()?, names.get(x["domainId"].as_str()?)?).to_lowercase())).collect())
                    .unwrap_or_default();
                aliases.sort();
                json!({ "id": a["id"], "name": a["name"], "email": a["emailAddress"], "description": a["description"], "domain": a["domainId"],
                        "quota": a["quotas"]["maxDiskQuota"], "used": a["usedDiskQuota"], "disabled": off, "aliases": aliases })
            })
            .collect();
        Ok(json!({ "domains": domains, "accounts": accounts }))
    }

    /// An operation from admin → mail (`op` and its arguments); its answer.
    pub async fn op(&self, op: &Value) -> Result<Value, String> {
        let id = || op["id"].as_str().filter(|s| !s.is_empty()).map(str::to_string).ok_or("which one? (id)");
        match op["op"].as_str().unwrap_or("") {
            "state" => self.state().await,
            "domain_add" => {
                let name = op["name"].as_str().unwrap_or("").trim().to_ascii_lowercase();
                if !crate::placement::valid_host(&name) || !name.contains('.') {
                    return Err(format!("{name:?} is not a domain name"));
                }
                let r = self.call(vec![("x:Domain/set", json!({ "create": { "d": { "name": name, "description": "admin → mail" } } }))]).await?;
                Ok(json!({ "id": r[0]["created"]["d"]["id"] }))
            }
            "domain_enable" => {
                let on = op["on"].as_bool().ok_or("on: true or false")?;
                self.call(vec![("x:Domain/set", json!({ "update": { id()?: { "isEnabled": on } } }))]).await?;
                Ok(json!({}))
            }
            "domain_remove" => {
                // its mailboxes first (by hand: they hold mail); its DKIM keys go with it
                let id = id()?;
                let r = self
                    .call(vec![
                        ("x:Account/get", json!({ "properties": ["domainId", "emailAddress"] })),
                        ("x:DkimSignature/get", json!({ "properties": ["domainId"] })),
                    ])
                    .await?;
                let of = |v: &Value| -> Vec<Value> { v["list"].as_array().map(|l| l.iter().filter(|x| x["domainId"].as_str() == Some(&id)).cloned().collect()).unwrap_or_default() };
                let boxes = of(&r[0]);
                if !boxes.is_empty() {
                    let names: Vec<&str> = boxes.iter().filter_map(|a| a["emailAddress"].as_str()).collect();
                    return Err(format!("{} mailbox(es) in it ({}): remove them first", names.len(), names.join(", ")));
                }
                let keys: Vec<Value> = of(&r[1]).into_iter().map(|k| k["id"].clone()).collect();
                let mut calls = Vec::new();
                if !keys.is_empty() {
                    calls.push(("x:DkimSignature/set", json!({ "destroy": keys })));
                }
                calls.push(("x:Domain/set", json!({ "destroy": [id] })));
                self.call(calls).await?;
                Ok(json!({}))
            }
            "account_create" => {
                let address = op["address"].as_str().unwrap_or("").to_string();
                if !crate::directory::valid_address(&address) {
                    return Err(format!("{address:?} is not a mail address"));
                }
                let (local, domain) = address.split_once('@').expect("checked");
                let make = op["make_domain"].as_bool() == Some(true);
                let aliases = strings(&op["aliases"]);
                let mut wanted: Vec<&str> = vec![domain];
                wanted.extend(aliases.iter().filter_map(|a| a.split_once('@').map(|(_, d)| d)));
                let (ids, made) = self.domain_ids(&wanted, make).await?;
                let did = ids[domain].clone();
                let made = made.contains(&domain.to_string());
                let mut a = json!({ "@type": "User", "name": local, "domainId": did, "permissions": permissions(op["off"].as_bool().unwrap_or(false)) });
                if !aliases.is_empty() {
                    a["aliases"] = alias_map(&aliases, &ids)?;
                }
                // Stalwart takes no empty description
                if let Some(d) = op["description"].as_str().map(str::trim).filter(|d| !d.is_empty()) {
                    a["description"] = json!(d);
                }
                if let Some(q) = op["quota"].as_u64().filter(|q| *q > 0) {
                    a["quotas"] = json!({ "maxDiskQuota": q });
                }
                let r = self.call(vec![("x:Account/set", json!({ "create": { "a": a } }))]).await?;
                let id = r[0]["created"]["a"]["id"].clone();
                if made {
                    self.call(vec![("x:Domain/set", json!({ "update": { did.as_str().unwrap_or(""): { "catchAllAddress": address } } }))]).await?;
                }
                Ok(json!({ "id": id, "domain_made": made }))
            }
            "account_update" => {
                let mut u = json!({});
                let make = op["make_domain"].as_bool() == Some(true);
                let new_address = op["address"].as_str().map(str::to_string);
                let aliases = op.get("aliases").map(strings);
                let mut wanted: Vec<String> = Vec::new();
                if let Some(a) = &new_address {
                    if !crate::directory::valid_address(a) {
                        return Err(format!("{a:?} is not a mail address"));
                    }
                    wanted.push(a.split_once('@').expect("checked").1.to_string());
                }
                for a in aliases.iter().flatten() {
                    wanted.push(a.split_once('@').map(|(_, d)| d.to_string()).ok_or_else(|| format!("{a:?} is not a mail address"))?);
                }
                if !wanted.is_empty() {
                    let names: Vec<&str> = wanted.iter().map(String::as_str).collect();
                    let (ids, _) = self.domain_ids(&names, make).await?;
                    if let Some(a) = &new_address {
                        let (local, domain) = a.split_once('@').expect("checked");
                        u["name"] = json!(local);
                        u["domainId"] = ids[domain].clone();
                    }
                    if let Some(list) = &aliases {
                        u["aliases"] = alias_map(list, &ids)?;
                    }
                } else if aliases.is_some() {
                    u["aliases"] = json!({});
                }
                if let Some(off) = op["off"].as_bool() {
                    u["permissions"] = permissions(off);
                }
                if op.get("quota").is_some() {
                    u["quotas"] = match op["quota"].as_u64() {
                        Some(0) | None => json!({}),
                        Some(b) => json!({ "maxDiskQuota": b }),
                    };
                }
                self.call(vec![("x:Account/set", json!({ "update": { id()?: u } }))]).await?;
                Ok(json!({}))
            }
            "catch_all" => {
                // a domain's catch-all: the mailbox, or none
                let domain = op["domain"].as_str().unwrap_or("").to_ascii_lowercase();
                let to = op["address"].as_str().map(str::to_lowercase);
                let make = to.is_some() && op["make_domain"].as_bool() == Some(true);
                let (ids, _) = self.domain_ids(&[domain.as_str()], make).await?;
                self.call(vec![("x:Domain/set", json!({ "update": { ids[&domain].as_str().unwrap_or(""): { "catchAllAddress": to } } }))]).await?;
                Ok(json!({}))
            }
            "account_destroy" => {
                self.call(vec![("x:Account/set", json!({ "destroy": [id()?] }))]).await?;
                Ok(json!({}))
            }
            "quota" => {
                let quotas = match op["bytes"].as_u64() {
                    Some(0) | None => json!({}),
                    Some(b) => json!({ "maxDiskQuota": b }),
                };
                self.call(vec![("x:Account/set", json!({ "update": { id()?: { "quotas": quotas } } }))]).await?;
                Ok(json!({}))
            }
            other => Err(format!("unknown mail operation {other:?}")),
        }
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(str::to_lowercase)).collect()).unwrap_or_default()
}

/// Aliases (`name@domain`) as Stalwart keeps them: a map of {name, domainId}.
fn alias_map(aliases: &[String], ids: &std::collections::BTreeMap<String, Value>) -> Result<Value, String> {
    let mut m = serde_json::Map::new();
    for (i, a) in aliases.iter().enumerate() {
        let (local, domain) = a.split_once('@').ok_or_else(|| format!("{a:?} is not a mail address"))?;
        let id = ids.get(domain).ok_or_else(|| format!("Stalwart has no domain {domain}"))?;
        m.insert(i.to_string(), json!({ "name": local, "domainId": id }));
    }
    Ok(Value::Object(m))
}

/// JMAP as a person (Round 15: osada's contacts and calendar pages): `token` is an access token
/// osada signed for them (Stalwart's OIDC directory takes it); `calls` are method calls whose
/// accountId is filled in by kind (Calendar…, ContactCard / AddressBook, Email…).
pub async fn jmap_as(token: &str, calls: &Value) -> Result<Value, String> {
    let base = std::env::var("OSADA_STALWART_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into()).trim_end_matches('/').to_string();
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().map_err(e)?;
    let auth = format!("Bearer {token}");
    let get = |r: reqwest::RequestBuilder| async { 
        let r = r.header("authorization", &auth).send().await.map_err(e)?;
        let status = r.status();
        let body = r.bytes().await.map_err(e)?;
        if !status.is_success() {
            return Err(e(format!("HTTP {status}: {}", String::from_utf8_lossy(&body[..body.len().min(200)]))));
        }
        serde_json::from_slice::<Value>(&body).map_err(e)
    };
    let s = get(http.get(format!("{base}/jmap/session"))).await?;
    let acct = |cap: &str| s["primaryAccounts"][cap].as_str().map(str::to_string);
    let path = s["apiUrl"].as_str().and_then(|u| u.split("://").nth(1)).and_then(|r| r.split_once('/')).map_or("jmap/", |(_, p)| p).to_string();
    let mut mc = Vec::new();
    for c in calls.as_array().cloned().unwrap_or_default() {
        let (m, mut a, id) = (c[0].as_str().unwrap_or("").to_string(), c[1].clone(), c[2].clone());
        let cap = if m.starts_with("Calendar") { "urn:ietf:params:jmap:calendars" } else if m.starts_with("ContactCard") || m.starts_with("AddressBook") { "urn:ietf:params:jmap:contacts" } else { "urn:ietf:params:jmap:mail" };
        if a.get("accountId").is_none() {
            a["accountId"] = json!(acct(cap).ok_or_else(|| e(format!("no account for {cap}")))?);
        }
        mc.push(json!([m, a, id]));
    }
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:calendars", "urn:ietf:params:jmap:contacts"], "methodCalls": mc });
    let v = get(http.post(format!("{base}/{path}")).header("content-type", "application/json").body(body.to_string())).await?;
    Ok(v["methodResponses"].clone())
}

/// An account's permissions: its own (`Inherit`), or no signing in (off; mail still comes in).
fn permissions(off: bool) -> Value {
    if off {
        json!({ "@type": "Merge", "enabledPermissions": {}, "disabledPermissions": { "authenticate": true } })
    } else {
        json!({ "@type": "Inherit" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against a real Stalwart (D-U43): OSADA_TEST_STALWART=<url>, OSADA_TEST_STALWART_AUTH=<file with
    /// user:password>. A mailbox in a domain made with it (its catch-all), its size and off, then
    /// removed with its domain.
    #[tokio::test]
    #[ignore]
    async fn mailboxes_in_a_real_stalwart() {
        let base = std::env::var("OSADA_TEST_STALWART").expect("OSADA_TEST_STALWART");
        let cred = std::fs::read_to_string(std::env::var("OSADA_TEST_STALWART_AUTH").unwrap()).unwrap();
        use base64::Engine;
        let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(cred.trim()));
        let sw = Stalwart { http: reqwest::Client::new(), base, auth };
        let acct = |st: &Value, addr: &str| st["accounts"].as_array().unwrap().iter().find(|a| a["email"].as_str() == Some(addr)).cloned();
        let none = sw.op(&json!({ "op": "account_create", "address": "anna@anna.ca.example.org", "make_domain": false })).await.unwrap_err();
        assert!(none.contains("no domain anna.ca.example.org"), "{none}");
        let made = sw
            .op(&json!({ "op": "account_create", "address": "anna@anna.ca.example.org", "make_domain": true, "quota": 1u64 << 30, "description": "Anna" }))
            .await
            .unwrap();
        assert_eq!(made["domain_made"], true);
        let st = sw.state().await.unwrap();
        let a = acct(&st, "anna@anna.ca.example.org").expect("the account");
        assert_eq!(a["quota"], 1u64 << 30);
        assert_eq!(a["disabled"], false);
        let d = st["domains"].as_array().unwrap().iter().find(|d| d["name"] == "anna.ca.example.org").cloned().unwrap();
        assert_eq!(d["catch_all"], "anna@anna.ca.example.org");
        // a second in the same domain: no new domain
        let b = sw.op(&json!({ "op": "account_create", "address": "ben@anna.ca.example.org", "make_domain": true, "off": true })).await.unwrap();
        assert_eq!(b["domain_made"], false);
        let st = sw.state().await.unwrap();
        assert_eq!(acct(&st, "ben@anna.ca.example.org").unwrap()["disabled"], true);
        assert!(acct(&st, "ben@anna.ca.example.org").unwrap()["quota"].as_u64().is_none());
        // size and off, then on and no limit
        sw.op(&json!({ "op": "account_update", "id": a["id"], "quota": 2u64 << 30, "off": true })).await.unwrap();
        let st = sw.state().await.unwrap();
        let x = acct(&st, "anna@anna.ca.example.org").unwrap();
        assert_eq!((x["quota"].as_u64(), x["disabled"].as_bool()), (Some(2 << 30), Some(true)));
        sw.op(&json!({ "op": "account_update", "id": a["id"], "quota": null, "off": false })).await.unwrap();
        let st = sw.state().await.unwrap();
        let x = acct(&st, "anna@anna.ca.example.org").unwrap();
        assert!(x["quota"].as_u64().is_none_or(|q| q == 0), "{x}");
        assert_eq!(x["disabled"], false);
        // the plan sees nothing to do now
        let users = vec![crate::directory::User {
            id: "u-anna".into(),
            name: "anna".into(),
            display: "Anna".into(),
            ou: crate::placement::OuId(1),
            admin: false,
            disabled: false,
            outside: String::new(),
            mailbox: Some(crate::directory::Mailbox { address: "anna@anna.ca.example.org".into(), aliases: Some(vec!["*@anna.ca.example.org".into()]), quota: None, off: false, instance: None }),
        }];
        let plan = crate::node::mail_plan(&users, st["accounts"].as_array().unwrap(), st["domains"].as_array().unwrap(), true, None);
        assert!(plan.is_empty(), "{plan:?}");
        // the plan once anna has aliases and the catch-all of another domain (D-U44): Stalwart follows
        let mut users = users;
        users[0].mailbox.as_mut().unwrap().aliases = Some(vec!["a@anna.ca.example.org".into(), "*@anna.ca.example.org".into(), "*@two.example.org".into()]);
        for (_, op) in crate::node::mail_plan(&users, st["accounts"].as_array().unwrap(), st["domains"].as_array().unwrap(), true, None) {
            sw.op(&op).await.unwrap();
        }
        let st = sw.state().await.unwrap();
        assert_eq!(acct(&st, "anna@anna.ca.example.org").unwrap()["aliases"], json!(["a@anna.ca.example.org"]));
        let two = st["domains"].as_array().unwrap().iter().find(|d| d["name"] == "two.example.org").cloned().expect("made with the catch-all");
        assert_eq!(two["catch_all"], "anna@anna.ca.example.org");
        assert!(crate::node::mail_plan(&users, st["accounts"].as_array().unwrap(), st["domains"].as_array().unwrap(), true, None).is_empty());
        // a new address: the account renamed, its aliases kept
        sw.op(&json!({ "op": "account_update", "id": a["id"], "address": "anna@two.example.org" })).await.unwrap();
        let st = sw.state().await.unwrap();
        assert_eq!(acct(&st, "anna@two.example.org").unwrap()["aliases"], json!(["a@anna.ca.example.org"]));
        sw.op(&json!({ "op": "catch_all", "domain": "two.example.org", "address": null })).await.unwrap();
        sw.op(&json!({ "op": "catch_all", "domain": "anna.ca.example.org", "address": null })).await.unwrap();
        let mut st = sw.state().await.unwrap();
        for x in st["accounts"].as_array_mut().unwrap() {
            if x["email"] == "anna@two.example.org" {
                x["email"] = json!("anna@anna.ca.example.org");
            }
        }
        sw.op(&json!({ "op": "domain_remove", "id": two["id"] })).await.unwrap_err();
        // removed: the accounts, then the domain (refused while they are in it)
        let refused = sw.op(&json!({ "op": "domain_remove", "id": d["id"] })).await.unwrap_err();
        assert!(refused.contains("mailbox"), "{refused}");
        let bid = acct(&st, "ben@anna.ca.example.org").unwrap()["id"].clone();
        sw.op(&json!({ "op": "account_destroy", "id": a["id"] })).await.unwrap();
        sw.op(&json!({ "op": "account_destroy", "id": bid })).await.unwrap();
        sw.op(&json!({ "op": "domain_remove", "id": d["id"] })).await.unwrap();
        sw.op(&json!({ "op": "domain_remove", "id": two["id"] })).await.unwrap();
        let st = sw.state().await.unwrap();
        assert!(acct(&st, "anna@anna.ca.example.org").is_none());
        assert!(!st["domains"].as_array().unwrap().iter().any(|d| d["name"] == "anna.ca.example.org"));
    }
}
