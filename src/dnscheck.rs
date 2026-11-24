//! The DNS checker (T11): every record the deployment needs, what public
//! DNS says (DNS over HTTPS, Cloudflare's resolver: the home router's DNS
//! answers for itself), and zone files: all of them, and the missing ones.
//!
//! What is wanted: the names of the sites, certificates and proxies, at the
//! public addresses of the edges that answer them (node control's `addrs`);
//! a site behind Cloudflare at Cloudflare's addresses (any); and every record
//! Stalwart says its domains need (its `dnsZoneFile`, through the agent on
//! the mail node).

use std::collections::BTreeMap;

use futures_util::StreamExt;
use serde::Serialize;

/// A record that should be there.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Want {
    pub name: String,
    pub rtype: String,
    /// The value as a zone file has it (`10 mail.example.org.`, `"v=spf1 mx -all"`).
    pub value: String,
    /// What wants it.
    pub why: String,
    /// Any address of these networks will do (a site behind Cloudflare).
    #[serde(skip)]
    pub any_of: Option<Vec<crate::osada1::edge::Cidr>>,
}

/// What public DNS says of a wanted record.
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    #[serde(flatten)]
    pub want: Want,
    pub found: Vec<String>,
    /// ok, missing, differs, error
    pub state: String,
}

/// The records of a zone file (`name. IN TYPE value`, values over lines in
/// parentheses put together).
pub fn parse_zone(text: &str, why: &str) -> Vec<Want> {
    let mut out = Vec::new();
    let mut pending = String::new();
    let mut depth = 0i32;
    for line in text.lines() {
        // a comment starts at a ';' outside quotes (TXT values have ';' inside)
        let mut q = false;
        let cut = line.char_indices().find(|&(_, c)| {
            if c == '"' {
                q = !q;
            }
            c == ';' && !q
        });
        let l = cut.map_or(line, |(i, _)| &line[..i]);
        if l.trim().is_empty() && depth == 0 {
            continue;
        }
        pending.push(' ');
        pending.push_str(l.trim());
        depth += l.matches('(').count() as i32 - l.matches(')').count() as i32;
        if depth > 0 {
            continue;
        }
        let rec = pending.replace(['(', ')'], " ");
        pending.clear();
        depth = 0;
        let mut it = rec.split_whitespace();
        let Some(name) = it.next() else { continue };
        let mut rest: Vec<&str> = it.collect();
        // an optional TTL and class
        if rest.first().is_some_and(|t| t.parse::<u32>().is_ok()) {
            rest.remove(0);
        }
        if rest.first().is_some_and(|c| c.eq_ignore_ascii_case("IN")) {
            rest.remove(0);
        }
        let Some((rtype, value)) = rest.split_first() else { continue };
        out.push(Want { name: name.trim_end_matches('.').to_ascii_lowercase(), rtype: rtype.to_ascii_uppercase(), value: value.join(" "), why: why.into(), any_of: None });
    }
    out
}

/// A value as compared: TXT strings put together, names without the final
/// dot, lower case but for TXT.
pub fn norm(rtype: &str, v: &str) -> String {
    let v = v.trim();
    match rtype {
        "TXT" => {
            if v.contains('"') {
                v.split('"').enumerate().filter(|(i, _)| i % 2 == 1).map(|(_, s)| s).collect::<String>()
            } else {
                v.to_string()
            }
        }
        "A" | "AAAA" => v.parse::<std::net::IpAddr>().map(|a| a.to_string()).unwrap_or_else(|_| v.to_ascii_lowercase()),
        _ => v.split_whitespace().map(|p| p.trim_end_matches('.').to_ascii_lowercase()).collect::<Vec<_>>().join(" "),
    }
}

const TYPES: &[(&str, u16)] = &[("A", 1), ("NS", 2), ("CNAME", 5), ("MX", 15), ("TXT", 16), ("AAAA", 28), ("SRV", 33), ("CAA", 257)];

