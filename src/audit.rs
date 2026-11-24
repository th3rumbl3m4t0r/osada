//! Logs (§16.3): the kinds, the switches the admin UI sets per log, and the
//! caution for tokens that outlive the authentication log.

use std::{collections::BTreeMap, time::Duration};

use serde::{Deserialize, Serialize};

const DAY: u64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogKind {
    /// Sign-ins, second factors, sessions, tokens, credential changes.
    Auth,
    /// Administrative changes.
    Admin,
    /// Reads and writes of files, the wiki and mail; shares and links used.
    Access,
    /// Cluster events: membership, handovers, deferred-write outcomes.
    Node,
    /// The program's own diagnostics.
    Ops,
    /// Every request a node answers (D-U31).
    Http,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    Off,
    Minimal,
    Standard,
    Detailed,
    Debug,
}

/// The switches of one log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switches {
    pub verbosity: Verbosity,
    pub retention: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchError {
    /// The log cannot be set below this.
    BelowFloor { kind: LogKind, floor: Verbosity },
    /// Retention is whole days, at least one.
    Retention,
}

impl LogKind {
    pub const ALL: [LogKind; 6] = [LogKind::Auth, LogKind::Admin, LogKind::Access, LogKind::Http, LogKind::Node, LogKind::Ops];

    /// The lowest verbosity the admin UI allows: `auth` and `admin` are
    /// never off.
    pub fn floor(self) -> Verbosity {
        match self {
            LogKind::Auth | LogKind::Admin => Verbosity::Minimal,
            _ => Verbosity::Off,
        }
    }

    /// The switches of a new cluster.
    pub fn defaults(self) -> Switches {
        let (verbosity, days) = match self {
            LogKind::Auth => (Verbosity::Standard, 365),
            LogKind::Admin => (Verbosity::Standard, 365),
            LogKind::Access => (Verbosity::Minimal, 90),
            LogKind::Node => (Verbosity::Standard, 90),
            LogKind::Ops => (Verbosity::Minimal, 14),
            LogKind::Http => (Verbosity::Standard, 30),
        };
        Switches { verbosity, retention: Duration::from_secs(days * DAY) }
    }
}

impl Switches {
    pub fn check(&self, kind: LogKind) -> Result<(), SwitchError> {
        if self.verbosity < kind.floor() {
            return Err(SwitchError::BelowFloor { kind, floor: kind.floor() });
        }
        let secs = self.retention.as_secs();
        if secs < DAY || secs % DAY != 0 || self.retention.subsec_nanos() != 0 {
            return Err(SwitchError::Retention);
        }
        Ok(())
    }

    /// Whether an event of `level` is written.
    pub fn logs(&self, level: Verbosity) -> bool {
        level != Verbosity::Off && self.verbosity >= level
    }
}

/// One log's switches as the cluster log keeps them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSetting {
    pub verbosity: Verbosity,
    pub retention_days: u32,
}

impl LogSetting {
    pub fn switches(&self) -> Switches {
        Switches { verbosity: self.verbosity, retention: Duration::from_secs(u64::from(self.retention_days) * DAY) }
    }
}

/// The cluster's settings the admin UI changes (kept in the cluster log).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub logs: BTreeMap<LogKind, LogSetting>,
    /// How long a sign-in lasts at most, all its use included.
    pub session_days: u32,
    /// Where the logs go out to as JSON (T3).
    #[serde(default)]
    pub ship: Ship,
    /// Other storage (T2), by name; changed on its own (set_storage), never
    /// by the settings page.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub storages: BTreeMap<String, crate::storage::Storage>,
    /// Rules over the logs (D-U32); changed on their own (set_rule).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<crate::rules::Rule>,
    /// Whether the rules a cluster starts with were added (once: removing them sticks).
    #[serde(default)]
    pub rules_seeded: bool,
    /// Addresses and subnets never banned, besides private ranges and the nodes' (D-U33).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub never_ban: Vec<String>,
    /// New mailboxes (D-U43); changed on their own (admin → mail).
    #[serde(default)]
    pub mail: MailSettings,
    /// The domain of every account's UPN, `<name>@<domain>` (Round 22): its sign-in name besides
    /// the bare name, the share window's and federation's, and with a mailbox an address of it too.
    /// Empty: the drop-in's public host (set once at its start).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub upn_domain: String,
    /// The backups (Round 14: live; Round 17: nightly too); changed on their own (admin → files).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backups: Vec<LiveBackup>,
}

