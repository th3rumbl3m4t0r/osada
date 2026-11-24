//! osada 0.2. The design is docs/ARCHITECTURE.md; section numbers in comments
//! (§5.2 and so on) point there.

pub mod acme;
pub mod audit;
pub mod chat;
pub mod cluster;
pub mod containers;
pub mod cron;
pub mod directory;
pub mod dnscheck;
pub mod egress;
pub mod edgecache;
pub mod exposure;
pub mod idpkey;
pub mod dropin;
pub mod join;
pub mod kv;
pub mod link;
pub mod log;
pub mod filesdir;
pub mod linkstats;
pub mod livetarget;
pub mod mail;
pub mod node;
pub mod nodebackup;
pub mod oidc;
pub mod nodeinfo;
pub mod passthrough;
pub mod pki;
pub mod placement;
pub mod rbac;
pub mod relay;
pub mod repl;
pub mod osada1;
pub mod signin;
pub mod sites;
pub mod storage;
pub mod ql;
pub mod logstore;
pub mod accesslog;
pub mod rules;
pub mod bans;
pub mod table;
pub mod totp;
pub mod web;
pub mod webauthn;
pub mod websrv;

#[cfg(test)]
mod sim;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const COPYRIGHT: &str = "Copyright (C) 2026 th3rumbl3m4t0r";

pub const LICENCE: &str = "GNU Affero General Public License, version 3 (AGPL-3.0-only); see LICENSE";

#[cfg(test)]
pub(crate) mod testutil {
    use std::{
        path::{Path, PathBuf},
        sync::atomic::{AtomicU32, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    /// A folder of its own for one test, removed when dropped.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("osada-test-{tag}-{}-{nanos}-{n}", std::process::id()));
            std::fs::create_dir(&p).unwrap();
            TempDir(p)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