/// The answers of type `rtype` for `name`, by DNS over HTTPS.
pub async fn lookup(http: &reqwest::Client, name: &str, rtype: &str) -> Result<Vec<String>, String> {
    let code = TYPES.iter().find(|(t, _)| *t == rtype).map(|(_, c)| *c).ok_or_else(|| format!("type {rtype}"))?;
    let r = http
        .get("https://cloudflare-dns.com/dns-query")
        .query(&[("name", name), ("type", rtype)])
        .header("accept", "application/dns-json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_slice(&r.bytes().await.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(v["Answer"].as_array().map(|a| a.iter().filter(|x| x["type"].as_u64() == Some(code as u64)).filter_map(|x| x["data"].as_str().map(str::to_string)).collect()).unwrap_or_default())
}

/// Every wanted record looked up (16 at a time).
pub async fn check(wants: Vec<Want>) -> Vec<Row> {
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().expect("an http client");
    // one lookup per name and type
    let mut keys: Vec<(String, String)> = wants.iter().map(|w| (w.name.clone(), w.rtype.clone())).collect();
    keys.sort();
    keys.dedup();
    let found: BTreeMap<(String, String), Result<Vec<String>, String>> = futures_util::stream::iter(keys)
        .map(|k| {
            let http = http.clone();
            async move {
                let r = lookup(&http, &k.0, &k.1).await;
                (k, r)
            }
        })
        .buffer_unordered(16)
        .collect()
        .await;
    // every value wanted for a name and type (by any row)
    let mut wanted: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for w in &wants {
        wanted.entry((w.name.clone(), w.rtype.clone())).or_default().push(norm(&w.rtype, &w.value));
    }
    wants
        .into_iter()
        .map(|w| {
            let key = (w.name.clone(), w.rtype.clone());
            let (found, state) = match found.get(&key) {
                Some(Ok(list)) => (list.clone(), state(&w, list, wanted.get(&key).map(Vec::as_slice).unwrap_or(&[])).to_string()),
                Some(Err(e)) => (vec![e.clone()], "error".into()),
                None => (vec![], "error".into()),
            };
            Row { want: w, found, state }
        })
        .collect()
}

/// The state of a wanted record, given what public DNS has for its name and
/// type and every value wanted there (by this row and others): found → ok;
/// nothing found, or only what other rows want (two edges' A records, one
/// published) → missing; something nobody wants → differs.
pub fn state(w: &Want, found: &[String], wanted: &[String]) -> &'static str {
    if let Some(nets) = &w.any_of {
        let ips: Vec<std::net::IpAddr> = found.iter().filter_map(|a| a.parse().ok()).collect();
        return if ips.is_empty() {
            "missing"
        } else if ips.iter().all(|ip| nets.iter().any(|n| n.contains(*ip))) {
            "ok"
        } else {
            "differs"
        };
    }
    let mine = norm(&w.rtype, &w.value);
    if found.iter().any(|f| norm(&w.rtype, f) == mine) {
        "ok"
    } else if found.iter().all(|f| wanted.contains(&norm(&w.rtype, f))) {
        "missing"
    } else {
        "differs"
    }
}

/// A zone file of these records (absolute names, TTL 300).
pub fn zone(rows: &[&Want]) -> String {
    let mut out = String::from("; osada: the records this deployment needs (T11)\n");
    let mut last = String::new();
    for w in rows {
        if w.why != last {
            out.push_str(&format!("\n; {}\n", w.why));
            last = w.why.clone();
        }
        if w.any_of.is_some() {
            out.push_str(&format!("; {}. {} — proxied by Cloudflare (its orange cloud), at any address of Cloudflare's\n", w.name, w.rtype));
            continue;
        }
        out.push_str(&format!("{}. 300 IN {} {}\n", w.name, w.rtype, w.value));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_files() {
        let z = "202604r._domainkey.e.eu. IN TXT (\n    \"v=DKIM1; k=rsa; p=MIIB\"\n    \"AQAB\"\n)\ne.eu. IN TXT \"v=spf1 mx -all\"\ne.eu. IN MX 10 mail.example.org.\n_imaps._tcp.e.eu. IN SRV 0 1 993 mail.example.org.\nmta-sts.e.eu. 300 IN CNAME mail.example.org.\n";
        let w = parse_zone(z, "Stalwart: e.eu");
        assert_eq!(w.len(), 5);
        assert_eq!((w[0].name.as_str(), w[0].rtype.as_str()), ("202604r._domainkey.e.eu", "TXT"));
        assert_eq!(norm("TXT", &w[0].value), "v=DKIM1; k=rsa; p=MIIBAQAB");
        assert_eq!(norm("MX", &w[2].value), "10 mail.example.org");
        assert_eq!(norm("MX", "10 Mail.EXample.org."), "10 mail.example.org");
        assert_eq!(w[4].rtype, "CNAME");
        assert_eq!(norm("AAAA", "2001:2:0:0992:0:0:0:1"), "2001:2:0:992::1");
        let zf = zone(&w.iter().collect::<Vec<_>>());
        // two edges' A records, one published: the other is missing, not differing
        let a = |v: &str| Want { name: "x.eu".into(), rtype: "A".into(), value: v.into(), why: "site x".into(), any_of: None };
        let wanted = vec!["1.2.3.4".to_string(), "5.6.7.8".to_string()];
        assert_eq!(state(&a("1.2.3.4"), &["1.2.3.4".into()], &wanted), "ok");
        assert_eq!(state(&a("5.6.7.8"), &["1.2.3.4".into()], &wanted), "missing");
        assert_eq!(state(&a("5.6.7.8"), &[], &wanted), "missing");
        assert_eq!(state(&a("5.6.7.8"), &["1.2.3.4".into(), "9.9.9.9".into()], &wanted), "differs");
        let mx = Want { name: "e.eu".into(), rtype: "MX".into(), value: "10 mail.example.org.".into(), why: "".into(), any_of: None };
        assert_eq!(state(&mx, &["10 Mail.example.org.".into()], &["10 mail.example.org".into()]), "ok");
        assert!(zf.contains("e.eu. 300 IN MX 10 mail.example.org.") && zf.contains("; Stalwart: e.eu"));
    }
}
