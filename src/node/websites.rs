//! Admin → web (D-U21, D-U23): every site with where it is exposed and runs,
//! and its container's state as the node it runs on tells it; the portal's
//! container actions (upgrade, restart) go to that node, whose root agent
//! does them.

use std::{collections::BTreeMap, path::PathBuf};

use serde_json::{Value, json};

use super::{Actor, Answer, Call, Node};
use crate::{
    cluster::ROOT_OU,
    placement::NodeId,
    rbac::Perm,
    sites::{self, Content},
};
use std::sync::Arc;

impl Node {
    /// The node's data folder (its `web/` is the sites' and containers'); set once at start.
    pub fn set_data(&self, dir: PathBuf) {
        let _ = self.data.set(dir);
    }

    /// Where sites linked to a osada folder are (set where osada's files are).
    pub fn set_links(&self, links: crate::websrv::Links) {
        let _ = self.links.set(links);
    }

    /// The sites' server of this node (its data folder; osada's folders where they are), for
    /// /web/<site>/ (Round 18).
    pub fn site_web(&self) -> Option<Arc<crate::websrv::Web>> {
        let data = self.data.get()?;
        Some(
            self.site_web
                .get_or_init(|| match self.links.get() {
                    Some(l) => crate::websrv::Web::with_links(data, l.clone()),
                    None => crate::websrv::Web::new(data),
                })
                .clone(),
        )
    }