/// A backup (admin → files → backups), of one of two kinds:
/// - `live` (Round 14): a copy of the files kept up to date on a target: of all files, or of one
///   node's (`source`: "all" or a node's id); `target`: "storage:<name>" or "node:<id>".
/// - `nightly` (Round 17): complete snapshots every night at `time` (local, `HH:MM`) on SFTP
///   storage (`target`: "storage:<name>"), of `what` ([`NIGHTLY_PARTS`]; empty: all of them),
///   from which the whole cluster can be rebuilt; earlier versions of files come from them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveBackup {
    pub name: String,
    #[serde(default = "live_kind")]
    pub kind: String,
    #[serde(default = "all_source")]
    pub source: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub time: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub what: Vec<String>,
}

fn live_kind() -> String {
    "live".into()
}

fn all_source() -> String {
    "all".into()
}

/// What a nightly backup can copy (Round 17), with what people read.
pub const NIGHTLY_PARTS: &[(&str, &str)] = &[
    ("files", "files (everyone's homes, the groups' folders, deleted users' files)"),
    ("wiki", "the wiki's pictures"),
    ("database", "database and settings (osada 0.1's database, its key, the unit, the program)"),
    ("nodes", "every node's data (the cluster's store: directory, sign-in, chat; keys, certificates, static sites)"),
    ("mail", "mail (Stalwart's data, settings and program, on the nodes it runs on)"),
];

impl LiveBackup {
    pub fn nightly(&self) -> bool {
        self.kind == "nightly"
    }

    /// A nightly backup's parts: those chosen, else all.
    pub fn parts(&self) -> Vec<String> {
        if self.what.is_empty() { NIGHTLY_PARTS.iter().map(|p| p.0.to_string()).collect() } else { self.what.clone() }
    }

    /// A nightly backup's time, `03:15` if none was given.
    pub fn at(&self) -> String {
        if self.time.is_empty() { "03:15".into() } else { self.time.clone() }
    }
}

/// How osada makes mailboxes (D-U43): the address of a new one (`{user}`: the account's name as a
/// DNS label, e.g. `{user}@{user}.ca.example.org` or `{user}@example.org`), whether a domain that
/// Stalwart lacks is made with it (its catch-all then the mailbox), its size (none: no limit), and
/// whether every new account gets one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailSettings {
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub make_domains: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<u64>,
    #[serde(default)]
    pub auto: bool,
}

/// A user name as a DNS label: lower case, letters, digits and `-` (as the old sync made them).
pub fn label_of(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(63).collect();
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "user".into() } else { out }
}

impl MailSettings {
    /// The address a new mailbox of `user` gets; none without a template.
    pub fn address_for(&self, user: &str) -> Option<String> {
        let t = self.template.trim();
        (!t.is_empty()).then(|| t.replace("{user}", &label_of(user)).to_lowercase())
    }

    pub fn check(&self) -> Result<(), String> {
        if self.template.trim().is_empty() {
            if self.auto {
                return Err("a mailbox for every new account needs the address template".into());
            }
            return Ok(());
        }
        if !self.template.contains("{user}") {
            return Err("the address template has {user} in it".into());
        }
        match self.address_for("anna-maria") {
            Some(a) if crate::directory::valid_address(&a) => Ok(()),
            _ => Err(format!("the address template {:?} does not make a mail address", self.template)),
        }
    }
}

/// Shipping the logs out (T3): every node POSTs its own new lines of
/// `logs` to `url` as a JSON array, with `Authorization: Bearer <secret>`.
/// The secret is kept in the cluster log and never shown back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ship {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub logs: Vec<LogKind>,
    /// From the page only: forget the secret (an empty one keeps it).
    #[serde(default, skip_serializing)]
    pub clear_secret: bool,
}

