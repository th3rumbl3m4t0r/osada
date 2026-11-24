//! The store on disk: one SQLite table of byte keys and values (decision D1).
//!
//! WAL, and `synchronous = FULL`, so a batch is on the disk when `write`
//! returns (§8.1, `local` durability). Keys are BLOBs, which SQLite compares
//! byte by byte, so the table's order is the store's order. One connection,
//! behind a mutex: the store is one file per node and its writes are small.

use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Batch, Error, Kv, Op, Result};

pub struct SqliteKv {
    conn: Mutex<Connection>,
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Engine(e.to_string())
    }
}

impl SqliteKv {
    /// Opens the store at `path`, making it when it is not there. The file
    /// is the owner's alone (0600): it holds credentials and keys. SQLite
    /// gives its -wal and -shm files the same mode.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| Error::Engine(format!("{}: {e}", path.display())))?;
        let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Engine(format!("{}: journal mode stays {mode}, not WAL", path.display())));
        }
        Self::setup(conn)
    }

    /// A store that lives as long as the value: for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::setup(Connection::open_in_memory()?)
    }

    fn setup(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS kv (k BLOB PRIMARY KEY NOT NULL, v BLOB NOT NULL) WITHOUT ROWID")?;
        Ok(SqliteKv { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic while the lock was held left no transaction open: a dropped
        // rusqlite Transaction rolls back.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn pair(r: &Row<'_>) -> rusqlite::Result<(Vec<u8>, Vec<u8>)> {
    Ok((r.get(0)?, r.get(1)?))
}

impl Kv for SqliteKv {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let conn = self.lock();
        let mut st = conn.prepare_cached("SELECT v FROM kv WHERE k = ?1")?;
        Ok(st.query_row(params![key], |r| r.get(0)).optional()?)
    }

    fn scan(&self, start: &[u8], end: Option<&[u8]>, limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.lock();
        let rows = match end {
            Some(end) => {
                let mut st = conn.prepare_cached("SELECT k, v FROM kv WHERE k >= ?1 AND k < ?2 ORDER BY k LIMIT ?3")?;
                st.query_map(params![start, end, limit], pair)?.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut st = conn.prepare_cached("SELECT k, v FROM kv WHERE k >= ?1 ORDER BY k LIMIT ?2")?;
                st.query_map(params![start, limit], pair)?.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    fn write(&self, batch: Batch) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        {
            let mut put = tx.prepare_cached("INSERT INTO kv (k, v) VALUES (?1, ?2) ON CONFLICT (k) DO UPDATE SET v = excluded.v")?;
            let mut del = tx.prepare_cached("DELETE FROM kv WHERE k = ?1")?;
            for op in &batch.ops {
                match op {
                    Op::Put(k, v) => put.execute(params![k, v])?,
                    Op::Delete(k) => del.execute(params![k])?,
                };
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn contract() {
        crate::kv::contract::all(&|| Box::new(SqliteKv::open_in_memory().unwrap()));
    }

    #[test]
    fn kept_on_disk() {
        let dir = TempDir::new("sqlite");
        let path = dir.path().join("store.db");
        {
            let kv = SqliteKv::open(&path).unwrap();
            let mut b = Batch::new();
            b.put(b"k", b"v");
            kv.write(b).unwrap();
        }
        let kv = SqliteKv::open(&path).unwrap();
        assert_eq!(kv.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
        for f in ["store.db", "store.db-wal", "store.db-shm"] {
            let mode = std::fs::metadata(dir.path().join(f)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{f}");
        }
    }
}
