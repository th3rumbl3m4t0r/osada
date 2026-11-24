//! Websites (D-U21): the module kind `web`, a site per instance, kept in
//! node control with the instance. The instance's node is where the site
//! runs (its folder, its container: the decoupler); `exposed_on` are the edge
//! nodes that answer its names, and pass requests over the node link when it
//! runs elsewhere.
//!
//! - content: static files from the site's own folder `<data>/web/<site>/`
//!   (D-U22: always the webroot; it may be a link to a osada folder), a
//!   container on the node (D-U23: podman, `osada-web-<site>`), or an
//!   upstream the node reaches;
//! - routes: path prefixes that go to an upstream instead (pplc.example.net's
//!   `/api/pi/`);
//! - front: `tls` (a certificate for its names, D-U24) or `cloudflare`
//!   (plain http, Cloudflare's addresses only, the visitor from
//!   CF-Connecting-IP).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::placement::{Module, ModuleKind, NodeId, OtherKind, Placement, Role, valid_host};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    /// Its host names, lower case.
    pub names: Vec<String>,
    /// The edge nodes that answer them (none: the site is off).
    #[serde(default)]
    pub exposed_on: Vec<NodeId>,
    /// What answers every path no route takes.
    #[serde(default)]
    pub content: Content,
    /// Path prefixes that go to an upstream, the longest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,
    #[serde(default)]
    pub front: Front,
    /// The OU it belongs to (D-U28): who manages it and where its files
    /// belong follow it; the root when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ou: Option<crate::placement::OuId>,
    /// Also at `/web/<name>/` on osada's own names (Round 18). Its pages there are osada's origin:
    /// they could act as whoever is signed in, so it is for sites the cluster's administrators
    /// trust (infrastructure rights); osada's cookies are not passed to it, nor cookies it sets.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub under_web: bool,
    /// Round 18's "signed-in only": since Round 20 every site at /web/ is (see `open`); read, never
    /// written again.
    #[serde(default, skip_serializing)]
    pub signed_in: bool,
    /// At /web/<name>/ for anyone, signed in or not (Round 20). Not set (the default): osada checks
    /// the session on every request first (others are sent to sign in, a page, or refused,
    /// anything else), its answers are revalidated through osada each time (private, no-cache),
    /// and no service worker is let in: only osada itself faces the internet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
    /// Headers every answer of the site carries, e.g. Cross-Origin-Opener-Policy and
    /// Cross-Origin-Embedder-Policy for a WebAssembly program with threads.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// An office suite in the browser (Round 18): osada's file view offers "open in office" for
    /// documents, spreadsheets and presentations, with this site's page at `/web/<name>/`
    /// (`?loc=&path=`). One site at most; it must be under /web/.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub office: bool,
}

impl Site {
    /// Whether changing it needs infrastructure rights (D-U28): a container,
    /// an upstream or routes reach the nodes and what is behind them.
    pub fn infra(&self) -> bool {
        !matches!(self.content, Content::Static { .. }) || !self.routes.is_empty() || self.under_web
    }
}

/// Headers a site may not set: the transfer's own, and cookies (D-U21, Round 18).
const NOT_SITE_HEADERS: &[&str] = &[
    "connection", "keep-alive", "transfer-encoding", "content-length", "content-encoding", "content-range", "content-type", "set-cookie",
    "location", "etag", "last-modified", "upgrade", "te", "trailer", "vary", "accept-ranges", "date", "server",
];