/// The logs that can be shipped: those the nodes write.
pub const SHIPPED: [LogKind; 2] = [LogKind::Auth, LogKind::Admin];

impl Ship {
    pub fn check(&self) -> Result<(), String> {
        if self.url.len() > 2048 || self.url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("log shipping: the URL is too long or has spaces".into());
        }
        if self.secret.len() > 4096 || self.secret.chars().any(|c| c.is_control()) {
            return Err("log shipping: the secret is too long or has control characters".into());
        }
        if let Some(k) = self.logs.iter().find(|k| !SHIPPED.contains(k)) {
            return Err(format!("log shipping: the {k:?} log is not shipped").to_lowercase());
        }
        if !self.enabled {
            return Ok(());
        }
        if self.logs.is_empty() {
            return Err("log shipping: choose the logs to ship".into());
        }
        let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|p| self.url.strip_prefix(p).is_some_and(|r| r.is_empty() || r.starts_with(':') || r.starts_with('/')));
        let host = self.url.strip_prefix("https://").map(|r| r.split(['/', '?', '#']).next().unwrap_or(""));
        if !(local || host.is_some_and(|h| !h.is_empty() && !h.contains('@'))) {
            return Err("log shipping: the URL is https://host/... (http only to this machine)".into());
        }
        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        let logs = LogKind::ALL
            .into_iter()
            .map(|k| {
                let s = k.defaults();
                (k, LogSetting { verbosity: s.verbosity, retention_days: (s.retention.as_secs() / DAY) as u32 })
            })
            .collect();
        Settings { logs, session_days: 30, ship: Ship::default(), storages: BTreeMap::new(), rules: Vec::new(), rules_seeded: false, never_ban: Vec::new(), mail: MailSettings::default(), upn_domain: String::new(), backups: Vec::new() }
    }
}

impl Settings {
    pub fn check(&self) -> Result<(), String> {
        self.mail.check()?;
        let mut names = std::collections::BTreeSet::new();
        for b in &self.backups {
            if !crate::storage::valid_name(&b.name) || !names.insert(&b.name) {
                return Err(format!("a backup's name: {:?} (a-z, 0-9, -; once)", b.name));
            }
            match b.kind.as_str() {
                "live" => {
                    if b.source.is_empty() || !(b.target.starts_with("storage:") || b.target.starts_with("node:")) {
                        return Err(format!("live backup {}: its source and target", b.name));
                    }
                }
                "nightly" => {
                    if !b.target.starts_with("storage:") {
                        return Err(format!("nightly backup {}: it goes to other storage (SFTP)", b.name));
                    }
                    if self.backups.iter().any(|o| o.nightly() && o.name != b.name && o.target == b.target) {
                        return Err(format!("nightly backup {}: another nightly backup goes to {} already (add other storage with another folder)", b.name, &b.target[8..]));
                    }
                    let t = b.at();
                    let ok = t.len() == 5 && t.as_bytes()[2] == b':' && t[..2].parse::<u32>().is_ok_and(|h| h < 24) && t[3..].parse::<u32>().is_ok_and(|m| m < 60);
                    if !ok {
                        return Err(format!("nightly backup {}: its time is HH:MM, not {t:?}", b.name));
                    }
                    if let Some(w) = b.what.iter().find(|w| !NIGHTLY_PARTS.iter().any(|p| p.0 == w.as_str())) {
                        return Err(format!("nightly backup {}: no part {w:?}", b.name));
                    }
                }
                k => return Err(format!("backup {}: no kind {k:?} (live or nightly)", b.name)),
            }
        }
        for k in LogKind::ALL {
            // a kind newer than the cluster's settings has its defaults (http, D-U31)
            let Some(s) = self.logs.get(&k) else { continue };
            s.switches().check(k).map_err(|e| format!("{k:?} log: {e:?}"))?;
        }
        if !(1..=3650).contains(&self.session_days) {
            return Err("a session lasts 1 to 3650 days".into());
        }
        crate::storage::check_all(&self.storages)?;
        let mut names = std::collections::BTreeSet::new();
        for r in &self.rules {
            r.check(crate::logstore::schemas())?;
            if !names.insert(&r.name) {
                return Err(format!("two rules are called {}", r.name));
            }
        }
        if self.rules.len() > 100 {
            return Err("at most 100 rules".into());
        }
        crate::rules::check_never(&self.never_ban)?;
        self.ship.check()
    }

