//! Where a node keeps copies of files (D-U41): node control's `files_dir` (a volume of their own,
//! picked on the node card next to its drives) as `<files_dir>/osada-<node id>`, else its data
//! folder. The node runs as an account of its own, often with `ProtectSystem=strict`: the root
//! web agent makes the folder (owned by the node's account) and lets the unit write there (a
//! drop-in with `ReadWritePaths`), then restarts it. The node asks by a file,
//! `<data>/web/files-dir.json`; the agent answers in `files-dir.done`.
//!
//! What the agent does for a request is narrow, as the node that writes it is not trusted with
//! more: only a new folder named `osada-<id>` right on a mount point that is not one of the
//! system's (/, /boot, /usr, /etc, …), owned like the node's data folder; only a unit named
//! `osada…`.

use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::placement::NodeId;

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    /// The folder to use (`<files_dir>/osada-<id>`); none: back to the data folder.
    dir: Option<String>,
    /// The node's systemd unit.
    unit: String,
}

fn request_file(data: &Path) -> PathBuf {
    data.join("web/files-dir.json")
}

/// The folder for `files_dir` of node `me`.
pub fn folder(files_dir: &str, me: &NodeId) -> PathBuf {
    let old = Path::new(files_dir).join(format!("shelf-{}", me.0));
    let new = Path::new(files_dir).join(format!("osada-{}", me.0));
    if old.is_dir() && !new.exists() { old } else { new }
}

/// Where this node keeps its copies: the folder for `files_dir` when this process may write
/// there, else `data` (and then whether the agent should be asked).
pub fn base(data: &Path, files_dir: Option<&str>, me: &NodeId) -> (PathBuf, bool) {
    let Some(d) = files_dir else { return (data.to_path_buf(), false) };
    let f = folder(d, me);
    let probe = f.join(".osada-write-test");
    if std::fs::write(&probe, b"ok").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return (f, false);
    }
    (data.to_path_buf(), true)
}

/// The systemd unit this process runs in (/proc/self/cgroup), if any.
pub fn own_unit() -> Option<String> {
    let c = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    c.lines().find_map(|l| l.rsplit('/').next().filter(|u| u.ends_with(".service")).map(str::to_string))
}

/// Asks the root agent to make `dir` this node's and restart its unit (none: back to the data
/// folder). False when this process is not a systemd service.
pub fn request(data: &Path, dir: Option<&Path>) -> bool {
    let Some(unit) = own_unit() else { return false };
    let r = Request { dir: dir.map(|d| d.display().to_string()), unit };
    let f = request_file(data);
    let _ = std::fs::remove_file(f.with_file_name("files-dir.done"));
    std::fs::write(&f, serde_json::to_vec(&r).unwrap_or_default()).is_ok()
}

/// The mount points of the machine.
fn mounts() -> Vec<String> {
    std::fs::read_to_string("/proc/self/mounts").unwrap_or_default().lines().filter_map(|l| l.split_whitespace().nth(1)).map(|m| m.replace("\\040", " ")).collect()
}

const SYSTEM: [&str; 12] = ["/", "/boot", "/boot/efi", "/usr", "/etc", "/var", "/var/lib", "/home", "/root", "/tmp", "/proc", "/sys"];

/// Whether the agent may make `dir` for the node with data folder `data`: `<mount point>/osada-<id>`.
fn allowed(dir: &Path, mounts: &[String]) -> Result<(), String> {
    let name = dir.file_name().and_then(|n| n.to_str()).ok_or("no folder name")?;
    if !name.starts_with("osada-") || !crate::placement::valid_files_dir(&dir.display().to_string()) {
        return Err(format!("{}: not a node's files folder", dir.display()));
    }
    let parent = dir.parent().and_then(|p| p.to_str()).ok_or("no parent")?;
    if SYSTEM.contains(&parent) || parent.starts_with("/proc") || parent.starts_with("/sys") || parent.starts_with("/dev") || parent.starts_with("/run") {
        return Err(format!("{parent} is the system's, not a volume for files"));
    }
    if !mounts.iter().any(|m| m == parent) {
        return Err(format!("{parent} is not a mount point (a volume of its own)"));
    }
    Ok(())
}

fn valid_unit(u: &str) -> bool {
    (u.starts_with("osada") || u.starts_with("shelf")) && u.ends_with(".service") && u.len() < 100 && u.chars().all(|c| c.is_ascii_alphanumeric() || "@._-".contains(c))
}

