//! The embedded [`SetStore`] over a real yesnodb database.
//!
//! # What this adapter is careful about
//!
//! **Ordering within a batch is preserved.** `Writer::put` clears a row before
//! writing it, so a replace is a `remove` run followed by inserts. Reordering
//! those -- or letting a store apply them as sets rather than in sequence --
//! turns a replace into a merge, and a merged row is a code that was never
//! written. yesnodb's `WriteBatch` applies operations in order, and this adapter
//! must not batch them into anything that does not.
//!
//! **One commit per batch.** yesnodb assigns one version per batch under all
//! participating shard locks, which is what makes a document's forward bits and
//! its liveness land together. Splitting a batch to be helpful would break the
//! only property `Writer` promises.
//!
//! **A whole row is cleared as a range, not as `row_bits` operations.** A
//! commit's cost tracks operations touched rather than work done -- each one
//! pays a tree descent under the exclusive lock a concurrent reader waits on --
//! and a forward row is contiguous by construction, so `Op::RemoveRange` maps
//! straight onto the storage layer's own range form. Both ends are inclusive on
//! both sides of this boundary, deliberately, so the two cannot disagree at the
//! edge.
//!
//! **Fresh residual tiles use native chunk patches.** The writer proves the
//! rows are new, aligned and consecutive before constructing masks. This
//! adapter passes each mask to yesnodb in the original batch order with LIVE
//! and ATTR changes; ordinary point and range writes still handle churn.
//!
//! # What it reads through
//!
//! `Snapshot::key_stream`, for both a single block and a held-open lane cursor.
//! This block used to say the adapter did **not** use it yet -- that `load` was
//! the M1 shape and streaming was wanted at M2 -- which stopped being true when
//! the read path was rewritten and stayed in the header for a release. `load`
//! materializes an `OrdSet` for a whole key, once per block per query dimension;
//! a stream plus a `seek` reads the one container instead, which is what
//! `key_stream` was added upstream for.
//!
//! The lane cursor goes further and holds the stream open across blocks, because
//! opening one per block was the forward path's dominant cost at exactly the
//! selectivities that path is chosen for.
//!
//! Forward scoring visits borrowed bitmap words through `Lanes::with_block`.
//! Copying a whole 8 KiB bitmap to inspect a 32-byte row amplifies sparse reads
//! by 256; borrowing lets the scorer touch only its rows. The inverted path
//! borrows all lane containers for one block together and resolves their bitmap
//! words once for a tiled arithmetic visit. Arrays, runs and unaligned bitmaps
//! use masks allocated lazily per lane and reused across blocks. An absent lane
//! always supplies zero, even if its scratch held a previous block's bits.
//! Every path still passes the storage engine's identity, checksum and
//! snapshot-liveness checks.

use yesno_core::{Db, Snapshot};

use crate::Result;
use crate::store::{Batch, Op, SetSnapshot, SetStore, Version};

static ZERO_BLOCK: crate::slice::BlockMask = crate::slice::zero_mask();

/// A [`SetStore`] backed by an embedded yesnodb database.
#[derive(Clone)]
pub struct YesnoStore {
    db: Db,
}

/// How many physical shards a newly created index gets.
///
/// **This is a read-concurrency setting, not a storage one.** yesnodb guards
/// each shard's store with a mutex and takes it once per chunk read, so the
/// shard count is the number of readers that can decode a chunk at the same
/// time. At the default of eight, twenty threads spend their time queueing.
///
/// Measured, 2 097 152 documents at D=256, Hamming, k=10 on a twenty-core
/// machine -- query latency in milliseconds, ingest in documents per second:
///
/// ```text
/// shards   ingest/s   checkpoint   serial   8 threads   20 threads   speedup
///      8     37 724        0.2 s   8.41 ms     5.93 ms      7.93 ms     1.06x
///     16     35 937        0.3 s   8.33 ms     4.36 ms      4.77 ms     1.74x
///     32     33 161        0.6 s   7.65 ms     3.71 ms      3.72 ms     2.05x
///     64     28 626        1.1 s   8.40 ms     2.95 ms      2.88 ms     2.91x
/// ```
///
/// Thirty-two is where the trade stops being one-sided: it doubles parallel
/// throughput for 12% of ingest rate, where sixty-four buys a further 1.3x for
/// another 12% of ingest and twice the checkpoint time. Serial latency is flat
/// across the whole range, so nothing is given up by a single-threaded caller.
///
/// Reading the same numbers from the other end, the parallel speedup at the old
/// default was **1.06x on twenty cores** -- which is to say the parallel scan
/// was very nearly pointless, and the reason had nothing to do with the scan.
///
/// # It is fixed at creation
///
/// The count is written to the MANIFEST and `opts.shards` is ignored on reopen,
/// so this choice cannot be revised later without rebuilding the index. That is
/// why it is `pub` and why [`YesnoStore::open_with_shards`] exists: an ingest-
/// heavy index with one querying thread wants a smaller number than this, and
/// it has to say so before the first write.
pub const DEFAULT_SHARDS: usize = 32;