    pub fn switches(&self, kind: LogKind) -> Switches {
        self.logs.get(&kind).map_or_else(|| kind.defaults(), |s| s.switches())
    }

    pub fn session(&self) -> Duration {
        Duration::from_secs(u64::from(self.session_days) * DAY)
    }

    /// Whether sessions outlive the authentication log (§16.3).
    pub fn sessions_outlive_logs(&self) -> bool {
        outlives_logs(Some(self.session()), self.switches(LogKind::Auth).retention)
    }
}

/// Whether something that grants access for `lifetime` (None: no expiry)
/// must caution its user: it lives longer than 90 % of the `auth` log's
/// retention, so it can outlive the record of the sign-in it came from. For
/// a session, the lifetime counts all its refreshes.
pub fn outlives_logs(lifetime: Option<Duration>, auth_retention: Duration) -> bool {
    match lifetime {
        None => true,
        Some(l) => l.as_millis() * 10 > auth_retention.as_millis() * 9,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(n: u64) -> Duration {
        Duration::from_secs(n * DAY)
    }

    #[test]
    fn caution_past_ninety_percent() {
        let r = days(100);
        assert!(!outlives_logs(Some(days(30)), r));
        assert!(!outlives_logs(Some(days(90)), r));
        assert!(outlives_logs(Some(days(90) + Duration::from_secs(1)), r));
        assert!(outlives_logs(Some(days(400)), r));
        assert!(outlives_logs(None, r));
        // osada 0.1's refresh tokens (30 days) under the default retention
        assert!(!outlives_logs(Some(days(30)), LogKind::Auth.defaults().retention));
    }

    #[test]
    fn switches_have_floors() {
        let off = Switches { verbosity: Verbosity::Off, retention: days(30) };
        assert_eq!(off.check(LogKind::Auth), Err(SwitchError::BelowFloor { kind: LogKind::Auth, floor: Verbosity::Minimal }));
        assert!(off.check(LogKind::Admin).is_err());
        assert_eq!(off.check(LogKind::Access), Ok(()));
        let bad = Switches { verbosity: Verbosity::Standard, retention: Duration::from_secs(DAY + 5) };
        assert_eq!(bad.check(LogKind::Ops), Err(SwitchError::Retention));
        let none = Switches { verbosity: Verbosity::Standard, retention: Duration::ZERO };
        assert_eq!(none.check(LogKind::Ops), Err(SwitchError::Retention));
        for kind in LogKind::ALL {
            kind.defaults().check(kind).unwrap();
        }
    }

    #[test]
    fn verbosity_levels_include_the_lower() {
        let s = Switches { verbosity: Verbosity::Standard, retention: days(1) };
        assert!(s.logs(Verbosity::Minimal) && s.logs(Verbosity::Standard));
        assert!(!s.logs(Verbosity::Detailed) && !s.logs(Verbosity::Off));
        let off = Switches { verbosity: Verbosity::Off, retention: days(1) };
        assert!(!off.logs(Verbosity::Minimal) && !off.logs(Verbosity::Off));
    }

    #[test]
    fn settings_default_and_check() {
        let s = Settings::default();
        s.check().unwrap();
        assert!(!s.sessions_outlive_logs());
        let mut short = s.clone();
        short.logs.get_mut(&LogKind::Auth).unwrap().retention_days = 30;
        assert!(short.sessions_outlive_logs());
        short.logs.get_mut(&LogKind::Auth).unwrap().verbosity = Verbosity::Off;
        assert!(short.check().is_err());
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"auth\":{\"verbosity\":\"standard\",\"retention_days\":365}"), "{json}");
    }
}
