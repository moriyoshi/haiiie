//! Offline weight-order compaction and explicit handoff of caller-owned IDs.
//!
//! `DocId` is the storage ordinal, so moving it changes an application's join
//! key. Opening the directory ourselves acquires yesnodb's exclusive writer
//! lock, excluding ordinary YesnoStore handles. Upstream foreign read-only
//! handles bypass that lock and must be stopped by the caller. With all users
//! quiesced, every view present in that index layout and the recovery mapping
//! commit in one
//! batch. Opening the index is then refused until the application acknowledges
//! that it has migrated its own objects and saved filters.
//!
//! The mapping is part of the commit, not a file written after it. A crash after
//! commit but before the return therefore cannot lose the only record of which
//! document now occupies an ordinal. Repeating `compact` recovers the pending
//! mapping; it never performs another rewrite while one is awaiting handoff.
//!
//! This rewrites one entire namespace and holds its live rows and write batch
//! in memory. It is an explicit maintenance operation, not online or automatic
//! compaction. With yesno-core `03cd5a3` or later, allocated disk space can
//! return when compaction and acknowledgement each run in a fresh process;
//! an earlier open of the directory in the same process prevents that punch.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::Path;

use crate::store::Version;
use crate::{
    BLOCK_ORDINALS, Batch, BlockStats, DocId, Error, Index, IndexMeta, KeySpace, Kind, Result,
    SetSnapshot, SetStore, YesnoStore, block_of, slice,
};

/// The durable handoff produced by [`compact`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compaction {
    /// Identifies this operation for [`acknowledge_compaction`].
    pub source_version: Version,
    /// `old_ids[new_id]` is the previous ID of that live document.
    ///
    /// New IDs are exactly `0..old_ids.len()`, ordered by descending code weight
    /// then ascending old ID. Every other ordinal is free of this namespace's data after
    /// the rewrite. Deleted documents have no entry. Score ties can change
    /// order because queries break them by the new ID.
    pub old_ids: Vec<DocId>,
}