    /// A call about the containers of the sites that run here.
    pub(super) fn answer_web(&self, op: &str, site: Option<String>) -> Answer {
        let Some(dir) = self.data.get() else { return Answer::Refused { why: "this node has no data folder".into() } };
        match op {
            "status" => {
                // and what the egress proxies did here (Round 19), by site:<name> / job:<name>
                let mut v = crate::containers::status(dir);
                if !v.is_object() {
                    v = json!({});
                }
                v["egress"] = crate::egress::report();
                Answer::Web { value: v }
            }
            // what the agent last learnt of Stalwart (T11, T12)
            "stalwart" => Answer::Web { value: std::fs::read(dir.join("web/stalwart.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null) },
            "upgrade" | "restart" | "run" => {
                let Some(site) = site else { return Answer::Refused { why: "which site?".into() } };
                let p = self.cluster_state().placement;
                // a container's upgrade and restart; a job's run now (Round 18b)
                let here = sites::sites(&p).any(|(n, m, s)| {
                    n == &site && m.node == self.me && if op == "run" { matches!(s.content, Content::Job(_)) } else { matches!(s.content, Content::Container(_)) }
                });
                if !here {
                    return Answer::Refused { why: format!("{site} has no {} on this node", if op == "run" { "job" } else { "container" }) };
                }
                match crate::containers::request(dir, &site, op) {
                    Ok(id) => Answer::Web { value: json!({ "requested": id }) },
                    Err(why) => Answer::Refused { why },
                }
            }
            _ => Answer::Refused { why: format!("no web call {op}") },
        }
    }

    /// Whether the actor may manage the site (D-U28): infrastructure rights at
    /// the root, or ou-manage in its OU for a static site.
    fn may_site(&self, actor: &Actor, s: &crate::sites::Site) -> bool {
        self.may(&actor.user, Perm::ClusterManage, ROOT_OU) || (!s.infra() && self.may(&actor.user, Perm::OuManage, s.ou.unwrap_or(ROOT_OU)))
    }

    /// The sites, for admin → web (those the actor may manage); the containers' state from the nodes they run on.
    pub async fn web_overview(self: &Arc<Self>, actor: &Actor) -> Result<Value, String> {
        let p = self.cluster_state().placement;
        let runners: Vec<NodeId> = {
            let mut v: Vec<NodeId> = sites::sites(&p).filter(|(_, _, s)| matches!(s.content, Content::Container(_) | Content::Job(_))).map(|(_, m, _)| m.node.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        let mut status: BTreeMap<NodeId, Value> = BTreeMap::new();
        for n in runners {
            let v = match self.ask(&n, Call::Web { op: "status".into(), site: None }).await {
                Ok(Answer::Web { value }) => value,
                Ok(other) => json!({ "error": format!("{other:?}") }),
                Err(e) => json!({ "error": e }),
            };
            status.insert(n, v);
        }
        let list: Vec<Value> = sites::sites(&p)
            .filter(|(_, _, s)| self.may_site(actor, s))
            .map(|(name, m, s)| {
                let (kind, what) = match &s.content {
                    Content::Static { link, .. } => ("static", link.as_ref().map_or_else(|| format!("web/{name}/"), |l| format!("web/{name}/ → {}:{}", l.user, l.path))),
                    Content::Container(c) if c.folders.is_empty() => ("container", c.image.clone()),
                    Content::Container(c) => ("container", format!("{} + {}", c.image, c.folders.iter().map(|f| format!("{} → {}{}", f.from.label(), f.to, if f.write { " (rw)" } else { "" })).collect::<Vec<_>>().join(", "))),
                    Content::Upstream { url } => ("upstream", url.clone()),
                    Content::Job(j) => ("job", format!("{} · {}{}", j.image, j.schedule, if j.writes.is_empty() { String::new() } else { format!(" → {}", j.writes.iter().map(|w| format!("{}:{}", w.site, w.path)).collect::<Vec<_>>().join(", ")) })),
                };
                let container = matches!(s.content, Content::Container(_)).then(|| {
                    let st = &status.get(&m.node).cloned().unwrap_or(Value::Null);
                    st.get("sites").and_then(|x| x.get(name)).cloned().or_else(|| st.get("error").map(|e| json!({ "error": e }))).unwrap_or(json!({ "state": "unknown" }))
                });
                // a job's: running or waiting, the next run, the last (Round 18b)
                let job = matches!(s.content, Content::Job(_)).then(|| {
                    let st = &status.get(&m.node).cloned().unwrap_or(Value::Null);
                    st.get("jobs").and_then(|x| x.get(name)).cloned().or_else(|| st.get("error").map(|e| json!({ "error": e }))).unwrap_or(json!({ "state": "not started yet" }))
                });
                // its connections out by another node (Round 19)
                let egress = match &s.content {
                    Content::Container(c) => c.egress.as_ref().map(|e| (e, format!("site:{name}"))),
                    Content::Job(j) => j.egress.as_ref().map(|e| (e, format!("job:{name}"))),
                    _ => None,
                }
                .map(|(e, key)| json!({ "exit": e, "exit_name": p.node_label(e), "stats": status.get(&m.node).map(|st| st["egress"][&key].clone()).unwrap_or(Value::Null) }));
                let ou = s.ou.unwrap_or(ROOT_OU);
                // a static site's files belong to its OU: on the OU's authority for files (D-U28)
                let home = p.authority(crate::placement::Domain::Files, ou).ok().map(|a| a.node);
                let elsewhere = matches!(s.content, Content::Static { link: None, .. }) && home.as_ref().is_some_and(|h| *h != m.node);
                json!({
                    "ou": ou.0, "ou_path": p.ou_path(ou), "files_home": home.as_ref().map(|h| p.node_label(h)), "files_elsewhere": elsewhere,
                    "name": name, "title": m.title, "names": s.names, "kind": kind, "what": what,
                    "runs_on": m.node, "runs_on_name": p.node_label(&m.node),
                    "exposed_on": s.exposed_on, "exposed_on_names": s.exposed_on.iter().map(|e| p.node_label(e)).collect::<Vec<_>>(),
                    "front": s.front, "routes": s.routes, "site": s, "container": container, "job": job, "egress": egress,
                })
            })
            .collect();
        Ok(json!({ "sites": list }))
    }

    /// A container action from the portal, done by the site's node.
    pub async fn web_action(self: &Arc<Self>, actor: &Actor, site: &str, action: &str) -> Result<Value, String> {
        // a container's: infrastructure rights at the root (D-U28)
        if !self.may(&actor.user, Perm::ClusterManage, ROOT_OU) {
            return Err("you cannot manage containers".into());
        }
        let p = self.cluster_state().placement;
        let (_, m, _) = sites::sites(&p).find(|(n, _, _)| n.as_str() == site).ok_or_else(|| format!("no site {site}"))?;
        let node = m.node.clone();
        let v = match self.ask(&node, Call::Web { op: action.into(), site: Some(site.into()) }).await? {
            Answer::Web { value } => value,
            Answer::Refused { why } => return Err(why),
            other => return Err(format!("{other:?}")),
        };
        self.log_admin(actor, "site_action", json!({ "site": site, "action": action, "node": node }));
        Ok(v)
    }
}

impl Node {
    /// `osada web import <file>` (as the server's account, as `osada rescue`):
    /// sites to add to node control, `{"<site>": {"title", "node", "site"}}`;
    /// nodes by name or id. Those that are there already stay as they are.
    /// The file becomes `.done` or `.failed` with what happened.
    pub fn watch_web_import(self: &Arc<Self>, file: PathBuf) {
        let node = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tick.tick().await;
                let Ok(text) = std::fs::read_to_string(&file) else { continue };
                let res = node.web_import(&text).await;
                let (ext, msg) = match &res {
                    Ok(m) => ("done", m.clone()),
                    Err(e) => ("failed", e.clone()),
                };
                println!("{}: web import: {msg}", node.me);
                let _ = std::fs::write(file.with_extension(ext), format!("{msg}\n"));
                let _ = std::fs::remove_file(&file);
            }
        });
    }

    async fn web_import(self: &Arc<Self>, text: &str) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        struct One {
            #[serde(default)]
            title: String,
            node: String,
            site: crate::sites::Site,
            /// A site there already is replaced by this one (else it stays).
            #[serde(default)]
            replace: bool,
        }
        #[derive(serde::Deserialize)]
        struct Cert {
            names: Vec<String>,
            /// None: the first of the edges node control knows a link for (by name).
            #[serde(default)]
            issuer: Option<String>,
            /// None: those edges but the issuer.
            #[serde(default)]
            copies: Option<Vec<String>>,
        }
        let mut all: BTreeMap<String, Value> = serde_json::from_str(text).map_err(|e| format!("not a list of sites: {e}"))?;
        let mut p = self.cluster_state().placement;
        // `"nodes": {"<name>": {"addrs": [...], "roles_add": [...], "cache_gb": n}}`: a node's public
        // addresses (T10, T11), roles it gets besides its own and its cache's size (D-U40), each optional
        let mut set_nodes = Vec::new();
        if let Some(Value::Object(nodes)) = all.remove("nodes") {
            for (n, v) in nodes {
                let id = p.resolve_node(&n).ok_or_else(|| format!("no node {n}"))?;
                let x = p.nodes.get_mut(&id).ok_or_else(|| format!("no node {n}"))?;
                if !v["addrs"].is_null() {
                    x.addrs = serde_json::from_value(v["addrs"].clone()).map_err(|e| format!("nodes.{n}.addrs: {e}"))?;
                }
                if !v["roles_add"].is_null() {
                    let add: Vec<crate::placement::Role> = serde_json::from_value(v["roles_add"].clone()).map_err(|e| format!("nodes.{n}.roles_add: {e}"))?;
                    x.roles.extend(add);
                }
                match &v["files_dir"] {
                    Value::Null => {}
                    Value::String(d) if d.is_empty() => x.files_dir = None,
                    Value::String(d) => x.files_dir = Some(d.clone()),
                    _ => return Err(format!("nodes.{n}.files_dir: a path, or empty")),
                }
                if let Some(b) = v["log_batch"].as_u64() {
                    x.log_batch = Some(u32::try_from(b).map_err(|_| format!("nodes.{n}.log_batch: too big"))?);
                }
                if let Some(gb) = v["cache_gb"].as_u64() {
                    x.cache_gb = Some(u32::try_from(gb).map_err(|_| format!("nodes.{n}.cache_gb: too big"))?.max(1));
                }
                set_nodes.push(n);
            }
        }

        // `"mail": {"host", "node", "upstream"}` (Round 24, osada-setup mail): the mail instance on that
        // node, made when the cluster has none with that name
        let mut set_mail = false;
        if let Some(v) = all.remove("mail") {
            let host = v["host"].as_str().unwrap_or("").trim().to_ascii_lowercase();
            if !crate::placement::valid_host(&host) || !host.contains('.') {
                return Err(format!("mail.host: {host:?} is not a host name"));
            }
            // no node: the one asked (osada-setup mail runs on the mail node)
            let on = v["node"].as_str().unwrap_or("");
            let node = if on.is_empty() { self.me.clone() } else { p.resolve_node(on).ok_or_else(|| format!("mail.node: no node {on}"))? };
            let upstream = v["upstream"].as_str().unwrap_or("tls://127.0.0.1:8443").to_string();
            let kind = crate::placement::ModuleKind::Data(crate::placement::Domain::Mail);
            if !p.modules.values().any(|m| m.kind == kind && m.host.as_deref() == Some(host.as_str())) {
                let name = if p.modules.contains_key("mail") { format!("mail-{}", p.node_label(&node)) } else { "mail".to_string() };
                let mut m = crate::placement::Module::new(kind, "Mail", node);
                m.host = Some(host);
                m.upstream = Some(upstream);
                p.modules.insert(name, m);
                set_mail = true;
            }
        }

        // `"certs": {"<name>": {"names", "issuer", "copies"}}`: set (replaced) in node control (D-U26)
        let certs: BTreeMap<String, Cert> = match all.remove("certs") {
            Some(v) => serde_json::from_value(v).map_err(|e| format!("certs: {e}"))?,
            None => BTreeMap::new(),
        };
        let req: BTreeMap<String, One> = serde_json::from_value(Value::Object(all.into_iter().collect())).map_err(|e| format!("not a list of sites: {e}"))?;
        let mut set_certs = Vec::new();
        // the edges reached at an address (node/dial.rs): the ones that answer the public names
        let mut edges: Vec<NodeId> = p.nodes.iter().filter(|(_, n)| n.has(crate::placement::Role::EdgeHttp) && n.link.is_some()).map(|(id, _)| id.clone()).collect();
        edges.sort_by_key(|id| p.node_label(id));
        for (name, c) in certs {
            let node = |s: &str| p.resolve_node(s).ok_or_else(|| format!("cert {name}: no node {s}"));
            let issuer = match &c.issuer {
                Some(i) => node(i)?,
                // a certificate there already keeps its issuer
                None if p.certs.contains_key(&name) => p.certs[&name].issuer.clone(),
                None => edges.first().cloned().ok_or_else(|| format!("cert {name}: no edge known yet (none has a link address): give the issuer"))?,
            };
            let copies = match &c.copies {
                Some(v) => v.iter().map(|s| node(s)).collect::<Result<_, _>>()?,
                None => edges.iter().filter(|e| **e != issuer).cloned().collect(),
            };
            let spec = crate::acme::CertSpec { names: c.names, issuer, copies };
            p.certs.insert(name.clone(), spec);
            set_certs.push(name);
        }
        let (mut added, mut kept, mut replaced) = (Vec::new(), Vec::new(), Vec::new());
        for (name, one) in req {
            let there = p.modules.get(&name).map(|m| m.kind);
            match there {
                Some(k) if !one.replace || k != crate::placement::ModuleKind::WEB => {
                    kept.push(name);
                    continue;
                }
                Some(_) => replaced.push(name.clone()),
                None => {}
            }
            let node = p.resolve_node(&one.node).ok_or_else(|| format!("{name}: no node {}", one.node))?;
            let mut site = one.site;
            site.exposed_on = site.exposed_on.iter().map(|e| p.resolve_node(&e.0).ok_or_else(|| format!("{name}: no node {e}"))).collect::<Result<_, _>>()?;
            let mut m = crate::placement::Module::new(crate::placement::ModuleKind::WEB, &one.title, node);
            m.site = Some(site);
            p.modules.insert(name.clone(), m);
            if !replaced.contains(&name) {
                added.push(name);
            }
        }
        if !added.is_empty() || !replaced.is_empty() || !set_certs.is_empty() || !set_nodes.is_empty() || set_mail {
            self.set_placement(&Actor::node(&self.me), p).await?;
        }
        let list = |v: &Vec<String>| if v.is_empty() { "none".to_string() } else { v.join(", ") };
        Ok(format!(
            "added {}; replaced {}; there already: {}; certificates set: {}; nodes set: {}; mail instance: {}",
            list(&added),
            list(&replaced),
            list(&kept),
            list(&set_certs),
            list(&set_nodes),
            if set_mail { "made" } else { "unchanged" }
        ))
    }
}

// ---- certificates (D-U26) ---------------------------------------------------

impl Node {
    pub(super) fn answer_acme_token(&self, token: &str) -> Answer {
        match self.data.get().and_then(|d| crate::acme::token(d, token)) {
            Some(k) => Answer::Web { value: json!(k) },
            None => Answer::Refused { why: "no such token here".into() },
        }
    }