impl YesnoStore {
    /// Open or create a database in `dir`, with [`DEFAULT_SHARDS`] shards if it
    /// is being created.
    ///
    /// yesnodb takes an exclusive directory lock, so exactly one process may
    /// hold this open for writing.
    pub fn open(dir: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::open_with_shards(dir, DEFAULT_SHARDS)
    }

    /// Open or create a database in `dir`, deferring automatic checkpoints so
    /// the caller decides when the stall happens.
    ///
    /// # What this changes, and what it does not
    ///
    /// A checkpoint blocks every query in flight on the shard it is working on,
    /// and normally it is triggered from inside `commit` on whichever thread
    /// happened to write -- so a serving process receives that stall at a moment
    /// nothing chose. This raises the automatic triggers out of the way, leaving
    /// [`SetStore::flush`] as the only thing that starts one.
    ///
    /// **It moves the stall, it does not shrink it**, and it does not touch the
    /// part of the tail most callers care about. Measured: with checkpoints
    /// deferred so none run at all, a concurrent writer still takes p99 from
    /// 4.38 ms to 82.61 ms -- the p99 body is **ordinary commits**, and this
    /// changes none of it. What deferring does move is the worst case, which
    /// checkpoints own: 363.76 ms with two of them against 138.29 ms with none.
    ///
    /// Those two figures are from before `Writer::put` learned to emit only the
    /// difference, and with overwrites that rewrote each document with its own
    /// code. Both are therefore upper bounds on what a caller sees today: the
    /// commit half fell by about 5x for a real update. The **shape** is what
    /// this comment is for and it is unchanged -- deferring moves the worst case
    /// and leaves the body alone -- but do not quote 82.61 ms as current.
    ///
    /// So the value is narrow and worth stating narrowly: a load-then-serve
    /// deployment can take the whole checkpoint cost before it starts answering,
    /// paying it once at a moment it chose. A continuously ingesting one gains
    /// almost nothing, because what it is suffering from is not checkpoints.
    ///
    /// # The cost is disk, and `max_dirty_bytes` is deliberately left alone
    ///
    /// Deferring a checkpoint defers the reclamation and log truncation it
    /// performs, so the write-ahead log and the unreclaimed space grow until one
    /// runs. `max_dirty_bytes` is **not** raised: it is the stall-the-writer
    /// valve, and removing it would trade a bounded latency problem for an
    /// unbounded memory one. A caller who never calls `flush` therefore still
    /// gets a checkpoint eventually -- it is just no longer a surprise.
    pub fn open_deferring_checkpoints(
        dir: impl AsRef<std::path::Path>,
        shards: usize,
    ) -> Result<Self> {
        let d = yesno_core::DbOptions::default();
        let opts = yesno_core::DbOptions {
            shards,
            policy: yesno_core::checkpoint::CheckpointPolicy {
                // Each trigger pushed far enough out that ordinary operation
                // does not reach it, and every one of them left *reachable*:
                // a policy that can never fire is a policy that cannot bound
                // anything, and these are the bounds.
                dirty_bytes: d.policy.max_dirty_bytes,
                wal_bytes: d.policy.max_wal_bytes,
                interval_secs: u64::MAX,
                ..d.policy
            },
            ..d
        };
        Ok(Self {
            db: Db::open_with(dir, opts)?,
        })
    }

    /// Open or create a database in `dir` with an explicit shard count.
    ///
    /// `shards` applies only when the database is **created**; reopening reads
    /// the count from the MANIFEST and ignores this argument, because the shard
    /// count is part of the routing function and a reopen at a different count
    /// would misroute every key. See [`DEFAULT_SHARDS`] for what the number
    /// trades off.
    pub fn open_with_shards(dir: impl AsRef<std::path::Path>, shards: usize) -> Result<Self> {
        let opts = yesno_core::DbOptions {
            shards,
            ..Default::default()
        };
        Ok(Self {
            db: Db::open_with(dir, opts)?,
        })
    }

    /// Wrap an already-open database.
    #[must_use]
    pub fn from_db(db: Db) -> Self {
        Self { db }
    }

