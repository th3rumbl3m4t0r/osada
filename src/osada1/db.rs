use std::{path::Path, sync::Mutex};

use rusqlite::Connection;

use crate::osada1::error::{AppError, Res};

/// One SQLite connection behind a mutex. The load is a handful of users, so
/// that is plenty; never hold it across an await (the guard is !Send anyway).
pub struct Db(Mutex<Connection>);

const SCHEMA_V1: &str = "
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  username TEXT NOT NULL UNIQUE,
  pw_hash TEXT NOT NULL,
  is_admin INTEGER NOT NULL DEFAULT 0,
  disabled INTEGER NOT NULL DEFAULT 0,
  created INTEGER NOT NULL
);
CREATE TABLE sessions (
  token_hash TEXT PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  expires INTEGER NOT NULL
);
-- external storage; secret is sealed with secret.key (crypto.rs)
CREATE TABLE mounts (
  id INTEGER PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'sftp',
  host TEXT NOT NULL,
  port INTEGER NOT NULL DEFAULT 22,
  username TEXT NOT NULL,
  auth TEXT NOT NULL,
  secret TEXT NOT NULL,
  root TEXT NOT NULL DEFAULT '',
  host_key TEXT,
  created INTEGER NOT NULL
);
-- an item (owner's home when mount_id is NULL, else a mount) shared with a user
CREATE TABLE shares (
  id INTEGER PRIMARY KEY,
  owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  mount_id INTEGER REFERENCES mounts(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  is_dir INTEGER NOT NULL,
  recipient_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  can_write INTEGER NOT NULL DEFAULT 0,
  created INTEGER NOT NULL
);
CREATE UNIQUE INDEX shares_item ON shares(owner_id, IFNULL(mount_id, 0), path, recipient_id);
CREATE INDEX shares_recipient ON shares(recipient_id);
-- public links (/s/<token>), read-only
CREATE TABLE links (
  id INTEGER PRIMARY KEY,
  token TEXT NOT NULL UNIQUE,
  owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  mount_id INTEGER REFERENCES mounts(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  is_dir INTEGER NOT NULL,
  expires INTEGER,
  created INTEGER NOT NULL,
  hits INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX links_owner ON links(owner_id);
CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE backup_runs (
  id INTEGER PRIMARY KEY,
  started INTEGER NOT NULL,
  finished INTEGER,
  ok INTEGER,
  uploaded INTEGER NOT NULL DEFAULT 0,
  bytes INTEGER NOT NULL DEFAULT 0,
  versioned INTEGER NOT NULL DEFAULT 0,
  message TEXT
);
";

const SCHEMA_V4: &str = "
-- pictures in users' homes (pictures.rs); times are wall-clock seconds, as if UTC
CREATE TABLE pictures (
  owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  dev INTEGER NOT NULL,
  ino INTEGER NOT NULL,
  exif INTEGER,
  taken INTEGER NOT NULL,
  src TEXT NOT NULL,
  PRIMARY KEY (owner_id, path)
) WITHOUT ROWID;
-- networks announced by the AS numbers in users.remote_cidrs (edge.rs)
CREATE TABLE asn_prefixes (
  asn INTEGER PRIMARY KEY,
  prefixes TEXT NOT NULL,
  fetched INTEGER NOT NULL
);
";

const SCHEMA_V5: &str = "
-- phones that upload into their owner's home with a token of their own (sync.rs)
CREATE TABLE devices (
  id INTEGER PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  created INTEGER NOT NULL,
  last_seen INTEGER NOT NULL
);
-- what each device put there, as it left it: only those files may it replace
CREATE TABLE device_files (
  device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  etag TEXT NOT NULL,
  PRIMARY KEY (device_id, path)
) WITHOUT ROWID;
";

const SCHEMA_V6: &str = "
-- keys that give a program one folder of their owner's (api.rs); only the
-- token's hash is kept
CREATE TABLE api_keys (
  id INTEGER PRIMARY KEY,
  owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  mount_id INTEGER REFERENCES mounts(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  name TEXT NOT NULL,
  can_write INTEGER NOT NULL DEFAULT 0,
  token_hash TEXT NOT NULL UNIQUE,
  expires INTEGER,
  created INTEGER NOT NULL,
  last_used INTEGER
);
CREATE INDEX api_keys_owner ON api_keys(owner_id);
";

const SCHEMA_V9: &str = "
-- programs that let people log in with their osada account (oidc.rs): name is
-- the client_id; only the secret's hash is kept, NULL for a public client
CREATE TABLE oidc_clients (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  secret_hash TEXT,
  redirect_uris TEXT NOT NULL,
  users TEXT NOT NULL DEFAULT '',
  created INTEGER NOT NULL,
  last_used INTEGER
);
-- their access and refresh tokens, by hash; a grant is one login and what
-- was refreshed from it
CREATE TABLE oidc_tokens (
  token_hash TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  client_id INTEGER NOT NULL REFERENCES oidc_clients(id) ON DELETE CASCADE,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  scope TEXT NOT NULL,
  grant_id TEXT NOT NULL,
  auth_time INTEGER NOT NULL,
  created INTEGER NOT NULL,
  expires INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX oidc_tokens_grant ON oidc_tokens(grant_id);
CREATE INDEX oidc_tokens_user ON oidc_tokens(user_id);
-- the subject in tokens: random, so never reused as ids and names can be
ALTER TABLE users ADD COLUMN sub TEXT;
CREATE UNIQUE INDEX users_sub ON users(sub);
";

const SCHEMA_V10: &str = "
-- the wiki (wiki.rs): an article, its address made from its first title
CREATE TABLE wiki_pages (
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL UNIQUE,
  created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
  created INTEGER NOT NULL
);
-- every save of a text; the newest of a language is its text. author: the name
-- then, the account may be gone
CREATE TABLE wiki_revs (
  id INTEGER PRIMARY KEY,
  page_id INTEGER NOT NULL REFERENCES wiki_pages(id) ON DELETE CASCADE,
  lang TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  author_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
  author TEXT NOT NULL,
  created INTEGER NOT NULL,
  note TEXT NOT NULL DEFAULT ''
);
CREATE INDEX wiki_revs_page ON wiki_revs(page_id, lang, id);
-- an article's pictures, files in <data>/wiki/<page id>/<name>
CREATE TABLE wiki_media (
  page_id INTEGER NOT NULL REFERENCES wiki_pages(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  size INTEGER NOT NULL,
  author_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
  author TEXT NOT NULL,
  created INTEGER NOT NULL,
  PRIMARY KEY (page_id, name)
) WITHOUT ROWID;
";

const SCHEMA_V11: &str = "
-- a wiki article's family tree (wiki.rs), every save of it; data is JSON,
-- 'null' when it was removed
CREATE TABLE wiki_trees (
  id INTEGER PRIMARY KEY,
  page_id INTEGER NOT NULL REFERENCES wiki_pages(id) ON DELETE CASCADE,
  data TEXT NOT NULL,
  author_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
  author TEXT NOT NULL,
  created INTEGER NOT NULL,
  note TEXT NOT NULL DEFAULT ''
);
CREATE INDEX wiki_trees_page ON wiki_trees(page_id, id);
";

pub fn get_setting(db: &Db, key: &str) -> Res<Option<String>> {
    db.opt(|c| c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)))
}

pub fn set_setting(db: &Db, key: &str, value: &str) -> Res<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )
    })?;
    Ok(())
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let c = Connection::open(path)?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        let v: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if v < 1 {
            c.execute_batch(&format!("BEGIN; {SCHEMA_V1} PRAGMA user_version=1; COMMIT;"))?;
        }
        if v < 2 {
            // backups became hard-linked snapshots
            c.execute_batch("BEGIN; ALTER TABLE backup_runs RENAME COLUMN versioned TO linked; PRAGMA user_version=2; COMMIT;")?;
        }
        if v < 3 {
            // networks an account may be used from through the public edge; empty: from anywhere (0.2; 0.1 read it as LAN only)
            c.execute_batch("BEGIN; ALTER TABLE users ADD COLUMN remote_cidrs TEXT NOT NULL DEFAULT ''; PRAGMA user_version=3; COMMIT;")?;
        }
        if v < 4 {
            c.execute_batch(&format!("BEGIN; {SCHEMA_V4} PRAGMA user_version=4; COMMIT;"))?;
        }
        if v < 5 {
            c.execute_batch(&format!("BEGIN; {SCHEMA_V5} PRAGMA user_version=5; COMMIT;"))?;
        }
        if v < 6 {
            c.execute_batch(&format!("BEGIN; {SCHEMA_V6} PRAGMA user_version=6; COMMIT;"))?;
        }
        if v < 7 {
            // the UI language each user picked (users::LANGS); empty: the browser's
            c.execute_batch("BEGIN; ALTER TABLE users ADD COLUMN lang TEXT NOT NULL DEFAULT ''; PRAGMA user_version=7; COMMIT;")?;
        }
        if v < 8 {
            // room in the home, bytes (quota.rs); NULL: no limit
            c.execute_batch("BEGIN; ALTER TABLE users ADD COLUMN quota INTEGER; PRAGMA user_version=8; COMMIT;")?;
        }
        if v < 9 {
            c.execute_batch(&format!("BEGIN; {SCHEMA_V9} PRAGMA user_version=9; COMMIT;"))?;
        }
        if v < 10 {
            c.execute_batch(&format!("BEGIN; {SCHEMA_V10} PRAGMA user_version=10; COMMIT;"))?;
        }
        if v < 11 {
            c.execute_batch(&format!("BEGIN; {SCHEMA_V11} PRAGMA user_version=11; COMMIT;"))?;
        }
        if v < 12 {
            // wiki articles pinned to the top of the list, for everyone
            c.execute_batch("BEGIN; ALTER TABLE wiki_pages ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0; PRAGMA user_version=12; COMMIT;")?;
        }
        if v < 13 {
            // where the cards of a family tree go (JSON, tree.js), worked out by whoever saved it; NULL: by each page
            c.execute_batch("BEGIN; ALTER TABLE wiki_trees ADD COLUMN layout TEXT; PRAGMA user_version=13; COMMIT;")?;
        }
        if v < 14 {
            // apps1's apps in a tab (apptab.rs), for whom an admin turned it on
            c.execute_batch("BEGIN; ALTER TABLE users ADD COLUMN apptab INTEGER NOT NULL DEFAULT 0; PRAGMA user_version=14; COMMIT;")?;
        }
        if v < 15 {
            // the mail address the mail server's sync gave the account (mail.rs), the email claim; NULL: none
            c.execute_batch(
                "BEGIN; ALTER TABLE users ADD COLUMN mail TEXT;
                 CREATE UNIQUE INDEX users_mail ON users(mail) WHERE mail IS NOT NULL;
                 PRAGMA user_version=15; COMMIT;",
            )?;
        }
        // osada 0.2's own (D-U29): what members share on from a group's folder. Tables of their
        // own, the schema version untouched, so osada 0.1 started on this data never reads them
        // as items of a user's home.
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS group_shares (
               id INTEGER PRIMARY KEY, group_id TEXT NOT NULL, path TEXT NOT NULL, is_dir INTEGER NOT NULL,
               recipient_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, can_write INTEGER NOT NULL DEFAULT 0,
               by_id INTEGER REFERENCES users(id) ON DELETE SET NULL, created INTEGER NOT NULL);
             CREATE UNIQUE INDEX IF NOT EXISTS group_shares_item ON group_shares(group_id, path, recipient_id);
             CREATE TABLE IF NOT EXISTS group_links (
               id INTEGER PRIMARY KEY, token TEXT NOT NULL UNIQUE, group_id TEXT NOT NULL, path TEXT NOT NULL, is_dir INTEGER NOT NULL,
               expires INTEGER, by_id INTEGER REFERENCES users(id) ON DELETE SET NULL, created INTEGER NOT NULL, hits INTEGER NOT NULL DEFAULT 0);",
        )?;
        // Round 17: several nightly backups, each run under its backup's name (osada 0.1's runs
        // are `nightly`'s; 0.1 started on this data still writes its runs as before)
        let named: bool = c.query_row("SELECT count(*) FROM pragma_table_info('backup_runs') WHERE name = 'name'", [], |r| r.get::<_, i64>(0))? > 0;
        if !named {
            c.execute_batch("ALTER TABLE backup_runs ADD COLUMN name TEXT NOT NULL DEFAULT 'nightly';")?;
        }
        // Round 21: the address a link was mailed to (public email); osada 0.1 never reads it
        for t in ["links", "group_links"] {
            let has: bool = c.query_row(&format!("SELECT count(*) FROM pragma_table_info('{t}') WHERE name = 'sent_to'"), [], |r| r.get::<_, i64>(0))? > 0;
            if !has {
                c.execute_batch(&format!("ALTER TABLE {t} ADD COLUMN sent_to TEXT;"))?;
            }
        }
        // Round 22: shares with people of federated instances (osada 0.1 never reads them)
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS fed_out (
               id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
               mount_id INTEGER REFERENCES mounts(id) ON DELETE CASCADE, path TEXT NOT NULL, is_dir INTEGER NOT NULL,
               peer TEXT NOT NULL, to_upn TEXT NOT NULL, secret_hash TEXT NOT NULL, created INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS fed_out_item ON fed_out (owner_id, path);
             CREATE TABLE IF NOT EXISTS fed_in (
               id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
               peer TEXT NOT NULL, remote_id INTEGER NOT NULL, owner_upn TEXT NOT NULL, owner_display TEXT NOT NULL,
               name TEXT NOT NULL, is_dir INTEGER NOT NULL, size INTEGER, secret TEXT NOT NULL, created INTEGER NOT NULL,
               UNIQUE (peer, remote_id));",
        )?;
        Ok(Db(Mutex::new(c)))
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Res<T> {
        let c = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&c).map_err(AppError::from)
    }

    /// Like `with`, but a missing row is `None` instead of an error.
    pub fn opt<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Res<Option<T>> {
        let c = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match f(&c) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Shares, links and API keys follow their item when it is renamed or moved ...
pub fn repath(c: &Connection, owner: i64, mount: Option<i64>, from: &str, to: &str) -> rusqlite::Result<()> {
    for t in ["shares", "links", "api_keys"] {
        c.execute(
            &format!(
                "UPDATE {t} SET path = ?1 || substr(path, length(?2) + 1)
                 WHERE owner_id = ?3 AND IFNULL(mount_id, 0) = ?4
                   AND (path = ?2 OR substr(path, 1, length(?2) + 1) = ?2 || '/')"
            ),
            rusqlite::params![to, from, owner, mount.unwrap_or(0)],
        )?;
    }
    Ok(())
}

/// ... and go away with it when it is deleted.
pub fn unshare(c: &Connection, owner: i64, mount: Option<i64>, path: &str) -> rusqlite::Result<()> {
    for t in ["shares", "links", "api_keys"] {
        c.execute(
            &format!(
                "DELETE FROM {t} WHERE owner_id = ?1 AND IFNULL(mount_id, 0) = ?2
                   AND (path = ?3 OR substr(path, 1, length(?3) + 1) = ?3 || '/')"
            ),
            rusqlite::params![owner, mount.unwrap_or(0), path],
        )?;
    }
    Ok(())
}