/// A header a site may set: a token for its name, one line of visible ASCII for its value.
pub fn valid_header(name: &str, value: &str) -> Result<(), String> {
    let n = name.to_ascii_lowercase();
    if n.is_empty() || n.len() > 64 || !n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(format!("{name:?} is not a header's name"));
    }
    if NOT_SITE_HEADERS.contains(&n.as_str()) || n.starts_with("x-shelf-") || n.starts_with("cf-") {
        return Err(format!("{name} is not one a site sets"));
    }
    if value.is_empty() || value.len() > 1024 || !value.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(format!("{name}: its value is one line of plain text"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Content {
    /// Files from the site's folder.
    Static {
        /// A path with no file gets `index.html` (single-page apps).
        #[serde(default)]
        spa: bool,
        /// The webroot is a link to this osada folder (D-U22).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link: Option<Link>,
        /// The page a path with no file gets, with 404 (`404.html` when not set).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_found: Option<String>,
    },
    /// A container on the site's node.
    Container(Container),
    /// A service the site's node reaches: `http(s)://host[:port][/path]`.
    Upstream { url: String },
    /// A job (Round 18b): not served; a container run on a schedule.
    Job(Job),
}

/// A job (Round 18b): a container its node's root agent runs on a schedule, as the account of
/// the node's data folder, in the job's own folder (`<data>/web/<name>/`, at /job, where it
/// starts) with /out for what it makes. What it may write elsewhere it names: after a run that
/// ends well, each such file left in /out is put in a site's folder; nothing else is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// `registry/name:tag`, as podman pulls it.
    pub image: String,
    /// What it runs (one argument each), e.g. `python3`, `morning.py`, `/out/morning.html`;
    /// none: the image's own command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// When: cron's five fields (minute hour day month weekday), the node's time.
    pub schedule: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The files it may write in sites' folders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<JobWrite>,
    /// The longest a run may take, in seconds (none: 10 minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
    /// Names it finds at an address of its own (podman's --add-host): a service whose public
    /// name leads home, reached from home at its address there.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hosts: BTreeMap<String, String>,
    /// The node its outgoing connections exit by (Round 19, egress.rs); none: its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<NodeId>,
}

/// A file a job may write: `file` it leaves in /out goes to `path` in `site`'s folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobWrite {
    pub file: String,
    pub site: String,
    pub path: String,
}

impl Job {
    pub const DEFAULT_TIMEOUT: u32 = 600;

    pub fn timeout(&self) -> u32 {
        self.timeout.unwrap_or(Self::DEFAULT_TIMEOUT)
    }
}

/// A path in a site's folder a job may write: from /, a file, nothing hidden, no `..`.
pub fn valid_write_path(p: &str) -> bool {
    p.starts_with('/') && !p.ends_with('/') && p.len() <= 512 && p[1..].split('/').all(|c| !c.is_empty() && !c.starts_with('.') && !c.contains(['\\', '\0']) && !c.chars().any(char::is_control))
}

/// The jobs that write into `site`'s folder: (job, the path in the site, its schedule).
pub fn writers<'a>(p: &'a Placement, site: &str) -> Vec<(&'a String, &'a str, &'a str)> {
    sites(p)
        .filter_map(|(n, _, s)| match &s.content {
            Content::Job(j) => Some((n, j)),
            _ => None,
        })
        .flat_map(|(n, j)| j.writes.iter().filter(|w| w.site == site).map(move |w| (n, w.path.as_str(), j.schedule.as_str())))
        .collect()
}

impl Default for Content {
    fn default() -> Self {
        Content::Static { spa: false, link: None, not_found: None }
    }
}

/// A osada folder: a user's (name or id), a group's (D-U29: name or id) or a public link's
/// (Round 18: its token, the part after /s/), and its path there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    #[serde(default)]
    pub user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<String>,
    pub path: String,
}

impl Link {
    /// What is wrong with it, if anything: exactly one of a user, a group, a link; a path from /.
    pub fn check(&self) -> Option<String> {
        let group = self.group.as_deref().is_some_and(|g| !g.is_empty());
        let share = self.share.as_deref().is_some_and(|t| !t.is_empty());
        if [!self.user.is_empty(), group, share].iter().filter(|x| **x).count() != 1 {
            return Some("a osada folder is a user's, a group's or a public link's (one of them)".into());
        }
        if let Some(t) = self.share.as_deref().filter(|t| !t.is_empty()) {
            if t.len() > 64 || !t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                return Some(format!("{t:?} is not a link's token (the part after /s/)"));
            }
        }
        if !self.path.starts_with('/') || self.path.split('/').any(|p| p == "..") || self.path.contains('\0') {
            return Some(format!("{:?} is not a path from /", self.path));
        }
        None
    }

    /// As people write it: `share:<token>/path`, `user:<name>/path`, `group:<name>/path`.
    pub fn label(&self) -> String {
        let rest = if self.path == "/" { String::new() } else { self.path.clone() };
        match (&self.share, &self.group) {
            (Some(t), _) if !t.is_empty() => format!("share:{t}{rest}"),
            (_, Some(g)) if !g.is_empty() => format!("group:{g}{rest}"),
            _ => format!("user:{}{rest}", self.user),
        }
    }
}

