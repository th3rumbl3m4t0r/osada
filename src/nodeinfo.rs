//! What a node is and has (T10): load, memory, its data folder's file
//! system, its disks; and the one disk operation the admin pages offer: a
//! file system on an empty disk, done by the node's root agent (`osada
//! web-agent`, which writes `<data>/web/agent.json` every round).
//!
//! Formatting is refused unless the disk is a whole disk with no partitions,
//! no file system signature and nothing mounted, and the request names it
//! exactly (the admin types its name). The agent checks it all again, makes
//! XFS with the label, mounts it at /var/lib/osada-disks/<label> and adds it
//! to /etc/fstab (by UUID, nofail).

use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn read(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// kB values of /proc/meminfo.
fn meminfo(key: &str) -> Option<u64> {
    read("/proc/meminfo").lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
}

/// Size and free bytes of the file system holding `p`.
pub fn fs_of(p: &Path) -> Option<(u64, u64)> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let c = CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a statvfs the call fills
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some((s.f_blocks as u64 * s.f_frsize as u64, s.f_bavail as u64 * s.f_frsize as u64))
}

/// The disks as lsblk sees them (JSON).
/// The bytes and files under `p` (links not followed); None when it is not there.
pub fn du(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::symlink_metadata(p).ok()?;
    if !m.is_dir() {
        return Some((m.len(), 1));
    }
    let (mut bytes, mut files) = (0u64, 0u64);
    let mut todo = vec![p.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                todo.push(e.path());
            } else if m.is_file() {
                bytes += m.len();
                files += 1;
            }
        }
    }
    Some((bytes, files))
}

