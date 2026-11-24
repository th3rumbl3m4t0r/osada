//! The containers of sites (D-U23) on the node a site runs on.
//!
//! The node runs as an unprivileged user; podman needs root. So, as with
//! `osada rescue`, the node writes files and a root agent acts on them:
//!
//! - `<data>/web/containers.json`: what should run, by site (the node writes
//!   it from node control, `write_wanted`);
//! - `<data>/web/actions/<id>.json`: `{"site", "action": "upgrade"|"restart"}`
//!   (the portal's buttons, through the node);
//! - `osada web-agent --data <data>` (root, `osada-web-agent.service`) makes
//!   podman so every few seconds: `osada-web-<site>`, restarted always,
//!   published on 127.0.0.1 only, named volumes, and host paths only for osada
//!   folders (Round 18: under `<osada data>/users` or `/groups`, the node
//!   folder's parent, as the node resolved them), no
//!   privileges; it removes its containers whose site is gone (their volumes
//!   stay), and writes `<data>/web/ports.json` (the port each is published on,
//!   which websrv proxies to) and `<data>/web/containers-status.json`.
//!
//! The agent checks every entry again (it trusts the file no more than the
//! node's user).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    placement::{NodeId, Placement},
    sites::{self, Container, Content},
};

/// The first port the agent publishes containers on (127.0.0.1).
const FIRST_PORT: u16 = 18100;
const LAST_PORT: u16 = 18999;

fn web(data: &Path) -> PathBuf {
    data.join("web")
}

/// A container as the agent runs it: as node control has it, with its osada folders where the
/// node found them on its disk (Round 18), or why it may not run now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    #[serde(flatten)]
    pub c: Container,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binds: Vec<Bind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

/// A osada folder at a path on this node, at `to` in the container.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bind {
    pub host: String,
    pub to: String,
    #[serde(default)]
    pub write: bool,
}

/// Where a osada folder is on this node's disk, or why not (the node with osada's files).
pub type Resolve<'a> = &'a dyn Fn(&sites::Link) -> Result<PathBuf, String>;

/// What should run on `me`: the containers of the sites that run here, their osada folders
/// found with `resolve` (None: osada's files are not here).
pub fn wanted(p: &Placement, me: &NodeId, resolve: Option<Resolve>) -> BTreeMap<String, Run> {
    sites::sites(p)
        .filter(|(_, m, _)| &m.node == me)
        .filter_map(|(n, _, s)| match &s.content {
            Content::Container(c) => {
                let mut run = Run { c: c.clone(), binds: Vec::new(), blocked: None };
                for f in &c.folders {
                    let found = match resolve {
                        Some(r) => r(&f.from),
                        None => Err("osada's files are not on this node".into()),
                    };
                    match found {
                        Ok(host) => run.binds.push(Bind { host: host.display().to_string(), to: f.to.trim_end_matches('/').to_string(), write: f.write }),
                        Err(why) => {
                            run.blocked = Some(format!("its folder {}: {why}", f.from.label()));
                            run.binds.clear();
                            break;
                        }
                    }
                }
                Some((n.clone(), run))
            }
            _ => None,
        })
        .collect()
}

/// Writes what should run, when it changed (atomically, readable by root).
pub fn write_wanted(data: &Path, w: &BTreeMap<String, Run>) -> std::io::Result<bool> {
    let dir = web(data);
    std::fs::create_dir_all(dir.join("actions"))?;
    let body = serde_json::to_vec_pretty(w).map_err(std::io::Error::other)?;
    let file = dir.join("containers.json");
    if std::fs::read(&file).is_ok_and(|b| b == body) {
        return Ok(false);
    }
    let tmp = dir.join(".containers.json.tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &file)?;
    Ok(true)
}

/// Asks the agent for an action on a site's container.
pub fn request(data: &Path, site: &str, action: &str) -> Result<String, String> {
    if !matches!(action, "upgrade" | "restart" | "run") {
        return Err(format!("no action {action}"));
    }
    let dir = web(data).join("actions");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let id = format!("{}-{}", SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis()), rand::random::<u32>());
    let body = json!({ "site": site, "action": action });
    std::fs::write(dir.join(format!("{id}.json")), body.to_string()).map_err(|e| e.to_string())?;
    Ok(id)
}