    pub(super) fn answer_cert(&self, from: &NodeId, name: &str) -> Answer {
        let p = self.cluster_state().placement;
        let Some(spec) = crate::acme::wanted(&p).remove(name) else { return Answer::Refused { why: format!("no certificate {name}") } };
        if spec.issuer != self.me || !spec.copies.contains(from) {
            return Answer::Refused { why: format!("{} gets no copy of {name} from here", p.node_label(from)) };
        }
        match self.data.get().and_then(|d| crate::acme::bundle(d, name)) {
            Some(v) => Answer::Web { value: v },
            None => Answer::Refused { why: format!("{name} is not issued yet") },
        }
    }

    /// For an edge asked for an http-01 token of `host`: the key
    /// authorization, from the issuer of the certificate with that name.
    pub async fn acme_for(self: &Arc<Self>, host: &str, token: &str) -> Option<String> {
        let p = self.cluster_state().placement;
        let issuer = crate::acme::wanted(&p).into_values().find(|c| c.names.iter().any(|n| n == host)).map(|c| c.issuer)?;
        if issuer == self.me {
            return None;
        }
        match self.ask(&issuer, Call::AcmeToken { token: token.into() }).await {
            Ok(Answer::Web { value }) => value.as_str().map(str::to_string),
            _ => None,
        }
    }

