//! Rules over the cluster's logs (D-U32) and bans (D-U33), as sift has them: a query that runs by
//! itself on the logs authority. `event`: each new matching line (by arrival); `aggregate`: each
//! result row over the last `window`, every `every`. `key` makes firings one alert, `suppress`
//! holds repeats. A firing is a row in `alerts`; a rule with `ban` also bans the address in the
//! row's `ban.field`, for `ban.time`, doubled (`factor`) for each earlier ban of it, up to `ban.max`.
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Event,
    #[default]
    Aggregate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BanAction {
    /// The result field with the address.
    pub field: String,
    /// The first ban, as "1h".
    pub time: String,
    /// The longest a ban grows to.
    pub max: String,
    /// Each earlier ban multiplies the time by this.
    #[serde(default = "two")]
    pub factor: u32,
}

fn two() -> u32 {
    2
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: Mode,
    pub query: String,
    /// "10m"
    pub window: String,
    pub every: String,
    #[serde(default)]
    pub key: Vec<String>,
    pub suppress: String,
    #[serde(default = "low")]
    pub severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ban: Option<BanAction>,
}

fn low() -> String {
    "low".into()
}

pub const SEVERITIES: [&str; 4] = ["low", "medium", "high", "critical"];

/// "15s", "10m", "1h", "1d", "1w" → seconds.
pub fn secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (n, u) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = n.parse().ok()?;
    let k = match u {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    n.checked_mul(k).filter(|&x| x > 0)
}