/// What the agent last wrote about the containers (state, image, last action).
pub fn status(data: &Path) -> Value {
    let mut v: Value = std::fs::read(web(data).join("containers-status.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    // and the jobs' (Round 18b)
    if let Some(j) = std::fs::read(web(data).join("jobs-status.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        if !v.is_object() {
            v = json!({});
        }
        v["jobs"] = j["jobs"].clone();
    }
    v
}

/// Whether the agent would run this container (the same checks as node
/// control's, again).
fn acceptable(site: &str, c: &Container) -> Result<(), String> {
    if !crate::placement::valid_module_name(site) {
        return Err(format!("{site:?} is not a site's name"));
    }
    let mut p = Placement::default();
    let me = NodeId("agent".into());
    let mut n = crate::placement::Node::new([crate::placement::Role::Worker, crate::placement::Role::EdgeHttp], []);
    n.name = None;
    p.nodes.insert(me.clone(), n);
    let mut m = crate::placement::Module::new(crate::placement::ModuleKind::WEB, "", me);
    // its exit node is another node of the cluster (Round 19): node control checked it, this
    // placement has no other
    let c = Container { egress: None, ..c.clone() };
    m.site = Some(sites::Site { names: vec!["agent.invalid".into()], exposed_on: vec![], content: Content::Container(c), routes: vec![], front: sites::Front::Tls, ou: None, ..Default::default() });
    match sites::check(site, &m, &p) {
        None => Ok(()),
        Some(why) => Err(why),
    }
}

/// The osada folders a container may have (Round 18): each a folder under `<osada data>/users` or
/// `/groups` (the node folder's parent) as it really is (no link leads out), with no `:` (podman's
/// separator), at the path inside its folder says. Their real paths.
fn binds_ok(data: &Path, r: &Run) -> Result<Vec<Bind>, String> {
    if r.binds.len() != r.c.folders.len() {
        return Err("its osada folders were not all found".into());
    }
    let osada = data.parent().ok_or("no osada data folder")?;
    let roots: Vec<PathBuf> = ["users", "groups"].iter().filter_map(|d| std::fs::canonicalize(osada.join(d)).ok()).collect();
    let mut out = Vec::new();
    for (b, f) in r.binds.iter().zip(&r.c.folders) {
        if b.to != f.to.trim_end_matches('/') || b.write != f.write {
            return Err(format!("{}: not as node control says", b.to));
        }
        let real = std::fs::canonicalize(&b.host).map_err(|e| format!("{}: {e}", b.host))?;
        if !real.is_dir() || !roots.iter().any(|root| real.starts_with(root) && real != *root) {
            return Err(format!("{} is not a osada folder", b.host));
        }
        let host = real.to_str().filter(|h| !h.contains([':', '\n', '\0'])).ok_or_else(|| format!("{}: a path with : cannot be given to podman", real.display()))?;
        out.push(Bind { host: host.to_string(), to: b.to.clone(), write: b.write });
    }
    Ok(out)
}

/// The podman arguments that make the container (its spec's hash in a label).
pub fn run_args(site: &str, c: &Container, host_port: u16) -> Vec<String> {
    run_args_with(site, c, &[], host_port, None)
}

/// The same, with osada folders (checked: [`binds_ok`]).
/// `data`: the agent's node folder, in a label (another agent on the host, e.g. a test's, leaves
/// its containers alone).
pub fn run_args_with(site: &str, c: &Container, binds: &[Bind], host_port: u16, data: Option<&Path>) -> Vec<String> {
    let spec = spec_hash(c, binds, host_port);
    let mut a = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        sites::container_name(site),
        "--label".into(),
        format!("shelf.site={site}"),
        "--label".into(),
        format!("shelf.spec={spec}"),
    ];
    if let Some(d) = data {
        a.push("--label".into());
        a.push(format!("shelf.data={}", d.display()));
    }
    a.extend::<Vec<String>>(vec![
        "--restart=always".into(),
        "--security-opt=no-new-privileges".into(),
        "--cap-drop=NET_RAW".into(),
        "-p".into(),
        format!("127.0.0.1:{host_port}:{}", c.port),
    ]);
    for v in &c.volumes {
        a.push("-v".into());
        a.push(v.clone());
    }
    // osada folders, as they are (not relabelled: SELinux lets containers in by policy,
    // deploy/selinux/osada-containers.te)
    let mut groups: Vec<u32> = Vec::new();
    for b in binds {
        a.push("-v".into());
        a.push(format!("{}:{}:{}", b.host, b.to, if b.write { "rw" } else { "ro" }));
        // the folder's group (osada's: its files are 0640), for a container that runs as a user of its own
        use std::os::unix::fs::MetadataExt;
        if let Ok(g) = std::fs::metadata(&b.host).map(|m| m.gid()) {
            if g != 0 && !groups.contains(&g) {
                groups.push(g);
                a.push(format!("--group-add={g}"));
            }
        }
    }
    // an exit node (Round 19): the proxy in its own network, before its own environment
    if c.egress.is_some() {
        for (k, v) in crate::egress::proxy_env(&[]) {
            a.push("-e".into());
            a.push(format!("{k}={v}"));
        }
    }
    for (k, v) in &c.env {
        a.push("-e".into());
        a.push(format!("{k}={v}"));
    }
    a.push(c.image.clone());
    a
}

/// Starts a container made by `args` (`run -d …`) with its egress proxy in its network before
/// its process runs (Round 19): podman create, init (its network made), the agent's listener in
/// it, start. A proxy that could not be made is said; the container starts all the same (its
/// proxied connections fail rather than leave by this node).
fn start_with_proxy(args: &[String], name: &str, data: &Path, from: &str, egress: &mut crate::egress::Agent) -> Result<String, String> {
    let mut create: Vec<String> = vec!["create".into()];
    create.extend(args.iter().skip(2).cloned());
    podman_owned(&create)?;
    podman(&["init", name])?;
    let pid: i64 = podman(&["inspect", "--format", "{{.State.Pid}}", name])?.trim().parse().unwrap_or(0);
    let proxy = egress.ensure(data, from, pid);
    podman(&["start", name])?;
    match proxy {
        Ok(_) => Ok(format!("its proxy at 127.0.0.1:{}", crate::egress::PORT)),
        Err(why) => Err(format!("started, but {why}")),
    }
}

fn spec_hash(c: &Container, binds: &[Bind], host_port: u16) -> String {
    // as before when there are no osada folders (no container made anew for nothing)
    let j = if binds.is_empty() { json!({ "c": c, "port": host_port }) } else { json!({ "c": c, "port": host_port, "binds": binds }) }.to_string();
    hex::encode(&Sha256::digest(j.as_bytes())[..8])
}

/// A container podman has, as the agent sees it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Have {
    name: String,
    state: String,
    spec: String,
    image: String,
    image_id: String,
    /// Its process (0 when not running): the network namespace of its egress (Round 19).
    #[serde(default)]
    pid: i64,
}

fn podman(args: &[&str]) -> Result<String, String> {
    let (code, out, err) = spawn_capture("podman", args).map_err(|e| format!("podman: {e}"))?;
    if code == 0 {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        Err(String::from_utf8_lossy(&err).lines().last().unwrap_or("podman failed").to_string())
    }
}

