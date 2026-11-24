//! The store in memory, a BTreeMap: for tests and the simulation.

use std::{
    collections::BTreeMap,
    ops::Bound,
    sync::{Mutex, MutexGuard},
};

use super::{Batch, Kv, Op, Result};

#[derive(Default)]
pub struct MemKv {
    map: Mutex<BTreeMap<Vec<u8>, Vec<u8>>>,
}

impl MemKv {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<Vec<u8>, Vec<u8>>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Kv for MemKv {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.lock().get(key).cloned())
    }

    fn scan(&self, start: &[u8], end: Option<&[u8]>, limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        // BTreeMap::range panics on a range that ends before it starts.
        if end.is_some_and(|e| e <= start) {
            return Ok(Vec::new());
        }
        let hi = end.map_or(Bound::Unbounded, Bound::Excluded);
        let map = self.lock();
        Ok(map
            .range::<[u8], _>((Bound::Included(start), hi))
            .take(limit)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    fn write(&self, batch: Batch) -> Result<()> {
        let mut map = self.lock();
        for op in batch.ops {
            match op {
                Op::Put(k, v) => {
                    map.insert(k, v);
                }
                Op::Delete(k) => {
                    map.remove(&k);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract() {
        crate::kv::contract::all(&|| Box::new(MemKv::new()));
    }
}