pub fn valid_name(n: &str) -> bool {
    (1..=48).contains(&n.len()) && n.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl Rule {
    /// What a rule must be; `schemas` to compile its query against.
    pub fn check(&self, schemas: &crate::ql::Schemas) -> Result<(), String> {
        let n = &self.name;
        if !valid_name(n) {
            return Err(format!("rule {n:?}: a name of small letters, digits, - and _ (up to 48)"));
        }
        let d = |what: &str, v: &str, min: u64| -> Result<u64, String> {
            match secs(v) {
                Some(x) if x >= min => Ok(x),
                Some(_) => Err(format!("rule {n}: {what} at least {min} s")),
                None => Err(format!("rule {n}: {what} like 15s, 10m, 1h, 1d")),
            }
        };
        let window = d("window", &self.window, 15)?;
        d("every", &self.every, 15)?;
        d("suppress", &self.suppress, 1)?;
        if window > 400 * 86_400 {
            return Err(format!("rule {n}: a window of at most 400 days"));
        }
        if !SEVERITIES.contains(&self.severity.as_str()) {
            return Err(format!("rule {n}: severity is one of {}", SEVERITIES.join(", ")));
        }
        if self.description.len() > 500 || self.query.len() > 8000 {
            return Err(format!("rule {n}: the description or the query is too long"));
        }
        let range = crate::ql::Range { from: 0, to: 1, now: 1, ingested: if self.mode == Mode::Event { Some((0, 1)) } else { None } };
        let c = crate::ql::compile(&self.query, schemas, range, 100).map_err(|e| format!("rule {n}: {e}"))?;
        for k in &self.key {
            if !c.columns.contains(k) {
                return Err(format!("rule {n}: the key {k:?} is not in its result ({})", c.columns.join(", ")));
            }
        }
        if let Some(b) = &self.ban {
            if !c.columns.contains(&b.field) {
                return Err(format!("rule {n}: the address field {:?} is not in its result ({})", b.field, c.columns.join(", ")));
            }
            let t = d("ban time", &b.time, 60)?;
            let m = d("longest ban", &b.max, 60)?;
            if m < t {
                return Err(format!("rule {n}: the longest ban is shorter than the first"));
            }
            if !(1..=10).contains(&b.factor) {
                return Err(format!("rule {n}: the growth factor is 1 to 10"));
            }
        }
        Ok(())
    }
}

/// How long the `times`-th ban of an address lasts (seconds; 1 for the first).
pub fn ban_secs(b: &BanAction, times: u32) -> u64 {
    let (t, m) = (secs(&b.time).unwrap_or(3600), secs(&b.max).unwrap_or(86_400));
    let mut x = t;
    for _ in 1..times.min(64) {
        x = x.saturating_mul(u64::from(b.factor.max(1)));
        if x >= m {
            return m;
        }
    }
    x.min(m)
}

/// Addresses never banned: loopback, private and link-local ranges, Cloudflare's, the nodes',
/// and the settings' own list (addresses or subnets).
pub fn never_banned(ip: IpAddr, list: &[String], nodes: &[IpAddr]) -> bool {
    let ip = ip.to_canonical();
    let private = match ip {
        IpAddr::V4(v) => v.is_loopback() || v.is_private() || v.is_link_local() || v.is_unspecified() || v.is_broadcast(),
        IpAddr::V6(v) => v.is_loopback() || v.is_unspecified() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
    };
    private || crate::accesslog::is_cloudflare(ip) || nodes.iter().any(|n| n.to_canonical() == ip) || list.iter().any(|n| crate::ql::ip_in(&ip.to_string(), n))
}

/// Checks the settings' never-ban list.
pub fn check_never(list: &[String]) -> Result<(), String> {
    if list.len() > 200 {
        return Err("at most 200 addresses that are never banned".into());
    }
    for n in list {
        let (a, p) = n.split_once('/').unwrap_or((n, ""));
        let ok = a.trim().parse::<IpAddr>().is_ok() && (p.is_empty() || p.parse::<u8>().is_ok());
        if !ok {
            return Err(format!("never banned: {n:?} is not an address or a subnet"));
        }
    }
    Ok(())
}

/// The rules a cluster starts with: edge1's fail2ban jails (osada-login, osada-scan), now over
/// every node's lines.
pub fn builtin() -> Vec<Rule> {
    // fail2ban's osada-scan filter; a path a scanner asks for, any status. Its lookahead (not
    // under /api /s /static /files /pictures) is the !~= below.
    let ext = r"(?i)\\.(php[0-9]?|phtml|phar|asp|aspx|ashx|asmx|axd|jsp|jspa|jspx|jsf|do|action|cgi|pl|cfm|env|ini|bak|old|sql|swp|htaccess|htpasswd|ds_store)(/|$)";
    let root = r"(?i)^/(wp-|wordpress|xmlrpc|wlwmanifest|phpmyadmin|pma/|myadmin|adminer|cgi-bin|vpns?/|logon/logonpoint|nitro/v1|citrix|netscaler|\\+csco[et]\\+|remote/(fgt_lang|login|logincheck)|dana-na/|global-protect/|ssl-?vpn|mifs/|console/|wls-wsat|_async/|bea_wls|uddiexplorer|weblogic|owa/|ecp/|autodiscover|ews/|mapi/|rpc/rpcproxy|manager/html|host-manager|jmx-console|web-console|invoker/|axis2|solr/|jenkins|hudson|struts|actuator|druid|nacos|geoserver|zabbix|telescope|_ignition|vendor/phpunit|containers/json|\\.aws/|\\.ssh/|\\.git/|\\.svn/|\\.env|\\.vscode|server-status|api/jsonws|seeyon|weaver|rdweb|reportserver|boaform|hnap1|gponform|goform/|tmui/|mgmt/tm|jolokia|hsqldb|sonicui|cf_scripts|etc/passwd|win\\.ini)";
    vec![
        Rule {
            name: "osada-login".into(),
            description: "5 failed sign-ins from one address in 10 minutes (fail2ban's osada-login): banned 1 h, doubled for each earlier ban, up to 7 d.".into(),
            enabled: true,
            mode: Mode::Aggregate,
            query: "dataset = auth | filter event = \"sign_in_failed\" and addr != \"\" | stats count() as n, values(name) as names by addr | filter n >= 5".into(),
            window: "10m".into(),
            every: "30s".into(),
            key: vec!["addr".into()],
            suppress: "10m".into(),
            severity: "medium".into(),
            ban: Some(BanAction { field: "addr".into(), time: "1h".into(), max: "7d".into(), factor: 2 }),
        },
        Rule {
            name: "osada-scan".into(),
            description: "A request for a path scanners look for (fail2ban's osada-scan): banned 1 d, doubled for each earlier ban, up to 30 d.".into(),
            enabled: true,
            mode: Mode::Event,
            query: format!(
                "dataset = http | filter front != \"passthrough\" and client != \"\" and ((path ~= \"{ext}\" and path !~= \"^/(api|s|static|files|pictures)/\") or path ~= \"{root}\") | fields client, host, path, status"
            ),
            window: "1h".into(),
            every: "15s".into(),
            key: vec!["client".into()],
            suppress: "1d".into(),
            severity: "low".into(),
            ban: Some(BanAction { field: "client".into(), time: "1d".into(), max: "30d".into(), factor: 2 }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_growth() {
        assert_eq!(secs("15s"), Some(15));
        assert_eq!(secs("10m"), Some(600));
        assert_eq!(secs("7d"), Some(604_800));
        assert!(secs("0m").is_none() && secs("m").is_none() && secs("5y").is_none());
        let b = BanAction { field: "a".into(), time: "1h".into(), max: "7d".into(), factor: 2 };
        assert_eq!(ban_secs(&b, 1), 3600);
        assert_eq!(ban_secs(&b, 2), 7200);
        assert_eq!(ban_secs(&b, 4), 8 * 3600);
        assert_eq!(ban_secs(&b, 30), 604_800);
    }

    #[test]
    fn builtin_rules_check_and_match() {
        let schemas = crate::logstore::schemas();
        for r in builtin() {
            r.check(schemas).unwrap_or_else(|e| panic!("{e}"));
        }
        // the scan rule's patterns, run against lines
        let dir = std::env::temp_dir().join(format!("osada-rules-{}", std::process::id()));
        let s = crate::logstore::Store::open(&dir).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let paths = [
            ("/wp-login.php", true),
            ("/.env", true),
            ("/cgi-bin/luci", true),
            ("/+CSCOE+/logon.html", true),
            ("/files/home/x.php", false),
            ("/static/app.php", false),
            ("/api/x.php", false),
            ("/index.html", false),
            ("/", false),
            ("/old/report.BAK", true),
            ("/.git/config", true),
        ];
        let lines: Vec<crate::logstore::Line> = paths
            .iter()
            .enumerate()
            .map(|(i, (p, _))| crate::logstore::Line { log: "http".into(), seq: i as u64 + 1, time: now - 1000, event: serde_json::json!({ "client": format!("203.0.113.{i}"), "path": p, "front": "tls", "status": 404 }) })
            .collect();
        s.ingest("n", "n", &lines).unwrap();
        let scan = builtin().into_iter().find(|r| r.name == "osada-scan").unwrap();
        let v = s.query(&scan.query, now as i64 - 60_000, now as i64 + 1000, 100).unwrap();
        let mut hit: Vec<String> = v["rows"].as_array().unwrap().iter().map(|r| r[2].as_str().unwrap().to_string()).collect();
        hit.sort();
        let mut want: Vec<String> = paths.iter().filter(|p| p.1).map(|p| p.0.to_string()).collect();
        want.sort();
        assert_eq!(hit, want);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checks() {
        let s = crate::logstore::schemas();
        let mut r = builtin().remove(0);
        r.key = vec!["nothere".into()];
        assert!(r.check(s).unwrap_err().contains("key"));
        let mut r = builtin().remove(0);
        r.ban.as_mut().unwrap().field = "n".into();
        r.ban.as_mut().unwrap().max = "1m".into();
        assert!(r.check(s).unwrap_err().contains("ban"));
        let mut r = builtin().remove(0);
        r.every = "5s".into();
        assert!(r.check(s).is_err());
        assert!(check_never(&["198.18.0.182".into(), "2001:2:0:6bf8::/64".into()]).is_ok());
        assert!(check_never(&["nope".into()]).is_err());
    }

    #[test]
    fn never_banned_addresses() {
        let list = vec!["198.18.0.182".to_string(), "198.51.100.0/24".to_string()];
        let nodes = vec!["198.18.0.42".parse().unwrap()];
        for ip in ["127.0.0.1", "192.168.1.20", "10.0.0.3", "::1", "fd00::1", "198.18.0.182", "198.51.100.7", "198.18.0.42", "104.16.0.1"] {
            assert!(never_banned(ip.parse().unwrap(), &list, &nodes), "{ip}");
        }
        for ip in ["203.0.113.5", "2001:db8::7", "8.8.8.8"] {
            assert!(!never_banned(ip.parse().unwrap(), &list, &nodes), "{ip}");
        }
    }
}