/// Runs `prog` (found on PATH) with `args`, stdin from /dev/null, stdout and
/// stderr captured: its exit code and both. Not std's Command: it makes the
/// binary need glibc 2.39 (pidfd_spawnp), and edge1 (Rocky 9) has 2.34.
pub fn spawn_capture(prog: &str, args: &[&str]) -> std::io::Result<(i32, Vec<u8>, Vec<u8>)> {
    use std::{ffi::CString, io::Read, os::fd::FromRawFd};
    unsafe extern "C" {
        static environ: *const *mut libc::c_char;
    }
    let cstr = |s: &str| CString::new(s).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "a NUL in an argument"));
    let cprog = cstr(prog)?;
    let cargs: Vec<CString> = std::iter::once(prog).chain(args.iter().copied()).map(cstr).collect::<Result<_, _>>()?;
    let mut argv: Vec<*mut libc::c_char> = cargs.iter().map(|c| c.as_ptr() as *mut libc::c_char).collect();
    argv.push(std::ptr::null_mut());
    let devnull = cstr("/dev/null")?;
    let (mut out, mut err) = ([0i32; 2], [0i32; 2]);
    // SAFETY: plain libc calls on fds and buffers owned here; every fd is closed or handed to a File.
    unsafe {
        if libc::pipe2(out.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::pipe2(err.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            let e = std::io::Error::last_os_error();
            libc::close(out[0]);
            libc::close(out[1]);
            return Err(e);
        }
        let mut fa: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut fa);
        libc::posix_spawn_file_actions_addopen(&mut fa, 0, devnull.as_ptr(), libc::O_RDONLY, 0);
        libc::posix_spawn_file_actions_adddup2(&mut fa, out[1], 1);
        libc::posix_spawn_file_actions_adddup2(&mut fa, err[1], 2);
        let mut pid: libc::pid_t = 0;
        let rc = libc::posix_spawnp(&mut pid, cprog.as_ptr(), &fa, std::ptr::null(), argv.as_ptr(), environ);
        libc::posix_spawn_file_actions_destroy(&mut fa);
        libc::close(out[1]);
        libc::close(err[1]);
        if rc != 0 {
            libc::close(out[0]);
            libc::close(err[0]);
            return Err(std::io::Error::from_raw_os_error(rc));
        }
        let mut fo = std::fs::File::from_raw_fd(out[0]);
        let mut fe = std::fs::File::from_raw_fd(err[0]);
        let reader = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = fe.read_to_end(&mut v);
            v
        });
        let mut o = Vec::new();
        let _ = fo.read_to_end(&mut o);
        let e = reader.join().unwrap_or_default();
        let mut status = 0;
        while libc::waitpid(pid, &mut status, 0) == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {}
        let code = if libc::WIFEXITED(status) { libc::WEXITSTATUS(status) } else { -1 };
        Ok((code, o, e))
    }
}