/// Compact a closed database's namespace, or recover its pending ID mapping.
///
/// Stop the server and drop **all** database handles first. The directory lock
/// excludes ordinary YesnoStore handles across processes. Foreign read-only
/// handles opened through upstream `Db::open_reader` and wrapped by
/// [`YesnoStore::from_db`] bypass that lock: the caller must stop them too.
/// Other namespaces and
/// application keys outside this index's key kinds are untouched.
///
/// Live rows are packed by descending weight, breaking ties by ascending old ID.
/// Forward codes, liveness and attributes are rebuilt in one atomic commit.
/// Binary indexes also rebuild dimension postings, weight planes and statistics;
/// residual indexes do not store those views. Deleted documents' attributes are
/// discarded. The ID mapping and a pending marker land in that same commit.
/// [`Index::open`] refuses this namespace until [`acknowledge_compaction`].
///
/// On an I/O error the commit may already have landed: retry this function to
/// recover the mapping. Never reconstruct it from the now-reordered corpus.
/// To return allocated disk space with yesno-core `03cd5a3` or later, invoke
/// this from a process that has not previously opened the directory, then
/// acknowledge from a separate fresh process after migrating caller IDs.
pub fn compact(dir: impl AsRef<Path>, namespace: u8) -> Result<Compaction> {
    let store = YesnoStore::open(dir)?;
    let keys = KeySpace::new(namespace);
    let snap = store.snapshot()?;
    // Refuse a missing or incompatible namespace even during recovery.
    IndexMeta::from_ordinals(&snap.load(keys.meta())?)?;
    if let Some(pending) = read_pending(&snap, keys)? {
        return Ok(pending);
    }
    let index = Index::open(store.clone(), namespace)?;
    let meta = index.meta();
    let binary_views = !meta.is_residual();
    let mut rows = Vec::new();
    let mut live = slice::zero_mask();
    let mut forward = slice::zero_mask();
    let mut forward_block = None;
    let mut offsets = Vec::new();
    if let Some(last) = snap.max(keys.live())? {
        if last > index.max_doc_id() {
            return Err(Error::CorruptMeta(
                "compaction found an unaddressable live ID",
            ));
        }
        for block in 0..=block_of(last) {
            snap.load_block(keys.live(), block, &mut live)?;
            slice::offsets(&live, &mut offsets);
            for &offset in &offsets {
                let old = DocId(block * BLOCK_ORDINALS + u64::from(offset));
                let addr = index.row_addr(old);
                if forward_block != Some(addr.block) {
                    snap.load_block(keys.forward(), addr.block, &mut forward)?;
                    forward_block = Some(addr.block);
                }
                let lo = (addr.base % BLOCK_ORDINALS) as usize / 64;
                let words = (meta.row_bits / 64) as usize;
                let code = forward[lo..lo + words].to_vec();
                let weight = code.iter().map(|w| w.count_ones()).sum::<u32>();
                // A valid stored row has no padding bits. Refuse instead of
                // silently converting corruption into a different live code.
                if !meta.dims.is_multiple_of(64) && code[words - 1] >> (meta.dims % 64) != 0 {
                    return Err(Error::CorruptMeta("compaction found nonzero row padding"));
                }
                rows.push((old, weight, code));
            }
        }
    }
    // Match the measured ordinal construction: heavier codes first. The scan
    // carries its top-k threshold between blocks, so reversing the order is
    // not equivalent merely because the weight ranges remain equally narrow.
    rows.sort_unstable_by_key(|(old, weight, _)| (Reverse(*weight), *old));
    let result = Compaction {
        source_version: snap.version(),
        old_ids: rows.iter().map(|(old, _, _)| *old).collect(),
    };
    let new_ids: BTreeMap<u64, u64> = result
        .old_ids
        .iter()
        .enumerate()
        .map(|(new, old)| (old.get(), new as u64))
        .collect();

    let mut batch = Batch::new();
    // Enumerate persisted keys as well as memtable keys. In particular, ATTR
    // has no term registry, and dead-only terms must be cleared too.
    let raw = store.db().snapshot()?;
    let mut attributes = Vec::new();
    for kind in [
        Kind::Live,
        Kind::Forward,
        Kind::Dim,
        Kind::ZPlane,
        Kind::Stat,
        Kind::Attr,
    ] {
        let (lo, hi) = keys.kind_range(kind);
        for key in raw.key_range(lo, hi)? {
            batch.delete_key(key);
            if kind == Kind::Attr {
                attributes.push(key);
            }
        }
    }
    batch.delete_key(keys.remap());
    let mut stats: BTreeMap<u64, BlockStats> = BTreeMap::new();
    for (new, (old, weight, code)) in rows.iter().enumerate() {
        let id = new as u64;
        let addr = index.row_addr(DocId(id));
        for (word, &bits) in code.iter().enumerate() {
            let mut bits = bits;
            while bits != 0 {
                let bit = (word as u32) * 64 + bits.trailing_zeros();
                bits &= bits - 1;
                batch.insert(keys.forward(), addr.base + u64::from(bit));
                if binary_views {
                    batch.insert(keys.dim(bit), id);
                }
            }
        }
        if binary_views {
            let z = meta.dims - weight;
            for plane in 0..meta.z_planes() {
                if z >> plane & 1 != 0 {
                    batch.insert(keys.zplane(plane), id);
                }
            }
            stats
                .entry(block_of(id))
                .and_modify(|s| {
                    s.w_min = s.w_min.min(*weight);
                    s.w_max = s.w_max.max(*weight);
                })
                .or_insert(BlockStats {
                    w_min: *weight,
                    w_max: *weight,
                });
        }
        batch.insert(keys.live(), id);
        // Each mapping row is one word. Bit 63 is presence, so old ID zero
        // is representable. IDs are bounded by 2^36 - 1 by the STAT key space.
        let mut bits = old.get() | (1u64 << 63);
        while bits != 0 {
            batch.insert(keys.remap(), id * 64 + u64::from(bits.trailing_zeros()));
            bits &= bits - 1;
        }
    }
    let mut attr = slice::zero_mask();
    for key in attributes {
        // Visit the blocks containing live source IDs, not a dead attribute's
        // possibly enormous ordinal span. Deleting the key already removed
        // every stale membership; only live documents get one inserted back.
        let mut loaded = None;
        for (&old, &new) in &new_ids {
            let block = block_of(old);
            if loaded != Some(block) {
                snap.load_block(key, block, &mut attr)?;
                loaded = Some(block);
            }
            let offset = (old % BLOCK_ORDINALS) as usize;
            if attr[offset / 64] >> (offset % 64) & 1 != 0 {
                batch.insert(key, new);
            }
        }
    }
    for (block, stats) in stats {
        for bit in stats.to_ordinals() {
            batch.insert(keys.stat(block), bit);
        }
    }
    batch.insert(keys.compaction(), 0);
    for bit in 0..64 {
        if result.source_version >> bit & 1 != 0 {
            batch.insert(keys.compaction(), bit + 1);
        }
    }
    batch.group_by_key();
    store.write(&batch)?;
    // Drop snapshots before checkpointing, allowing the old storage to retire.
    drop(raw);
    drop(snap);
    store.flush()?;
    Ok(result)
}