    /// The underlying database.
    #[must_use]
    pub fn db(&self) -> &Db {
        &self.db
    }
}

/// A consistent read view of a yesnodb database.
pub struct YesnoSnapshot {
    snap: Snapshot,
}

impl std::fmt::Debug for YesnoStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YesnoStore").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for YesnoSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YesnoSnapshot")
            .field("version", &self.snap.version())
            .finish_non_exhaustive()
    }
}

impl SetStore for YesnoStore {
    type Snap = YesnoSnapshot;

    fn snapshot(&self) -> Result<Self::Snap> {
        Ok(YesnoSnapshot {
            snap: self.db.snapshot()?,
        })
    }

    fn write(&self, batch: &Batch) -> Result<Version> {
        let mut b = self.db.batch();
        for op in batch.ops() {
            match op {
                Op::Insert(k, o) => {
                    b.insert(*k, *o);
                }
                Op::Remove(k, o) => {
                    b.remove(*k, *o);
                }
                Op::DeleteKey(k) => {
                    b.delete_key(*k);
                }
                Op::RemoveRange(k, lo, hi) => {
                    // Both ends inclusive on both sides of this boundary.
                    b.remove_range(*k, *lo, *hi);
                }
                Op::PatchChunk(k, prefix, masks) => {
                    let (clear, set) = masks.as_ref();
                    b.patch_chunk(*k, *prefix, clear, set);
                }
            }
        }
        Ok(b.commit()?.version)
    }

    fn supports_chunk_patch(&self) -> bool {
        true
    }

    fn flush(&self) -> Result<()> {
        self.db.checkpoint()?;
        Ok(())
    }
}

impl SetSnapshot for YesnoSnapshot {
    fn version(&self) -> Version {
        self.snap.version()
    }

    fn load(&self, key: u64) -> Result<Vec<u64>> {
        Ok(self.snap.load(key)?.iter().collect())
    }

    fn key_range(&self, lo: u64, hi: u64) -> Result<Vec<u64>> {
        Ok(self.snap.key_range(lo, hi)?)
    }

    fn cardinality(&self, key: u64) -> Result<u64> {
        Ok(self.snap.cardinality(key)?)
    }

    fn contains(&self, key: u64, ordinal: u64) -> Result<bool> {
        Ok(self.snap.contains(key, ordinal)?)
    }

    fn max(&self, key: u64) -> Result<Option<u64>> {
        Ok(self.snap.max(key)?)
    }

    /// Stream to the one chunk this block needs.
    ///
    /// The default routes through `load`, which materializes an `OrdSet` for the
    /// whole key -- once per block per query dimension. `key_stream` plus a
    /// `seek` reads the one container instead, which is what
    /// `Snapshot::key_stream` was added upstream for.
    ///
    /// A stream is opened per call, and **that is now the slow path**. The
    /// reasoning that once justified it -- reopening a source is a flat ~61 ns
    /// upstream since its plan moved behind an `Arc` -- priced the wrong thing:
    /// what a scan pays is not one reopen but `query width * blocks` of them,
    /// and measured end to end that is 8.68 ms against 2.27 ms for the same
    /// bytes through streams held open. [`SetSnapshot::open_lanes`] is the path
    /// the scan takes; this one remains for single reads and for callers that
    /// have no fixed key list, where opening once is all there is to do.
    fn load_block(&self, key: u64, block: u64, out: &mut crate::slice::BlockMask) -> Result<bool> {
        use yesno_core::stream::ChunkStream;

        out.fill(0);
        let mut stream = self.snap.key_stream(key)?;
        stream.seek(block)?;
        let Some((prefix, container)) = stream.next_chunk()? else {
            return Ok(false);
        };
        if prefix != block {
            return Ok(false);
        }
        Ok(fill_from(&container, out))
    }