fn podman_owned(args: &[String]) -> Result<String, String> {
    podman(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

/// osada's containers on this host, by site.
/// The host ports podman's containers have (all of them, whoever made them).
fn podman_ports() -> Vec<u16> {
    let out = podman(&["ps", "-a", "--format", "json"]).unwrap_or_default();
    host_ports(&out)
}

fn host_ports(json_out: &str) -> Vec<u16> {
    let list: Vec<Value> = serde_json::from_str(json_out).unwrap_or_default();
    list.iter()
        .flat_map(|c| c["Ports"].as_array().cloned().unwrap_or_default())
        .filter_map(|p| p["host_port"].as_u64().and_then(|x| u16::try_from(x).ok()))
        .collect()
}

/// The containers of this agent's (`data`: its node folder; those made before the label are its).
fn podman_have(data: &Path) -> Result<BTreeMap<String, Have>, String> {
    let out = podman(&["ps", "-a", "--filter", "label=shelf.site", "--format", "json"])?;
    let list: Vec<Value> = serde_json::from_str(&out).unwrap_or_default();
    let me = data.display().to_string();
    Ok(list
        .into_iter()
        .filter_map(|c| {
            let labels = &c["Labels"];
            if labels["shelf.data"].as_str().is_some_and(|d| d != me) {
                return None;
            }
            let site = labels["shelf.site"].as_str()?.to_string();
            Some((site, Have {
                name: c["Names"][0].as_str().unwrap_or_default().to_string(),
                state: c["State"].as_str().unwrap_or_default().to_string(),
                spec: labels["shelf.spec"].as_str().unwrap_or_default().to_string(),
                image: c["Image"].as_str().unwrap_or_default().to_string(),
                image_id: c["ImageID"].as_str().unwrap_or_default().to_string(),
                pid: c["Pid"].as_i64().unwrap_or(0),
            }))
        })
        .collect())
}

fn read_json<T: for<'a> Deserialize<'a> + Default>(file: &Path) -> T {
    std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Writes a file the node's user reads: owned like the data folder.
fn write_for_node(data: &Path, file: &Path, v: &Value) {
    let tmp = file.with_extension("tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(v).unwrap_or_default()).is_ok() {
        if let Ok(meta) = std::fs::metadata(data) {
            use std::os::unix::fs::MetadataExt;
            let _ = std::os::unix::fs::chown(&tmp, Some(meta.uid()), Some(meta.gid()));
        }
        let _ = std::fs::rename(&tmp, file);
    }
}

/// One round of the agent: podman made as wanted, actions done, status written.
pub fn agent_round(data: &Path, log: &mut BTreeMap<String, Value>, egress: &mut crate::egress::Agent) {
    let dir = web(data);
    let want: BTreeMap<String, Run> = read_json(&dir.join("containers.json"));
    let mut ports: BTreeMap<String, u16> = read_json(&dir.join("ports.json"));
    ports.retain(|s, _| want.contains_key(s));
    let mut note = |site: &str, what: String| {
        // the same again (a container that may not run, every round): said once
        if log.get(site).and_then(|v| v["what"].as_str()) == Some(what.as_str()) {
            return;
        }
        eprintln!("web-agent: {site}: {what}");
        log.insert(site.to_string(), json!({ "at": SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()), "what": what }));
    };
    // nothing to run here (a node without containers, maybe without podman): leave podman alone
    if want.is_empty() && !dir.join("ports.json").exists() {
        return;
    }
    let mut have = match podman_have(data) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("web-agent: {e}");
            return;
        }
    };
    // gone from node control: removed (volumes stay)
    for (site, h) in &have {
        if !want.contains_key(site) {
            let r = podman(&["rm", "-f", &h.name]);
            note(site, format!("removed ({})", r.err().unwrap_or_else(|| "ok".into())));
        }
    }
    // actions the portal asked for
    let mut actions: Vec<PathBuf> = std::fs::read_dir(dir.join("actions"))
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json") && !p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("format-"))).collect())
        .unwrap_or_default();
    actions.sort();
    let mut upgrade = Vec::new();
    for a in actions {
        let v: Value = read_json(&a);
        // a job's run now is the jobs round's (Round 18b)
        if v["action"] == "run" {
            continue;
        }
        let _ = std::fs::remove_file(&a);
        let (Some(site), Some(action)) = (v["site"].as_str(), v["action"].as_str()) else { continue };
        if !want.contains_key(site) {
            continue;
        }
        match action {
            "restart" => {
                let r = podman(&["restart", &sites::container_name(site)]);
                note(site, format!("restarted: {}", r.err().unwrap_or_else(|| "ok".into())));
            }
            "upgrade" => upgrade.push(site.to_string()),
            _ => {}
        }
    }
    for (site, r) in &want {
        let c = &r.c;
        // not to run now (a osada folder not there), or not as it should be: not left running
        let ok = match &r.blocked {
            Some(why) => Err(why.clone()),
            None => acceptable(site, c).and_then(|()| binds_ok(data, r)),
        };
        let binds = match ok {
            Err(why) => {
                if have.contains_key(site) {
                    let _ = podman(&["rm", "-f", &sites::container_name(site)]);
                    let _ = podman(&["rm", "-f", &sites::legacy_container_name(site)]);
                    have.remove(site);
                }
                note(site, format!("not run: {why}"));
                continue;
            }
            Ok(b) => b,
        };
        let port = match ports.get(site) {
            Some(p) => *p,
            None => {
                // rootful podman forwards a port without listening on it (nftables): the ports its
                // containers have, every agent's, are taken too (Round 18: a test's agent took one)
                let mut used: Vec<u16> = ports.values().copied().collect();
                used.extend(podman_ports());
                let Some(p) = (FIRST_PORT..=LAST_PORT).find(|p| !used.contains(p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok()) else {
                    note(site, "no free port".into());
                    continue;
                };
                ports.insert(site.clone(), p);
                p
            }
        };
        let name = sites::container_name(site);
        let mut old_image = None;
        if upgrade.contains(site) {
            match podman(&["pull", "-q", &c.image]) {
                Ok(id) => {
                    let id = id.trim().to_string();
                    let cur = have.get(site).map(|h| h.image_id.clone()).unwrap_or_default();
                    if !cur.is_empty() && !id.starts_with(&cur) && !cur.starts_with(&id) {
                        old_image = Some(cur);
                        let _ = podman(&["rm", "-f", &name]);
                        let _ = podman(&["rm", "-f", &sites::legacy_container_name(site)]);
                        have.remove(site);
                        note(site, format!("upgrading to {}", &id[..id.len().min(12)]));
                    } else {
                        note(site, "upgrade: already the newest image".into());
                    }
                }
                Err(e) => note(site, format!("upgrade: pull failed: {e}")),
            }
        }
        let spec = spec_hash(c, &binds, port);
        match have.get(site) {
            Some(h) if h.spec == spec && h.state == "running" => {}
            Some(h) if h.spec == spec => {
                let r = podman(&["start", &name]);
                note(site, format!("started: {}", r.err().unwrap_or_else(|| "ok".into())));
            }
            other => {
                if other.is_some() {
                    let _ = podman(&["rm", "-f", &name]);
                        let _ = podman(&["rm", "-f", &sites::legacy_container_name(site)]);
                }
                let args = run_args_with(site, c, &binds, port, Some(data));
                let r = if c.egress.is_some() { start_with_proxy(&args, &name, data, &format!("site:{site}"), egress).map(|_| ()) } else { podman_owned(&args).map(|_| ()) };
                note(site, format!("{} on 127.0.0.1:{port}: {}", if other.is_some() { "made anew" } else { "made" }, r.err().unwrap_or_else(|| "ok".into())));
            }
        }
        if let Some(old) = old_image {
            let _ = podman(&["rmi", &old]);
        }
    }
    write_for_node(data, &dir.join("ports.json"), &json!(ports));
    let now = podman_have(data).unwrap_or_default();
    // the egress proxies (Round 19): one in each running container with an exit node (again after
    // a restart: a new network), none in the others
    let mut proxied = Vec::new();
    for (site, r) in &want {
        let Some(h) = now.get(site).filter(|h| r.c.egress.is_some() && h.state == "running" && h.pid > 0) else { continue };
        let from = format!("site:{site}");
        match egress.ensure(data, &from, h.pid) {
            Ok(true) => note(site, format!("its proxy at 127.0.0.1:{} (outgoing connections exit by {})", crate::egress::PORT, r.c.egress.as_ref().map_or("?", |e| e.0.as_str()))),
            Ok(false) => {}
            Err(why) => note(site, format!("its proxy: {why}")),
        }
        proxied.push(from);
    }
    egress.keep_only("site:", &proxied);
    let status: BTreeMap<&String, Value> = want
        .keys()
        .map(|s| {
            let h = now.get(s);
            (s, json!({
                "container": sites::container_name(s),
                "state": h.map_or("missing", |h| h.state.as_str()),
                "image": h.map(|h| h.image.clone()),
                "image_id": h.map(|h| h.image_id.chars().take(12).collect::<String>()),
                "port": ports.get(s),
                "last": log.get(s.as_str()),
            }))
        })
        .collect();
    write_for_node(data, &dir.join("containers-status.json"), &json!({ "at": SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()), "sites": status }));
}

/// `osada web-agent --data <data>`: rounds every 5 s until stopped.
pub fn agent(data: &Path) -> ! {
    let mut log = BTreeMap::new();
    let mut jobs = JobState::load(data);
    let mut egress = crate::egress::Agent::default();
    let mut disk_results = Vec::new();
    let mut stalwart_at = std::time::Instant::now() - Duration::from_secs(3600);
    let _ = std::fs::create_dir_all(web(data));
    eprintln!("web-agent: containers of {}", web(data).display());
    loop {
        // Stalwart's DNS records and state (T11, T12), every 10 minutes where configured
        if stalwart_at.elapsed() > Duration::from_secs(600) {
            stalwart_at = std::time::Instant::now();
            crate::nodeinfo::stalwart_round(data);
        }
        // disks first (T10): a file system asked for, then the heartbeat
        crate::nodeinfo::agent_round(data, &mut disk_results);
        // the node's files folder (D-U41)
        crate::filesdir::agent_round(data);
        agent_round(data, &mut log, &mut egress);
        jobs_round(data, &mut jobs, &mut egress);
        std::thread::sleep(Duration::from_secs(5));
    }
}