    /// The issuer's and the copies' rounds: issues what is due here (every
    /// 30 minutes, 6 hours after a failure), fetches copies that changed
    /// (every 10 minutes). `lan`: home's https folder and its public name:
    /// a copy for that name goes there too.
    pub fn spawn_certs(self: &Arc<Self>, lan: Option<(PathBuf, String)>) {
        let node = self.clone();
        tokio::spawn(async move {
            // a failed certificate waits an hour (Let's Encrypt allows 5 failed validations a name an
            // hour), unless it changes (names, issuer, copies: an edge more that answers its names)
            let mut failed: BTreeMap<String, (std::time::Instant, crate::acme::CertSpec)> = BTreeMap::new();
            // OSADA_CERT_ROUND: seconds between rounds (60; tests use less). A round is cheap: the
            // issuer looks at its files, a copy asks the issuer for the serial.
            let every = std::env::var("OSADA_CERT_ROUND").ok().and_then(|v| v.parse::<u64>().ok()).filter(|s| *s > 0).unwrap_or(60);
            tokio::time::sleep(std::time::Duration::from_secs(every.min(20))).await;
            loop {
                let Some(data) = node.data.get().cloned() else { return };
                let p = node.cluster_state().placement;
                let wanted = crate::acme::wanted(&p);
                for (name, spec) in wanted.iter().filter(|(_, c)| c.issuer == node.me) {
                    let waiting = failed.get(name).is_some_and(|(t, was)| was == spec && t.elapsed() < std::time::Duration::from_secs(3600));
                    if !crate::acme::due(&data, name, spec) || waiting {
                        continue;
                    }
                    match crate::acme::issue(&data, name, spec).await {
                        Ok(m) => {
                            failed.remove(name);
                            println!("{}: certificate {name} issued for {}", node.me, m.names.join(", "));
                        }
                        Err(e) => {
                            failed.insert(name.clone(), (std::time::Instant::now(), spec.clone()));
                            eprintln!("{}: certificate {name}: {e} (again in an hour, or when it changes)", node.me);
                        }
                    }
                }
                for (name, spec) in wanted.iter().filter(|(_, c)| c.issuer != node.me && c.copies.contains(&node.me)) {
                    let have = crate::acme::meta(&data, name).map(|m| m.serial);
                    let Ok(Answer::Web { value }) = node.ask(&spec.issuer, Call::Cert { name: name.clone() }).await else { continue };
                    let Ok(m) = serde_json::from_value::<crate::acme::Meta>(value["meta"].clone()) else { continue };
                    let (Some(chain), Some(key)) = (value["fullchain"].as_str(), value["privkey"].as_str()) else { continue };
                    if have.as_deref() == Some(m.serial.as_str()) || crate::acme::serial(chain) != m.serial {
                        continue;
                    }
                    match crate::acme::store(&data, name, chain, key, &m) {
                        Ok(()) => println!("{}: a copy of certificate {name} from {}", node.me, p.node_label(&spec.issuer)),
                        Err(e) => eprintln!("{}: certificate {name}: {e}", node.me),
                    }
                    if let Some((dir, host)) = lan.as_ref().filter(|(_, h)| m.names.contains(h)) {
                        if crate::acme::store_in(dir, chain, key, &m).is_ok() {
                            println!("{}: {host}'s certificate for the LAN's https in {}", node.me, dir.display());
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(every)).await;
            }
        });
    }
}

// ---- a site's files (T13) ---------------------------------------------------

/// The largest file read or written through admin → web (base64 in a call).
const WEB_FILE_MAX: u64 = 10 << 20;

/// `rel` under `base`: relative, no `..`; None when it is not such a path.
fn under(base: &std::path::Path, rel: &str) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." || part.contains('\0') {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// `rwxr-x---`, with `d` in front for a folder.
fn mode_string(mode: u32, dir: bool) -> String {
    let mut s = String::from(if dir { "d" } else { "-" });
    for (bit, ch) in [(0o400, 'r'), (0o200, 'w'), (0o100, 'x'), (0o040, 'r'), (0o020, 'w'), (0o010, 'x'), (0o004, 'r'), (0o002, 'w'), (0o001, 'x')] {
        s.push(if mode & bit != 0 { ch } else { '-' });
    }
    s
}

/// A user's or a group's name from /etc/passwd or /etc/group, else its number.
fn id_name(file: &str, id: u32) -> String {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| t.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() > 2 && f[2] == id.to_string()).then(|| f[0].to_string())
        }))
        .unwrap_or_else(|| id.to_string())
}

