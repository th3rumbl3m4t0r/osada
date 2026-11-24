//! The node's ordered key-value store (§5.5, decision D1).
//!
//! Everything a node keeps apart from blobs goes in here: the collection logs,
//! the tables derived from them, the node's own state. Keys are bytes and sort
//! as bytes; a collection's keys all start with its prefix (log.rs). Engines:
//! [`sqlite::SqliteKv`] on disk, [`mem::MemKv`] for tests and the simulation.

pub mod mem;
pub mod sqlite;

use std::fmt;

/// What a store can fail with.
#[derive(Debug)]
pub enum Error {
    /// The engine failed: I/O, a full disk, a broken database.
    Engine(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Engine(m) => write!(f, "store: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// One change in a [`Batch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
}

/// Changes written together: all of them or none, in order.
#[derive(Clone, Debug, Default)]
pub struct Batch {
    ops: Vec<Op>,
}

impl Batch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> &mut Self {
        self.ops.push(Op::Put(key.into(), value.into()));
        self
    }

    pub fn delete(&mut self, key: impl Into<Vec<u8>>) -> &mut Self {
        self.ops.push(Op::Delete(key.into()));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn ops(&self) -> &[Op] {
        &self.ops
    }
}

/// An ordered key-value store. Engines are shared between threads; a write
/// is atomic and, on a durable engine, on the disk when it returns.
pub trait Kv: Send + Sync {
    /// The value under `key`.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Up to `limit` pairs with `start <= key < end` (without `end`: to the
    /// last key), in ascending key order, as of one moment.
    fn scan(&self, start: &[u8], end: Option<&[u8]>, limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;

    /// Applies the batch: all of it or none, in order.
    fn write(&self, batch: Batch) -> Result<()>;
}

/// The first key after every key that starts with `prefix`, the end of a
/// prefix scan; `None` when there is none (an empty or all-0xff prefix).
pub fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last < 0xff {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

/// Up to `limit` pairs whose key starts with `prefix`, in key order.
pub fn scan_prefix(kv: &dyn Kv, prefix: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    kv.scan(prefix, prefix_end(prefix).as_deref(), limit)
}

/// Removes every key that starts with `prefix`, in one batch; how many.
pub fn delete_prefix(kv: &dyn Kv, prefix: &[u8]) -> Result<usize> {
    let rows = scan_prefix(kv, prefix, usize::MAX)?;
    let mut b = Batch::new();
    for (k, _) in &rows {
        b.delete(k.clone());
    }
    kv.write(b)?;
    Ok(rows.len())
}

/// What every engine must do. Each engine's tests run [`contract::all`] with
/// a maker of fresh, empty stores.
#[cfg(test)]
pub(crate) mod contract {
    use super::*;

    pub fn all(make: &dyn Fn() -> Box<dyn Kv>) {
        get_put_delete(&*make());
        batch_in_order(&*make());
        scan_bounds(&*make());
    }

    fn put(kv: &dyn Kv, k: &[u8], v: &[u8]) {
        let mut b = Batch::new();
        b.put(k, v);
        kv.write(b).unwrap();
    }

    fn get_put_delete(kv: &dyn Kv) {
        assert_eq!(kv.get(b"a").unwrap(), None);
        put(kv, b"a", b"1");
        assert_eq!(kv.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
        put(kv, b"a", b"2");
        assert_eq!(kv.get(b"a").unwrap().as_deref(), Some(&b"2"[..]));
        put(kv, b"empty", b"");
        assert_eq!(kv.get(b"empty").unwrap(), Some(Vec::new()));
        let mut b = Batch::new();
        b.delete(b"a").delete(b"never there");
        kv.write(b).unwrap();
        assert_eq!(kv.get(b"a").unwrap(), None);
    }

    fn batch_in_order(kv: &dyn Kv) {
        let mut b = Batch::new();
        b.put(b"x", b"1").delete(b"x");
        b.delete(b"y").put(b"y", b"2");
        b.put(b"z", b"1").put(b"z", b"3");
        kv.write(b).unwrap();
        assert_eq!(kv.get(b"x").unwrap(), None);
        assert_eq!(kv.get(b"y").unwrap().as_deref(), Some(&b"2"[..]));
        assert_eq!(kv.get(b"z").unwrap().as_deref(), Some(&b"3"[..]));
        kv.write(Batch::new()).unwrap();
    }

    fn scan_bounds(kv: &dyn Kv) {
        let keys: [&[u8]; 7] = [&[0xff], &[0x01, 0x00], &[0x00], &[0x02], &[0x01, 0xff], &[0xff, 0xff], &[0x01]];
        let mut b = Batch::new();
        for k in keys {
            b.put(k, k);
        }
        kv.write(b).unwrap();
        let ks = |v: Vec<(Vec<u8>, Vec<u8>)>| -> Vec<Vec<u8>> {
            v.into_iter()
                .map(|(k, val)| {
                    assert_eq!(k, val);
                    k
                })
                .collect()
        };
        let sorted: Vec<Vec<u8>> =
            [&[0x00][..], &[0x01], &[0x01, 0x00], &[0x01, 0xff], &[0x02], &[0xff], &[0xff, 0xff]]
                .iter()
                .map(|k| k.to_vec())
                .collect();
        assert_eq!(ks(kv.scan(b"", None, 100).unwrap()), sorted);
        assert_eq!(ks(kv.scan(&[0x01], Some(&[0x02]), 100).unwrap()), sorted[1..4]);
        assert_eq!(ks(kv.scan(b"", None, 2).unwrap()), sorted[..2]);
        assert_eq!(ks(kv.scan(b"", None, 0).unwrap()), Vec::<Vec<u8>>::new());
        assert_eq!(ks(kv.scan(&[0x03], Some(&[0x03]), 10).unwrap()), Vec::<Vec<u8>>::new());
        assert_eq!(ks(kv.scan(&[0x03], Some(&[0x02]), 10).unwrap()), Vec::<Vec<u8>>::new());
        assert_eq!(ks(scan_prefix(kv, &[0x01], 100).unwrap()), sorted[1..4]);
        assert_eq!(ks(scan_prefix(kv, &[0xff], 100).unwrap()), sorted[5..]);
    }
}

#[cfg(test)]
mod tests {
    use super::prefix_end;

    #[test]
    fn prefix_ends() {
        assert_eq!(prefix_end(b"ab"), Some(b"ac".to_vec()));
        assert_eq!(prefix_end(&[0x01, 0xff]), Some(vec![0x02]));
        assert_eq!(prefix_end(&[0xff, 0xff]), None);
        assert_eq!(prefix_end(b""), None);
    }
}