// ---- jobs (Round 18b) ---------------------------------------------------------------

/// The jobs whose node `me` is.
pub fn jobs_wanted(p: &Placement, me: &NodeId) -> BTreeMap<String, sites::Job> {
    sites::sites(p)
        .filter(|(_, m, _)| &m.node == me)
        .filter_map(|(n, _, s)| match &s.content {
            Content::Job(j) => Some((n.clone(), j.clone())),
            _ => None,
        })
        .collect()
}

/// Writes the jobs for the agent (`web/jobs.json`), when they changed.
pub fn write_jobs(data: &Path, w: &BTreeMap<String, sites::Job>) -> std::io::Result<bool> {
    let dir = web(data);
    std::fs::create_dir_all(dir.join("actions"))?;
    let body = serde_json::to_vec_pretty(w).map_err(std::io::Error::other)?;
    let file = dir.join("jobs.json");
    if std::fs::read(&file).is_ok_and(|b| b == body) {
        return Ok(false);
    }
    let tmp = dir.join(".jobs.json.tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &file)?;
    Ok(true)
}

/// What the agent keeps of its jobs between rounds: the minute each last started in (also in
/// jobs-status.json, so that a restart within that minute does not run it again), and the last
/// run of each.
#[derive(Default)]
pub struct JobState {
    started_minute: BTreeMap<String, i64>,
    last: BTreeMap<String, Value>,
}

impl JobState {
    pub fn load(data: &Path) -> JobState {
        let v: Value = read_json(&web(data).join("jobs-status.json"));
        let mut st = JobState::default();
        for (name, j) in v["jobs"].as_object().into_iter().flatten() {
            if let Some(m) = j["started_minute"].as_i64() {
                st.started_minute.insert(name.clone(), m);
            }
            if !j["last"].is_null() {
                st.last.insert(name.clone(), j["last"].clone());
            }
        }
        st
    }
}

/// A job's container, as podman has it.
struct JobRun {
    name: String,
    pid: i64,
    running: bool,
    started: i64,
    exit: i64,
}

fn job_container(job: &str) -> String {
    format!("osada-job-{job}")
}

/// This agent's job containers (`data`: its node folder), by job.
fn podman_jobs(data: &Path) -> Result<BTreeMap<String, JobRun>, String> {
    let out = podman(&["ps", "-a", "--filter", "label=shelf.job", "--format", "json"])?;
    let list: Vec<Value> = serde_json::from_str(&out).unwrap_or_default();
    let me = data.display().to_string();
    Ok(list
        .into_iter()
        .filter(|c| c["Labels"]["shelf.data"].as_str() == Some(me.as_str()))
        .filter_map(|c| {
            let job = c["Labels"]["shelf.job"].as_str()?.to_string();
            Some((job, JobRun {
                name: c["Names"][0].as_str().unwrap_or_default().to_string(),
                pid: c["Pid"].as_i64().unwrap_or(0),
                running: c["State"].as_str() == Some("running") || c["State"].as_str() == Some("created") || c["State"].as_str() == Some("initialized"),
                started: c["StartedAt"].as_i64().unwrap_or(0),
                exit: c["ExitCode"].as_i64().unwrap_or(-1),
            }))
        })
        .collect())
}

/// The owner of the node's data folder (the node's account): jobs run as it, files are its.
fn data_owner(data: &Path) -> (u32, u32) {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(data).map(|m| (m.uid(), m.gid())).unwrap_or((65534, 65534))
}

fn chown_to(path: &Path, owner: (u32, u32)) {
    let _ = std::os::unix::fs::chown(path, Some(owner.0), Some(owner.1));
}

/// A job run's last lines, what it wrote to stdout and stderr (podman logs gives them on its own
/// stdout and stderr), at most 6000 characters.
fn job_log(name: &str) -> String {
    let Ok((_, out, err)) = spawn_capture("podman", &["logs", "--tail", "40", name]) else { return String::new() };
    let mut t = String::from_utf8_lossy(&out).to_string();
    let e = String::from_utf8_lossy(&err);
    if !e.trim().is_empty() {
        if !t.is_empty() && !t.ends_with('\n') {
            t.push('\n');
        }
        t.push_str(&e);
    }
    let n = t.chars().count();
    if n > 6000 { t.chars().skip(n - 6000).collect() } else { t }
}

/// The podman arguments that start a job's run.
pub fn job_args(data: &Path, job: &str, j: &sites::Job, owner: (u32, u32)) -> Vec<String> {
    let dir = web(data);
    let mut a: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        job_container(job),
        "--label".into(),
        format!("shelf.job={job}"),
        "--label".into(),
        format!("shelf.data={}", data.display()),
        "--user".into(),
        format!("{}:{}", owner.0, owner.1),
        "--security-opt=no-new-privileges".into(),
        "--cap-drop=all".into(),
        "-v".into(),
        format!("{}:/job:rw", dir.join(job).display()),
        "-v".into(),
        format!("{}:/out:rw", dir.join(".jobs-out").join(job).display()),
        "-w".into(),
        "/job".into(),
    ];
    // an exit node (Round 19): the proxy, but for the names it finds at addresses of its own
    if j.egress.is_some() {
        let direct: Vec<String> = j.hosts.keys().cloned().collect();
        for (k, v) in crate::egress::proxy_env(&direct) {
            a.push("-e".into());
            a.push(format!("{k}={v}"));
        }
    }
    for (k, v) in &j.env {
        a.push("-e".into());
        a.push(format!("{k}={v}"));
    }
    for (name, ip) in &j.hosts {
        a.push(format!("--add-host={name}:{ip}"));
    }
    a.push(j.image.clone());
    a.extend(j.command.iter().cloned());
    a
}

