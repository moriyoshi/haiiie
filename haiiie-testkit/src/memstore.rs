//! An in-memory [`SetStore`], and the reason it exists.
//!
//! `MemStore` is not a convenience. It is the **second implementation** that
//! makes the store abstraction checkable: M1's central property is that a search
//! over `MemStore` and a search over `YesnoStore` return byte-identical `Hits`
//! for the same corpus. A trait with one implementation is under-determined --
//! whatever that implementation does is the specification -- and this is the
//! cheapest way to stop that being true here.
//!
//! It is deliberately the dumbest possible implementation: a `BTreeMap` of
//! `BTreeSet`s, cloned on snapshot. Nothing about it is shared with the real
//! store, so an assumption that happens to hold in one is unlikely to hold in
//! both by accident.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use haiiie_core::Result;
use haiiie_core::keyspace::BLOCK_ORDINALS;
use haiiie_core::slice::BlockMask;
use haiiie_core::store::{Batch, Op, SetSnapshot, SetStore, Version};

type Sets = BTreeMap<u64, BTreeSet<u64>>;

/// An in-memory store.
#[derive(Default, Debug)]
pub struct MemStore {
    inner: Mutex<(Arc<Sets>, Version)>,
}

impl MemStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many keys hold at least one ordinal.
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.inner.lock().expect("poisoned").0.len()
    }
}

/// A cloned, therefore genuinely immutable, read view.
#[derive(Debug)]
pub struct MemSnapshot {
    sets: Arc<Sets>,
    version: Version,
}

impl SetStore for MemStore {
    type Snap = MemSnapshot;

    /// `O(1)`: bump a refcount, do not copy the data.
    ///
    /// This deep-copied the whole map until M5, which made taking a snapshot
    /// cost one allocation per key -- millions on a large corpus, inside every
    /// measured query. An allocation budget written to catch corpus-proportional
    /// work in *haiiie* was therefore measuring the test double instead, and
    /// said so in an assertion message blaming the query path.
    ///
    /// Copy-on-write also makes this a more faithful double: a real MVCC
    /// snapshot is a refcount bump, and a stand-in whose cost model differs from
    /// the thing it stands in for will mislead every measurement taken through
    /// it.
    fn snapshot(&self) -> Result<Self::Snap> {
        let g = self.inner.lock().expect("poisoned");
        Ok(MemSnapshot {
            sets: Arc::clone(&g.0),
            version: g.1,
        })
    }

    fn supports_chunk_patch(&self) -> bool {
        true
    }

    fn write(&self, batch: &Batch) -> Result<Version> {
        let mut g = self.inner.lock().expect("poisoned");
        let (arc, version) = &mut *g;
        // Copy-on-write: existing snapshots keep the old map.
        let sets = Arc::make_mut(arc);
        for op in batch.ops() {
            match op {
                Op::Insert(k, o) => {
                    sets.entry(*k).or_default().insert(*o);
                }
                Op::Remove(k, o) => {
                    if let Some(s) = sets.get_mut(k) {
                        s.remove(o);
                        if s.is_empty() {
                            sets.remove(k);
                        }
                    }
                }
                Op::DeleteKey(k) => {
                    sets.remove(k);
                }
                Op::RemoveRange(k, lo, hi) => {
                    // The slow obvious implementation, on purpose: this store is
                    // the oracle the fast one is checked against, and a range
                    // removal that shares an off-by-one with its counterpart
                    // would be invisible.
                    if let Some(s) = sets.get_mut(k) {
                        for o in *lo..=*hi {
                            s.remove(&o);
                        }
                        if s.is_empty() {
                            sets.remove(k);
                        }
                    }
                }
                Op::PatchChunk(k, prefix, masks) => {
                    let (clear, set) = masks.as_ref();
                    // Deliberately obvious: the independent store is the oracle
                    // for yesnodb's native masked-container operation.
                    let base = prefix << 16;
                    let members = sets.entry(*k).or_default();
                    for bit in clear.iter() {
                        members.remove(&(base | u64::from(bit)));
                    }
                    for bit in set.iter() {
                        members.insert(base | u64::from(bit));
                    }
                    if members.is_empty() {
                        sets.remove(k);
                    }
                }
            }
        }
        *version += 1;
        Ok(*version)
    }
}

impl SetSnapshot for MemSnapshot {
    fn version(&self) -> Version {
        self.version
    }

    fn load(&self, key: u64) -> Result<Vec<u64>> {
        Ok(self
            .sets
            .get(&key)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default())
    }

    fn key_range(&self, lo: u64, hi: u64) -> Result<Vec<u64>> {
        Ok(self.sets.range(lo..hi).map(|(&key, _)| key).collect())
    }

    fn cardinality(&self, key: u64) -> Result<u64> {
        Ok(self.sets.get(&key).map_or(0, |s| s.len() as u64))
    }

    fn contains(&self, key: u64, ordinal: u64) -> Result<bool> {
        Ok(self.sets.get(&key).is_some_and(|s| s.contains(&ordinal)))
    }

    fn max(&self, key: u64) -> Result<Option<u64>> {
        Ok(self.sets.get(&key).and_then(|s| s.last().copied()))
    }

    /// Range-scan the block directly.
    ///
    /// Overriding this is not an optimization detail: the default routes through
    /// `load`, which materializes the **whole** posting list once per block per
    /// key, so a four-block scan over 128 dimensions performed 512 full
    /// materializations. The allocation budget caught it at 4.4 million
    /// allocations against 525.
    fn load_block(&self, key: u64, block: u64, out: &mut BlockMask) -> Result<bool> {
        out.fill(0);
        let Some(set) = self.sets.get(&key) else {
            return Ok(false);
        };
        let base = block * BLOCK_ORDINALS;
        let mut any = false;
        for &o in set.range(base..base + BLOCK_ORDINALS) {
            let b = (o - base) as usize;
            out[b >> 6] |= 1u64 << (b & 63);
            any = true;
        }
        Ok(any)
    }
}