/// The agent's round (root): a request there is done, and answered.
pub fn agent_round(data: &Path) {
    let f = request_file(data);
    let Ok(b) = std::fs::read(&f) else { return };
    let _ = std::fs::remove_file(&f);
    let res = (|| -> Result<String, String> {
        let r: Request = serde_json::from_slice(&b).map_err(|e| format!("request: {e}"))?;
        if !valid_unit(&r.unit) {
            return Err(format!("{:?} is not a osada unit", r.unit));
        }
        let dropin_dir = PathBuf::from(format!("/etc/systemd/system/{}.d", r.unit));
        let dropin = dropin_dir.join("60-osada-files.conf");
        // the one written before the rename goes; this one says the same
        let _ = std::fs::remove_file(dropin_dir.join("60-shelf-files.conf"));
        let what = match &r.dir {
            Some(d) => {
                let dir = PathBuf::from(d);
                allowed(&dir, &mounts())?;
                // owned like the node's data folder (its account), not as the request says
                let meta = std::fs::metadata(data).map_err(|e| format!("{}: {e}", data.display()))?;
                if dir.exists() && std::fs::metadata(&dir).map_err(|e| e.to_string())?.uid() != meta.uid() {
                    return Err(format!("{} is there and belongs to someone else", dir.display()));
                }
                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                std::os::unix::fs::chown(&dir, Some(meta.uid()), Some(meta.gid())).map_err(|e| format!("chown {}: {e}", dir.display()))?;
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750)).map_err(|e| e.to_string())?;
                // its SELinux label as the data folder's, when there is one
                let _ = crate::containers::spawn_capture("chcon", &["--reference", &data.display().to_string(), &dir.display().to_string()]);
                std::fs::create_dir_all(&dropin_dir).map_err(|e| e.to_string())?;
                std::fs::write(&dropin, format!("# osada web agent (D-U41): the node's files folder\n[Service]\nReadWritePaths={}\n", dir.display())).map_err(|e| e.to_string())?;
                format!("{} for {}", dir.display(), r.unit)
            }
            None => {
                let _ = std::fs::remove_file(&dropin);
                format!("the data folder again for {}", r.unit)
            }
        };
        // not std's Command (glibc 2.39: containers::spawn_capture)
        let ok = |args: &[&str]| matches!(crate::containers::spawn_capture("systemctl", args), Ok((0, _, _)));
        if !ok(&["daemon-reload"]) {
            return Err("systemctl daemon-reload failed".into());
        }
        if !ok(&["restart", "--no-block", &r.unit]) {
            return Err(format!("systemctl restart {} failed", r.unit));
        }
        Ok(what)
    })();
    let done = match &res {
        Ok(w) => serde_json::json!({ "ok": true, "what": w }),
        Err(e) => serde_json::json!({ "ok": false, "why": e }),
    };
    eprintln!("web-agent: files folder: {}", res.unwrap_or_else(|e| e));
    let _ = std::fs::write(f.with_file_name("files-dir.done"), done.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_agent_makes() {
        let m = vec!["/".to_string(), "/mnt/HC_Volume_1".to_string(), "/var/lib/osada-disks/data1".to_string()];
        assert!(allowed(Path::new("/mnt/HC_Volume_1/osada-abc"), &m).is_ok());
        assert!(allowed(Path::new("/var/lib/osada-disks/data1/osada-abc"), &m).is_ok());
        assert!(allowed(Path::new("/mnt/HC_Volume_1/x/osada-abc"), &m).is_err(), "right on the mount point only");
        assert!(allowed(Path::new("/mnt/HC_Volume_1/etc"), &m).is_err(), "named osada-…");
        assert!(allowed(Path::new("/osada-abc"), &m).is_err(), "not the root file system");
        assert!(allowed(Path::new("/mnt/other/osada-abc"), &m).is_err(), "not a mount point");
        assert!(allowed(Path::new("/mnt/HC_Volume_1/../osada-abc"), &m).is_err());
        assert!(valid_unit("osada-node.service") && valid_unit("osada-edge-node.service"));
        assert!(!valid_unit("sshd.service") && !valid_unit("osada;rm.service"));
        assert_eq!(folder("/mnt/v", &NodeId("n1".into())), PathBuf::from("/mnt/v/osada-n1"));
    }
}