    fn open_lanes(&self, keys: &[u64]) -> Result<Option<Box<dyn crate::store::Lanes>>> {
        Ok(Some(Box::new(YesnoLanes {
            snap: self.snap.clone(),
            lanes: keys
                .iter()
                .map(|&key| {
                    Ok(Lane {
                        key,
                        stream: self.snap.key_stream(key)?,
                        next: 0,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            borrowed: Vec::with_capacity(keys.len()),
            expanded: std::iter::repeat_with(|| None).take(keys.len()).collect(),
        })))
    }
}

/// One key's stream, plus the block it is positioned before.
struct Lane {
    key: u64,
    stream: yesno_core::KeyStream,
    /// The lowest block this stream can still reach. Reading below it needs a
    /// fresh stream, because a chunk stream only moves forward.
    next: u64,
}

/// [`crate::store::Lanes`] over a yesnodb snapshot.
///
/// Holds a clone of the snapshot rather than borrowing it. `Snapshot` is a
/// cheap handle over `Arc`s and a reader slot, and owning one keeps this type
/// free of a lifetime -- which is what lets the scan park a cursor in its
/// per-worker buffers and rebuild it, snapshot and all, when a read is evicted.
struct YesnoLanes {
    snap: Snapshot,
    lanes: Vec<Lane>,
    /// Owned refcounted containers keep bitmap words alive throughout one
    /// all-lane visit. Clearing drops the prior block before fetching the next.
    borrowed: Vec<Option<yesno_core::Container>>,
    /// Index into the scan's reusable expansion masks for non-bitmap lanes.
    /// Bitmap lanes do not pay an 8 KiB allocation or copy.
    expanded: Vec<Option<usize>>,
}

// 1,024 references occupy 8 KiB on 64-bit targets, the same size as one
// BlockMask. This covers the measured D=256 fixture and D=512 binary queries
// including Z planes; wider queries use the allocation-free lazy view below.
const STACK_BORROWED_LANES: usize = 1024;

struct ResolvedBlockLanes<'a, 'b> {
    masks: &'a [&'b crate::slice::BlockMask],
}

impl crate::store::BlockLanes for ResolvedBlockLanes<'_, '_> {
    fn len(&self) -> usize {
        self.masks.len()
    }

    fn lane(&self, index: usize) -> &crate::slice::BlockMask {
        self.masks[index]
    }

    fn as_refs(&self) -> Option<&[&crate::slice::BlockMask]> {
        Some(self.masks)
    }
}

struct YesnoBlockLanes<'a> {
    borrowed: &'a [Option<yesno_core::Container>],
    expanded: &'a [Option<usize>],
    scratch: &'a [crate::slice::BlockMask],
}

impl crate::store::BlockLanes for YesnoBlockLanes<'_> {
    fn len(&self) -> usize {
        self.borrowed.len()
    }

    fn lane(&self, index: usize) -> &crate::slice::BlockMask {
        let Some(container) = self.borrowed[index].as_ref() else {
            return &ZERO_BLOCK;
        };
        if let Some(words) = yesno_core::unstable_arrow::bitmap_words(container)
            && let Ok(mask) = words.try_into()
        {
            return mask;
        }
        self.expanded[index]
            .map(|slot| &self.scratch[slot])
            .unwrap_or(&ZERO_BLOCK)
    }
}

impl YesnoLanes {
    fn chunk(&mut self, lane: usize, block: u64) -> Result<Option<yesno_core::Container>> {
        use yesno_core::stream::ChunkStream;

        let l = &mut self.lanes[lane];
        // Going backwards means the caller redid a block -- an eviction retry,
        // or a caller reusing the cursor for a second query. A stream cannot
        // rewind, and answering "nothing here" would delete every document in
        // the block from the result without any error. Reopen instead: it costs
        // exactly what `load_block` costs, on a path taken once per retry.
        if block < l.next {
            l.stream = self.snap.key_stream(l.key)?;
        }
        l.stream.seek(block)?;
        let Some((prefix, container)) = l.stream.next_chunk()? else {
            // Nothing at or after `block`. Every later block is empty too, so
            // `block + 1` is the right resting place: a request for anything
            // earlier reopens, anything later is genuinely absent.
            l.next = block + 1;
            return Ok(None);
        };
        // `next_chunk` **consumed** the chunk it returned, so the earliest block
        // this stream can still serve is the one after `prefix` -- not after
        // `block`, and not `prefix` itself. Whenever the seek crossed a gap
        // those differ, and getting it wrong makes the very next read land past
        // the chunk it asked for and report a populated block as empty.
        l.next = prefix + 1;
        Ok((prefix == block).then_some(container))
    }
}

impl crate::store::Lanes for YesnoLanes {
    fn read(&mut self, lane: usize, block: u64, out: &mut crate::slice::BlockMask) -> Result<bool> {
        out.fill(0);
        Ok(self.chunk(lane, block)?.is_some_and(|c| fill_from(&c, out)))
    }

    fn with_block(
        &mut self,
        lane: usize,
        block: u64,
        visit: &mut dyn FnMut(&crate::slice::BlockMask),
    ) -> Result<()> {
        let container = self.chunk(lane, block)?;
        if let Some(ref c) = container
            && let Some(words) = yesno_core::unstable_arrow::bitmap_words(c)
            && let Ok(mask) = words.try_into()
        {
            visit(mask);
        } else {
            let mut mask = crate::slice::zero_mask();
            if let Some(c) = container {
                fill_from(&c, &mut mask);
            }
            visit(&mask);
        }
        Ok(())
    }