/// A osada folder in a container (Round 18): at `to` inside it, read only unless `write` (what
/// the container writes belongs to its user). The site runs where osada's files are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    #[serde(flatten)]
    pub from: Link,
    pub to: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub write: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Container {
    /// `registry/name:tag`, as podman pulls it.
    pub image: String,
    /// The port inside the container that answers http.
    pub port: u16,
    /// Named volumes: `name:/path` or `name:/path:ro`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Osada folders in it (Round 18).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<Folder>,
    /// The node its outgoing connections exit by (Round 19, egress.rs); none: its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    /// `/api/pi/`: this path and everything below.
    pub prefix: String,
    /// `http(s)://host[:port][/path]`; the rest of the path follows it.
    pub upstream: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Front {
    #[default]
    Tls,
    Cloudflare,
}

impl Site {
    /// The route a path takes: the longest prefix that matches; None for the content.
    pub fn route(&self, path: &str) -> Option<&Route> {
        self.routes.iter().filter(|r| path.starts_with(&r.prefix) || path == r.prefix.trim_end_matches('/')).max_by_key(|r| r.prefix.len())
    }
}

/// The container of the site `name`, for podman.
/// A site's container as it was named before the rename (removed along with it).
pub fn legacy_container_name(site: &str) -> String {
    format!("shelf-web-{site}")
}

pub fn container_name(site: &str) -> String {
    format!("osada-web-{site}")
}

/// `http(s)://host[:port][/path]`.
pub fn valid_url(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://")) else { return false };
    let host = rest.split('/').next().unwrap_or("");
    !host.is_empty() && !host.contains(['?', '#', '@']) && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn valid_volume(v: &str) -> bool {
    let mut parts = v.split(':');
    let (Some(name), Some(path)) = (parts.next(), parts.next()) else { return false };
    let opt = parts.next();
    parts.next().is_none()
        && !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'))
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && path.starts_with('/')
        && !path.contains("..")
        && !path.chars().any(|c| c.is_whitespace() || c.is_control())
        && opt.is_none_or(|o| o == "ro" || o == "rw")
}

fn valid_env_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 128 && (k.as_bytes()[0].is_ascii_alphabetic() || k.starts_with('_')) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// What is wrong with the web instance `name`, if anything (node control's check).
pub fn check(name: &str, m: &Module, p: &Placement) -> Option<String> {
    let Some(site) = &m.site else { return Some("a web instance needs its site".into()) };
    if m.host.is_some() || m.upstream.is_some() {
        return Some("a web instance has names, not a host and an upstream".into());
    }
    let job = matches!(site.content, Content::Job(_));
    if job && (!site.names.is_empty() || !site.exposed_on.is_empty() || site.under_web || !site.routes.is_empty()) {
        return Some("a job is not served: no names, edges, /web/ or routes".into());
    }
    if site.names.is_empty() && !site.under_web && !job {
        return Some("a site needs a host name, or to be at /web/<name>/".into());
    }
    if site.open && !site.under_web {
        return Some("open to anyone is for a site at /web/<name>/ (a site on names of its own is anyway)".into());
    }
    if site.office && !site.under_web {
        return Some("an office suite is at /web/<name>/ (osada opens files with its page there)".into());
    }
    if site.office {
        if let Some((o, _, _)) = sites(p).find(|(o, _, s)| o.as_str() != name && s.office) {
            return Some(format!("{o} is the office suite already"));
        }
    }
    for (k, v) in &site.headers {
        if let Err(why) = valid_header(k, v) {
            return Some(why);
        }
    }
    for (i, n) in site.names.iter().enumerate() {
        if !valid_host(n) {
            return Some(format!("{n:?} is not a host name (lower case, with a dot)"));
        }
        if site.names[..i].contains(n) {
            return Some(format!("{n} twice"));
        }
        let taken = p.modules.iter().filter(|(o, _)| o.as_str() != name).find(|(_, o)| {
            o.host.as_deref() == Some(n.as_str()) || o.site.as_ref().is_some_and(|s| s.names.contains(n))
        });
        if let Some((o, _)) = taken {
            return Some(format!("{n} is the module {o}'s"));
        }
    }
    if let Some(ou) = site.ou.filter(|o| !p.ous.contains_key(o)) {
        return Some(format!("no OU {}", ou.0));
    }
    let Some(runs) = p.nodes.get(&m.node) else { return Some(format!("no node {}", m.node)) };
    for e in &site.exposed_on {
        match p.nodes.get(e) {
            None => return Some(format!("no node {e}")),
            Some(x) if !x.has(Role::EdgeHttp) => return Some(format!("{} is no edge (edge-http)", p.node_label(e))),
            _ => {}
        }
    }
    match &site.content {
        Content::Static { link: Some(l), .. } if l.check().is_some() => {
            return l.check();
        }
        Content::Static { not_found: Some(f), .. } if f.is_empty() || f.contains(['/', '\\']) || f.starts_with('.') => {
            return Some(format!("{f:?} is not a file of the site's folder"));
        }
        Content::Static { .. } => {}
        Content::Upstream { url } if !valid_url(url) => return Some(format!("{url:?} is not http(s)://host[:port][/path]")),
        Content::Upstream { .. } => {}
        Content::Job(j) => {
            if !runs.has(Role::Worker) {
                return Some(format!("{} does not run containers (worker)", p.node_label(&m.node)));
            }
            if j.image.is_empty() || j.image.len() > 255 || j.image.chars().any(|ch| ch.is_whitespace() || ch.is_control()) || j.image.starts_with('-') {
                return Some(format!("{:?} is not an image", j.image));
            }
            if let Err(why) = crate::cron::Cron::parse(&j.schedule) {
                return Some(why);
            }
            if j.command.len() > 64 || j.command.iter().any(|a| a.len() > 4096 || a.contains('\0')) {
                return Some("a command is up to 64 arguments, without NUL".into());
            }
            if let Some(k) = j.env.keys().find(|k| !valid_env_key(k)) {
                return Some(format!("{k:?} is not an environment variable's name"));
            }
            if j.env.values().any(|v| v.contains(['\n', '\0'])) {
                return Some("an environment value is one line".into());
            }
            for (name, ip) in &j.hosts {
                if !valid_host(name) || ip.parse::<std::net::IpAddr>().is_err() {
                    return Some(format!("{name} {ip}: a host name and an IP address"));
                }
            }
            if let Some(why) = egress_check(j.egress.as_ref(), m, p) {
                return Some(why);
            }
            if j.timeout.is_some_and(|t| !(10..=86_400).contains(&t)) {
                return Some("a job's longest run is 10 s to a day".into());
            }
            for w in &j.writes {
                if w.file.is_empty() || w.file.len() > 255 || w.file.starts_with('.') || w.file.contains(['/', '\\', '\0']) {
                    return Some(format!("{:?} is not a file's name in /out", w.file));
                }
                if !valid_write_path(&w.path) {
                    return Some(format!("{:?} is not a file's path in a site's folder (from /, nothing hidden)", w.path));
                }
                match p.modules.get(&w.site) {
                    Some(t) if t.site.as_ref().is_some_and(own_static) && (t.node == m.node || p.web.authority.as_ref() == Some(&m.node)) => {}
                    Some(_) => return Some(format!("{}: a job writes only into a static site of its own folder kept on its node", w.site)),
                    None => return Some(format!("no site {}", w.site)),
                }
            }
        }
        Content::Container(c) => {
            if !runs.has(Role::Worker) {
                return Some(format!("{} does not run containers (worker)", p.node_label(&m.node)));
            }
            if c.image.is_empty() || c.image.len() > 255 || c.image.chars().any(|ch| ch.is_whitespace() || ch.is_control()) || c.image.starts_with('-') {
                return Some(format!("{:?} is not an image", c.image));
            }
            if c.port == 0 {
                return Some("a container needs the port it answers on".into());
            }
            if let Some(why) = egress_check(c.egress.as_ref(), m, p) {
                return Some(why);
            }
            if let Some(v) = c.volumes.iter().find(|v| !valid_volume(v)) {
                return Some(format!("{v:?} is not name:/path[:ro]"));
            }
            if let Some(k) = c.env.keys().find(|k| !valid_env_key(k)) {
                return Some(format!("{k:?} is not an environment variable's name"));
            }
            if c.env.values().any(|v| v.contains(['\n', '\0'])) {
                return Some("an environment value is one line".into());
            }
            // osada folders (Round 18): where osada's files are; a path inside each its own
            let inside = |v: &str| v.split(':').nth(1).unwrap_or("").to_string();
            let mut seen: Vec<String> = c.volumes.iter().map(|v| inside(v)).collect();
            for f in &c.folders {
                if let Some(why) = f.from.check() {
                    return Some(why);
                }
                let to = f.to.trim_end_matches('/');
                if !to.starts_with('/') || to.is_empty() || to.split('/').any(|p| p == ".." || p == ".") || to.contains([':', ',']) || to.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
                    return Some(format!("{:?} is not a path inside the container (from /, no spaces, : or ,)", f.to));
                }
                if seen.iter().any(|x| x.trim_end_matches('/') == to) {
                    return Some(format!("{to} twice inside the container"));
                }
                seen.push(to.to_string());
            }
            if let Some(home) = p.home_node().filter(|h| !c.folders.is_empty() && *h != m.node) {
                return Some(format!("a container with osada folders runs where osada's files are ({})", p.node_label(&home)));
            }
        }
    }
    for r in &site.routes {
        if !r.prefix.starts_with('/') || r.prefix.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Some(format!("{:?} is not a path", r.prefix));
        }
        if !valid_url(&r.upstream) {
            return Some(format!("{:?} is not http(s)://host[:port][/path]", r.upstream));
        }
    }
    None
}