pub fn disks() -> Value {
    match crate::containers::spawn_capture("lsblk", &["-J", "-b", "-o", "NAME,PATH,SIZE,TYPE,FSTYPE,MOUNTPOINTS,MODEL,LABEL"]) {
        Ok((0, out, _)) => serde_json::from_slice::<Value>(&out).map(|v| v["blockdevices"].clone()).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// The agent's last word, if it spoke in the last 30 s.
fn agent(data: &Path) -> Value {
    let v: Value = std::fs::read(data.join("web/agent.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    let alive = v["at"].as_u64().is_some_and(|t| now().saturating_sub(t) < 30);
    json!({ "running": alive, "last": v["results"] })
}

/// This node's state, for the admin pages.
pub fn local(data: Option<&Path>) -> Value {
    let load: Vec<f64> = read("/proc/loadavg").split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
    let uptime = read("/proc/uptime").split_whitespace().next().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    let (total, avail) = (meminfo("MemTotal:").unwrap_or(0) * 1024, meminfo("MemAvailable:").unwrap_or(0) * 1024);
    let storage = data.and_then(fs_of).map(|(size, free)| json!({ "size": size, "free": free }));
    json!({
        "host": read("/proc/sys/kernel/hostname").trim(),
        "version": crate::VERSION,
        "uptime": uptime as u64,
        "cpus": std::thread::available_parallelism().map_or(1, |n| n.get()),
        "load": load,
        "mem": { "total": total, "available": avail },
        "data": data.map(|d| d.display().to_string()),
        "storage": storage,
        "disks": disks(),
        "agent": data.map_or(Value::Null, agent),
        "certs": data.map_or(Value::Null, certs),
        // the links' rates to this node and its uplink now (D-U40)
        "links": crate::linkstats::stats().report(),
        // what each link carries, and its round trip (D-U34, the map)
        "link_rates": crate::linkstats::rates(),
    })
}

/// The certificates this node has (acme.rs): by name, their host names and when issued.
fn certs(data: &Path) -> Value {
    let mut out = serde_json::Map::new();
    let Ok(dirs) = std::fs::read_dir(data.join("certs")) else { return Value::Object(out) };
    for d in dirs.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        if let Some(m) = crate::acme::meta(data, &name) {
            out.insert(name, json!({ "names": m.names, "issued": m.issued }));
        }
    }
    Value::Object(out)
}

fn valid_device(d: &str) -> bool {
    !d.is_empty() && d.len() <= 32 && d.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn valid_label(l: &str) -> bool {
    (1..=12).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !l.starts_with('-')
}

/// Whether lsblk's entry is an empty whole disk (no partitions, no file system, not mounted).
fn empty_disk(d: &Value) -> bool {
    d["type"] == "disk"
        && d["fstype"].is_null()
        && d["children"].as_array().is_none_or(|c| c.is_empty())
        && d["mountpoints"].as_array().is_none_or(|m| m.iter().all(Value::is_null))
}

/// The empty disks of this node (formatting is offered for these only).
pub fn empty_disks() -> Vec<String> {
    disks().as_array().map(|a| a.iter().filter(|d| empty_disk(d)).filter_map(|d| d["name"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// Asks the root agent to make a file system on `device` (an empty disk).
pub fn request_format(data: &Path, device: &str, label: &str) -> Result<Value, String> {
    if !valid_device(device) || !valid_label(label) {
        return Err("a device's name (sdb) and a label (a-z, 0-9, -; up to 12)".into());
    }
    if !agent(data)["running"].as_bool().unwrap_or(false) {
        return Err("no root agent runs on this node (osada-web-agent.service)".into());
    }
    if !empty_disks().iter().any(|d| d == device) {
        return Err(format!("{device} is not an empty whole disk (no partitions, no file system, not mounted)"));
    }
    let dir = data.join("web/actions");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let id = format!("format-{}-{}", now(), rand::random::<u32>());
    std::fs::write(dir.join(format!("{id}.json")), json!({ "action": "format", "device": device, "label": label }).to_string()).map_err(|e| e.to_string())?;
    Ok(json!({ "requested": id }))
}

/// The agent's part (root): every `format` request of `<data>/web/actions`,
/// checked again; then the heartbeat with the results so far.
pub fn agent_round(data: &Path, results: &mut Vec<Value>) {
    let dir = data.join("web/actions");
    let reqs: Vec<std::path::PathBuf> = std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("format-") && n.ends_with(".json"))).collect()).unwrap_or_default();
    for r in reqs {
        let v: Value = std::fs::read(&r).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        let _ = std::fs::remove_file(&r);
        let (Some(dev), Some(label)) = (v["device"].as_str(), v["label"].as_str()) else { continue };
        let res = format(dev, label);
        eprintln!("web-agent: format {dev} as {label}: {}", res.as_ref().map_or_else(|e| e.clone(), |m| m.clone()));
        results.push(json!({ "at": now(), "device": dev, "label": label, "ok": res.is_ok(), "what": res.unwrap_or_else(|e| e) }));
        if results.len() > 20 {
            results.remove(0);
        }
    }
    let file = data.join("web/agent.json");
    let tmp = data.join("web/.agent.json.tmp");
    if std::fs::write(&tmp, json!({ "at": now(), "results": results }).to_string()).is_ok() {
        if let Ok(meta) = std::fs::metadata(data) {
            use std::os::unix::fs::MetadataExt;
            let _ = std::os::unix::fs::chown(&tmp, Some(meta.uid()), Some(meta.gid()));
        }
        let _ = std::fs::rename(&tmp, &file);
    }
}

fn run(prog: &str, args: &[&str]) -> Result<String, String> {
    match crate::containers::spawn_capture(prog, args) {
        Ok((0, out, _)) => Ok(String::from_utf8_lossy(&out).trim().to_string()),
        Ok((code, _, err)) => Err(format!("{prog}: exit {code}: {}", String::from_utf8_lossy(&err).trim())),
        Err(e) => Err(format!("{prog}: {e}")),
    }
}

/// XFS on an empty disk, mounted at /var/lib/osada-disks/<label>, in fstab.
fn format(dev: &str, label: &str) -> Result<String, String> {
    if !valid_device(dev) || !valid_label(label) {
        return Err("not a device or label".into());
    }
    if !empty_disks().iter().any(|d| d == dev) {
        return Err(format!("{dev} is not an empty whole disk"));
    }
    let path = format!("/dev/{dev}");
    if !run("wipefs", &["-n", &path])?.is_empty() {
        return Err(format!("{path} has signatures (wipefs -n)"));
    }
    let at = format!("/var/lib/osada-disks/{label}");
    if Path::new(&at).exists() && std::fs::read_dir(&at).map(|mut d| d.next().is_some()).unwrap_or(true) {
        return Err(format!("{at} is there and not empty"));
    }
    run("mkfs.xfs", &["-L", label, &path])?;
    let uuid = run("blkid", &["-s", "UUID", "-o", "value", &path])?;
    if uuid.is_empty() {
        return Err("no UUID after mkfs".into());
    }
    std::fs::create_dir_all(&at).map_err(|e| e.to_string())?;
    let line = format!("UUID={uuid} {at} xfs defaults,nofail 0 2\n");
    let fstab = std::fs::read_to_string("/etc/fstab").map_err(|e| e.to_string())?;
    if !fstab.contains(&uuid) {
        std::fs::write("/etc/fstab.osada-tmp", format!("{fstab}{}{line}", if fstab.ends_with('\n') { "" } else { "\n" })).map_err(|e| e.to_string())?;
        std::fs::rename("/etc/fstab.osada-tmp", "/etc/fstab").map_err(|e| e.to_string())?;
    }
    let _ = run("systemctl", &["daemon-reload"]);
    run("mount", &[&at])?;
    Ok(format!("XFS {label} on {path}, mounted at {at} (UUID {uuid}, in /etc/fstab)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_may_be_formatted() {
        let d = |v: Value| empty_disk(&v);
        assert!(d(json!({ "name": "sdb", "type": "disk", "fstype": null, "mountpoints": [null] })));
        assert!(!d(json!({ "name": "sda", "type": "disk", "fstype": null, "children": [{ "name": "sda1" }] })), "partitions");
        assert!(!d(json!({ "name": "sdc", "type": "disk", "fstype": "xfs", "mountpoints": [null] })), "a file system");
        assert!(!d(json!({ "name": "sdd", "type": "disk", "fstype": null, "mountpoints": ["/mnt/x"] })), "mounted");
        assert!(!d(json!({ "name": "sr0", "type": "rom", "fstype": null })), "not a disk");
        assert!(valid_device("nvme1n1") && !valid_device("../sda") && !valid_device("sda1 ; rm"));
        assert!(valid_label("data1") && !valid_label("Data") && !valid_label("-x") && !valid_label("a-very-long-label"));
        let info = local(Some(Path::new("/tmp")));
        assert!(info["cpus"].as_u64().unwrap() >= 1 && info["mem"]["total"].as_u64().unwrap() > 0 && info["storage"]["size"].as_u64().unwrap() > 0);
    }
}

/// The agent's Stalwart round (T11, T12), where OSADA_STALWART_KEY_FILE names
/// an API key of Stalwart's (the sync's, on the mail host): every domain's
/// DNS records (its dnsZoneFile), Stalwart's state; into
/// `<data>/web/stalwart.json` for the node.
pub fn stalwart_round(data: &Path) {
    let Ok(key_file) = std::env::var("OSADA_STALWART_KEY_FILE") else { return };
    let url = std::env::var("OSADA_STALWART_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let Ok(key) = std::fs::read_to_string(&key_file) else { return };
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(_) => return,
    };
    let res: Result<Value, String> = rt.block_on(async {
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
        let auth = format!("Bearer {}", key.trim());
        let s: Value = serde_json::from_slice(&http.get(format!("{url}/jmap/session")).header("authorization", &auth).send().await.map_err(|e| e.to_string())?.bytes().await.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let acct = s["primaryAccounts"]["urn:stalwart:jmap"].as_str().map(str::to_string).or_else(|| s["accounts"].as_object().and_then(|a| a.keys().next().cloned())).ok_or("no account")?;
        let api = s["apiUrl"].as_str().ok_or("no apiUrl")?;
        let api = format!("{url}/{}", api.split("://").nth(1).and_then(|r| r.split_once('/')).map_or("jmap", |(_, p)| p));
        let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:stalwart:jmap"], "methodCalls": [["x:Domain/get", { "accountId": acct, "properties": ["name", "dnsZoneFile", "isEnabled"] }, "0"]] });
        let r = http.post(&api).header("authorization", &auth).header("content-type", "application/json").body(body.to_string()).send().await.map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&r.bytes().await.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let list = v["methodResponses"][0][1]["list"].as_array().cloned().unwrap_or_default();
        let domains: serde_json::Map<String, Value> = list.iter().filter_map(|d| Some((d["name"].as_str()?.to_string(), d["dnsZoneFile"].clone()))).collect();
        Ok(json!({ "domains": domains }))
    });
    let active = run("systemctl", &["is-active", "stalwart"]).unwrap_or_default();
    let version = run("/opt/stalwart/bin/stalwart", &["--version"]).unwrap_or_default();
    let size = fs_of(Path::new("/opt/stalwart/data")).map(|(s, f)| s - f);
    let du = run("du", &["-sb", "/opt/stalwart/data"]).ok().and_then(|o| o.split_whitespace().next().and_then(|x| x.parse::<u64>().ok()));
    let out = json!({ "at": now(), "active": active, "version": version, "data_bytes": du, "fs_used": size, "dns": res.as_ref().ok().map(|v| v["domains"].clone()), "error": res.err() });
    let file = data.join("web/stalwart.json");
    let tmp = data.join("web/.stalwart.json.tmp");
    if std::fs::write(&tmp, out.to_string()).is_ok() {
        if let Ok(meta) = std::fs::metadata(data) {
            use std::os::unix::fs::MetadataExt;
            let _ = std::os::unix::fs::chown(&tmp, Some(meta.uid()), Some(meta.gid()));
        }
        let _ = std::fs::rename(&tmp, &file);
    }
}