fn user_name(uid: u32) -> String {
    id_name("/etc/passwd", uid)
}

fn group_name(gid: u32) -> String {
    id_name("/etc/group", gid)
}

impl Node {
    pub(super) fn answer_web_files(&self, site: &str, op: &str, path: &str, to: Option<&str>, data: Option<&str>, all: bool) -> Answer {
        use base64::Engine;
        let p = self.cluster_state().placement;
        let Some((_, _, sdef)) = sites::sites(&p).find(|(n, m, s)| n.as_str() == site && sites::served_by(&p, m, s, &self.me)) else { return Answer::Refused { why: format!("{site} does not run here") } };
        let Some(dir) = self.data.get() else { return Answer::Refused { why: "this node has no data folder".into() } };
        // its folder, or the osada folder it links to (D-U22)
        let base = match &sdef.content {
            Content::Static { link: Some(l), .. } => match self.links.get().and_then(|f| f(l)) {
                Some(b) => b,
                None => return Answer::Refused { why: format!("{site}'s folder is a osada folder this node does not have") },
            },
            _ => dir.join("web").join(site),
        };
        let Some(target) = under(&base, path) else { return Answer::Refused { why: "not a path in the site's folder".into() } };
        let fail = |e: std::io::Error| Answer::Refused { why: format!("{path}: {e}") };
        let ok = |v: Value| Answer::Web { value: v };
        match op {
            "list" => {
                let _ = std::fs::create_dir_all(&base);
                let rd = match std::fs::read_dir(&target) {
                    Ok(r) => r,
                    Err(e) => return fail(e),
                };
                // who may change each (Round 18b): its mode and owner, and the jobs that write it
                let writers = sites::writers(&p, site);
                let here = path.trim_matches('/');
                let mut list: Vec<Value> = rd
                    .flatten()
                    .filter(|e| !(target == base && e.file_name() == ".jobs-out"))
                    .filter_map(|e| {
                        use std::os::unix::fs::MetadataExt;
                        let m = e.metadata().ok()?;
                        let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
                        let name = e.file_name().to_string_lossy().to_string();
                        let rel = if here.is_empty() { format!("/{name}") } else { format!("/{here}/{name}") };
                        let by: Vec<Value> = writers.iter().filter(|(_, p, _)| *p == rel).map(|(j, _, sch)| json!({ "job": j, "schedule": sch })).collect();
                        Some(json!({
                            "name": name, "dir": m.is_dir(), "size": m.len(), "mtime": mtime,
                            "mode": mode_string(m.mode(), m.is_dir()), "owner": user_name(m.uid()), "group": group_name(m.gid()),
                            "writers": by,
                        }))
                    })
                    .collect();
                list.sort_by(|a, b| (!a["dir"].as_bool().unwrap_or(false), a["name"].as_str().unwrap_or("")).cmp(&(!b["dir"].as_bool().unwrap_or(false), b["name"].as_str().unwrap_or(""))));
                let job = match &sdef.content {
                    Content::Job(j) => Some(json!({ "schedule": j.schedule, "writes": j.writes })),
                    _ => None,
                };
                ok(json!({ "path": path, "entries": list, "job": job }))
            }
            "read" => match std::fs::metadata(&target) {
                Ok(m) if m.len() > WEB_FILE_MAX => Answer::Refused { why: format!("{path}: over {} MiB; change it in the site's folder on its node", WEB_FILE_MAX >> 20) },
                Ok(_) => match std::fs::read(&target) {
                    Ok(b) => ok(json!({ "data": base64::engine::general_purpose::STANDARD.encode(b) })),
                    Err(e) => fail(e),
                },
                Err(e) => fail(e),
            },
            "write" => {
                let Some(b) = data.and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok()) else { return Answer::Refused { why: "no data".into() } };
                if b.len() as u64 > WEB_FILE_MAX {
                    return Answer::Refused { why: format!("over {} MiB", WEB_FILE_MAX >> 20) };
                }
                if target == base {
                    return Answer::Refused { why: "a file needs a name".into() };
                }
                let tmp = target.with_file_name(format!(".{}.tmp", target.file_name().and_then(|f| f.to_str()).unwrap_or("x")));
                let r = target.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&tmp, &b)).and_then(|()| std::fs::rename(&tmp, &target));
                match r {
                    Ok(()) => ok(json!({ "written": b.len() })),
                    Err(e) => fail(e),
                }
            }
            "mkdir" => match std::fs::create_dir_all(&target) {
                Ok(()) => ok(json!({ "made": path })),
                Err(e) => fail(e),
            },
            "rename" => {
                let Some(dest) = to.and_then(|t| under(&base, t)).filter(|d| *d != base) else { return Answer::Refused { why: "not a path in the site's folder".into() } };
                if dest.exists() {
                    return Answer::Refused { why: format!("{} is there already", to.unwrap_or("")) };
                }
                match dest.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::rename(&target, &dest)) {
                    Ok(()) => ok(json!({ "renamed": to })),
                    Err(e) => fail(e),
                }
            }
            "delete" => {
                if target == base {
                    return Answer::Refused { why: "not the site's folder itself".into() };
                }
                let r = match std::fs::symlink_metadata(&target) {
                    Ok(m) if m.is_dir() && all => std::fs::remove_dir_all(&target),
                    Ok(m) if m.is_dir() => std::fs::remove_dir(&target),
                    Ok(_) => std::fs::remove_file(&target),
                    Err(e) => Err(e),
                };
                match r {
                    Ok(()) => ok(json!({ "deleted": path })),
                    Err(e) => fail(e),
                }
            }
            _ => Answer::Refused { why: format!("no file operation {op}") },
        }
    }

    /// A file operation on a site's folder (admin → web, T13), done by the node it runs on.
    pub async fn web_files(self: &Arc<Self>, actor: &Actor, site: &str, op: &str, path: &str, to: Option<String>, data: Option<String>, all: bool) -> Result<Value, String> {
        let p = self.cluster_state().placement;
        let (_, m, s) = sites::sites(&p).find(|(n, _, _)| n.as_str() == site).ok_or_else(|| format!("no site {site}"))?;
        if !self.may_site(actor, s) {
            return Err(format!("you cannot manage {site}'s files"));
        }
        // the authoritative web node keeps a static site of its own folder: edits go there (Round 16)
        let node = match &p.web.authority {
            Some(a) if sites::own_static(s) => a.clone(),
            _ => m.node.clone(),
        };
        let changes = matches!(op, "write" | "mkdir" | "rename" | "delete");
        let v = match self.ask(&node, Call::WebFiles { site: site.into(), op: op.into(), path: path.into(), to: to.clone(), data, all }).await? {
            Answer::Web { value } => value,
            Answer::Refused { why } => return Err(why),
            other => return Err(format!("{other:?}")),
        };
        if changes {
            self.log_admin(actor, "site_file", json!({ "site": site, "op": op, "path": path, "to": to }));
        }
        Ok(v)
    }
}