/// An exit node (Round 19): another node of the cluster.
fn egress_check(e: Option<&NodeId>, m: &Module, p: &Placement) -> Option<String> {
    let e = e?;
    if !p.nodes.contains_key(e) {
        return Some(format!("its exit node: no node {e}"));
    }
    if *e == m.node {
        return Some(format!("its connections leave {} anyway: no exit node needed", p.node_label(e)));
    }
    None
}

/// The web instances, by name.
/// A static site of its own folder (not a container, an upstream, nor a link to a osada folder):
/// the authoritative web node keeps it and copies it to the node it runs on (Round 16).
pub fn own_static(s: &Site) -> bool {
    matches!(&s.content, Content::Static { link: None, .. })
}

/// Whether `me` serves the site: the node it runs on, or the authoritative web node for a static
/// site of its own folder (its master copy; the fallback when the node it runs on is away).
pub fn served_by(p: &Placement, m: &Module, s: &Site, me: &crate::placement::NodeId) -> bool {
    m.node == *me || (p.web.authority.as_ref() == Some(me) && own_static(s))
}

pub fn sites(p: &Placement) -> impl Iterator<Item = (&String, &Module, &Site)> {
    p.modules.iter().filter(|(_, m)| m.kind == ModuleKind::Other(OtherKind::Web)).filter_map(|(n, m)| Some((n, m, m.site.as_ref()?)))
}

/// The site at `/web/<name>/` on osada's own names (Round 18).
pub fn under_web<'a>(p: &'a Placement, name: &str) -> Option<(&'a String, &'a Module, &'a Site)> {
    sites(p).find(|(n, _, s)| n.as_str() == name && s.under_web)
}

/// The office suite's page (Round 18): `/web/<name>/`, when a site is one.
pub fn office(p: &Placement) -> Option<String> {
    sites(p).find(|(_, _, s)| s.office && s.under_web).map(|(n, _, _)| format!("/web/{n}/"))
}

/// The site a host name is, among those exposed on `node`.
pub fn exposed<'a>(p: &'a Placement, node: &NodeId, host: &str) -> Option<(&'a String, &'a Module, &'a Site)> {
    sites(p).find(|(_, _, s)| s.exposed_on.contains(node) && s.names.iter().any(|n| n == host))
}