/// A file a job left in /out, into the site's folder (not through a link, not hidden), as the
/// node's account's; the mode of the file it replaces, else 0640 (as osada's other files).
fn put_output(data: &Path, job: &str, w: &sites::JobWrite, owner: (u32, u32)) -> Result<u64, String> {
    use std::os::unix::fs::PermissionsExt;
    let src = web(data).join(".jobs-out").join(job).join(&w.file);
    let m = std::fs::symlink_metadata(&src).map_err(|_| format!("it left no {}", w.file))?;
    if !m.is_file() {
        return Err(format!("{} is not a file", w.file));
    }
    if m.len() > 256 << 20 {
        return Err(format!("{}: over 256 MiB", w.file));
    }
    if !crate::placement::valid_module_name(&w.site) || !sites::valid_write_path(&w.path) {
        return Err(format!("{}:{} is not a site's file", w.site, w.path));
    }
    // the site's folder, made (the node's account's) when it has none yet
    let site_dir = web(data).join(&w.site);
    if std::fs::symlink_metadata(&site_dir).is_err() && std::fs::create_dir_all(&site_dir).is_ok() {
        chown_to(&site_dir, owner);
    }
    let root = std::fs::canonicalize(&site_dir).map_err(|e| format!("{}'s folder: {e}", w.site))?;
    let dest = root.join(w.path.trim_start_matches('/'));
    let parent = dest.parent().ok_or("no folder")?;
    if std::fs::symlink_metadata(parent).is_err() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        // folders made on the way are the node's account's too
        let mut d = parent.to_path_buf();
        while d.starts_with(&root) && d != root {
            chown_to(&d, owner);
            if !d.pop() {
                break;
            }
        }
    }
    if !std::fs::canonicalize(parent).is_ok_and(|p| p.starts_with(&root)) {
        return Err(format!("{}:{} leads out of its folder", w.site, w.path));
    }
    let mode = match std::fs::symlink_metadata(&dest) {
        Ok(d) if d.file_type().is_symlink() || d.is_dir() => return Err(format!("{}:{} is a link or a folder", w.site, w.path)),
        Ok(d) => d.permissions().mode() & 0o777,
        Err(_) => 0o640,
    };
    let tmp = parent.join(format!(".{}.job-tmp", dest.file_name().and_then(|f| f.to_str()).unwrap_or("x")));
    let n = std::fs::copy(&src, &tmp).map_err(|e| format!("{}: {e}", w.file))?;
    chown_to(&tmp, owner);
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    std::fs::rename(&tmp, &dest).map_err(|e| format!("{}:{}: {e}", w.site, w.path))?;
    Ok(n)
}

/// One round of the jobs: a finished run's files put where they go, a run past its time
/// stopped, a run started when its minute has come (or asked for: run now); status written.
pub fn jobs_round(data: &Path, st: &mut JobState, egress: &mut crate::egress::Agent) {
    let dir = web(data);
    let want: BTreeMap<String, sites::Job> = read_json(&dir.join("jobs.json"));
    if want.is_empty() && st.last.is_empty() && st.started_minute.is_empty() {
        return;
    }
    let mut have = match podman_jobs(data) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("web-agent: jobs: {e}");
            return;
        }
    };
    let owner = data_owner(data);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let minute = now - now.rem_euclid(60);
    // gone from node control: its run stopped
    for (job, h) in &have {
        if !want.contains_key(job) {
            let _ = podman(&["rm", "-f", &h.name]);
            eprintln!("web-agent: job {job}: removed");
        }
    }
    have.retain(|j, _| want.contains_key(j));
    st.last.retain(|j, _| want.contains_key(j));
    st.started_minute.retain(|j, _| want.contains_key(j));
    // run now, from the portal
    let mut asked = Vec::new();
    for a in std::fs::read_dir(dir.join("actions")).map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>()).unwrap_or_default() {
        if a.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let v: Value = read_json(&a);
        if v["action"] == "run" {
            let _ = std::fs::remove_file(&a);
            if let Some(job) = v["site"].as_str().filter(|j| want.contains_key(*j)) {
                asked.push(job.to_string());
            }
        }
    }
    for (job, j) in &want {
        let cron = match crate::cron::Cron::parse(&j.schedule) {
            Ok(c) => c,
            Err(why) => {
                st.last.insert(job.clone(), json!({ "at": now, "result": format!("not run: {why}") }));
                continue;
            }
        };
        if let Some(h) = have.get(job) {
            if h.running {
                if h.started > 0 && now - h.started > i64::from(j.timeout()) {
                    let tail = job_log(&h.name);
                    let _ = podman(&["rm", "-f", &h.name]);
                    eprintln!("web-agent: job {job}: stopped after {} s", now - h.started);
                    st.last.insert(job.clone(), json!({ "at": now, "started": h.started, "seconds": now - h.started, "result": format!("stopped: over {} s", j.timeout()), "log": tail }));
                }
                continue;
            }
            // finished: its files where they go when it ended well, then the container away
            let tail = job_log(&h.name);
            let mut written = Vec::new();
            let mut problems = Vec::new();
            if h.exit == 0 {
                for w in &j.writes {
                    match put_output(data, job, w, owner) {
                        Ok(n) => written.push(json!({ "site": w.site, "path": w.path, "bytes": n })),
                        Err(why) => problems.push(why),
                    }
                }
            }
            let _ = podman(&["rm", "-f", &h.name]);
            let result = if h.exit != 0 { format!("failed (exit {})", h.exit) } else if problems.is_empty() { "done".to_string() } else { format!("done; {}", problems.join("; ")) };
            eprintln!("web-agent: job {job}: {result}");
            st.last.insert(job.clone(), json!({ "at": now, "started": h.started, "seconds": now - h.started, "exit": h.exit, "result": result, "written": written, "log": tail }));
            have.remove(job);
        }
        let due = cron.matches_at(now) && st.started_minute.get(job) != Some(&minute);
        if due || asked.contains(job) {
            let jd = dir.join(job);
            let out = dir.join(".jobs-out").join(job);
            let _ = std::fs::create_dir_all(&jd);
            let _ = std::fs::remove_dir_all(&out);
            let _ = std::fs::create_dir_all(&out);
            for d in [dir.join(".jobs-out"), out.clone(), jd.clone()] {
                // its own folder stays as the node made it; out and the folder are the node's account's
                if std::fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir()) {
                    chown_to(&d, owner);
                }
            }
            st.started_minute.insert(job.clone(), minute);
            let args = job_args(data, job, j, owner);
            let r = if j.egress.is_some() {
                // its proxy before its process (Round 19); a run whose proxy failed runs (its log says)
                match start_with_proxy(&args, &job_container(job), data, &format!("job:{job}"), egress) {
                    Err(why) if why.starts_with("started") => {
                        eprintln!("web-agent: job {job}: {why}");
                        Ok(String::new())
                    }
                    r => r,
                }
            } else {
                podman_owned(&args)
            };
            match r {
                Ok(_) => eprintln!("web-agent: job {job}: started{}", if due { "" } else { " (run now)" }),
                Err(e) => {
                    eprintln!("web-agent: job {job}: not started: {e}");
                    st.last.insert(job.clone(), json!({ "at": now, "result": format!("not started: {e}") }));
                }
            }
        }
    }
    let running = podman_jobs(data).unwrap_or_default();
    // the proxies of the runs with an exit node (again after the agent's restart)
    let mut proxied = Vec::new();
    for (job, h) in &running {
        // a run that has ended has no network any more (its pid may linger in podman's list)
        if h.running && h.pid > 0 && Path::new(&format!("/proc/{}/ns/net", h.pid)).exists() && want.get(job).is_some_and(|j| j.egress.is_some()) {
            let from = format!("job:{job}");
            if let Err(why) = egress.ensure(data, &from, h.pid) {
                eprintln!("web-agent: job {job}: its proxy: {why}");
            }
            proxied.push(from);
        }
    }
    egress.keep_only("job:", &proxied);
    let jobs: BTreeMap<&String, Value> = want
        .iter()
        .map(|(job, j)| {
            let r = running.get(job).filter(|h| h.running);
            let next = crate::cron::Cron::parse(&j.schedule).ok().and_then(|c| c.next_after(now));
            (job, json!({
                "state": if r.is_some() { "running" } else { "waiting" },
                "started": r.map(|h| h.started),
                "next": next,
                "started_minute": st.started_minute.get(job),
                "last": st.last.get(job),
            }))
        })
        .collect();
    write_for_node(data, &dir.join("jobs-status.json"), &json!({ "at": now, "jobs": jobs }));
}