// ---- nodes (T10) --------------------------------------------------------------

impl Node {
    /// Every node's state (admin → nodes): asked of each, at most 5 s.
    pub async fn nodes_info(self: &Arc<Self>, actor: &Actor) -> Result<Value, String> {
        if !self.may(&actor.user, Perm::ClusterManage, ROOT_OU) {
            return Err("you cannot see the nodes".into());
        }
        let p = self.cluster_state().placement;
        let mut out = serde_json::Map::new();
        for n in p.nodes.keys() {
            let v = match tokio::time::timeout(std::time::Duration::from_secs(5), self.ask(n, Call::NodeInfo)).await {
                Ok(Ok(Answer::Web { value })) => value,
                Ok(Ok(other)) => json!({ "error": format!("{other:?}") }),
                Ok(Err(e)) => json!({ "error": e }),
                Err(_) => json!({ "error": "no answer in 5 s" }),
            };
            out.insert(n.0.clone(), v);
        }
        Ok(Value::Object(out))
    }

    /// A file system on an empty disk of `node`, by its root agent (T10).
    pub async fn disk_format(self: &Arc<Self>, actor: &Actor, node: &str, device: &str, label: &str, confirm: &str) -> Result<Value, String> {
        if !self.may(&actor.user, Perm::ClusterManage, ROOT_OU) {
            return Err("you cannot manage the nodes".into());
        }
        if confirm != device {
            return Err(format!("type {device} to confirm"));
        }
        let p = self.cluster_state().placement;
        let id = p.resolve_node(node).ok_or_else(|| format!("no node {node}"))?;
        let v = match self.ask(&id, Call::DiskFormat { device: device.into(), label: label.into() }).await? {
            Answer::Web { value } => value,
            Answer::Refused { why } => return Err(why),
            other => return Err(format!("{other:?}")),
        };
        self.log_admin(actor, "disk_format", json!({ "node": id, "device": device, "label": label }));
        Ok(v)
    }
}