    fn with_blocks(
        &mut self,
        lane_count: usize,
        block: u64,
        scratch: &mut Vec<crate::slice::BlockMask>,
        visit: &mut dyn FnMut(&dyn crate::store::BlockLanes),
    ) -> Result<()> {
        debug_assert!(lane_count <= self.lanes.len());
        self.borrowed.clear();
        self.expanded.fill(None);
        scratch.clear();
        for lane in 0..lane_count {
            let container = self.chunk(lane, block)?;
            if let Some(ref c) = container
                && yesno_core::unstable_arrow::bitmap_words(c)
                    .is_none_or(|words| words.len() != crate::slice::BLOCK_WORDS)
            {
                let slot = scratch.len();
                scratch.push(crate::slice::zero_mask());
                fill_from(c, &mut scratch[slot]);
                self.expanded[lane] = Some(slot);
            }
            self.borrowed.push(container);
        }
        if lane_count <= STACK_BORROWED_LANES {
            let mut masks = [&ZERO_BLOCK; STACK_BORROWED_LANES];
            for (lane, slot) in masks.iter_mut().enumerate().take(lane_count) {
                *slot = if let Some(container) = self.borrowed[lane].as_ref() {
                    if let Some(words) = yesno_core::unstable_arrow::bitmap_words(container)
                        && let Ok(mask) = words.try_into()
                    {
                        mask
                    } else {
                        self.expanded[lane]
                            .map(|slot| &scratch[slot])
                            .unwrap_or(&ZERO_BLOCK)
                    }
                } else {
                    &ZERO_BLOCK
                };
            }
            visit(&ResolvedBlockLanes {
                masks: &masks[..lane_count],
            });
        } else {
            visit(&YesnoBlockLanes {
                borrowed: &self.borrowed,
                expanded: &self.expanded,
                scratch,
            });
        }
        Ok(())
    }
}

/// One chunk's bits, written into a block mask. Returns whether anything was set.
///
/// Three arms, cheapest first, and the split is the measured one.
///
/// `bitmap_words` lends a bitmap container's words directly -- one
/// `copy_from_slice` rather than reassembling 1 024 `u64`s from bytes. It is the
/// API this project asked upstream for.
///
/// It returns `None` for **two** distinct reasons, and conflating them would be
/// wrong: the container is not a bitmap ( array or run, which store positions
/// and have no word image to lend ), or it is a store-backed buffer whose bytes
/// are not 8-byte aligned.
fn fill_from(container: &yesno_core::Container, out: &mut crate::slice::BlockMask) -> bool {
    if let Some(words) = yesno_core::unstable_arrow::bitmap_words(container)
        && words.len() == out.len()
    {
        out.copy_from_slice(words);
        return out.iter().any(|w| *w != 0);
    }
    // A run container is not sparse, and treating it as though it were is what
    // the scatter below did. `LIVE` is a contiguous range of ordinals, so every
    // one of its chunks is a *single whole-chunk run* -- and rebuilding that as
    // a mask one position at a time costs 65 536 iterations to arrive at 1 024
    // words of all ones. Measured at 110 us per block against 2.3 us for a
    // dimension read: 19% of a whole query at two million documents, spent
    // setting bits individually. Filling word ranges instead makes the cost
    // O( runs ), which for `LIVE` is one.
    if let yesno_core::Container::Run(r) = container {
        for i in 0..r.nruns() {
            fill_range(out, r.start(i) as usize, r.end(i) as usize);
        }
        return r.nruns() > 0;
    }
    // What is left is an array container, which genuinely is sparse: the scatter
    // is both correct and faster than materializing a bitmap to copy out of.
    let mut any = false;
    for v in container.iter() {
        let b = v as usize;
        out[b >> 6] |= 1u64 << (b & 63);
        any = true;
    }
    any
}

/// Set bits `lo..=hi` of a block mask, a word at a time.
///
/// Both ends are inclusive because that is what `RunContainer::end` returns.
#[inline]
fn fill_range(out: &mut crate::slice::BlockMask, lo: usize, hi: usize) {
    debug_assert!(lo <= hi && hi < 65_536);
    let (wl, wh) = (lo >> 6, hi >> 6);
    let head = u64::MAX << (lo & 63);
    let tail = u64::MAX >> (63 - (hi & 63));
    if wl == wh {
        out[wl] |= head & tail;
        return;
    }
    out[wl] |= head;
    out[wl + 1..wh].fill(u64::MAX);
    out[wh] |= tail;
}