/// Finish a compaction after durably migrating the application's ID references.
///
/// This is the caller's assertion that objects, saved filters and optional float
/// rows now use the IDs in [`Compaction::old_ids`]. It deletes the recovery map
/// and pending marker atomically, enabling [`Index::open`] again. Keep a copy
/// if the application needs historical IDs. The database must still be closed.
pub fn acknowledge_compaction(
    dir: impl AsRef<Path>,
    namespace: u8,
    source_version: Version,
) -> Result<()> {
    let store = YesnoStore::open(dir)?;
    let keys = KeySpace::new(namespace);
    let snap = store.snapshot()?;
    let pending = read_pending(&snap, keys)?;
    if pending.as_ref().map(|p| p.source_version) != Some(source_version) {
        return Err(Error::CompactionMismatch(source_version));
    }
    let mut batch = Batch::new();
    batch.delete_key(keys.remap());
    batch.delete_key(keys.compaction());
    store.write(&batch)?;
    drop(snap);
    store.flush()
}

fn read_pending(snap: &impl SetSnapshot, keys: KeySpace) -> Result<Option<Compaction>> {
    let marker = snap.load(keys.compaction())?;
    if marker.is_empty() {
        return Ok(None);
    }
    if marker[0] != 0 || marker.iter().any(|&b| b > 64) {
        return Err(Error::CorruptMeta("invalid compaction marker"));
    }
    let source_version = marker[1..].iter().fold(0, |v, b| v | (1u64 << (b - 1)));
    let count = snap.cardinality(keys.live())?;
    if snap.max(keys.live())? != count.checked_sub(1)
        || snap.max(keys.remap())? != count.checked_mul(64).and_then(|n| n.checked_sub(1))
    {
        return Err(Error::CorruptMeta(
            "compaction mapping does not cover the live IDs",
        ));
    }
    let mut old_ids = Vec::new();
    let mut mask = slice::zero_mask();
    let mut remaining = count;
    let mut block = 0;
    while remaining != 0 {
        snap.load_block(keys.remap(), block, &mut mask)?;
        for &word in mask.iter().take(remaining.min(mask.len() as u64) as usize) {
            // Presence in bit 63; the allowed ID bits come from the same
            // STAT-key index and block geometry that bound Index::max_doc_id.
            let max_old = (KeySpace::INDEX_MAX + 1) * BLOCK_ORDINALS - 1;
            if word & !max_old != 1 << 63 {
                return Err(Error::CorruptMeta("invalid compaction mapping row"));
            }
            old_ids.push(DocId(word & !(1 << 63)));
        }
        remaining = remaining.saturating_sub(mask.len() as u64);
        block += 1;
    }
    let mut unique = old_ids.clone();
    unique.sort_unstable();
    if unique.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::CorruptMeta(
            "duplicate source ID in compaction mapping",
        ));
    }
    Ok(Some(Compaction {
        source_version,
        old_ids,
    }))
}