// ---- the DNS checker (T11) and where Stalwart is (T12) ----------------------------

impl Node {
    /// Each mail instance with a host (Round 18: several): its name, node, host, and what the
    /// agent on its node says of its Stalwart.
    pub async fn stalwart_state(self: &Arc<Self>) -> Vec<(String, NodeId, String, Value)> {
        let mut out = Vec::new();
        for (name, m) in self.mail_instances() {
            let Some(host) = m.host.clone() else { continue };
            let v = match tokio::time::timeout(std::time::Duration::from_secs(8), self.ask(&m.node, Call::Web { op: "stalwart".into(), site: None })).await {
                Ok(Ok(Answer::Web { value })) => value,
                Ok(Ok(other)) => json!({ "error": format!("{other:?}") }),
                Ok(Err(e)) => json!({ "error": e }),
                Err(_) => json!({ "error": "no answer in 8 s" }),
            };
            out.push((name, m.node, host, v));
        }
        out
    }

    /// Every record the deployment needs, checked against public DNS (T11).
    pub async fn dns_check(self: &Arc<Self>, actor: &Actor) -> Result<Value, String> {
        use crate::dnscheck::{Want, check, parse_zone, zone};
        if !self.may(&actor.user, Perm::ClusterManage, ROOT_OU) {
            return Err("you cannot see the DNS check".into());
        }
        let p = self.cluster_state().placement;
        let mut wants: Vec<Want> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let edge = |id: &NodeId| p.nodes.get(id).is_some_and(|n| n.has(crate::placement::Role::EdgeHttp));
        let addrs = |nodes: &[NodeId], why: &str, name: &str, wants: &mut Vec<Want>, notes: &mut Vec<String>| {
            for n in nodes {
                let a = &p.nodes.get(n).map(|x| x.addrs.clone()).unwrap_or_default();
                if a.is_empty() {
                    notes.push(format!("{name}: {} has no public addresses in node control (its card)", p.node_label(n)));
                }
                for ip in a {
                    let rtype = if ip.contains(':') { "AAAA" } else { "A" };
                    wants.push(Want { name: name.into(), rtype: rtype.into(), value: ip.clone(), why: why.into(), any_of: None });
                }
            }
        };
        let cf = self.data.get().map(|d| crate::websrv::cloudflare(d)).unwrap_or_default();
        // sites
        for (name, _, s) in sites::sites(&p) {
            for host in &s.names {
                let why = format!("site {name}");
                if s.front == crate::sites::Front::Cloudflare {
                    for rtype in ["A", "AAAA"] {
                        wants.push(Want { name: host.clone(), rtype: rtype.into(), value: "(Cloudflare)".into(), why: why.clone(), any_of: Some(cf.clone()) });
                    }
                } else {
                    let ex: Vec<NodeId> = s.exposed_on.iter().filter(|e| edge(e)).cloned().collect();
                    addrs(&ex, &why, host, &mut wants, &mut notes);
                }
            }
        }
        // certificates (osada's own names) and proxies: the edges that have them
        let mut done: Vec<String> = wants.iter().map(|w| w.name.clone()).collect();
        for (cname, c) in crate::acme::wanted(&p) {
            let nodes: Vec<NodeId> = std::iter::once(c.issuer.clone()).chain(c.copies.iter().cloned()).filter(|n| edge(n)).collect();
            let hosts: Vec<String> = c.names.iter().filter(|h| !done.contains(h)).cloned().collect();
            for host in hosts {
                addrs(&nodes, &format!("certificate {cname}"), &host, &mut wants, &mut notes);
                done.push(host);
            }
        }
        // each mail server's name, and every record its Stalwart wants (Round 18: an instance each)
        let stalwarts = self.stalwart_state().await;
        for (name, n, host, st) in &stalwarts {
            if !done.contains(host) {
                addrs(std::slice::from_ref(n), &format!("the mail server ({name})"), host, &mut wants, &mut notes);
                done.push(host.clone());
            }
            match st["dns"].as_object() {
                Some(domains) => {
                    for (d, z) in domains {
                        if let Some(z) = z.as_str() {
                            let why = if stalwarts.len() > 1 { format!("Stalwart {name}: {d}") } else { format!("Stalwart: {d}") };
                            wants.extend(parse_zone(z, &why));
                        }
                    }
                }
                None => notes.push(format!("Stalwart {name}'s records: {}", st["error"].as_str().unwrap_or("not known yet (the agent on its node asks every 10 minutes)"))),
            }
        }
        let rows = check(wants).await;
        let all: Vec<&Want> = rows.iter().map(|r| &r.want).collect();
        let missing: Vec<&Want> = rows.iter().filter(|r| r.state != "ok").map(|r| &r.want).collect();
        let bad = missing.len();
        Ok(json!({ "rows": rows, "zone_all": zone(&all), "zone_missing": zone(&missing), "notes": notes, "bad": bad, "total": rows.len(), "stalwarts": stalwarts.iter().map(|(name, n, host, st)| json!({ "instance": name, "node": p.node_label(n), "host": host, "state": st })).collect::<Vec<_>>() }))
    }
}