/// The last lines of a site's container's log (podman: as root, the agent).
pub fn tail(site: &str, lines: u32) -> Result<String, String> {
    podman(&["logs", "--tail", &lines.to_string(), &sites::container_name(site)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn searx() -> Container {
        Container {
            image: "docker.io/searxng/searxng:latest".into(),
            port: 8080,
            volumes: vec!["searxng:/etc/searxng".into(), "searxng-cache:/var/cache/searxng".into()],
            env: [("BASE_URL".to_string(), "https://searx.example.org/".to_string()), ("INSTANCE_NAME".to_string(), "searxng".to_string())].into(),
            folders: vec![],
            egress: None,
        }
    }

    #[test]
    fn spawning() {
        let (code, out, err) = spawn_capture("sh", &["-c", "echo out; echo err >&2; exit 3"]).unwrap();
        assert_eq!((code, out.as_slice(), err.as_slice()), (3, &b"out\n"[..], &b"err\n"[..]));
        let (code, out, _) = spawn_capture("printf", &["%s|%s", "a b", "c"]).unwrap();
        assert_eq!((code, out.as_slice()), (0, &b"a b|c"[..]));
        assert!(spawn_capture("no-such-program-here", &[]).is_err());
    }

    #[test]
    fn podman_arguments() {
        let a = run_args("searx", &searx(), 18100);
        let s = a.join(" ");
        assert!(s.starts_with("run -d --name osada-web-searx --label shelf.site=searx --label shelf.spec="), "{s}");
        assert!(s.contains("--restart=always --security-opt=no-new-privileges --cap-drop=NET_RAW -p 127.0.0.1:18100:8080"));
        assert!(s.contains("-v searxng:/etc/searxng -v searxng-cache:/var/cache/searxng"));
        assert!(s.contains("-e BASE_URL=https://searx.example.org/ -e INSTANCE_NAME=searxng"));
        assert!(s.ends_with(" docker.io/searxng/searxng:latest"));
        assert!(!s.contains("--privileged"));
        // the spec changes with what matters
        let mut c = searx();
        assert_eq!(spec_hash(&c, &[], 18100), spec_hash(&searx(), &[], 18100));
        assert_ne!(spec_hash(&c, &[], 18100), spec_hash(&c, &[], 18101));
        c.env.insert("X".into(), "1".into());
        assert_ne!(spec_hash(&c, &[], 18100), spec_hash(&searx(), &[], 18100));
        // the spec of a container without osada folders is what it was before them (Round 18)
        let before = hex::encode(&Sha256::digest(json!({ "c": searx(), "port": 18100 }).to_string().as_bytes())[..8]);
        assert_eq!(spec_hash(&searx(), &[], 18100), before);
    }

    /// Round 18: osada folders resolved by the node, checked again by the agent, given to podman.
    #[test]
    fn osada_folders() {
        let d = std::env::temp_dir().join(format!("osada-folders-{}", std::process::id()));
        let movies = d.join("users/vera/V&V/czech movies \\w subtitles");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(d.join("node")).unwrap();
        std::fs::create_dir_all(d.join("elsewhere")).unwrap();
        let mut c = searx();
        c.folders = vec![sites::Folder {
            from: sites::Link { user: String::new(), group: None, share: Some("JWNOauyrX-235E0mWqC6TA".into()), path: "/".into() },
            to: "/media/movies/".into(),
            write: false,
        }];
        assert!(acceptable("media", &c).is_ok(), "{:?}", acceptable("media", &c));
        // node control: where it is found
        let mut p = Placement::default();
        let me = NodeId("home".into());
        p.nodes.insert(me.clone(), crate::placement::Node::new([crate::placement::Role::Worker], []));
        let mut m = crate::placement::Module::new(crate::placement::ModuleKind::WEB, "", me.clone());
        m.site = Some(sites::Site { names: vec!["jf.example.org".into()], content: Content::Container(c.clone()), ..Default::default() });
        p.modules.insert("media".into(), m);
        let found = movies.clone();
        let w = wanted(&p, &me, Some(&|l: &sites::Link| if l.share.is_some() { Ok(found.clone()) } else { Err("no".into()) }));
        let r = &w["media"];
        assert_eq!(r.blocked, None);
        assert_eq!(r.binds, vec![Bind { host: movies.display().to_string(), to: "/media/movies".into(), write: false }]);
        let none = wanted(&p, &me, None);
        assert!(none["media"].blocked.as_deref().is_some_and(|b| b.contains("share:JWNOauyrX-235E0mWqC6TA") && b.contains("not on this node")), "{none:?}");
        // the agent: the real path under <osada data>/users, given read only
        let b = binds_ok(&d.join("node"), r).unwrap();
        let a = run_args_with("media", &c, &b, 18101, Some(&d.join("node"))).join(" ");
        assert!(a.contains(&format!("--label shelf.data={}", d.join("node").display())), "{a}");
        assert!(a.contains(&format!("-v {}:/media/movies:ro", std::fs::canonicalize(&movies).unwrap().display())), "{a}");
        assert_ne!(spec_hash(&c, &b, 18101), spec_hash(&c, &[], 18101));
        // not a osada folder, a link leading out, not as node control says: refused
        let mut bad = r.clone();
        bad.binds[0].host = d.join("elsewhere").display().to_string();
        assert!(binds_ok(&d.join("node"), &bad).is_err());
        std::os::unix::fs::symlink(d.join("elsewhere"), d.join("users/vera/out")).unwrap();
        bad.binds[0].host = d.join("users/vera/out").display().to_string();
        assert!(binds_ok(&d.join("node"), &bad).is_err(), "a symlink out of users/");
        bad.binds[0].host = d.join("users").display().to_string();
        assert!(binds_ok(&d.join("node"), &bad).is_err(), "all of users/");
        let mut bad = r.clone();
        bad.binds[0].write = true;
        assert!(binds_ok(&d.join("node"), &bad).is_err(), "written where node control says read");
        let mut bad = r.clone();
        bad.binds.clear();
        assert!(binds_ok(&d.join("node"), &bad).is_err());
        // a folder's checks: one source, a path inside
        let mut x = c.clone();
        x.folders[0].from.user = "vera".into();
        assert!(acceptable("media", &x).is_err(), "two sources");
        let mut x = c.clone();
        x.folders[0].to = "/media:/x".into();
        assert!(acceptable("media", &x).is_err());
        let mut x = c.clone();
        x.folders[0].to = "/etc/searxng".into();
        assert!(acceptable("media", &x).is_err(), "a volume's path");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_jobs_arguments() {
        let mut j = sites::Job { image: "docker.io/library/python:3.12-alpine".into(), command: vec!["python3".into(), "m.py".into(), "/out/m.html".into()], schedule: "4,24,44 * * * *".into(), env: Default::default(), writes: vec![], timeout: None, hosts: Default::default(), egress: None };
        j.hosts.insert("db.example.org".into(), "192.168.1.12".into());
        let a = job_args(Path::new("/home/osada/node"), "morning", &j, (990, 990)).join(" ");
        assert!(a.starts_with("run -d --name osada-job-morning --label shelf.job=morning --label shelf.data=/home/osada/node --user 990:990 --security-opt=no-new-privileges --cap-drop=all"), "{a}");
        assert!(a.contains("-v /home/osada/node/web/morning:/job:rw -v /home/osada/node/web/.jobs-out/morning:/out:rw -w /job"), "{a}");
        assert!(a.ends_with("--add-host=db.example.org:192.168.1.12 docker.io/library/python:3.12-alpine python3 m.py /out/m.html"), "{a}");
    }

    #[test]
    fn ports_podman_has() {
        let out = r#"[{"Names":["osada-web-media"],"Ports":[{"host_ip":"127.0.0.1","container_port":8096,"host_port":18100,"range":1,"protocol":"tcp"}]},{"Names":["x"],"Ports":null}]"#;
        assert_eq!(host_ports(out), vec![18100]);
        assert!(host_ports("not json").is_empty());
    }

    #[test]
    fn the_agent_checks_again() {
        assert!(acceptable("searx", &searx()).is_ok());
        let mut c = searx();
        c.volumes = vec!["/etc:/host-etc".into()];
        assert!(acceptable("searx", &c).is_err(), "a host path");
        let mut c = searx();
        c.image = "--privileged".into();
        assert!(acceptable("searx", &c).is_err());
        let mut c = searx();
        c.env.insert("A B".into(), "x".into());
        assert!(acceptable("searx", &c).is_err());
        assert!(acceptable("../x", &searx()).is_err());
    }

    #[test]
    fn wanted_and_requests() {
        let d = std::env::temp_dir().join(format!("osada-containers-{}", std::process::id()));
        let w: BTreeMap<String, Run> = [("searx".to_string(), Run { c: searx(), binds: vec![], blocked: None })].into();
        assert!(write_wanted(&d, &w).unwrap());
        assert!(!write_wanted(&d, &w).unwrap(), "unchanged: not written");
        let back: BTreeMap<String, Run> = read_json(&d.join("web/containers.json"));
        assert_eq!(back, w);
        // as the agent before Round 18 wrote it: a container alone
        let old: BTreeMap<String, Container> = read_json(&d.join("web/containers.json"));
        assert_eq!(old["searx"], searx());
        request(&d, "searx", "upgrade").unwrap();
        assert!(request(&d, "searx", "rm -rf").is_err());
        assert_eq!(std::fs::read_dir(d.join("web/actions")).unwrap().count(), 1);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