// ---- the authoritative web node (Round 16) -------------------------------------------------------

impl Node {
    /// As the authoritative web node: every 30 s, each static site of its own folder that runs on
    /// another node gets what changed in `web/<site>/` since the last time (a manifest per site and
    /// node in `web-mirror/`), and loses there what is gone here.
    pub(super) fn spawn_web_mirror(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let Some(n) = weak.upgrade() else { break };
                n.mirror_sites().await;
            }
        });
    }

    async fn mirror_sites(self: &Arc<Self>) {
        let p = self.cluster_state().placement;
        if p.web.authority.as_ref() != Some(&self.me) {
            return;
        }
        let Some(data) = self.data.get().cloned() else { return };
        let peers = self.link.peers();
        for (name, m, s) in sites::sites(&p) {
            if m.node == self.me || !sites::own_static(s) || !peers.contains(&m.node) {
                continue;
            }
            if let Err(e) = self.mirror_site(&data, name, &m.node).await {
                eprintln!("{}: web: copying {name} to {}: {e}", self.me, m.node);
            }
        }
    }

    async fn mirror_site(&self, data: &std::path::Path, name: &str, to: &NodeId) -> Result<(), String> {
        let root = data.join("web").join(name);
        let mf = data.join("web-mirror").join(format!("{name}@{}.json", to.0));
        // taken over (a new authoritative node, or a site new to it): first the files the node it
        // runs on has, so that what is kept here is the site as it is; what only this node has
        // stays and is copied out below
        // (also when an earlier version left an empty record and this node has none of its files)
        let record_empty = std::fs::read(&mf).map(|b| serde_json::from_slice::<std::collections::BTreeMap<String, Value>>(&b).map(|m| m.is_empty()).unwrap_or(true)).unwrap_or(true);
        let none_here = std::fs::read_dir(&root).map(|mut r| r.next().is_none()).unwrap_or(true);
        if !mf.exists() || (record_empty && none_here) {
            let theirs = crate::livetarget::list(&self.link, to, name).await?;
            for (rel, _) in &theirs {
                crate::livetarget::get(&self.link, to, name, rel, &root).await?;
            }
            // recorded as there already: nothing goes back that just came from there
            let mut m: std::collections::BTreeMap<String, (u64, i64, i64)> = std::collections::BTreeMap::new();
            for (rel, _) in &theirs {
                if let Ok(meta) = std::fs::metadata(root.join(rel)) {
                    use std::os::unix::fs::MetadataExt;
                    m.insert(rel.clone(), (meta.len(), meta.mtime(), meta.mtime_nsec()));
                }
            }
            let _ = std::fs::create_dir_all(mf.parent().expect("a folder"));
            std::fs::write(&mf, serde_json::to_vec(&m).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            println!("{}: web: {name} taken over from {}: {} files", self.me, to.0, theirs.len());
        }
        let walked = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || {
                let mut out = std::collections::BTreeMap::new();
                let mut todo = vec![root.clone()];
                while let Some(d) = todo.pop() {
                    let Ok(rd) = std::fs::read_dir(&d) else { continue };
                    for e in rd.flatten() {
                        let path = e.path();
                        let Ok(meta) = e.metadata() else { continue };
                        if meta.is_dir() {
                            todo.push(path);
                        } else if meta.is_file() {
                            use std::os::unix::fs::MetadataExt;
                            if let Ok(rel) = path.strip_prefix(&root) {
                                out.insert(rel.to_string_lossy().to_string(), (meta.len(), meta.mtime(), meta.mtime_nsec()));
                            }
                        }
                    }
                }
                out
            })
            .await
            .map_err(|e| e.to_string())?
        };
        let mut manifest: std::collections::BTreeMap<String, (u64, i64, i64)> =
            std::fs::read(&mf).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let save = |m: &std::collections::BTreeMap<String, (u64, i64, i64)>| {
            let _ = std::fs::create_dir_all(mf.parent().expect("a folder"));
            if let Ok(b) = serde_json::to_vec(m) {
                let tmp = mf.with_extension("json.new");
                if std::fs::write(&tmp, b).is_ok() {
                    let _ = std::fs::rename(&tmp, &mf);
                }
            }
        };
        let mut sent = 0;
        for (rel, seen) in &walked {
            if manifest.get(rel) == Some(seen) {
                continue;
            }
            let head = json!({ "op": "put", "site": name, "path": rel });
            crate::livetarget::send(&self.link, to, crate::livetarget::WEB_MIRROR, head, Some(&root.join(rel))).await?;
            manifest.insert(rel.clone(), *seen);
            sent += 1;
            if sent % 200 == 0 {
                save(&manifest);
            }
        }
        let gone: Vec<String> = manifest.keys().filter(|k| !walked.contains_key(*k)).cloned().collect();
        for rel in &gone {
            crate::livetarget::send(&self.link, to, crate::livetarget::WEB_MIRROR, json!({ "op": "delete", "site": name, "path": rel }), None).await?;
            manifest.remove(rel);
        }
        save(&manifest);
        if sent + gone.len() > 0 {
            println!("{}: web: {name} copied to {}: {sent} sent, {} removed", self.me, to.0, gone.len());
        }
        Ok(())
    }
}
